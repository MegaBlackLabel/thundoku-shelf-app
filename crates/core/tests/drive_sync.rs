//! Bidirectional Drive sync engine tests against an in-memory DriveApi fake.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use opfspack::{Identity, PackBuilder};
use thundoku_core::db;
use thundoku_core::drive::sync::{SyncError, sync};
use thundoku_core::drive::{DriveApi, DriveError, DriveFile};

struct FakeDrive {
    files: HashMap<String, FakeFile>,
    downloads: Arc<Mutex<usize>>,
    uploads: Arc<Mutex<usize>>,
}

#[derive(Clone)]
struct FakeFile {
    name: String,
    bytes: Vec<u8>,
    md5: String,
}

impl FakeDrive {
    fn new() -> Self {
        Self {
            files: HashMap::new(),
            downloads: Arc::new(Mutex::new(0)),
            uploads: Arc::new(Mutex::new(0)),
        }
    }

    fn seed(&mut self, name: &str, bytes: &[u8]) -> String {
        let id = format!("id-{name}");
        self.files.insert(
            id.clone(),
            FakeFile {
                name: name.into(),
                bytes: bytes.to_vec(),
                md5: format!("{:x}", md5::compute(bytes)),
            },
        );
        id
    }

    fn download_count(&self) -> usize {
        *self.downloads.lock()
    }

    fn upload_count(&self) -> usize {
        *self.uploads.lock()
    }
}

impl DriveApi for FakeDrive {
    fn list_files(&mut self, _folder_id: &str) -> Result<Vec<DriveFile>, DriveError> {
        Ok(self
            .files
            .values()
            .map(|f| DriveFile {
                id: format!("id-{}", f.name),
                name: f.name.clone(),
                size: Some(f.bytes.len() as i64),
                md5_checksum: Some(f.md5.clone()),
                modified_time: None,
            })
            .collect())
    }

    fn download(&mut self, file_id: &str) -> Result<Vec<u8>, DriveError> {
        *self.downloads.lock() += 1;
        let id = file_id.strip_prefix("id-").unwrap_or(file_id);
        self.files
            .get(&format!("id-{id}"))
            .or_else(|| self.files.values().find(|f| f.name == id))
            .map(|f| f.bytes.clone())
            .ok_or_else(|| DriveError::Http(404, "missing".into()))
    }

    fn upload_multipart(
        &mut self,
        name: &str,
        _folder_id: &str,
        bytes: &[u8],
    ) -> Result<String, DriveError> {
        *self.uploads.lock() += 1;
        let id = format!("id-{name}");
        self.files.insert(
            id.clone(),
            FakeFile {
                name: name.into(),
                bytes: bytes.to_vec(),
                md5: format!("{:x}", md5::compute(bytes)),
            },
        );
        Ok(id)
    }

    fn create_folder(&mut self, _name: &str) -> Result<String, DriveError> {
        Ok("folder-1".into())
    }

    fn touch(&mut self, _file_id: &str) -> Result<(), DriveError> {
        Ok(())
    }

    fn delete(&mut self, file_id: &str) -> Result<(), DriveError> {
        self.files.remove(file_id);
        Ok(())
    }
}

struct TestEnv {
    root: PathBuf,
    pool: thundoku_core::db::SqlitePool,
}

impl TestEnv {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("thundoku-sync-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("packs")).unwrap();
        std::fs::create_dir_all(root.join("downloads")).unwrap();
        let pool = thundoku_core::db::test_pool();
        thundoku_core::db::migrate(&pool).unwrap();
        Self { root, pool }
    }

    fn packs(&self) -> PathBuf {
        self.root.join("packs")
    }

    fn downloads(&self) -> PathBuf {
        self.root.join("downloads")
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn plain_pack(entry_path: &str, content: &[u8]) -> Vec<u8> {
    let mut builder = PackBuilder::new(1_700_000_000_000);
    builder.add_entry(
        entry_path,
        content.to_vec(),
        "application/octet-stream",
        false,
    );
    builder.build(None, false).unwrap()
}

fn metadata_pack(title: &str, author: &str, circle: &str, purchase_date: &str) -> Vec<u8> {
    let meta = serde_json::json!({
        "schemaVersion": 1,
        "title": title,
        "author": author,
        "circleName": circle,
        "purchaseDate": purchase_date,
    });
    let mut builder = PackBuilder::new(1_700_000_000_000);
    builder.add_entry(
        "metadata.json",
        serde_json::to_vec(&meta).unwrap(),
        "application/json",
        false,
    );
    builder.add_entry(
        "pages/page_0001.webp",
        b"PAGE-1".to_vec(),
        "image/webp",
        false,
    );
    builder.build(None, false).unwrap()
}

fn encrypted_pack(sub: &str, pack_id: &str, content: &[u8]) -> Vec<u8> {
    let mut builder = PackBuilder::new(1_700_000_000_000);
    builder.add_entry("pages/page_0001.webp", content.to_vec(), "image/webp", true);
    let identity = Identity {
        sub: sub.into(),
        pack_id: pack_id.into(),
    };
    builder.build(Some(&identity), true).unwrap()
}

fn sync_env(
    env: &mut TestEnv,
    drive: &mut dyn DriveApi,
) -> Result<thundoku_core::drive::sync::SyncOutcome, SyncError> {
    sync(
        &env.pool,
        drive,
        &env.packs(),
        &env.downloads(),
        None,
        None,
        "folder-1",
        None,
    )
}

#[test]
fn downloads_new_pack_and_imports_book_with_metadata() {
    let mut env = TestEnv::new("new-pack");
    let mut drive = FakeDrive::new();
    let bytes = metadata_pack("メタ本", "著者X", "サークルY", "2026-08-21");
    drive.seed("pack-1.opfspack", &bytes);
    drive.seed("notes.txt", b"not a pack");

    let outcome = sync_env(&mut env, &mut drive).unwrap();
    assert_eq!(outcome.downloaded, vec!["pack-1"]);
    assert!(outcome.uploaded.is_empty());
    assert_eq!(drive.download_count(), 1);

    // pack stored locally
    let local = env.packs().join("pack-1.opfspack");
    assert_eq!(std::fs::read(&local).unwrap(), bytes);
    // book row from metadata
    let book = db::books::get(&env.pool, "pack-1").unwrap().unwrap();
    assert_eq!(book.title, "メタ本");
    assert_eq!(book.author, "著者X");
    assert_eq!(book.circle_name, "サークルY");
    assert_eq!(book.purchase_date.as_deref(), Some("2026-08-21"));
    assert_eq!(book.pack_id.as_deref(), Some("pack-1"));
    // sync state row
    let state = db::sync_state::get(&env.pool, "pack-1").unwrap().unwrap();
    assert_eq!(state.md5, format!("{:x}", md5::compute(&bytes)));
}

#[test]
fn skips_unchanged_md5() {
    let mut env = TestEnv::new("skip");
    let mut drive = FakeDrive::new();
    let bytes = plain_pack("pages/page_0001.webp", b"data");
    drive.seed("pack-1.opfspack", &bytes);

    sync_env(&mut env, &mut drive).unwrap();
    assert_eq!(drive.download_count(), 1);

    // second sync: md5 matches stored state → skipped
    let outcome = sync_env(&mut env, &mut drive).unwrap();
    assert_eq!(outcome.skipped, vec!["pack-1"]);
    assert_eq!(drive.download_count(), 1);
}

#[test]
fn conflict_backs_up_local_and_takes_drive_version() {
    let mut env = TestEnv::new("conflict");
    let mut drive = FakeDrive::new();
    // local pack v1 (imported earlier)
    let local_bytes_v1 = plain_pack("pages/page_0001.webp", b"LOCAL-V1");
    std::fs::write(env.packs().join("pack-1.opfspack"), &local_bytes_v1).unwrap();
    db::books::insert(
        &env.pool,
        &db::books::Book {
            id: "pack-1".into(),
            title: "pack-1".into(),
            author: String::new(),
            circle_name: String::new(),
            purchase_date: None,
            file_name: "pack-1.opfspack".into(),
            file_size: local_bytes_v1.len() as i64,
            opfs_path: "pack-1.opfspack".into(),
            cover_thumbnail: None,
            tbf_product_id: None,
            site_id: None,
            tags_fetched: 1,
            pack_id: Some("pack-1".into()),
            is_favorite: 0,
            is_hidden: 0,
            created_at: "2026-08-21 00:00:00".into(),
            updated_at: "2026-08-21 00:00:00".into(),
        },
    )
    .unwrap();
    // stored state says md5 of v1; local file mtime is now, last_synced_at is old
    db::sync_state::upsert(
        &env.pool,
        &db::sync_state::DriveSyncState {
            pack_id: "pack-1".into(),
            drive_file_id: "id-pack-1.opfspack".into(),
            md5: format!("{:x}", md5::compute(&local_bytes_v1)),
            modified_time: None,
            last_synced_at: "2020-01-01 00:00:00".into(),
        },
    )
    .unwrap();
    // drive now has v2 with different md5
    let drive_bytes_v2 = plain_pack("pages/page_0001.webp", b"DRIVE-V2");
    drive.seed("pack-1.opfspack", &drive_bytes_v2);

    let outcome = sync_env(&mut env, &mut drive).unwrap();
    assert_eq!(outcome.conflicts, vec!["pack-1"]);
    assert_eq!(outcome.downloaded, vec!["pack-1"]);
    // local backup
    let backup = env.packs().join("pack-1.conflict-local.opfspack");
    assert_eq!(std::fs::read(&backup).unwrap(), local_bytes_v1);
    // local replaced by drive version
    assert_eq!(
        std::fs::read(env.packs().join("pack-1.opfspack")).unwrap(),
        drive_bytes_v2
    );
}

#[test]
fn uploads_local_pack_without_state_row() {
    let mut env = TestEnv::new("upload-new");
    let mut drive = FakeDrive::new();
    let bytes = plain_pack("pages/page_0001.webp", b"LOCAL-ONLY");
    std::fs::write(env.packs().join("pack-9.opfspack"), &bytes).unwrap();
    db::books::insert(
        &env.pool,
        &db::books::Book {
            id: "pack-9".into(),
            title: "pack-9".into(),
            author: String::new(),
            circle_name: String::new(),
            purchase_date: None,
            file_name: "pack-9.opfspack".into(),
            file_size: bytes.len() as i64,
            opfs_path: "pack-9.opfspack".into(),
            cover_thumbnail: None,
            tbf_product_id: None,
            site_id: None,
            tags_fetched: 1,
            pack_id: Some("pack-9".into()),
            is_favorite: 0,
            is_hidden: 0,
            created_at: "2026-08-21 00:00:00".into(),
            updated_at: "2026-08-21 00:00:00".into(),
        },
    )
    .unwrap();
    // 新仕様: 未所属(NULL) pack はアップロードされない。所有本としてテストする。
    let key = [11u8; 32];
    db::books::set_owner_sub(
        &env.pool,
        "pack-9",
        Some(thundoku_core::owner::encrypt(&key, "test-sub")),
    )
    .unwrap();

    let outcome = sync(
        &env.pool,
        &mut drive,
        &env.packs(),
        &env.downloads(),
        Some("test-sub"),
        Some(&key),
        "folder-1",
        None,
    )
    .unwrap();
    assert_eq!(outcome.uploaded, vec!["pack-9"]);
    assert_eq!(drive.upload_count(), 1);
    assert!(drive.files.contains_key("id-pack-9.opfspack"));
}

#[test]
fn reuploads_locally_modified_pack() {
    let mut env = TestEnv::new("reupload");
    let mut drive = FakeDrive::new();
    let bytes = plain_pack("pages/page_0001.webp", b"OLD");
    drive.seed("pack-2.opfspack", &bytes);
    sync_env(&mut env, &mut drive).unwrap();
    let key = [11u8; 32];
    // 新仕様: ダウンロードした本を「test-sub の所有」として扱い、再アップロードを確認する。
    db::books::set_owner_sub(
        &env.pool,
        "pack-2",
        Some(thundoku_core::owner::encrypt(&key, "test-sub")),
    )
    .unwrap();
    assert_eq!(drive.upload_count(), 0); // downloaded, not uploaded

    // modify local pack after sync
    let updated = plain_pack("pages/page_0001.webp", b"NEWER-CONTENT");
    std::fs::write(env.packs().join("pack-2.opfspack"), &updated).unwrap();
    // push last_synced_at into the past → local mtime strictly newer
    db::sync_state::upsert(
        &env.pool,
        &db::sync_state::DriveSyncState {
            pack_id: "pack-2".into(),
            drive_file_id: "id-pack-2.opfspack".into(),
            md5: format!("{:x}", md5::compute(&bytes)),
            modified_time: None,
            last_synced_at: "2020-01-01 00:00:00".into(),
        },
    )
    .unwrap();
    // last_synced_at in the past → mtime newer → re-upload
    let outcome = sync(
        &env.pool,
        &mut drive,
        &env.packs(),
        &env.downloads(),
        Some("test-sub"),
        Some(&key),
        "folder-1",
        None,
    )
    .unwrap();
    assert_eq!(outcome.uploaded, vec!["pack-2"]);
    assert_eq!(drive.upload_count(), 1);
    // drive file replaced
    let drive_file = drive.files.get("id-pack-2.opfspack").unwrap();
    assert_eq!(drive_file.bytes, updated);
}

#[test]
fn deletion_is_not_propagated_in_either_direction() {
    let mut env = TestEnv::new("deletion");
    let mut drive = FakeDrive::new();
    let bytes = plain_pack("pages/page_0001.webp", b"DATA");
    drive.seed("pack-3.opfspack", &bytes);
    sync_env(&mut env, &mut drive).unwrap();

    // drive-side deletion: local pack and state row remain untouched
    drive.files.remove("id-pack-3.opfspack");
    let outcome = sync_env(&mut env, &mut drive).unwrap();
    assert!(outcome.downloaded.is_empty());
    assert!(outcome.uploaded.is_empty());
    assert!(env.packs().join("pack-3.opfspack").exists());
    assert!(db::sync_state::get(&env.pool, "pack-3").unwrap().is_some());

    // local-side deletion: state row remains, nothing uploaded, drive file untouched
    std::fs::remove_file(env.packs().join("pack-3.opfspack")).unwrap();
    drive.seed("pack-3.opfspack", &bytes);
    // remove the just-added id key collision — seed() re-inserts with same id
    let outcome = sync_env(&mut env, &mut drive).unwrap();
    assert!(outcome.uploaded.is_empty());
    assert!(drive.files.contains_key("id-pack-3.opfspack"));
}

#[test]
fn malicious_drive_names_are_ignored() {
    let mut env = TestEnv::new("evil-name");
    let mut drive = FakeDrive::new();
    let bytes = plain_pack("pages/page_0001.webp", b"DATA");
    // traversal attempts must never escape packs_dir
    drive.seed("..\\evil.opfspack", &bytes);
    drive.seed("..\\..\\evil.opfspack", &bytes);
    drive.seed("C:\\evil.opfspack", &bytes);
    drive.seed("good-pack.opfspack", &bytes);

    let outcome = sync_env(&mut env, &mut drive).unwrap();
    assert_eq!(outcome.downloaded, vec!["good-pack"]);
    // nothing written outside packs_dir
    assert!(!env.root.join("..").join("evil.opfspack").exists());
    assert!(
        !env.root
            .join("..")
            .join("..")
            .join("evil.opfspack")
            .exists()
    );
    // traversal-named packs were not imported
    let books = db::books::list(&env.pool).unwrap();
    assert_eq!(books.len(), 1);
    assert_eq!(books[0].id, "good-pack");
}

#[test]
fn encrypted_pack_without_identity_is_rejected() {
    let mut env = TestEnv::new("encrypted-no-id");
    let mut drive = FakeDrive::new();
    let bytes = encrypted_pack("test-sub", "pack-e", b"SECRET");
    drive.seed("pack-e.opfspack", &bytes);

    let err = sync_env(&mut env, &mut drive).unwrap_err();
    assert!(matches!(err, SyncError::IdentityRequired(ref id) if id == "pack-e"));
    // nothing imported
    assert!(db::books::get(&env.pool, "pack-e").unwrap().is_none());
}

#[test]
fn encrypted_pack_imports_with_matching_identity() {
    let env = TestEnv::new("encrypted-with-id");
    let mut drive = FakeDrive::new();
    let bytes = encrypted_pack("test-sub", "pack-e", b"SECRET");
    drive.seed("pack-e.opfspack", &bytes);

    let identity = Identity {
        sub: "test-sub".into(),
        pack_id: "pack-e".into(),
    };
    let outcome = sync(
        &env.pool,
        &mut drive,
        &env.packs(),
        &env.downloads(),
        Some("test-sub"),
        None,
        "folder-1",
        None,
    )
    .unwrap();
    assert_eq!(outcome.downloaded, vec!["pack-e"]);
    let book = db::books::get(&env.pool, "pack-e").unwrap().unwrap();
    assert_eq!(book.title, "pack-e"); // no metadata.json in encrypted fixture
    // decryption round-trip via opfspack reader
    let local = std::fs::read(env.packs().join("pack-e.opfspack")).unwrap();
    let reader = opfspack::PackReader::open(&local).unwrap();
    assert_eq!(
        reader
            .read_entry("pages/page_0001.webp", Some(&identity))
            .unwrap(),
        b"SECRET"
    );
}
