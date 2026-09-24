//! OS keyring-backed secret storage (sessions, OAuth tokens). Passwords are
//! never stored — only session cookies / tokens.

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use rand::RngCore;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, OnceLock};

/// テスト用スイッチ: keychain に触れず、プロセス内メモリだけで完結させる。
///
/// テストは `BookshelfView::new` → 所有者フィルタ → `db_key()` の経路で
/// keychain に到達してしまう。開発機では許可ダイアログが出て（応答するまで
/// ブロックするためテストが数分止まる）、CI のランナーでは取得できない
/// アイテムの作成が走る。テストからは `use_memory_backend()` を呼んで
/// keychain を触らせない。
static MEMORY_ONLY: AtomicBool = AtomicBool::new(false);

/// プロセス内キャッシュ（`service:user` → secret）。
///
/// keychain の読み出しは OS の許可ダイアログを伴い得る。とくに開発ビルドは
/// 再ビルドのたびにバイナリの署名が変わり、アイテムの ACL が無効化されるため
/// 毎回確認される。`db_key()` のように本棚・履歴・ノートの表示経路から何度も
/// 読まれる値ではダイアログが連続してしまうので、プロセス内で 1 度読んだら
/// 使い回す（`save` / `delete` ではキャッシュも更新する）。
fn cache() -> &'static Mutex<HashMap<String, Option<String>>> {
    static CACHE: OnceLock<Mutex<HashMap<String, Option<String>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

fn cache_key(service: &str, user: &str) -> String {
    format!("{service}:{user}")
}

fn cache_get(key: &str) -> Option<Option<String>> {
    let guard = cache().lock().unwrap_or_else(|e| e.into_inner());
    guard.get(key).cloned()
}

fn cache_put(key: String, value: Option<String>) {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(key, value);
}

fn cache_remove(key: &str) {
    cache()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(key);
}

pub const SERVICE: &str = "com.megablacklabel.thundoku-shelf";
pub const USER_TECHBOOKFEST: &str = "techbookfest";
pub const USER_GOOGLE: &str = "google";
/// 取得済み Google プロフィール（`sub` 等）の保存キー。
///
/// `sub` は `books.owner_sub` の判定（バックアップの所有者フィルタ・本棚の絞り込み）に
/// 使うため、起動直後にネットワーク取得できない場合でも復元できるよう残す。
pub const USER_GOOGLE_PROFILE: &str = "google-profile";
/// BOOTH（booth.pm）のセッション Cookie の保存キー。
pub const USER_BOOTH: &str = "booth";
/// `books.owner_sub` 暗号化用のローカル鍵（keyring）。
pub const USER_DB_KEY: &str = "thundoku-shelf.db-key";
/// ストアのセッション Cookie 暗号化用のローカル鍵（keyring）。
/// `USER_DB_KEY` とは**別の鍵**にする（用途ごとに分離する）。
pub const USER_SESSION_KEY: &str = "thundoku-shelf.session-key";
/// pack のルート鍵（v3 = 方式 C の PRK）の keyring スロットの接頭辞。
///
/// user 名は `thundoku-shelf.pack-root-key:<owner_id>`（`docs/spec/10-pack-keys.md`
/// §3.1）。PRK は**アカウントごと**なので `owner_id` をスロットに含める。
pub const PACK_ROOT_KEY_PREFIX: &str = "thundoku-shelf.pack-root-key:";

/// `owner_id` の pack ルート鍵スロット名（keyring の user 名）。
pub fn pack_root_key_user(owner_id: &str) -> String {
    format!("{PACK_ROOT_KEY_PREFIX}{owner_id}")
}

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

    /// 以降の `load` / `save` / `delete` を keychain ではなくプロセス内メモリで
    /// 行う（テストのセットアップから呼ぶ）。
    pub fn use_memory_backend() {
        MEMORY_ONLY.store(true, Ordering::SeqCst);
    }

    fn memory_only() -> bool {
        MEMORY_ONLY.load(Ordering::SeqCst)
    }

    pub fn save(&self, user: &str, secret: &str) -> Result<(), SecretError> {
        if Self::memory_only() {
            cache_put(cache_key(self.service, user), Some(secret.to_string()));
            return Ok(());
        }
        let entry = keyring::Entry::new(self.service, user)
            .map_err(|e| SecretError::Keyring(e.to_string()))?;
        entry
            .set_password(secret)
            .map_err(|e| SecretError::Keyring(e.to_string()))?;
        cache_put(cache_key(self.service, user), Some(secret.to_string()));
        Ok(())
    }

    pub fn load(&self, user: &str) -> Result<Option<String>, SecretError> {
        let key = cache_key(self.service, user);
        if let Some(hit) = cache_get(&key) {
            return Ok(hit);
        }
        if Self::memory_only() {
            return Ok(None);
        }
        let entry = keyring::Entry::new(self.service, user)
            .map_err(|e| SecretError::Keyring(e.to_string()))?;
        let value = match entry.get_password() {
            Ok(secret) => Some(secret),
            Err(keyring::Error::NoEntry) => None,
            // 一過性の失敗は覚えない（次回に再試行させる）
            Err(e) => return Err(SecretError::Keyring(e.to_string())),
        };
        cache_put(key, value.clone());
        Ok(value)
    }

    pub fn delete(&self, user: &str) -> Result<(), SecretError> {
        if Self::memory_only() {
            cache_remove(&cache_key(self.service, user));
            return Ok(());
        }
        let entry = keyring::Entry::new(self.service, user)
            .map_err(|e| SecretError::Keyring(e.to_string()))?;
        let result = match entry.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretError::Keyring(e.to_string())),
        };
        if result.is_ok() {
            cache_remove(&cache_key(self.service, user));
        }
        result
    }

    /// keyring のスロットに保存された 32 byte 鍵を返す。無ければ新規生成して保存する。
    ///
    /// 用途ごとに **別のスロット**（＝別の鍵）を使う。1 つの鍵を複数の用途で
    /// 使い回すと、片方の漏洩が他方へ波及する。
    fn random_key(&self, slot: &str) -> Result<[u8; 32], SecretError> {
        if let Some(encoded) = self.load(slot)?
            && let Ok(decoded) = B64.decode(encoded.trim())
            && decoded.len() == 32
        {
            let mut key = [0u8; 32];
            key.copy_from_slice(&decoded);
            return Ok(key);
        }
        let mut key = [0u8; 32];
        rand::rngs::OsRng.fill_bytes(&mut key);
        self.save(slot, &B64.encode(key))?;
        Ok(key)
    }

    /// `books.owner_sub` の暗号化用ローカル鍵（32 byte）。無ければ新規生成して保存する。
    /// P1: 起動時に無い場合は新規生成（既存の `owner_sub` は復号不能になるが許容）。
    /// 鍵の BASE64 を keyring に保存する。
    pub fn db_key(&self) -> Result<[u8; 32], SecretError> {
        self.random_key(USER_DB_KEY)
    }

    /// ストアのセッション Cookie の暗号化用ローカル鍵（32 byte）。
    /// `db_key` とは別スロット＝別鍵（用途ごとに分離する）。
    pub fn session_key(&self) -> Result<[u8; 32], SecretError> {
        self.random_key(USER_SESSION_KEY)
    }

    /// `owner_id` の pack ルート鍵（v3 の PRK。値は `PackRootKey::to_base64()`）を読む。
    /// 無ければ `None`（＝この端末に鍵が無い。Drive の bundle から復元する経路がある）。
    ///
    /// DB 鍵（[`Self::db_key`]）・セッション鍵（[`Self::session_key`]）とは
    /// **別スロット**に置く（用途分離。1 つの鍵を使い回さない）。
    pub fn load_pack_root_key(&self, owner_id: &str) -> Result<Option<String>, SecretError> {
        self.load(&pack_root_key_user(owner_id))
    }

    /// `owner_id` の pack ルート鍵（base64）を保存する。
    pub fn save_pack_root_key(&self, owner_id: &str, value: &str) -> Result<(), SecretError> {
        self.save(&pack_root_key_user(owner_id), value)
    }

    /// `owner_id` の pack ルート鍵を削除する（端末を切り離すとき。
    /// 消しても Drive の bundle + `sub` / パスフレーズで復旧できる — 仕様 §5.2/§5.3）。
    pub fn delete_pack_root_key(&self, owner_id: &str) -> Result<(), SecretError> {
        self.delete(&pack_root_key_user(owner_id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// メモリバックエンドでは keychain に触れずに読み書きできること。
    /// （テストが実 keychain にアクセスして許可ダイアログやアイテム作成を
    ///   起こさないための経路を検証する）
    #[test]
    fn memory_backend_round_trips_without_keychain() {
        SecretStore::use_memory_backend();
        let store = SecretStore::new();
        assert!(store.load("test-slot").unwrap().is_none());
        store.save("test-slot", "secret-1").unwrap();
        assert_eq!(
            store.load("test-slot").unwrap().as_deref(),
            Some("secret-1"),
            "保存した値が読めること"
        );
        // db_key も keychain を経由せずに取得できること
        let key = store.db_key().unwrap();
        assert_eq!(store.db_key().unwrap(), key, "db_key が安定していること");
        store.delete("test-slot").unwrap();
        assert!(store.load("test-slot").unwrap().is_none());
    }

    /// pack のルート鍵（v3）は `owner_id` ごとの別スロットに保存し、
    /// DB 鍵 / セッション鍵と同居させない（用途とアカウントの分離）。
    #[test]
    fn pack_root_key_uses_a_per_owner_slot() {
        SecretStore::use_memory_backend();
        let store = SecretStore::new();
        let (owner_a, owner_b) = ("owner-a-slot-test", "owner-b-slot-test");
        assert!(store.load_pack_root_key(owner_a).unwrap().is_none());
        store.save_pack_root_key(owner_a, "AAA").unwrap();
        store.save_pack_root_key(owner_b, "BBB").unwrap();
        assert_eq!(
            store.load_pack_root_key(owner_a).unwrap().as_deref(),
            Some("AAA")
        );
        assert_eq!(
            store.load_pack_root_key(owner_b).unwrap().as_deref(),
            Some("BBB")
        );
        store.delete_pack_root_key(owner_a).unwrap();
        assert!(store.load_pack_root_key(owner_a).unwrap().is_none());
        assert_eq!(
            store.load_pack_root_key(owner_b).unwrap().as_deref(),
            Some("BBB"),
            "他のアカウントの鍵を消さない"
        );
        // スロット名が用途ごとに別であること（同居させない）。
        assert_ne!(pack_root_key_user(owner_a), USER_DB_KEY);
        assert_ne!(pack_root_key_user(owner_a), USER_SESSION_KEY);
        assert_ne!(pack_root_key_user(owner_a), pack_root_key_user(owner_b));
        assert!(pack_root_key_user(owner_a).starts_with("thundoku-shelf.pack-root-key:"));
    }
}
