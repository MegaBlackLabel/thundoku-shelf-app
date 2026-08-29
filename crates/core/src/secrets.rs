//! OS keyring-backed secret storage (sessions, OAuth tokens). Passwords are
//! never stored — only session cookies / tokens.

pub const SERVICE: &str = "com.megablacklabel.thundoku-shelf";
pub const USER_TECHBOOKFEST: &str = "techbookfest";
pub const USER_GOOGLE: &str = "google";
/// BOOTH（booth.pm）のセッション Cookie の保存キー。
pub const USER_BOOTH: &str = "booth";

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
}
