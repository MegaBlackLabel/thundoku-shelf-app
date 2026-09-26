//! v3 の鍵階層 — アカウントごとの乱数ルート鍵（PRK）、そのラップ、Drive に置く
//! `thundoku-keys.json`（[`PackKeyBundle`]）。仕様は `docs/spec/10-pack-keys.md`
//! §2〜§5 で、数値・文字列・バイト列は **TS 版とのバイト一致が要件**。
//!
//! ```text
//! PRK (32B 乱数)
//!   └─ pack_key(pack_id) = HKDF-SHA256(ikm = PRK, salt = UTF8(pack_id),
//!                                      info = "opfspack-entry-key")
//! PRK の保管 = ラップ = AES-256-GCM(KEK)(PRK)
//!   KEK_sub        = PBKDF2-SHA256(UTF8(sub) ‖ APP_SALT, APP_SALT, 100_000)
//!   KEK_passphrase = PBKDF2-SHA256(NFKC(passphrase), salt, iterations)
//!   ラップの AAD    = UTF8("thundoku-pack-root:1:" + owner_id)
//! ```
//!
//! §11 のメタデータバックアップ（`thundoku-backup.json` v3）も同じ PRK から導出する
//! （[`BackupEnvelope`]）:
//!
//! ```text
//! backup_cipher_key = HKDF-SHA256(ikm = PRK, salt = b"thundoku-backup:v1",
//!                                 info = b"thundoku-backup-key")
//! backup_hash_key   = HKDF-SHA256(ikm = PRK, salt = b"thundoku-backup:v1",
//!                                 info = b"thundoku-backup-hash")
//! 封筒の AAD         = UTF8("thundoku-backup:3:" + owner_id)
//! ```

use base64::Engine;
use base64::engine::general_purpose::STANDARD as BASE64;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use unicode_normalization::UnicodeNormalization;

use crate::{APP_SALT, PackError, SUB_WRAP_ITERATIONS};

/// `thundoku-keys.json` の `format_version`（pack の `FORMAT_VERSION` とは別物）。
pub const KEY_BUNDLE_FORMAT_VERSION: u32 = 1;

/// ラップの AAD 接頭辞。`owner_id` を連結して使う（別アカウントのラップへの
/// 差し替えを検出する）。
const WRAP_AAD_PREFIX: &str = "thundoku-pack-root:1:";

/// 現在唯一の KDF 識別子（`kdf` フィールド）。
const KDF_PBKDF2_SHA256: &str = "pbkdf2-sha256";

/// パスフレーズラップの salt 長（仕様 §3.2 の「base64 16B 乱数」）。
/// パスフレーズラップの `iterations` に許容する上限（1000 万回）。
///
/// 鍵ファイルは同期先（Drive）からも来るため、相手が `iterations` を書き換えられる。
/// 上限が無いと、復元しようとした利用者の CPU を何時間も焼かせられる（セキュリティ評価
/// F06）。アプリが書く値は 10 万回なので、将来の引き上げ余地を 100 倍残してこれを天井にする。
pub const MAX_PASSPHRASE_ITERATIONS: u32 = 10_000_000;

const PASSPHRASE_SALT_LEN: usize = 16;

/// AES-GCM nonce 長。
const NONCE_LEN: usize = 12;

/// ラップされた PRK の長さ（32B + 16B タグ・仕様 §3.2）。
const WRAP_CIPHERTEXT_LEN: usize = 32 + 16;

/// Drive に置くメタデータバックアップ（`thundoku-backup.json`）の v3 封筒の
/// `format_version`（仕様 §11）。v2 以前は平文のバックアップ JSON。
pub const BACKUP_FORMAT_VERSION: u32 = 3;

/// 封筒の AAD 接頭辞（`"{AAD}:{format_version}:{owner_id}"` を AAD にする）。
/// 別アカウントの封筒・別版の封筒への差し替えを検出する。
const BACKUP_AAD_PREFIX: &str = "thundoku-backup";

/// バックアップ鍵の HKDF salt（PRK から用途分離した 2 本の鍵を作る）。
const BACKUP_KDF_SALT: &[u8] = b"thundoku-backup:v1";

/// 封筒の暗号鍵の HKDF info。
const BACKUP_CIPHER_INFO: &[u8] = b"thundoku-backup-key";

/// 平文の HMAC 鍵の HKDF info。
const BACKUP_HASH_INFO: &[u8] = b"thundoku-backup-hash";

/// 封筒の `encryption.alg`（唯一の値）。
pub const BACKUP_ALG: &str = "aes-256-gcm";

/// 封筒の `encryption.kdf`（唯一の値）。
pub const BACKUP_KDF: &str = "hkdf-sha256";

/// 封筒の暗号文の最小長（AES-GCM のタグ 16B）。
const BACKUP_TAG_LEN: usize = 16;

/// 封筒の AAD バイト列。
fn backup_aad(owner_id: &str) -> Vec<u8> {
    format!("{BACKUP_AAD_PREFIX}:{BACKUP_FORMAT_VERSION}:{owner_id}").into_bytes()
}

/// HMAC-SHA256（RFC 2104。`backup_envelope.rs` のベクタで固定している）。
fn hmac_sha256(key: &[u8], message: &[u8]) -> [u8; 32] {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    mac.update(message);
    mac.finalize().into_bytes().into()
}

/// 定数時間比較（HMAC の検証に使う。早期 return で一致位置を漏らさない）。
fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }
    let mut diff = 0u8;
    for (a, b) in left.iter().zip(right) {
        diff |= a ^ b;
    }
    diff == 0
}

/// `sub` ラップの KEK = v2 の master key と同式
/// （`PBKDF2-SHA256(password = UTF8(sub) ‖ APP_SALT, salt = APP_SALT, 100_000)`）。
/// Web 版の既存 `deriveMasterKey` をそのまま `deriveSubWrapKek` として使える。
pub fn sub_wrap_kek(sub: &str) -> [u8; 32] {
    let mut password = sub.as_bytes().to_vec();
    password.extend_from_slice(APP_SALT);
    crate::crypto::pbkdf2_sha256(&password, APP_SALT, SUB_WRAP_ITERATIONS)
}

/// パスフレーズの KEK。**NFKC 正規化してから UTF-8 化**する（仕様 §2。IME /
/// OS による合成文字の差で同じパスフレーズが別鍵にならないようにする）。
fn passphrase_kek(passphrase: &str, salt: &[u8], iterations: u32) -> [u8; 32] {
    let normalized: String = passphrase.nfkc().collect();
    crate::crypto::pbkdf2_sha256(normalized.as_bytes(), salt, iterations)
}

/// ラップの AAD バイト列。
fn wrap_aad(owner_id: &str) -> Vec<u8> {
    format!("{WRAP_AAD_PREFIX}{owner_id}").into_bytes()
}

/// アカウントごとのルート鍵（PRK）。**鍵材料は乱数**で、`sub` からは導出できない。
/// `Debug` は中身を出さない（ログへの鍵漏洩を防ぐ）。
#[derive(Clone, PartialEq, Eq)]
pub struct PackRootKey([u8; 32]);

impl std::fmt::Debug for PackRootKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PackRootKey(<redacted>)")
    }
}

impl PackRootKey {
    pub fn from_bytes(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// 新しい PRK を `OsRng` で生成する（仕様 §5.1）。
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut bytes);
        Self(bytes)
    }

    /// keyring へ保存する形（標準 base64 + padding）。
    pub fn to_base64(&self) -> String {
        BASE64.encode(self.0)
    }

    /// keyring から読んだ base64 を戻す。長さ・アルファベットが違えば `None`。
    pub fn from_base64(value: &str) -> Option<Self> {
        let bytes: [u8; 32] = BASE64.decode(value.trim()).ok()?.try_into().ok()?;
        Some(Self(bytes))
    }

    /// pack 単位の鍵 = `HKDF-SHA256(ikm = PRK, salt = UTF8(pack_id),
    /// info = "opfspack-entry-key")`（仕様 §2）。
    pub fn derive_pack_key(&self, pack_id: &str) -> PackKey {
        PackKey(crate::crypto::hkdf_sha256(
            &self.0,
            pack_id.as_bytes(),
            crate::crypto::HKDF_INFO,
        ))
    }

    /// メタデータバックアップ（`thundoku-backup.json` v3 の封筒）の暗号鍵
    /// = `HKDF-SHA256(ikm = PRK, salt = b"thundoku-backup:v1",
    /// info = b"thundoku-backup-key")`（仕様 §11）。
    ///
    /// pack 鍵とは別の `info` を使う（同じ PRK から用途ごとに鍵を分ける）。
    pub fn derive_backup_cipher_key(&self) -> [u8; 32] {
        crate::crypto::hkdf_sha256(&self.0, BACKUP_KDF_SALT, BACKUP_CIPHER_INFO)
    }

    /// バックアップ平文の HMAC 鍵
    /// = `HKDF-SHA256(ikm = PRK, salt = b"thundoku-backup:v1",
    /// info = b"thundoku-backup-hash")`（仕様 §11）。
    ///
    /// 変更検知（アップロード要否・復元提案）に使う。暗号文は nonce が乱数で
    /// 毎回変わるため、暗号文の md5 では比較できない。
    pub fn derive_backup_hash_key(&self) -> [u8; 32] {
        crate::crypto::hkdf_sha256(&self.0, BACKUP_KDF_SALT, BACKUP_HASH_INFO)
    }
}

/// 1 つの pack を暗号化する鍵。PRK から `pack_id` 単位で導出する。
/// `Debug` は中身を出さない。
#[derive(Clone, PartialEq, Eq)]
pub struct PackKey([u8; 32]);

impl std::fmt::Debug for PackKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PackKey(<redacted>)")
    }
}

impl PackKey {
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }
}

/// ラップの種類（`thundoku-keys.json` の `wraps[].kind`）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WrapKind {
    /// `sub`（Google アカウントの subject）から導いた KEK。
    Sub,
    /// 利用者が入力するパスフレーズから導いた KEK。
    Passphrase,
}

impl WrapKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Sub => "sub",
            Self::Passphrase => "passphrase",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "sub" => Some(Self::Sub),
            "passphrase" => Some(Self::Passphrase),
            _ => None,
        }
    }
}

/// PRK を KEK で AES-256-GCM ラップしたもの（仕様 §3.2 の `wraps[]` 要素）。
/// 平文は PRK 32 バイトだけなので、`ciphertext` は常に 48 バイト。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackRootKeyWrap {
    kind: WrapKind,
    kdf: String,
    iterations: u32,
    salt: Vec<u8>,
    nonce: [u8; NONCE_LEN],
    ciphertext: Vec<u8>,
}

impl PackRootKeyWrap {
    /// 与えられた KEK・salt・nonce でラップする。決定的なのでテストベクタ
    /// （仕様 §8）の検算に使える。
    pub fn wrap_with_kek(
        root: &PackRootKey,
        kek: &[u8; 32],
        kind: WrapKind,
        salt: Vec<u8>,
        iterations: u32,
        nonce: [u8; NONCE_LEN],
        owner_id: &str,
    ) -> Self {
        let ciphertext = crate::crypto::encrypt_with(&root.0, kek, &nonce, &wrap_aad(owner_id));
        Self {
            kind,
            kdf: KDF_PBKDF2_SHA256.to_owned(),
            iterations,
            salt,
            nonce,
            ciphertext,
        }
    }

    /// `sub` ラップ。KEK は [`sub_wrap_kek`]、salt は `APP_SALT` 固定、
    /// nonce は乱数（仕様 §3.2）。
    ///
    /// `_now_ms` は呼び出し側と対称にするために受け取るが、時刻は bundle の
    /// `created_at` / `updated_at` が持つ（仕様 §3.2）ためここでは使わない。
    pub fn wrap_with_sub(root: &PackRootKey, sub: &str, owner_id: &str, _now_ms: i64) -> Self {
        let mut nonce = [0u8; NONCE_LEN];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        Self::wrap_with_kek(
            root,
            &sub_wrap_kek(sub),
            WrapKind::Sub,
            APP_SALT.to_vec(),
            SUB_WRAP_ITERATIONS,
            nonce,
            owner_id,
        )
    }

    /// パスフレーズラップ。salt は乱数 16B、nonce は乱数 12B、`iterations` は
    /// ラップごとに記録する（復号側は記録値を使う。仕様 §3.2）。
    ///
    /// `_now_ms` の扱いは [`Self::wrap_with_sub`] と同じ。
    pub fn wrap_with_passphrase(
        root: &PackRootKey,
        passphrase: &str,
        owner_id: &str,
        iterations: u32,
        _now_ms: i64,
    ) -> Self {
        let mut salt = vec![0u8; PASSPHRASE_SALT_LEN];
        rand::rngs::OsRng.fill_bytes(&mut salt);
        let mut nonce = [0u8; NONCE_LEN];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        let kek = passphrase_kek(passphrase, &salt, iterations);
        Self::wrap_with_kek(
            root,
            &kek,
            WrapKind::Passphrase,
            salt,
            iterations,
            nonce,
            owner_id,
        )
    }

    pub fn kind(&self) -> WrapKind {
        self.kind
    }

    pub fn iterations(&self) -> u32 {
        self.iterations
    }

    pub fn nonce(&self) -> &[u8; NONCE_LEN] {
        &self.nonce
    }

    pub fn ciphertext(&self) -> &[u8] {
        &self.ciphertext
    }

    /// KEK で PRK を取り出す。KEK が違う・AAD の `owner_id` が違う・復号結果が
    /// 32 バイトでない場合は `None`（平文へは落とさない）。
    pub fn unwrap(&self, kek: &[u8; 32], owner_id: &str) -> Option<PackRootKey> {
        if self.kdf != KDF_PBKDF2_SHA256 {
            return None;
        }
        let plain =
            crate::crypto::decrypt_with(&self.ciphertext, &self.nonce, kek, &wrap_aad(owner_id))
                .ok()?;
        Some(PackRootKey(plain.try_into().ok()?))
    }

    /// パスフレーズで PRK を取り出す。`kind = passphrase` 以外のラップ、
    /// およびパスフレーズが違う場合は `None`（`sub` ラップへ黙って落ちない）。
    pub fn unwrap_with_passphrase(&self, passphrase: &str, owner_id: &str) -> Option<PackRootKey> {
        if self.kind != WrapKind::Passphrase {
            return None;
        }
        self.unwrap(
            &passphrase_kek(passphrase, &self.salt, self.iterations),
            owner_id,
        )
    }
}

/// Drive の `thundoku-keys.json`（仕様 §3.2）。アカウント（`owner_id`）ごとに
/// 1 ファイルで、`wraps` は同じ `kind` を持てない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackKeyBundle {
    format_version: u32,
    owner_id: String,
    created_at: i64,
    updated_at: i64,
    wraps: Vec<PackRootKeyWrap>,
}

impl PackKeyBundle {
    /// `now_ms`（UNIX ミリ秒）を `created_at` / `updated_at` の初期値にする。
    pub fn new(owner_id: String, now_ms: i64) -> Self {
        Self {
            format_version: KEY_BUNDLE_FORMAT_VERSION,
            owner_id,
            created_at: now_ms,
            updated_at: now_ms,
            wraps: Vec::new(),
        }
    }

    pub fn format_version(&self) -> u32 {
        self.format_version
    }

    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    pub fn created_at(&self) -> i64 {
        self.created_at
    }

    pub fn updated_at(&self) -> i64 {
        self.updated_at
    }

    pub fn wraps(&self) -> &[PackRootKeyWrap] {
        &self.wraps
    }

    pub fn wrap(&self, kind: WrapKind) -> Option<&PackRootKeyWrap> {
        self.wraps.iter().find(|wrap| wrap.kind == kind)
    }

    pub fn has_wrap(&self, kind: WrapKind) -> bool {
        self.wrap(kind).is_some()
    }

    /// 同じ `kind` のラップを置換し、無ければ追加する（仕様 §5.1 のマージ規則）。
    /// `updated_at` は同期処理が [`Self::touch`] で更新する。
    pub fn upsert_wrap(&mut self, wrap: PackRootKeyWrap) {
        match self.wraps.iter_mut().find(|w| w.kind == wrap.kind) {
            Some(existing) => *existing = wrap,
            None => self.wraps.push(wrap),
        }
    }

    /// 同じ `kind` のラップを削除する（削除できたら `true`）。
    pub fn remove_wrap(&mut self, kind: WrapKind) -> bool {
        let before = self.wraps.len();
        self.wraps.retain(|wrap| wrap.kind != kind);
        self.wraps.len() != before
    }

    /// `updated_at` を更新する（ラップを触ったときに呼ぶ。仕様 §3.2）。
    pub fn touch(&mut self, now_ms: i64) {
        self.updated_at = now_ms;
    }

    /// `sub` ラップを解いて PRK を返す。ラップが無い・`sub` が違う場合は `None`。
    ///
    /// KEK は仕様 §2 の固定式（`APP_SALT` / `SUB_WRAP_ITERATIONS`）で導出する。
    /// ラップに記録した `salt` / `iterations` はこの経路では使わない
    /// （パスフレーズ側は記録値を使う — §3.2）。
    pub fn unwrap_with_sub(&self, sub: &str) -> Option<PackRootKey> {
        self.wrap(WrapKind::Sub)?
            .unwrap(&sub_wrap_kek(sub), &self.owner_id)
    }

    /// パスフレーズラップを解いて PRK を返す（`kind = passphrase` が無ければ `None`）。
    pub fn unwrap_with_passphrase(&self, passphrase: &str) -> Option<PackRootKey> {
        self.wrap(WrapKind::Passphrase)?
            .unwrap_with_passphrase(passphrase, &self.owner_id)
    }

    /// `thundoku-keys.json` のバイト列（TS `JSON.stringify` と同じ詰めた形）。
    pub fn to_json(&self) -> Result<Vec<u8>, PackError> {
        let wire = WireBundle {
            format_version: self.format_version,
            owner_id: self.owner_id.clone(),
            created_at: self.created_at,
            updated_at: self.updated_at,
            wraps: self.wraps.iter().map(WireWrap::from).collect(),
        };
        serde_json::to_vec(&wire)
            .map_err(|e| PackError::Corrupted(format!("failed to serialize key bundle: {e}")))
    }

    /// `thundoku-keys.json` を読む。`format_version` が 1 以外なら
    /// [`PackError::UnsupportedKeyBundle`]、構造が不正なら
    /// [`PackError::Corrupted`]。
    pub fn from_json(bytes: &[u8]) -> Result<Self, PackError> {
        let value: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|e| PackError::Corrupted(format!("invalid key bundle JSON: {e}")))?;
        // 版だけは構造が違っても先に読む（新しい版を「壊れている」と誤って
        // 報告しないため）。
        let version = value
            .get("format_version")
            .and_then(serde_json::Value::as_u64)
            .and_then(|v| u32::try_from(v).ok())
            .ok_or_else(|| PackError::Corrupted("key bundle has no format_version".into()))?;
        if version != KEY_BUNDLE_FORMAT_VERSION {
            return Err(PackError::UnsupportedKeyBundle(version));
        }
        let wire: WireBundle = serde_json::from_value(value)
            .map_err(|e| PackError::Corrupted(format!("invalid key bundle JSON: {e}")))?;
        let wraps = wire
            .wraps
            .iter()
            .map(WireWrap::to_wrap)
            .collect::<Result<Vec<_>, _>>()?;
        // `kind` の重複は仕様違反（同じ kind は置換されるべきもの）。
        for (index, wrap) in wraps.iter().enumerate() {
            if wraps[..index].iter().any(|other| other.kind == wrap.kind) {
                return Err(PackError::Corrupted(format!(
                    "duplicate wrap kind: {}",
                    wrap.kind.as_str()
                )));
            }
        }
        Ok(Self {
            format_version: version,
            owner_id: wire.owner_id,
            created_at: wire.created_at,
            updated_at: wire.updated_at,
            wraps,
        })
    }
}

#[derive(Serialize, Deserialize)]
struct WireBundle {
    format_version: u32,
    owner_id: String,
    created_at: i64,
    updated_at: i64,
    wraps: Vec<WireWrap>,
}

#[derive(Serialize, Deserialize)]
struct WireWrap {
    kind: String,
    kdf: String,
    iterations: u32,
    salt: String,
    nonce: String,
    ciphertext: String,
}

impl From<&PackRootKeyWrap> for WireWrap {
    fn from(wrap: &PackRootKeyWrap) -> Self {
        Self {
            kind: wrap.kind.as_str().to_owned(),
            kdf: wrap.kdf.clone(),
            iterations: wrap.iterations,
            salt: BASE64.encode(&wrap.salt),
            nonce: BASE64.encode(wrap.nonce),
            ciphertext: BASE64.encode(&wrap.ciphertext),
        }
    }
}

impl WireWrap {
    fn to_wrap(&self) -> Result<PackRootKeyWrap, PackError> {
        let invalid = |detail: &str| PackError::Corrupted(format!("invalid wrap: {detail}"));
        let kind = WrapKind::parse(&self.kind)
            .ok_or_else(|| invalid(&format!("unknown kind {:?}", self.kind)))?;
        if self.kdf != KDF_PBKDF2_SHA256 {
            return Err(invalid(&format!("unknown kdf {:?}", self.kdf)));
        }
        if self.iterations == 0 {
            return Err(invalid("iterations must not be zero"));
        }
        // **復号の前**に弾く（改変された鍵ファイルで PBKDF2 を走らせない）。
        if self.iterations > MAX_PASSPHRASE_ITERATIONS {
            return Err(invalid(&format!(
                "iterations too large: {} > {MAX_PASSPHRASE_ITERATIONS}",
                self.iterations
            )));
        }
        let salt = BASE64
            .decode(&self.salt)
            .map_err(|_| invalid("salt is not base64"))?;
        if salt.is_empty() {
            return Err(invalid("salt is empty"));
        }
        let nonce: [u8; NONCE_LEN] = BASE64
            .decode(&self.nonce)
            .map_err(|_| invalid("nonce is not base64"))?
            .try_into()
            .map_err(|_| invalid("nonce is not 12 bytes"))?;
        let ciphertext = BASE64
            .decode(&self.ciphertext)
            .map_err(|_| invalid("ciphertext is not base64"))?;
        if ciphertext.len() != WRAP_CIPHERTEXT_LEN {
            return Err(invalid("ciphertext is not 48 bytes"));
        }
        Ok(PackRootKeyWrap {
            kind,
            kdf: self.kdf.clone(),
            iterations: self.iterations,
            salt,
            nonce,
            ciphertext,
        })
    }
}

// ---- §11: メタデータバックアップ（`thundoku-backup.json` v3）の封筒 -------------

/// Drive のメタデータバックアップ（`thundoku-backup.json`）v3 の封筒（仕様 §11）。
///
/// - 平文は**現行のバックアップ JSON そのもの**（`db::backup::export_json` の出力）
/// - 暗号化は `AES-256-GCM(backup_cipher_key)`、AAD は `thundoku-backup:3:<owner_id>`
/// - `nonce` は乱数なので**暗号文は毎回変わる** — 変更検知は [`Self::content_hmac`]
///   （平文の HMAC-SHA256）で行い、暗号文の md5 は使わない
///
/// 鍵は PRK から導出した [`PackRootKey::derive_backup_cipher_key`] /
/// [`PackRootKey::derive_backup_hash_key`] だけを使う（新しい鍵管理を増やさない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupEnvelope {
    format_version: u32,
    owner_id: String,
    alg: String,
    kdf: String,
    nonce: [u8; NONCE_LEN],
    ciphertext: Vec<u8>,
    content_hmac: [u8; 32],
}

impl BackupEnvelope {
    /// 平文を封筒に入れる（nonce は乱数）。本番の作成経路。
    pub fn seal(plaintext: &[u8], root: &PackRootKey, owner_id: &str) -> Self {
        let mut nonce = [0u8; NONCE_LEN];
        rand::rngs::OsRng.fill_bytes(&mut nonce);
        Self::seal_with_nonce(plaintext, root, owner_id, nonce)
    }

    /// nonce を指定して封をする（**決定的**。テストベクタの検算に使う）。
    pub fn seal_with_nonce(
        plaintext: &[u8],
        root: &PackRootKey,
        owner_id: &str,
        nonce: [u8; NONCE_LEN],
    ) -> Self {
        let ciphertext = crate::crypto::encrypt_with(
            plaintext,
            &root.derive_backup_cipher_key(),
            &nonce,
            &backup_aad(owner_id),
        );
        let content_hmac = hmac_sha256(&root.derive_backup_hash_key(), plaintext);
        Self {
            format_version: BACKUP_FORMAT_VERSION,
            owner_id: owner_id.to_owned(),
            alg: BACKUP_ALG.to_owned(),
            kdf: BACKUP_KDF.to_owned(),
            nonce,
            ciphertext,
            content_hmac,
        }
    }

    pub fn format_version(&self) -> u32 {
        self.format_version
    }

    pub fn owner_id(&self) -> &str {
        &self.owner_id
    }

    pub fn nonce(&self) -> &[u8; NONCE_LEN] {
        &self.nonce
    }

    pub fn ciphertext(&self) -> &[u8] {
        &self.ciphertext
    }

    /// 平文の HMAC。**同じ平文なら（nonce が違っても）同じ値**になるので、
    /// 「内容が変わったか」の判定に使う。
    pub fn content_hmac(&self) -> &[u8; 32] {
        &self.content_hmac
    }

    /// [`Self::content_hmac`] の小文字 hex（設定に保存する基準値）。
    pub fn content_hmac_hex(&self) -> String {
        hex_encode(&self.content_hmac)
    }

    /// 復号する。**鍵違い・改変・`owner_id` 違いはすべてエラー**で、平文を返さない。
    ///
    /// `owner_id` は AAD に入れる値（呼び出し側が確認した所有者）。封筒に記録された
    /// [`Self::owner_id`] を渡すのが通常で、別の値を渡すと復号に失敗する
    /// （＝別アカウントの封筒への差し替えを検出できる）。
    pub fn open(&self, root: &PackRootKey, owner_id: &str) -> Result<Vec<u8>, PackError> {
        self.validate()?;
        let plaintext = crate::crypto::decrypt_with(
            &self.ciphertext,
            &self.nonce,
            &root.derive_backup_cipher_key(),
            &backup_aad(owner_id),
        )
        .map_err(|_| {
            PackError::Corrupted("backup decryption failed (wrong key or tampered)".into())
        })?;
        let expected = hmac_sha256(&root.derive_backup_hash_key(), &plaintext);
        if !constant_time_eq(&expected, &self.content_hmac) {
            return Err(PackError::Corrupted("backup content hmac mismatch".into()));
        }
        Ok(plaintext)
    }

    /// 封筒（`thundoku-backup.json` の中身）のバイト列。TS `JSON.stringify` と同じ詰めた形。
    pub fn to_json(&self) -> Result<Vec<u8>, PackError> {
        let wire = WireEnvelope {
            format_version: self.format_version,
            owner_id: self.owner_id.clone(),
            encryption: WireEncryption {
                alg: self.alg.clone(),
                kdf: self.kdf.clone(),
                nonce: BASE64.encode(self.nonce),
                ciphertext: BASE64.encode(&self.ciphertext),
            },
            content_hmac: self.content_hmac_hex(),
        };
        serde_json::to_vec(&wire)
            .map_err(|e| PackError::Corrupted(format!("failed to serialize backup envelope: {e}")))
    }

    /// 封筒を読む。`format_version` が 3 以外・構造が不正なら `Corrupted`
    /// （**平文として扱える形では返さない**）。
    pub fn from_json(bytes: &[u8]) -> Result<Self, PackError> {
        let invalid =
            |detail: String| PackError::Corrupted(format!("invalid backup envelope: {detail}"));
        let wire: WireEnvelope = serde_json::from_slice(bytes)
            .map_err(|e| invalid(format!("not a JSON envelope: {e}")))?;
        if wire.format_version != BACKUP_FORMAT_VERSION {
            return Err(invalid(format!(
                "unsupported format_version {}",
                wire.format_version
            )));
        }
        if wire.encryption.alg != BACKUP_ALG {
            return Err(invalid(format!("unknown alg {:?}", wire.encryption.alg)));
        }
        if wire.encryption.kdf != BACKUP_KDF {
            return Err(invalid(format!("unknown kdf {:?}", wire.encryption.kdf)));
        }
        let nonce: [u8; NONCE_LEN] = BASE64
            .decode(&wire.encryption.nonce)
            .map_err(|_| invalid("nonce is not base64".into()))?
            .try_into()
            .map_err(|_| invalid("nonce is not 12 bytes".into()))?;
        let ciphertext = BASE64
            .decode(&wire.encryption.ciphertext)
            .map_err(|_| invalid("ciphertext is not base64".into()))?;
        if ciphertext.len() < BACKUP_TAG_LEN {
            return Err(invalid("ciphertext is shorter than the GCM tag".into()));
        }
        let content_hmac = hex_decode_32(&wire.content_hmac)
            .ok_or_else(|| invalid("content_hmac is not 32 hex bytes".into()))?;
        Ok(Self {
            format_version: wire.format_version,
            owner_id: wire.owner_id,
            alg: wire.encryption.alg,
            kdf: wire.encryption.kdf,
            nonce,
            ciphertext,
            content_hmac,
        })
    }

    /// 読んだ値の自己検査（`open` の前段）。
    fn validate(&self) -> Result<(), PackError> {
        if self.format_version != BACKUP_FORMAT_VERSION {
            return Err(PackError::Corrupted(format!(
                "unsupported backup format version: {}",
                self.format_version
            )));
        }
        if self.alg != BACKUP_ALG || self.kdf != BACKUP_KDF {
            return Err(PackError::Corrupted(format!(
                "unsupported backup encryption: {}/{}",
                self.alg, self.kdf
            )));
        }
        Ok(())
    }
}

#[derive(Serialize, Deserialize)]
struct WireEnvelope {
    format_version: u32,
    owner_id: String,
    encryption: WireEncryption,
    content_hmac: String,
}

#[derive(Serialize, Deserialize)]
struct WireEncryption {
    alg: String,
    kdf: String,
    nonce: String,
    ciphertext: String,
}

/// 32 バイトの小文字 hex（`content_hmac` の表現）。
fn hex_encode(bytes: &[u8; 32]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// hex 64 文字を 32 バイトに戻す（大文字も許す。書き出しは常に小文字）。
fn hex_decode_32(value: &str) -> Option<[u8; 32]> {
    if value.len() != 64 {
        return None;
    }
    let mut out = [0u8; 32];
    for (index, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(value.get(index * 2..index * 2 + 2)?, 16).ok()?;
    }
    Some(out)
}
