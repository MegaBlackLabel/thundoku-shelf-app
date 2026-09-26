//! ストアのセッション（BOOTH / FANZA / DLsite）の暗号化保存。
//!
//! これらのセッション Cookie は Windows Credential Manager の上限（2560 UTF-16 文字）を
//! 超えることがあるため、アプリ DB（`app_settings`）に置いている。しかし平文で置くと、
//! DB のコピー・バックアップ・サポートへのファイル添付からセッションを再利用される。
//!
//! そこで鍵だけを keyring の専用スロット（`thundoku-shelf.session-key`）に置き、値は
//! AES-256-GCM（AAD に用途名 = サービス + 形式版）で暗号化し、**保存時刻を暗号文の中に
//! 包んで** `enc:v2:` を前置して保存する。**復号できない値（旧平文・改ざん・別鍵）と
//! 保存から 7 日を過ぎた値は未ログインとして扱い、行を削除する**（平文へのフォールバックは
//! 禁止）。
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
const FORMAT_VERSION: u32 = 2;
/// 暗号化された値の接頭辞。**これが無い値は旧平文として拒否する**。
///
/// v1 → v2 で「保存時刻」を暗号文の中に入れた（DB を書き換えても期限を延ばせない）。
/// v1 の値は接頭辞が違うため復号できず、未ログイン扱いで破棄される（＝再ログイン）。
const PREFIX: &str = "enc:v2:";

/// 保存したセッションの有効期限（秒）。これより古い値は復元せず、行ごと破棄する
/// （＝再ログイン）。長く持つほど、端末の共有・売却やバックアップ流出時の影響が伸びる。
/// 7 日 = 週 1 回の再ログインで済む線（README にも記載）。
pub const SESSION_MAX_AGE_SECONDS: i64 = 7 * 24 * 60 * 60;

/// 現在時刻（Unix 秒）。期限判定に使う。
fn now_unix() -> i64 {
    chrono::Utc::now().timestamp()
}

/// 保存する値の包み。**保存時刻を暗号文の中に入れる**ので、DB を書き換えても
/// 期限を延ばせない（改ざんは GCM のタグで検出される）。
#[derive(serde::Serialize, serde::Deserialize)]
struct Envelope<T> {
    /// 保存した時刻（Unix 秒）。
    saved_at: i64,
    session: T,
}

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
    /// 技術書典（techbookfest.org）。旧版は keyring に期限なしで置いていたため、
    /// [`SessionVault::adopt_legacy`] で vault へ移行する。
    Techbookfest,
}

impl StoreSession {
    /// `app_settings` のキー。
    pub fn settings_key(self) -> &'static str {
        match self {
            Self::Booth => "booth.session",
            Self::Fanza => "fanza.session",
            Self::Dlsite => "dlsite.session",
            Self::Techbookfest => "tbf.session",
        }
    }

    /// ログ・利用者向け表示に使う短い名前。
    pub fn label(self) -> &'static str {
        match self {
            Self::Booth => "booth",
            Self::Fanza => "fanza",
            Self::Dlsite => "dlsite",
            Self::Techbookfest => "tbf",
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

    /// 旧版が keyring に置いていた保存値（**期限なしの平文 JSON**）を vault へ一度だけ
    /// 移してから復元する。技術書典がこの経路（セキュリティ評価 2026-09-25 の F05）。
    ///
    /// - vault に値があるときは移行しない（vault のほうが新しい）。keyring の旧値は消すだけ。
    /// - keyring の削除に失敗したら印を残す（＝次の起動では復元しない。期限の無い旧値を
    ///   残したまま使い続けると「保存は 7 日」という約束を破るため、再ログインを求める）。
    /// - 印が付いている起動では移行し直さず、keyring の削除を再試行するだけにする
    ///   （消せていない旧値は使わない。成功したら印を外す）。
    pub fn adopt_legacy<T: serde::Serialize + serde::de::DeserializeOwned>(
        &self,
        secrets: &SecretStore,
        pool: &SqlitePool,
        store: StoreSession,
        legacy_key: &str,
    ) -> Option<T> {
        self.adopt_legacy_value::<T>(secrets, pool, store, legacy_key);
        self.load(pool, store)
    }

    /// [`Self::adopt_legacy`] の移行部分（復元はしない）。
    fn adopt_legacy_value<T: serde::Serialize + serde::de::DeserializeOwned>(
        &self,
        secrets: &SecretStore,
        pool: &SqlitePool,
        store: StoreSession,
        legacy_key: &str,
    ) {
        let label = store.label();
        let Ok(Some(raw)) = secrets.load(legacy_key) else {
            // 旧値は無い（移行が済んだ端末はこちら）。
            return;
        };
        if self.marker.is_pending(label) {
            // 前回のログアウトで消せなかった残骸。**移行し直さない**（期限の管理外の値を
            // 再利用しない）。消せたら印を外す。
            match secrets.delete(legacy_key) {
                Ok(()) => {
                    self.marker.clear(label);
                    log::info!("{label} session: 前回消せなかった旧 keyring 値を削除しました");
                }
                Err(error) => log::warn!(
                    "{label} session: 旧 keyring 値を削除できません（次回起動で再試行）: {error}"
                ),
            }
            return;
        }
        // vault に値があればそちらが新しい。旧値は移行せずに消すだけ。
        let vault_empty = settings::get(pool, store.settings_key())
            .ok()
            .flatten()
            .is_none();
        if vault_empty {
            match serde_json::from_str::<T>(&raw) {
                Ok(session) => match self.save(pool, store, &session) {
                    Ok(()) => {
                        log::info!("{label} session: keyring の旧保存値を vault へ移行しました")
                    }
                    Err(error) => log::warn!("{label} session: 旧保存値を移行できません: {error}"),
                },
                Err(error) => log::warn!(
                    "{label} session: keyring の旧保存値を解釈できません（破棄）: {error}"
                ),
            }
        }
        match secrets.delete(legacy_key) {
            Ok(()) => {}
            Err(error) => {
                // 期限の管理外の資格情報が端末に残る。次の起動では復元しない。
                log::warn!("{label} session: keyring の旧保存値を削除できません: {error}");
                if let Err(marker_error) = self.marker.mark(label) {
                    log::error!("{label} session: 印の記録にも失敗しました: {marker_error}");
                }
            }
        }
    }

    /// 暗号化して `app_settings` に保存する（保存時刻を包んで期限判定に使う）。
    pub fn save<T: serde::Serialize>(
        &self,
        pool: &SqlitePool,
        store: StoreSession,
        value: &T,
    ) -> Result<(), SessionError> {
        self.save_at(pool, store, value, now_unix())
    }

    /// 保存時刻を指定して保存する（テスト用の入口。`save` は現在時刻を渡す）。
    fn save_at<T: serde::Serialize>(
        &self,
        pool: &SqlitePool,
        store: StoreSession,
        value: &T,
        saved_at: i64,
    ) -> Result<(), SessionError> {
        let envelope = Envelope {
            saved_at,
            session: value,
        };
        let json =
            serde_json::to_vec(&envelope).map_err(|e| SessionError::Encoding(e.to_string()))?;
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
            Ok(bytes) => match serde_json::from_slice::<Envelope<T>>(&bytes) {
                Ok(envelope) => {
                    // 期限は暗号文の中の保存時刻で判定する（DB を書き換えても延ばせない）。
                    let age = now_unix() - envelope.saved_at;
                    if !(0..=SESSION_MAX_AGE_SECONDS).contains(&age) {
                        log::info!(
                            "{} session: 保存から {age} 秒たっているため破棄します（期限 {} 秒・再ログインが必要）",
                            store.label(),
                            SESSION_MAX_AGE_SECONDS
                        );
                        let _ = settings::delete(pool, store.settings_key());
                        return None;
                    }
                    Some(envelope.session)
                }
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

    /// 期限（`SESSION_MAX_AGE_SECONDS`）を過ぎたセッションは復元せず、行も残さない。
    #[test]
    fn expired_session_is_refused_and_removed() {
        let dir = temp_dir("expired");
        let pool = test_pool();
        let vault = vault(&dir);

        let saved_at = now_unix() - SESSION_MAX_AGE_SECONDS - 1;
        vault
            .save_at(&pool, StoreSession::Fanza, &fake_session(), saved_at)
            .unwrap();

        let loaded: Option<FakeSession> = vault.load(&pool, StoreSession::Fanza);
        assert!(loaded.is_none(), "期限切れを復元してはいけない");
        assert!(
            stored(&pool, StoreSession::Fanza).is_none(),
            "期限切れの行が残っている"
        );
    }

    /// 期限内なら復元できる（境界: ちょうど期限はまだ有効）。
    #[test]
    fn session_within_the_retention_period_is_restored() {
        let dir = temp_dir("within");
        let pool = test_pool();
        let vault = vault(&dir);

        vault
            .save_at(
                &pool,
                StoreSession::Fanza,
                &fake_session(),
                now_unix() - SESSION_MAX_AGE_SECONDS,
            )
            .unwrap();

        let loaded: FakeSession = vault
            .load(&pool, StoreSession::Fanza)
            .expect("期限内は復元できる");
        assert_eq!(loaded, fake_session());
    }

    /// 未来の保存時刻（改ざん・時計ずれ）は復元しない。
    #[test]
    fn session_saved_in_the_future_is_refused() {
        let dir = temp_dir("future");
        let pool = test_pool();
        let vault = vault(&dir);

        vault
            .save_at(
                &pool,
                StoreSession::Dlsite,
                &fake_session(),
                now_unix() + 3600,
            )
            .unwrap();

        let loaded: Option<FakeSession> = vault.load(&pool, StoreSession::Dlsite);
        assert!(loaded.is_none(), "未来の保存時刻を復元してはいけない");
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

    /// 技術書典の保存値にも他のストアと同じ期限（7 日）が効く。
    ///
    /// 旧版は keyring へ期限なしで置いていた（セキュリティ評価 2026-09-25 の F05）。
    /// 移行先の `StoreSession::Techbookfest` が期限判定の経路に載っていることを固定する。
    #[test]
    fn techbookfest_session_expires_like_the_other_stores() {
        let dir = temp_dir("tbf-expired");
        let pool = test_pool();
        let vault = vault(&dir);

        vault
            .save_at(
                &pool,
                StoreSession::Techbookfest,
                &fake_session(),
                now_unix() - SESSION_MAX_AGE_SECONDS - 1,
            )
            .unwrap();

        let loaded: Option<FakeSession> = vault.load(&pool, StoreSession::Techbookfest);
        assert!(
            loaded.is_none(),
            "期限切れの技術書典セッションを復元してはいけない"
        );
        assert!(
            stored(&pool, StoreSession::Techbookfest).is_none(),
            "期限切れの行が残っている"
        );
    }

    /// 旧版の keyring 保存値（期限なし）を vault へ移し、keyring からは消す。
    #[test]
    fn legacy_keyring_value_is_adopted_into_the_vault() {
        let dir = temp_dir("legacy-adopt");
        let pool = test_pool();
        let vault = vault(&dir);
        let secrets = SecretStore::new();
        let legacy_key = "legacy-techbookfest";
        let plaintext = serde_json::to_string(&fake_session()).unwrap();
        secrets.save(legacy_key, &plaintext).unwrap();

        let adopted: Option<FakeSession> =
            vault.adopt_legacy(&secrets, &pool, StoreSession::Techbookfest, legacy_key);

        assert_eq!(adopted, Some(fake_session()), "移行した値を復元できない");
        let raw = stored(&pool, StoreSession::Techbookfest).expect("vault へ移行されている");
        assert!(raw.starts_with(PREFIX), "平文のまま置いている: {raw}");
        assert!(
            !raw.contains("secret-cookie-value"),
            "平文の Cookie が DB に残っている: {raw}"
        );
        assert_eq!(
            secrets.load(legacy_key).unwrap(),
            None,
            "keyring に旧値が残っている"
        );
    }

    /// vault に値があるときは keyring の旧値で上書きしない（vault のほうが新しい）。
    #[test]
    fn legacy_keyring_value_does_not_replace_a_newer_vault_value() {
        let dir = temp_dir("legacy-newer");
        let pool = test_pool();
        let vault = vault(&dir);
        let secrets = SecretStore::new();
        let legacy_key = "legacy-techbookfest-newer";

        let fresh = FakeSession {
            cookies: [("session".to_string(), "new".to_string())]
                .into_iter()
                .collect(),
        };
        let stale = FakeSession {
            cookies: [("session".to_string(), "old".to_string())]
                .into_iter()
                .collect(),
        };
        vault
            .save(&pool, StoreSession::Techbookfest, &fresh)
            .unwrap();
        secrets
            .save(legacy_key, &serde_json::to_string(&stale).unwrap())
            .unwrap();

        let adopted: Option<FakeSession> =
            vault.adopt_legacy(&secrets, &pool, StoreSession::Techbookfest, legacy_key);

        assert_eq!(adopted, Some(fresh), "古い keyring 値で上書きしている");
        // 使わない旧値は残さない（期限の管理外の資格情報を端末に残さない）。
        assert_eq!(secrets.load(legacy_key).unwrap(), None);
    }

    /// 前回のログアウトで旧 keyring 値を消せなかった端末（印あり）では、旧値を
    /// **移行し直さない**（期限の管理外の値を再利用しない）。消せたら印を外す。
    #[test]
    fn legacy_keyring_value_is_not_adopted_while_its_purge_is_pending() {
        let dir = temp_dir("legacy-pending");
        let pool = test_pool();
        let vault = vault(&dir);
        let secrets = SecretStore::new();
        let legacy_key = "legacy-techbookfest-pending";

        vault.marker.mark(StoreSession::Techbookfest.label()).unwrap();
        secrets
            .save(legacy_key, &serde_json::to_string(&fake_session()).unwrap())
            .unwrap();

        let adopted: Option<FakeSession> =
            vault.adopt_legacy(&secrets, &pool, StoreSession::Techbookfest, legacy_key);

        assert!(adopted.is_none(), "印があるのに旧値を移行（再利用）している");
        assert!(
            stored(&pool, StoreSession::Techbookfest).is_none(),
            "印があるのに vault へ書き込んでいる"
        );
        assert_eq!(
            secrets.load(legacy_key).unwrap(),
            None,
            "削除の再試行をしていない"
        );
        assert!(
            !vault.marker.is_pending(StoreSession::Techbookfest.label()),
            "削除できたのに印が残っている"
        );
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
