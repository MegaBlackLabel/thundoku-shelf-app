//! pack の鍵（v3 = ランダムなルート鍵 PRK + ラップ）の解決と保管。
//!
//! 仕様は `docs/spec/10-pack-keys.md`。v3 では pack の鍵材料は **乱数の
//! ルート鍵**（PRK）で、`sub` からは導出できない。PRK は
//!
//! - 端末: OS keyring（`thundoku-shelf.pack-root-key:<owner_id>`）
//! - Drive: `thundoku-keys.json`（`thundoku-backup.json` と同じフォルダ）
//!
//! の 2 箇所に置かれ、Drive 側は「ラップ」＝ AES-256-GCM で包んだ PRK を持つ
//! （`kind=sub` は Google ログインだけでの復号、`kind=passphrase` は
//! パスフレーズを知る端末だけでの復号）。
//!
//! パスフレーズは **UI が尋ねる**（core は UI を知らない）ため、コールバックで
//! 受け取る。

use opfspack::{PackKeyBundle, PackRootKey, PackRootKeyWrap, WrapKind, derive_owner_id};

use crate::db::{SqlitePool, settings};
use crate::drive::DriveApi;
use crate::secrets::SecretStore;

/// Drive に置く鍵 bundle のファイル名（`thundoku-backup.json` と同じフォルダ）。
pub const KEY_BUNDLE_NAME: &str = "thundoku-keys.json";

/// 鍵 bundle のアップロードが未完了であることを記録する設定キー（値は `owner_id`）。
///
/// アップロードに失敗しても取り込みは止めない（ネットワークが無いと本を取り込めない
/// のは困る）ため、印を残して次の同期で再試行する（仕様 §5.1）。
pub const PENDING_UPLOAD_KEY: &str = "drive.pack_keys.pending";

/// パスフレーズラップの PBKDF2 反復回数（暫定。ラップに記録するので後から上げられる）。
pub const PASSPHRASE_ITERATIONS: u32 = opfspack::PASSPHRASE_WRAP_ITERATIONS;

/// パスフレーズを尋ねる口。`None` は「利用者がスキップした」。
pub type PassphrasePrompt<'a> = &'a mut dyn FnMut() -> Option<String>;

/// パスフレーズの最低文字数。
///
/// PRK を守る強度は PBKDF2 の反復回数（実測: release で 600k ≒ 60ms）より**長さ**に
/// 強く効く。短いパスフレーズは「保護したつもり」になるため、設定時に拒否する。
pub const MIN_PASSPHRASE_CHARS: usize = 12;

#[derive(Debug, thiserror::Error)]
pub enum PackKeysError {
    #[error("keyring error: {0}")]
    Secret(#[from] crate::secrets::SecretError),
    #[error("drive error: {0}")]
    Drive(#[from] crate::drive::DriveError),
    #[error("key bundle error: {0}")]
    Pack(#[from] opfspack::PackError),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    /// bundle はあるが、どのラップも解けなかった（仕様 §4.1 手順 5）。
    /// **平文へは落とさない**（既存の pack を読めなくするため、新規作成もしない）。
    #[error("鍵を復元できません。別端末で設定したパスフレーズを入力してください")]
    Unavailable,
    /// 入力されたパスフレーズが違う。`sub` ラップへ**黙って落ちない**（仕様 §4.1 手順 3）。
    #[error("パスフレーズが違います")]
    PassphraseFailed,
    /// 設定しようとしたパスフレーズが短すぎる（[`MIN_PASSPHRASE_CHARS`] 未満）。
    #[error("パスフレーズは {min} 文字以上にしてください")]
    PassphraseTooShort { min: usize },
    /// Drive の bundle が別アカウントのもの（`owner_id` 不一致）。
    /// 黙って上書きすると相手の鍵を失うためエラーにする（仕様 §3.2）。
    #[error("{KEY_BUNDLE_NAME} は別のアカウントの鍵です（owner_id: {found}）")]
    OwnerMismatch { found: String },
}

/// 鍵 bundle の読み書きに必要な文脈（keyring / 設定 DB / Drive フォルダ）。
pub struct PackKeyStore<'a> {
    secrets: &'a SecretStore,
    pool: &'a SqlitePool,
    folder_id: &'a str,
}

impl<'a> PackKeyStore<'a> {
    pub fn new(secrets: &'a SecretStore, pool: &'a SqlitePool, folder_id: &'a str) -> Self {
        Self {
            secrets,
            pool,
            folder_id,
        }
    }

    /// 解決順序（仕様 §4.1）: keyring → bundle のパスフレーズラップ → bundle の `sub` ラップ。
    /// どれも無ければ `Ok(None)`（＝この端末に鍵が無い。新規作成は [`Self::ensure`]）。
    pub fn resolve(
        &self,
        drive: &mut dyn DriveApi,
        sub: &str,
        prompt: PassphrasePrompt<'_>,
    ) -> Result<Option<PackRootKey>, PackKeysError> {
        let owner_id = derive_owner_id(sub);
        // 1. keyring にあればそれを使う（何も尋ねない）
        if let Some(root) = load_keyring(self.secrets, &owner_id)? {
            return Ok(Some(root));
        }
        // 2. Drive の bundle（無ければ「鍵が無い」＝正常系）
        let Some(bundle) = load_bundle(drive, self.folder_id, &owner_id)? else {
            return Ok(None);
        };
        // 3. パスフレーズラップがあれば尋ねる。入力が失敗したら sub へ黙って落ちない。
        if bundle.has_wrap(WrapKind::Passphrase)
            && let Some(passphrase) = prompt()
        {
            let root = bundle
                .unwrap_with_passphrase(&passphrase)
                .ok_or(PackKeysError::PassphraseFailed)?;
            save_keyring(self.secrets, &owner_id, &root)?;
            return Ok(Some(root));
            // 利用者がスキップしたときだけ sub ラップを使う（仕様 §4.1 手順 3）。
        }
        // 4. sub ラップ
        let root = bundle.unwrap_with_sub(sub).ok_or(PackKeysError::Unavailable)?;
        save_keyring(self.secrets, &owner_id, &root)?;
        Ok(Some(root))
    }

    /// PRK を用意する（無ければ新規作成 — 仕様 §4.1 手順 5 / §5.1）。
    ///
    /// 作成時は keyring に保存し、`sub` ラップを bundle に載せて Drive へ上げる。
    /// **アップロード失敗でも `Err` にしない**（未アップロードを記録して次の同期で
    /// 再試行する）。bundle はあるのに解けない場合は作成せずエラー（既存の pack を
    /// 読めなくしない）。
    pub fn ensure(
        &self,
        drive: &mut dyn DriveApi,
        sub: &str,
        prompt: PassphrasePrompt<'_>,
    ) -> Result<PackRootKey, PackKeysError> {
        if let Some(root) = self.resolve(drive, sub, prompt)? {
            return Ok(root);
        }
        let owner_id = derive_owner_id(sub);
        let root = PackRootKey::generate();
        // keyring に置けないと「この端末にしか無い鍵」よりもさらに悪い（消える）ので、
        // ここは fail-closed（取り込みを失敗させる）。
        save_keyring(self.secrets, &owner_id, &root)?;
        let now_ms = now_ms();
        let mut bundle = PackKeyBundle::new(owner_id.clone(), now_ms);
        bundle.upsert_wrap(PackRootKeyWrap::wrap_with_sub(&root, sub, &owner_id, now_ms));
        bundle.touch(now_ms);
        // 先に印を残す（アップロード中に落ちても再試行される）
        mark_pending(self.pool, &owner_id)?;
        match upload_bundle(drive, self.folder_id, &bundle) {
            Ok(()) => {
                clear_pending(self.pool)?;
                log::info!("pack keys: 鍵 bundle を Drive へアップロードした（owner_id={owner_id}）");
            }
            Err(error) => {
                // 取り込みは続行する（ネットワークが無くても本は読める）。
                // ただし鍵はこの端末にしか無い＝端末故障で復元不能なので警告する。
                log::warn!(
                    "pack keys: 鍵 bundle をアップロードできない（次の同期で再試行する）: {error}"
                );
            }
        }
        Ok(root)
    }

    /// パスフレーズを設定 / 変更する（同じ `kind` を置換。仕様 §5.2）。PRK は変えない。
    /// 操作後は bundle を Drive へ反映する（失敗はエラー＝利用者がやり直せる）。
    pub fn set_passphrase(
        &self,
        drive: &mut dyn DriveApi,
        sub: &str,
        root: &PackRootKey,
        passphrase: &str,
    ) -> Result<(), PackKeysError> {
        // 短いパスフレーズは「保護したつもり」になるので拒否する（長さが強度に効く）。
        if passphrase.chars().count() < MIN_PASSPHRASE_CHARS {
            return Err(PackKeysError::PassphraseTooShort {
                min: MIN_PASSPHRASE_CHARS,
            });
        }
        let owner_id = derive_owner_id(sub);
        let now_ms = now_ms();
        let mut bundle = load_or_new_bundle(drive, self.folder_id, &owner_id, now_ms)?;
        bundle.upsert_wrap(PackRootKeyWrap::wrap_with_passphrase(
            root,
            passphrase,
            &owner_id,
            PASSPHRASE_ITERATIONS,
            now_ms,
        ));
        bundle.touch(now_ms);
        upload_bundle(drive, self.folder_id, &bundle)
    }

    /// パスフレーズラップを削除する（仕様 §5.2）。他のラップ（`sub` など）は残す。
    /// 削除できたら `true`（bundle が無い / ラップが無いときは `false`）。
    pub fn remove_passphrase(
        &self,
        drive: &mut dyn DriveApi,
        sub: &str,
    ) -> Result<bool, PackKeysError> {
        let owner_id = derive_owner_id(sub);
        let Some(mut bundle) = load_bundle(drive, self.folder_id, &owner_id)? else {
            return Ok(false);
        };
        if !bundle.remove_wrap(WrapKind::Passphrase) {
            return Ok(false);
        }
        bundle.touch(now_ms());
        upload_bundle(drive, self.folder_id, &bundle)?;
        Ok(true)
    }

    /// パスフレーズが設定されているか（設定画面の表示用。Drive の bundle が正）。
    pub fn has_passphrase(
        &self,
        drive: &mut dyn DriveApi,
        sub: &str,
    ) -> Result<bool, PackKeysError> {
        let owner_id = derive_owner_id(sub);
        Ok(load_bundle(drive, self.folder_id, &owner_id)?
            .is_some_and(|bundle| bundle.has_wrap(WrapKind::Passphrase)))
    }

    /// 未アップロードの鍵 bundle の `owner_id`（設定画面とログの警告に使う）。
    pub fn pending_owner(&self) -> Result<Option<String>, PackKeysError> {
        Ok(settings::get(self.pool, PENDING_UPLOAD_KEY)?)
    }

    /// pending があれば bundle をアップロードして印を消す（同期の最後に呼ぶ。仕様 §5.1）。
    ///
    /// この端末に PRK が無い（未ログイン / 別アカウント / keyring 障害）ときは何もしない。
    /// アップロードできたら `true`。
    pub fn retry_pending_upload(
        &self,
        drive: &mut dyn DriveApi,
        sub: &str,
    ) -> Result<bool, PackKeysError> {
        let owner_id = derive_owner_id(sub);
        if self.pending_owner()?.as_deref() != Some(owner_id.as_str()) {
            return Ok(false);
        }
        let Some(root) = load_keyring(self.secrets, &owner_id)? else {
            log::warn!("pack keys: pending があるが keyring に鍵が無い（復旧は別端末から）");
            return Ok(false);
        };
        let now_ms = now_ms();
        // 既存 bundle のラップ（パスフレーズなど）は残し、sub ラップを足して上げ直す。
        let mut bundle = load_or_new_bundle(drive, self.folder_id, &owner_id, now_ms)?;
        bundle.upsert_wrap(PackRootKeyWrap::wrap_with_sub(&root, sub, &owner_id, now_ms));
        bundle.touch(now_ms);
        upload_bundle(drive, self.folder_id, &bundle)?;
        clear_pending(self.pool)?;
        log::info!("pack keys: 未アップロードだった鍵 bundle をアップロードした");
        Ok(true)
    }
}

/// keyring のスロットから PRK を読む（無ければ `None`。壊れた値も `None`）。
fn load_keyring(
    secrets: &SecretStore,
    owner_id: &str,
) -> Result<Option<PackRootKey>, PackKeysError> {
    let Some(encoded) = secrets.load_pack_root_key(owner_id)? else {
        return Ok(None);
    };
    let root = PackRootKey::from_base64(&encoded);
    if root.is_none() {
        log::warn!("pack keys: keyring の値が PRK として読めない（owner_id={owner_id}）");
    }
    Ok(root)
}

/// PRK を keyring に保存する（仕様 §4.1 手順 4 / §5.1）。
fn save_keyring(
    secrets: &SecretStore,
    owner_id: &str,
    root: &PackRootKey,
) -> Result<(), PackKeysError> {
    secrets.save_pack_root_key(owner_id, &root.to_base64())?;
    Ok(())
}

/// Drive の `thundoku-keys.json` を読む（無ければ `Ok(None)`）。
///
/// `owner_id` が一致しない bundle は**使わない**（`OwnerMismatch`）。
/// 名前で一覧から探し、複数あるときは `updated_at` が新しいものを選ぶ。
pub fn load_bundle(
    drive: &mut dyn DriveApi,
    folder_id: &str,
    owner_id: &str,
) -> Result<Option<PackKeyBundle>, PackKeysError> {
    let files = drive.list_files(folder_id)?;
    let Some(file) = files.iter().find(|file| file.name == KEY_BUNDLE_NAME) else {
        return Ok(None);
    };
    let bytes = drive.download(&file.id)?;
    let bundle = PackKeyBundle::from_json(&bytes)?;
    if bundle.owner_id() != owner_id {
        return Err(PackKeysError::OwnerMismatch {
            found: bundle.owner_id().to_string(),
        });
    }
    Ok(Some(bundle))
}

/// bundle を読む（無ければ新しい空の bundle を作る。`owner_id` 不一致はエラー）。
fn load_or_new_bundle(
    drive: &mut dyn DriveApi,
    folder_id: &str,
    owner_id: &str,
    now_ms: i64,
) -> Result<PackKeyBundle, PackKeysError> {
    Ok(
        load_bundle(drive, folder_id, owner_id)?
            .unwrap_or_else(|| PackKeyBundle::new(owner_id.to_string(), now_ms)),
    )
}

/// Drive の `thundoku-keys.json` を置き換える。
///
/// 先に新しいファイルを上げてから、同名の旧ファイルを消す（`thundoku-backup.json` と
/// 同じ順序。削除→アップロードだと途中で失敗したときに鍵を失う）。
pub fn upload_bundle(
    drive: &mut dyn DriveApi,
    folder_id: &str,
    bundle: &PackKeyBundle,
) -> Result<(), PackKeysError> {
    let bytes = bundle.to_json()?;
    let previous: Vec<String> = drive
        .list_files(folder_id)?
        .into_iter()
        .filter(|file| file.name == KEY_BUNDLE_NAME)
        .map(|file| file.id)
        .collect();
    let uploaded = drive.upload_multipart(KEY_BUNDLE_NAME, folder_id, &bytes)?;
    for file_id in previous {
        if file_id == uploaded {
            continue;
        }
        if let Err(error) = drive.delete(&file_id) {
            // 旧ファイルが残っても新しい bundle は存在するので致命的ではない。
            log::warn!("pack keys: 旧 bundle を削除できない: {error}");
        }
    }
    Ok(())
}

/// 未アップロードの印を立てる。
fn mark_pending(pool: &SqlitePool, owner_id: &str) -> Result<(), PackKeysError> {
    settings::set(pool, PENDING_UPLOAD_KEY, owner_id)?;
    Ok(())
}

/// 未アップロードの印を消す。
fn clear_pending(pool: &SqlitePool) -> Result<(), PackKeysError> {
    settings::delete(pool, PENDING_UPLOAD_KEY)?;
    Ok(())
}

/// UNIX ミリ秒（bundle の `created_at` / `updated_at`）。
fn now_ms() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drive::{DriveError, DriveFile};
    use std::collections::HashMap;

    /// テスト用 Drive（`crates/core/tests/drive_sync.rs` の `FakeDrive` と同じ形）。
    /// アップロードは毎回新しい id を振るので、同名ファイルの重複も検証できる。
    /// `fail_upload` を立てると `upload_multipart` が失敗する（pending の検証用）。
    struct FakeDrive {
        files: HashMap<String, (String, Vec<u8>)>,
        next_id: usize,
        fail_upload: bool,
        downloads: usize,
        uploads: usize,
    }

    impl FakeDrive {
        fn new() -> Self {
            Self {
                files: HashMap::new(),
                next_id: 1,
                fail_upload: false,
                downloads: 0,
                uploads: 0,
            }
        }

        fn seed(&mut self, name: &str, bytes: &[u8]) -> String {
            let id = format!("seeded-{name}-{}", self.next_id);
            self.next_id += 1;
            self.files.insert(id.clone(), (name.to_string(), bytes.to_vec()));
            id
        }

        /// `name` という名前のファイルの一覧（重複検出用）。
        fn named(&self, name: &str) -> Vec<(&String, &Vec<u8>)> {
            self.files
                .iter()
                .filter(|(_, (file_name, _))| file_name == name)
                .map(|(id, (_, bytes))| (id, bytes))
                .collect()
        }
    }

    impl DriveApi for FakeDrive {
        fn list_files(&mut self, _folder_id: &str) -> Result<Vec<DriveFile>, DriveError> {
            Ok(self
                .files
                .iter()
                .map(|(id, (name, bytes))| DriveFile {
                    id: id.clone(),
                    name: name.clone(),
                    size: Some(bytes.len() as i64),
                    md5_checksum: Some(format!("{:x}", md5::compute(bytes))),
                    modified_time: None,
                })
                .collect())
        }

        fn download(&mut self, file_id: &str) -> Result<Vec<u8>, DriveError> {
            self.downloads += 1;
            self.files
                .get(file_id)
                .map(|(_, bytes)| bytes.clone())
                .ok_or_else(|| DriveError::Http(404, "missing".into()))
        }

        fn upload_multipart(
            &mut self,
            name: &str,
            _folder_id: &str,
            bytes: &[u8],
        ) -> Result<String, DriveError> {
            if self.fail_upload {
                return Err(DriveError::Http(503, "offline".into()));
            }
            self.uploads += 1;
            let id = format!("uploaded-{}-{}", name, self.next_id);
            self.next_id += 1;
            self.files.insert(id.clone(), (name.to_string(), bytes.to_vec()));
            Ok(id)
        }

        fn create_folder(&mut self, _name: &str) -> Result<String, DriveError> {
            Ok("folder-1".into())
        }

        fn delete(&mut self, file_id: &str) -> Result<(), DriveError> {
            self.files.remove(file_id);
            Ok(())
        }

        fn touch(&mut self, _file_id: &str) -> Result<(), DriveError> {
            Ok(())
        }
    }

    fn pool() -> SqlitePool {
        let pool = crate::db::test_pool();
        crate::db::migrate(&pool).unwrap();
        pool
    }

    /// keyring に触れない（keychain を使わない）テスト用ストア。
    fn secrets() -> SecretStore {
        SecretStore::use_memory_backend();
        SecretStore::new()
    }

    /// パスフレーズを尋ねない（尋ねたらテストの誤り）。
    fn no_prompt() -> impl FnMut() -> Option<String> {
        || panic!("パスフレーズを尋ねてはいけない")
    }

    /// 失敗するアップロードを許さないテスト用の keyring 値。
    fn wrap_sub(root: &PackRootKey, sub: &str) -> PackRootKeyWrap {
        PackRootKeyWrap::wrap_with_sub(root, sub, &derive_owner_id(sub), 1_700_000_000_000)
    }

    /// 1. keyring にあればそれをそのまま使う（Drive を読まない・何も尋ねない）。
    #[test]
    fn keyring_hit_resolves_without_drive_or_prompt() {
        let sub = "sub-pack-keys-keyring";
        let root = PackRootKey::generate();
        let secrets = secrets();
        secrets
            .save_pack_root_key(&derive_owner_id(sub), &root.to_base64())
            .unwrap();
        let pool = pool();
        let mut drive = FakeDrive::new();
        let keys = PackKeyStore::new(&secrets, &pool, "folder-1");

        let mut prompt = no_prompt();
        let resolved = keys
            .resolve(&mut drive, sub, &mut prompt)
            .unwrap()
            .expect("keyring の鍵を使う");
        assert_eq!(resolved.as_bytes(), root.as_bytes());
        assert_eq!(drive.downloads, 0, "Drive を読まないこと");
    }

    /// 2. keyring が空なら bundle の `sub` ラップから復号し、keyring に保存する（§4.1 手順 4）。
    #[test]
    fn sub_wrap_resolves_and_is_cached_in_keyring() {
        let sub = "sub-pack-keys-sub-wrap";
        let root = PackRootKey::generate();
        let owner_id = derive_owner_id(sub);
        let mut bundle = PackKeyBundle::new(owner_id.clone(), 1_700_000_000_000);
        bundle.upsert_wrap(wrap_sub(&root, sub));
        let secrets = secrets();
        let pool = pool();
        let mut drive = FakeDrive::new();
        upload_bundle(&mut drive, "folder-1", &bundle).unwrap();
        let keys = PackKeyStore::new(&secrets, &pool, "folder-1");

        let mut prompt = no_prompt();
        let resolved = keys
            .resolve(&mut drive, sub, &mut prompt)
            .unwrap()
            .expect("sub ラップから復号する");
        assert_eq!(resolved.as_bytes(), root.as_bytes());
        assert_eq!(
            secrets.load_pack_root_key(&owner_id).unwrap().as_deref(),
            Some(root.to_base64().as_str()),
            "復号できたら keyring に保存する"
        );
    }

    /// 3. パスフレーズラップがあれば入力で復号できる（`sub` ラップが無くてもよい）。
    #[test]
    fn passphrase_wrap_resolves_with_input() {
        let sub = "sub-pack-keys-passphrase";
        let root = PackRootKey::generate();
        let owner_id = derive_owner_id(sub);
        let mut bundle = PackKeyBundle::new(owner_id, 1_700_000_000_000);
        bundle.upsert_wrap(PackRootKeyWrap::wrap_with_passphrase(
            &root,
            "pw-1",
            &derive_owner_id(sub),
            PASSPHRASE_ITERATIONS,
            1_700_000_000_000,
        ));
        let secrets = secrets();
        let pool = pool();
        let mut drive = FakeDrive::new();
        upload_bundle(&mut drive, "folder-1", &bundle).unwrap();
        let keys = PackKeyStore::new(&secrets, &pool, "folder-1");

        let mut asked = 0;
        let mut prompt = || {
            asked += 1;
            Some("pw-1".to_string())
        };
        let resolved = keys
            .resolve(&mut drive, sub, &mut prompt)
            .unwrap()
            .expect("パスフレーズで復号する");
        assert_eq!(resolved.as_bytes(), root.as_bytes());
        assert_eq!(asked, 1, "パスフレーズを尋ねること");
    }

    /// 4. 入力が失敗したら `sub` ラップへ**黙って落ちない**（§4.1 手順 3）。
    ///    利用者がスキップしたときだけ `sub` を使う。
    #[test]
    fn wrong_passphrase_fails_and_skip_falls_back_to_sub() {
        let sub = "sub-pack-keys-wrong-pass";
        let root = PackRootKey::generate();
        let owner_id = derive_owner_id(sub);
        let mut bundle = PackKeyBundle::new(owner_id.clone(), 1_700_000_000_000);
        bundle.upsert_wrap(wrap_sub(&root, sub));
        bundle.upsert_wrap(PackRootKeyWrap::wrap_with_passphrase(
            &root,
            "pw-1",
            &owner_id,
            PASSPHRASE_ITERATIONS,
            1_700_000_000_000,
        ));
        let secrets = secrets();
        let pool = pool();
        let mut drive = FakeDrive::new();
        upload_bundle(&mut drive, "folder-1", &bundle).unwrap();
        let keys = PackKeyStore::new(&secrets, &pool, "folder-1");

        let mut wrong = || Some("wrong".to_string());
        let error = keys
            .resolve(&mut drive, sub, &mut wrong)
            .expect_err("違うパスフレーズで sub ラップに落ちてはいけない");
        assert!(matches!(error, PackKeysError::PassphraseFailed), "{error:?}");
        assert!(
            secrets.load_pack_root_key(&owner_id).unwrap().is_none(),
            "失敗した鍵を keyring に残さない"
        );

        // スキップ（None）のときだけ sub ラップを使う。
        let mut skip = || None;
        let resolved = keys
            .resolve(&mut drive, sub, &mut skip)
            .unwrap()
            .expect("スキップなら sub ラップを使う");
        assert_eq!(resolved.as_bytes(), root.as_bytes());
    }

    /// 5. keyring も bundle も無ければ `Ok(None)`（新規作成の判断は呼び出し側）。
    #[test]
    fn nothing_available_resolves_to_none() {
        let secrets = secrets();
        let pool = pool();
        let mut drive = FakeDrive::new();
        let keys = PackKeyStore::new(&secrets, &pool, "folder-1");

        let mut prompt = no_prompt();
        assert!(
            keys.resolve(&mut drive, "sub-pack-keys-none", &mut prompt)
                .unwrap()
                .is_none()
        );
        assert!(!keys.has_passphrase(&mut drive, "sub-pack-keys-none").unwrap());
    }

    /// 6. bundle はあるのにどのラップも解けない場合はエラー（新規作成して既存 pack を
    ///    読めなくしない）。
    #[test]
    fn unusable_bundle_is_an_error_instead_of_a_new_key() {
        let sub = "sub-pack-keys-unusable";
        let root = PackRootKey::generate();
        let owner_id = derive_owner_id(sub);
        let mut bundle = PackKeyBundle::new(owner_id.clone(), 1_700_000_000_000);
        bundle.upsert_wrap(PackRootKeyWrap::wrap_with_passphrase(
            &root,
            "pw-1",
            &owner_id,
            PASSPHRASE_ITERATIONS,
            1_700_000_000_000,
        ));
        let secrets = secrets();
        let pool = pool();
        let mut drive = FakeDrive::new();
        upload_bundle(&mut drive, "folder-1", &bundle).unwrap();
        let keys = PackKeyStore::new(&secrets, &pool, "folder-1");

        let mut skip = || None;
        let error = keys.ensure(&mut drive, sub, &mut skip).expect_err("作成しない");
        assert!(matches!(error, PackKeysError::Unavailable), "{error:?}");
        assert_eq!(drive.uploads, 1, "上書きアップロードしないこと");
    }

    /// 7. 初回作成: keyring に保存し、`sub` ラップ付き bundle を Drive へ上げる。
    ///    同名ファイルの重複も残さない。
    #[test]
    fn ensure_creates_and_uploads_the_sub_wrap() {
        let sub = "sub-pack-keys-ensure";
        let secrets = secrets();
        let pool = pool();
        let mut drive = FakeDrive::new();
        let keys = PackKeyStore::new(&secrets, &pool, "folder-1");

        let mut prompt = no_prompt();
        let root = keys.ensure(&mut drive, sub, &mut prompt).unwrap();

        let owner_id = derive_owner_id(sub);
        assert_eq!(
            secrets.load_pack_root_key(&owner_id).unwrap().as_deref(),
            Some(root.to_base64().as_str()),
            "PRK を keyring に保存する"
        );
        let bundle = load_bundle(&mut drive, "folder-1", &owner_id)
            .unwrap()
            .expect("bundle が Drive にある");
        assert_eq!(
            bundle.unwrap_with_sub(sub).unwrap().as_bytes(),
            root.as_bytes(),
            "sub ラップで同じ PRK に戻れる"
        );
        assert_eq!(
            drive.named(KEY_BUNDLE_NAME).len(),
            1,
            "同名ファイルを重複させない"
        );
        assert!(keys.pending_owner().unwrap().is_none(), "pending は残らない");

        // 2 回目は keyring から解決する（新規作成しない）。
        let mut prompt = no_prompt();
        assert_eq!(
            keys.ensure(&mut drive, sub, &mut prompt)
                .unwrap()
                .as_bytes(),
            root.as_bytes()
        );
        assert_eq!(drive.uploads, 1, "2 回目はアップロードしない");
    }

    /// 8. アップロード失敗でも取り込みは止めず、pending を残して次の同期で再試行する（§5.1）。
    #[test]
    fn upload_failure_records_pending_and_retry_clears_it() {
        let sub = "sub-pack-keys-pending";
        let secrets = secrets();
        let pool = pool();
        let mut drive = FakeDrive::new();
        drive.fail_upload = true;
        let keys = PackKeyStore::new(&secrets, &pool, "folder-1");
        let owner_id = derive_owner_id(sub);

        let mut prompt = no_prompt();
        let root = keys
            .ensure(&mut drive, sub, &mut prompt)
            .expect("アップロード失敗でも取り込みは続行する");
        assert_eq!(
            keys.pending_owner().unwrap().as_deref(),
            Some(owner_id.as_str()),
            "未アップロードを記録する"
        );
        assert!(
            load_bundle(&mut drive, "folder-1", &owner_id)
                .unwrap()
                .is_none(),
            "Drive には上がっていない"
        );

        drive.fail_upload = false;
        assert!(
            keys.retry_pending_upload(&mut drive, sub).unwrap(),
            "再試行でアップロードする"
        );
        assert!(keys.pending_owner().unwrap().is_none(), "印を消す");
        let bundle = load_bundle(&mut drive, "folder-1", &owner_id)
            .unwrap()
            .expect("bundle が上がっている");
        assert_eq!(bundle.unwrap_with_sub(sub).unwrap().as_bytes(), root.as_bytes());
        assert!(
            !keys.retry_pending_upload(&mut drive, sub).unwrap(),
            "2 回目は何もしない"
        );
    }

    /// 9. pending の再試行は別アカウントの印では動かない（アカウント切替の保護）。
    #[test]
    fn pending_retry_ignores_other_accounts() {
        let secrets = secrets();
        let pool = pool();
        let mut drive = FakeDrive::new();
        let keys = PackKeyStore::new(&secrets, &pool, "folder-1");
        settings::set(&pool, PENDING_UPLOAD_KEY, &derive_owner_id("other-account")).unwrap();

        assert!(!keys.retry_pending_upload(&mut drive, "sub-pack-keys-other").unwrap());
        assert_eq!(drive.uploads, 0);
        assert!(keys.pending_owner().unwrap().is_some(), "印は残す");
    }

    /// 短いパスフレーズは拒否する（PRK を守る強度は反復回数より**長さ**に効く）。
    /// 拒否したときは bundle を作らない（中途半端な保護状態にしない）。
    #[test]
    fn set_passphrase_rejects_short_passphrases() {
        let sub = "sub-pack-keys-too-short";
        let secrets = secrets();
        let pool = pool();
        let mut drive = FakeDrive::new();
        let keys = PackKeyStore::new(&secrets, &pool, "folder-1");
        let mut prompt = no_prompt();
        let root = keys.ensure(&mut drive, sub, &mut prompt).unwrap();

        let error = keys
            .set_passphrase(&mut drive, sub, &root, "short")
            .expect_err("短いパスフレーズを受け付けている");
        assert!(
            matches!(error, PackKeysError::PassphraseTooShort { min } if min == MIN_PASSPHRASE_CHARS),
            "{error:?}"
        );
        assert!(
            !keys.has_passphrase(&mut drive, sub).unwrap(),
            "拒否したのに passphrase ラップができている"
        );
    }

    /// 10. パスフレーズの設定 / 変更 / 削除は bundle に反映される（§5.2）。
    #[test]
    fn passphrase_operations_are_reflected_on_drive() {
        let sub = "sub-pack-keys-passphrase-ops";
        let secrets = secrets();
        let pool = pool();
        let mut drive = FakeDrive::new();
        let keys = PackKeyStore::new(&secrets, &pool, "folder-1");
        let owner_id = derive_owner_id(sub);
        let mut prompt = no_prompt();
        let root = keys.ensure(&mut drive, sub, &mut prompt).unwrap();

        keys.set_passphrase(&mut drive, sub, &root, "pw-1-is-long-enough")
            .unwrap();
        assert!(keys.has_passphrase(&mut drive, sub).unwrap());
        let bundle = load_bundle(&mut drive, "folder-1", &owner_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            bundle
                .unwrap_with_passphrase("pw-1-is-long-enough")
                .unwrap()
                .as_bytes(),
            root.as_bytes()
        );
        assert!(bundle.has_wrap(WrapKind::Sub), "sub ラップは残す");
        assert_eq!(
            drive.named(KEY_BUNDLE_NAME).len(),
            1,
            "同名ファイルを重複させない"
        );

        // 変更は置換（同じ kind を 2 つ持たない）。
        keys.set_passphrase(&mut drive, sub, &root, "pw-2-is-long-enough")
            .unwrap();
        let bundle = load_bundle(&mut drive, "folder-1", &owner_id)
            .unwrap()
            .unwrap();
        assert_eq!(bundle.wraps().len(), 2, "sub + passphrase の 2 つだけ");
        assert!(
            bundle.unwrap_with_passphrase("pw-1-is-long-enough").is_none(),
            "旧は無効"
        );
        assert!(
            bundle
                .unwrap_with_passphrase("pw-2-is-long-enough")
                .is_some()
        );

        assert!(keys.remove_passphrase(&mut drive, sub).unwrap());
        assert!(!keys.has_passphrase(&mut drive, sub).unwrap());
        let bundle = load_bundle(&mut drive, "folder-1", &owner_id)
            .unwrap()
            .unwrap();
        assert!(bundle.has_wrap(WrapKind::Sub), "sub ラップは消さない");
        assert!(!keys.remove_passphrase(&mut drive, sub).unwrap(), "2 回目は false");
    }

    /// 12. 同名ファイルが Drive に複数あっても、アップロードで 1 つに収束させる
    ///     （Drive は同名ファイルを許すので、放置すると bundle が分裂する）。
    #[test]
    fn upload_replaces_previous_files_with_the_same_name() {
        let sub = "sub-pack-keys-duplicates";
        let owner_id = derive_owner_id(sub);
        let mut drive = FakeDrive::new();
        let root = PackRootKey::generate();
        let mut stale = PackKeyBundle::new(owner_id.clone(), 1_700_000_000_000);
        stale.upsert_wrap(wrap_sub(&root, sub));
        // 別端末が同時に上げたような 2 ファイル（id が違う同名ファイル）。
        drive.seed(KEY_BUNDLE_NAME, &stale.to_json().unwrap());
        drive.seed(KEY_BUNDLE_NAME, &stale.to_json().unwrap());

        let mut fresh = PackKeyBundle::new(owner_id.clone(), 1_700_000_001_000);
        fresh.upsert_wrap(wrap_sub(&root, sub));
        upload_bundle(&mut drive, "folder-1", &fresh).unwrap();

        let files = drive.named(KEY_BUNDLE_NAME);
        assert_eq!(files.len(), 1, "同名ファイルは 1 つに収束する");
        assert_eq!(
            PackKeyBundle::from_json(files[0].1).unwrap(),
            fresh,
            "最新の bundle が残る"
        );
        // 読み出しは残った 1 つを使う。
        assert_eq!(
            load_bundle(&mut drive, "folder-1", &owner_id)
                .unwrap()
                .unwrap(),
            fresh
        );
    }

    /// 13. 壊れた / 形式の違う bundle はエラー（黙って作り直して相手の鍵を失わない）。
    #[test]
    fn malformed_bundle_is_an_error() {
        let sub = "sub-pack-keys-malformed";
        let secrets = secrets();
        let pool = pool();
        let mut drive = FakeDrive::new();
        drive.seed(KEY_BUNDLE_NAME, b"{\"format_version\":1}");
        let keys = PackKeyStore::new(&secrets, &pool, "folder-1");

        let mut prompt = no_prompt();
        let error = keys
            .resolve(&mut drive, sub, &mut prompt)
            .expect_err("壊れた bundle はエラー");
        assert!(matches!(error, PackKeysError::Pack(_)), "{error:?}");
        let mut prompt = no_prompt();
        assert!(keys.ensure(&mut drive, sub, &mut prompt).is_err());
        assert_eq!(drive.uploads, 0, "作り直して上書きしないこと");
        assert!(keys.pending_owner().unwrap().is_none(), "印も立てない");
    }

    /// 11. 別アカウントの bundle は使わない（黙って上書きしない）。
    #[test]
    fn bundle_of_another_owner_is_rejected() {
        let sub = "sub-pack-keys-owner-mismatch";
        let other_sub = "sub-pack-keys-other-owner";
        let other_root = PackRootKey::generate();
        let other_owner = derive_owner_id(other_sub);
        let mut bundle = PackKeyBundle::new(other_owner.clone(), 1_700_000_000_000);
        bundle.upsert_wrap(wrap_sub(&other_root, other_sub));
        let secrets = secrets();
        let pool = pool();
        let mut drive = FakeDrive::new();
        let file_id = drive.seed(KEY_BUNDLE_NAME, &bundle.to_json().unwrap());
        let original = drive.files[&file_id].1.clone();
        let keys = PackKeyStore::new(&secrets, &pool, "folder-1");

        let mut prompt = no_prompt();
        let error = keys
            .resolve(&mut drive, sub, &mut prompt)
            .expect_err("別アカウントの bundle は使わない");
        assert!(matches!(error, PackKeysError::OwnerMismatch { .. }), "{error:?}");
        let mut prompt = no_prompt();
        assert!(keys.ensure(&mut drive, sub, &mut prompt).is_err());
        assert_eq!(drive.uploads, 0, "上書きしないこと");
        assert_eq!(drive.files[&file_id].1, original, "相手の bundle を壊さない");
    }
}
