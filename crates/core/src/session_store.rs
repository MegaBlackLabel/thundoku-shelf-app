//! ストアのセッション（BOOTH / FANZA / DLsite）の暗号化保存。
//!
//! これらのセッション Cookie は Windows Credential Manager の上限（2560 UTF-16 文字）を
//! 超えることがあるため、アプリ DB（`app_settings`）に置いている。しかし平文で置くと、
//! DB のコピー・バックアップ・サポートへのファイル添付からセッションを再利用される。
//!
//! そこで鍵だけを keyring の専用スロット（`thundoku-shelf.session-key`）に置き、値は
//! AES-256-GCM（AAD に用途名 = サービス + 形式版）で暗号化して `enc:v1:` を前置して
//! 保存する。**復号できない値（旧平文・改ざん・別鍵）は未ログインとして扱い、行を
//! 削除する**（平文へのフォールバックは禁止）。
//!
//! 併せて「ログアウトしたのに DB の削除に失敗した」セッションを次回起動で復元しない
//! ための印を、DB とは別の場所（データディレクトリのファイル）に残す。

use std::path::{Path, PathBuf};

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use rand::RngCore;

use crate::db::{SqlitePool, settings};
use crate::secrets::{SecretError, SecretStore};

/// 保存値の形式版。AAD に混ぜるので、形式を変えたら別物として扱われる。
const FORMAT_VERSION: u32 = 1;
/// 暗号化された値の接頭辞。**これが無い値は旧平文として拒否する**。
const PREFIX: &str = "enc:v1:";

#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    #[error("secret store error: {0}")]
    Secret(#[from] SecretError),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("session encoding error: {0}")]
    Encoding(String),
    #[error("session encryption error: {0}")]
    Crypto(String),
    #[error("session decryption failed")]
    Decrypt,
}

/// 暗号化して保存するセッションの種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreSession {
    Booth,
    Fanza,
    Dlsite,
}

impl StoreSession {
    /// `app_settings` のキー。
    pub fn settings_key(self) -> &'static str {
        match self {
            Self::Booth => "booth.session",
            Self::Fanza => "fanza.session",
            Self::Dlsite => "dlsite.session",
        }
    }

    /// ログ・利用者向け表示に使う短い名前。
    pub fn label(self) -> &'static str {
        match self {
            Self::Booth => "booth",
            Self::Fanza => "fanza",
            Self::Dlsite => "dlsite",
        }
    }

    /// AAD に混ぜる用途名。**別のサービスへコピーされた暗号文は復号できなくなる。**
    fn purpose(self) -> String {
        format!("thundoku-shelf/session/v{FORMAT_VERSION}/{}", self.label())
    }
}

/// セッションの暗号化保存・復元・削除。
pub struct SessionVault {
    key: [u8; 32],
    marker: PurgeMarker,
}

impl SessionVault {
    /// keyring から鍵を取得して作る。鍵が無ければ新規生成される。
    /// keyring が使えない環境では `Err`（＝セッションを保存しない）。
    pub fn new(secrets: &SecretStore, data_dir: &Path) -> Result<Self, SessionError> {
        Ok(Self {
            key: secrets.session_key()?,
            marker: PurgeMarker::new(data_dir),
        })
    }

    /// 暗号化して `app_settings` に保存する。
    pub fn save<T: serde::Serialize>(
        &self,
        pool: &SqlitePool,
        store: StoreSession,
        value: &T,
    ) -> Result<(), SessionError> {
        let json = serde_json::to_vec(value).map_err(|e| SessionError::Encoding(e.to_string()))?;
        let blob = self.encrypt(store, &json)?;
        settings::set(pool, store.settings_key(), &blob)?;
        // 保存できた = もう「削除できなかった」状態ではない。
        self.marker.clear(store.label());
        Ok(())
    }

    /// 復元する。**復号できない値は未ログインとして扱い、行も削除する。**
    pub fn load<T: serde::de::DeserializeOwned>(
        &self,
        pool: &SqlitePool,
        store: StoreSession,
    ) -> Option<T> {
        // 前回のログアウトで削除に失敗したセッションは復元しない。
        if self.marker.is_pending(store.label()) {
            let _ = settings::delete(pool, store.settings_key());
            if settings::get(pool, store.settings_key())
                .ok()
                .flatten()
                .is_none()
            {
                self.marker.clear(store.label());
            }
            log::warn!(
                "{} session: 前回のログアウトで削除できなかったため復元しません",
                store.label()
            );
            return None;
        }
        let raw = settings::get(pool, store.settings_key()).ok().flatten()?;
        match self.decrypt(store, &raw) {
            Ok(bytes) => match serde_json::from_slice::<T>(&bytes) {
                Ok(value) => Some(value),
                Err(error) => {
                    log::warn!(
                        "{} session: 復号はできたが内容を解釈できないため破棄します: {error}",
                        store.label()
                    );
                    let _ = settings::delete(pool, store.settings_key());
                    None
                }
            },
            Err(error) => {
                // 旧平文・改ざん・別鍵。平文へはフォールバックせず、残骸も消す。
                log::warn!(
                    "{} session: 保存値を復号できないため破棄します（再ログインが必要）: {error}",
                    store.label()
                );
                let _ = settings::delete(pool, store.settings_key());
                None
            }
        }
    }

    /// ログアウト時の削除。失敗したら「次回起動で復元しない」印を残す。
    pub fn clear(&self, pool: &SqlitePool, store: StoreSession) -> Result<(), SessionError> {
        match settings::delete(pool, store.settings_key()) {
            Ok(()) => {
                self.marker.clear(store.label());
                Ok(())
            }
            Err(error) => {
                // DB が読取専用・破損などで消せない場合、次回起動で復元されてしまう。
                // DB とは別の場所（データディレクトリのファイル）に印を残す。
                if let Err(marker_error) = self.marker.mark(store.label()) {
                    log::error!(
                        "{} session: 削除にも印の記録にも失敗しました: {marker_error}",
                        store.label()
                    );
                }
                Err(SessionError::Db(error))
            }
        }
    }

    fn encrypt(&self, store: StoreSession, plaintext: &[u8]) -> Result<String, SessionError> {
        let cipher = Aes256Gcm::new_from_slice(&self.key)
            .map_err(|e| SessionError::Crypto(e.to_string()))?;
        let mut iv = [0u8; 12];
        rand::rngs::OsRng.fill_bytes(&mut iv);
        let aad = store.purpose();
        let ciphertext = cipher
            .encrypt(
                Nonce::from_slice(&iv),
                Payload {
                    msg: plaintext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|e| SessionError::Crypto(e.to_string()))?;
        let mut out = Vec::with_capacity(12 + ciphertext.len());
        out.extend_from_slice(&iv);
        out.extend_from_slice(&ciphertext);
        Ok(format!("{PREFIX}{}", B64.encode(&out)))
    }

    fn decrypt(&self, store: StoreSession, raw: &str) -> Result<Vec<u8>, SessionError> {
        let encoded = raw.strip_prefix(PREFIX).ok_or(SessionError::Decrypt)?;
        let data = B64
            .decode(encoded.trim())
            .map_err(|_| SessionError::Decrypt)?;
        if data.len() < 12 {
            return Err(SessionError::Decrypt);
        }
        let (iv, ciphertext) = data.split_at(12);
        let cipher = Aes256Gcm::new_from_slice(&self.key)
            .map_err(|e| SessionError::Crypto(e.to_string()))?;
        let aad = store.purpose();
        cipher
            .decrypt(
                Nonce::from_slice(iv),
                Payload {
                    msg: ciphertext,
                    aad: aad.as_bytes(),
                },
            )
            .map_err(|_| SessionError::Decrypt)
    }
}

/// 「ログアウトしたのに保存値を消せなかった」印。
///
/// 削除が失敗する状況（DB が読取専用・破損・ロック、keyring の拒否）では同じ領域への
/// 書き込みも期待できないため、**データディレクトリのファイル**に記録する
/// （障害領域を分ける）。次回起動時にこの印を見て「復元しない」と判断する。
pub struct PurgeMarker {
    path: PathBuf,
}

const MARKER_FILE: &str = "session-purge.pending";

impl PurgeMarker {
    pub fn new(data_dir: &Path) -> Self {
        Self {
            path: data_dir.join(MARKER_FILE),
        }
    }

    /// 印が付いているスロット（サービス名）の一覧。
    pub fn pending(&self) -> Vec<String> {
        std::fs::read_to_string(&self.path)
            .map(|text| {
                text.lines()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn is_pending(&self, slot: &str) -> bool {
        self.pending().iter().any(|name| name == slot)
    }

    /// 印を付ける（同じスロットに重複して付けない）。
    pub fn mark(&self, slot: &str) -> std::io::Result<()> {
        if self.is_pending(slot) {
            return Ok(());
        }
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut text = self.pending().join("\n");
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(slot);
        text.push('\n');
        std::fs::write(&self.path, text)
    }

    /// 印を外す（他スロットの印は残す。空になったらファイルごと消す）。
    pub fn clear(&self, slot: &str) {
        let remaining: Vec<String> = self
            .pending()
            .into_iter()
            .filter(|name| name != slot)
            .collect();
        if remaining.is_empty() {
            let _ = std::fs::remove_file(&self.path);
            return;
        }
        let mut text = remaining.join("\n");
        text.push('\n');
        let _ = std::fs::write(&self.path, text);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{block_on, test_pool};

    fn vault(dir: &Path) -> SessionVault {
        SecretStore::use_memory_backend();
        SessionVault::new(&SecretStore::new(), dir).expect("vault")
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "thundoku-session-store-{tag}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[derive(Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
    struct FakeSession {
        cookies: std::collections::BTreeMap<String, String>,
    }

    fn fake_session() -> FakeSession {
        FakeSession {
            cookies: [(
                "__DLsite_SID".to_string(),
                "secret-cookie-value".to_string(),
            )]
            .into_iter()
            .collect(),
        }
    }

    fn stored(pool: &SqlitePool, store: StoreSession) -> Option<String> {
        settings::get(pool, store.settings_key()).unwrap()
    }

    /// 保存した値は復元でき、DB には暗号文しか残らない。
    #[test]
    fn session_round_trips_and_is_encrypted_at_rest() {
        let dir = temp_dir("roundtrip");
        let pool = test_pool();
        let vault = vault(&dir);

        vault
            .save(&pool, StoreSession::Booth, &fake_session())
            .unwrap();
        let raw = stored(&pool, StoreSession::Booth).expect("保存されている");
        assert!(raw.starts_with(PREFIX), "形式の接頭辞が無い: {raw}");
        assert!(
            !raw.contains("secret-cookie-value"),
            "平文の Cookie が DB に残っている: {raw}"
        );

        let loaded: FakeSession = vault.load(&pool, StoreSession::Booth).expect("復元できる");
        assert_eq!(loaded, fake_session());
    }

    /// 旧バージョンが保存した平文は復元せず、行ごと削除する（平文へ戻らない）。
    #[test]
    fn legacy_plaintext_is_refused_and_removed() {
        let dir = temp_dir("legacy");
        let pool = test_pool();
        let vault = vault(&dir);

        let plaintext = serde_json::to_string(&fake_session()).unwrap();
        settings::set(&pool, StoreSession::Booth.settings_key(), &plaintext).unwrap();

        let loaded: Option<FakeSession> = vault.load(&pool, StoreSession::Booth);
        assert!(loaded.is_none(), "旧平文を復元してはいけない");
        assert!(
            stored(&pool, StoreSession::Booth).is_none(),
            "旧平文を残してはいけない"
        );
    }

    /// 改ざんされた暗号文は復元せず、行ごと削除する。
    #[test]
    fn tampered_blob_is_refused_and_removed() {
        let dir = temp_dir("tamper");
        let pool = test_pool();
        let vault = vault(&dir);
        vault
            .save(&pool, StoreSession::Fanza, &fake_session())
            .unwrap();

        let raw = stored(&pool, StoreSession::Fanza).unwrap();
        let mut bytes = B64.decode(raw.strip_prefix(PREFIX).unwrap()).unwrap();
        let last = bytes.len() - 1;
        bytes[last] ^= 0xff;
        settings::set(
            &pool,
            StoreSession::Fanza.settings_key(),
            &format!("{PREFIX}{}", B64.encode(&bytes)),
        )
        .unwrap();

        let loaded: Option<FakeSession> = vault.load(&pool, StoreSession::Fanza);
        assert!(loaded.is_none(), "改ざんされた値を復元してはいけない");
        assert!(stored(&pool, StoreSession::Fanza).is_none());
    }

    /// 別サービスの用途名で作られた暗号文は復号できない（AAD の束縛）。
    #[test]
    fn aad_binds_the_blob_to_its_service() {
        let dir = temp_dir("aad");
        let pool = test_pool();
        let vault = vault(&dir);

        vault
            .save(&pool, StoreSession::Dlsite, &fake_session())
            .unwrap();
        let raw = stored(&pool, StoreSession::Dlsite).unwrap();
        // Dlsite 用の暗号文を booth のキーへ差し替える
        settings::set(&pool, StoreSession::Booth.settings_key(), &raw).unwrap();

        let loaded: Option<FakeSession> = vault.load(&pool, StoreSession::Booth);
        assert!(loaded.is_none(), "別サービスの暗号文を復号してはいけない");
        assert!(
            stored(&pool, StoreSession::Dlsite).is_some(),
            "別キーの値を消してはいけない"
        );
    }

    /// ログアウト（削除）で行が消える。
    #[test]
    fn clear_removes_the_row() {
        let dir = temp_dir("clear");
        let pool = test_pool();
        let vault = vault(&dir);
        vault
            .save(&pool, StoreSession::Booth, &fake_session())
            .unwrap();

        vault.clear(&pool, StoreSession::Booth).unwrap();
        assert!(stored(&pool, StoreSession::Booth).is_none());
        assert!(!vault.marker.is_pending("booth"));
    }

    /// 削除に失敗したら印を残し、次回起動では（行が残っていても）復元しない。
    #[test]
    fn failed_purge_is_not_restored_on_the_next_start() {
        let dir = temp_dir("purge");
        let pool = test_pool();
        let vault = vault(&dir);
        vault
            .save(&pool, StoreSession::Booth, &fake_session())
            .unwrap();

        // DB を閉じて削除を失敗させる（読取専用・破損の代用）。
        block_on(pool.close());
        let error = vault.clear(&pool, StoreSession::Booth);
        assert!(error.is_err(), "削除に失敗したことを報告する");
        assert!(
            vault.marker.is_pending("booth"),
            "次回起動用の印が残っていない"
        );

        // 行が残っている状態（別の接続で書き戻す）でも復元しない。
        let pool = test_pool();
        vault
            .save(&pool, StoreSession::Booth, &fake_session())
            .unwrap();
        vault.marker.mark("booth").unwrap();
        let loaded: Option<FakeSession> = vault.load(&pool, StoreSession::Booth);
        assert!(loaded.is_none(), "削除に失敗したセッションを復元している");
        assert!(
            stored(&pool, StoreSession::Booth).is_none(),
            "復元しないだけでなく行も消す"
        );
        assert!(!vault.marker.is_pending("booth"), "消せたので印は外す");
    }

    /// 印は他のサービスのログアウトを巻き込まない。
    #[test]
    fn purge_marker_is_per_service() {
        let dir = temp_dir("marker");
        let marker = PurgeMarker::new(&dir);
        marker.mark("booth").unwrap();
        marker.mark("booth").unwrap();
        assert_eq!(marker.pending(), vec!["booth".to_string()]);

        marker.clear("fanza");
        assert!(marker.is_pending("booth"));
        marker.clear("booth");
        assert!(!marker.is_pending("booth"));
        assert!(!marker.path.exists(), "空になったらファイルを消す");
    }
}
