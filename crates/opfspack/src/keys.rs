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
const PASSPHRASE_SALT_LEN: usize = 16;

/// AES-GCM nonce 長。
const NONCE_LEN: usize = 12;

/// ラップされた PRK の長さ（32B + 16B タグ・仕様 §3.2）。
const WRAP_CIPHERTEXT_LEN: usize = 32 + 16;

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
