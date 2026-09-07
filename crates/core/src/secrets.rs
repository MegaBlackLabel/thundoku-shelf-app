//! OS keyring-backed secret storage (sessions, OAuth tokens). Passwords are
//! never stored — only session cookies / tokens.

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use rand::RngCore;

pub const SERVICE: &str = "com.megablacklabel.thundoku-shelf";
pub const USER_TECHBOOKFEST: &str = "techbookfest";
pub const USER_GOOGLE: &str = "google";
/// BOOTH（booth.pm）のセッション Cookie の保存キー。
pub const USER_BOOTH: &str = "booth";
/// `books.owner_sub` 暗号化用のローカル鍵（keyring）。
pub const USER_DB_KEY: &str = "thundoku-shelf.db-key";

#[derive(Debug, thiserror::Error)]
pub enum SecretError {
    #[error("keyring error: {0}")]
    Keyring(String),
    #[error("secret encoding error: {0}")]
    Encoding(String),
}

#[derive(Clone)]
pub struct SecretStore {
    service: &'static str,
}

impl Default for SecretStore {
    fn default() -> Self {
        Self::new()
    }
}

impl SecretStore {
    pub fn new() -> Self {
        Self { service: SERVICE }
    }

    pub fn save(&self, user: &str, secret: &str) -> Result<(), SecretError> {
        let entry = keyring::Entry::new(self.service, user)
            .map_err(|e| SecretError::Keyring(e.to_string()))?;
        entry
            .set_password(secret)
            .map_err(|e| SecretError::Keyring(e.to_string()))
    }

    pub fn load(&self, user: &str) -> Result<Option<String>, SecretError> {
        let entry = keyring::Entry::new(self.service, user)
            .map_err(|e| SecretError::Keyring(e.to_string()))?;
        match entry.get_password() {
            Ok(secret) => Ok(Some(secret)),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(SecretError::Keyring(e.to_string())),
        }
    }

    pub fn delete(&self, user: &str) -> Result<(), SecretError> {
        let entry = keyring::Entry::new(self.service, user)
            .map_err(|e| SecretError::Keyring(e.to_string()))?;
        match entry.delete_credential() {
            Ok(()) => Ok(()),
            Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError::Keyring(e.to_string())),
        }
    }

    /// `books.owner_sub` の暗号化用ローカル鍵（32 byte）。無ければ新規生成して保存する。
    /// P1: 起動時に無い場合は新規生成（既存の `owner_sub` は復号不能になるが許容）。
    /// 鍵の BASE64 を keyring に保存する。
    pub fn db_key(&self) -> Result<[u8; 32], SecretError> {
        if let Some(encoded) = self.load(USER_DB_KEY)? {
            if let Ok(decoded) = B64.decode(encoded.trim()) {
                if decoded.len() == 32 {
                    let mut key = [0u8; 32];
                    key.copy_from_slice(&decoded);
                    return Ok(key);
                }
            }
        }
        let mut key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);
        self.save(USER_DB_KEY, &B64.encode(&key))?;
        Ok(key)
    }
}
