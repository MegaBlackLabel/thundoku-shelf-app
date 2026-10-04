//! Bidirectional Drive sync engine tests against an in-memory DriveApi fake.

use parking_lot::Mutex;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use opfspack::{BACKUP_LABEL, PackBuilder, PackRootKey, SealedEnvelope};
use thundoku_core::db;
use thundoku_core::drive::sync::{
    BACKUP_BASELINE_KEY, SyncError, SyncPhase, SyncProgress, sync, sync_with_progress,
};
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

    /// 実際の `DriveClient` と同じく、**送れたバイト数**を報告する
    /// （既定実装は進捗なしで委譲するため、進捗の検証にはこちらを使う）。
    fn upload_resumable_from_file_with_progress(
        &mut self,
        name: &str,
        folder_id: &str,
        path: &std::path::Path,
        on_progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<String, DriveError> {
        let bytes = std::fs::read(path).map_err(|error| DriveError::Io(error.to_string()))?;
        let total = bytes.len() as u64;
        for sent in [total / 2, total] {
            if !on_progress(sent, total) {
                return Err(DriveError::Cancelled);
            }
        }
        self.upload_multipart(name, folder_id, &bytes)
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

    fn find_root_folders(&mut self, _name: &str) -> Result<Vec<DriveFile>, DriveError> {
        Ok(Vec::new())
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

/// CRC-32 (IEEE)。v2 の pack を組み立てるためにテスト側で計算する
/// （`opfspack` は v2 を書き出さないため）。
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for byte in bytes {
        crc ^= *byte as u32;
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (0xedb8_8320 & mask);
        }
    }
    !crc
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

/// v3 の暗号化 pack（鍵は PRK から pack id 単位で導出する）。
fn encrypted_pack(root: &PackRootKey, pack_id: &str, content: &[u8]) -> Vec<u8> {
    let mut builder = PackBuilder::new(1_700_000_000_000);
    builder.add_entry("pages/page_0001.webp", content.to_vec(), "image/webp", true);
    let key = root.derive_pack_key(pack_id);
    builder.build(Some(&key), true).unwrap()
}

fn sync_env(
    env: &mut TestEnv,
    drive: &mut dyn DriveApi,
) -> Result<thundoku_core::drive::sync::SyncOutcome, SyncError> {
    sync_env_with_progress(env, drive, &mut |_| true)
}

fn sync_env_with_progress(
    env: &mut TestEnv,
    drive: &mut dyn DriveApi,
    on_progress: &mut dyn FnMut(&SyncProgress) -> bool,
) -> Result<thundoku_core::drive::sync::SyncOutcome, SyncError> {
    sync_with_progress(
        thundoku_core::drive::sync::SyncRequest {
            pool: &env.pool,
            drive,
            packs_dir: &env.packs(),
            downloads_dir: &env.downloads(),
            identity_sub: None,
            pack_root_key: None,
            owner_key: None,
            folder_id: "folder-1",
            sync_books: true,
            db_path: None,
            thumbnails_dir: None,
        },
        on_progress,
    )
}

/// 書籍 pack のバックアップ ON/OFF を明示した同期（未ログイン = 所有者なし）。
fn sync_env_with_books(
    env: &mut TestEnv,
    drive: &mut dyn DriveApi,
    sync_books: bool,
) -> Result<thundoku_core::drive::sync::SyncOutcome, SyncError> {
    sync(thundoku_core::drive::sync::SyncRequest {
        pool: &env.pool,
        drive,
        packs_dir: &env.packs(),
        downloads_dir: &env.downloads(),
        identity_sub: None,
        pack_root_key: None,
        owner_key: None,
        folder_id: "folder-1",
        sync_books,
        db_path: None,
        thumbnails_dir: None,
    })
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

    // pack から取り込み状態（document / contents / ページ行）が再構築される
    let document = db::documents::get_document_by_book_id(&env.pool, "pack-1")
        .unwrap()
        .expect("document 行が復元される");
    assert_eq!(document.total_pages, 1);
    let contents = db::contents::list_with_formats(&env.pool, "pack-1").unwrap();
    assert_eq!(contents.len(), 1, "1 コンテンツとして復元される");
    assert_eq!(contents[0].0.display_name, "メタ本");
    assert_eq!(contents[0].1[0].page_count, 1);
    let images = db::documents::images_for_book(&env.pool, "pack-1").unwrap();
    let pages: Vec<&db::documents::DocumentImage> = images
        .iter()
        .filter(|image| image.image_type == "page")
        .collect();
    assert_eq!(pages.len(), 1);
    assert_eq!(
        pages[0].pack_entry_path.as_deref(),
        Some("pages/page_0001.webp")
    );
}

// pack からの再構築は、すでに行があるときは何もしない（ローカルの取り込みを壊さない）。
#[test]
fn pack_rebuild_is_idempotent_and_skips_existing_rows() {
    let mut env = TestEnv::new("pack-rebuild");
    let mut drive = FakeDrive::new();
    let bytes = metadata_pack("本", "", "", "2026-08-21");
    drive.seed("pack-1.opfspack", &bytes);
    // sync（pack 復元）で book + 取り込み状態が作られる
    sync_env(&mut env, &mut drive).unwrap();

    // 2 回目の再構築は何もしない（既存の取り込みを壊さない）
    assert!(
        {
            let reader = opfspack::PackReader::open(&bytes).unwrap();
            !thundoku_core::import::rebuild_from_pack(&env.pool, "pack-1", &reader, None).unwrap()
        },
        "既に行があれば何もしない"
    );
    let images = db::documents::images_for_book(&env.pool, "pack-1").unwrap();
    assert_eq!(images.len(), 1, "二重に作られない");
    assert!(
        db::documents::get_document_by_book_id(&env.pool, "pack-1")
            .unwrap()
            .is_some(),
        "document は 1 件のまま"
    );
}

/// `metadata.json` に取り込み元（`source`）を持つ pack（§7.3 の残件 (a)）。
fn metadata_pack_with_source(title: &str, site_id: &str, product_id: &str) -> Vec<u8> {
    let meta = serde_json::json!({
        "schemaVersion": 1,
        "title": title,
        "author": "",
        "circleName": "",
        "purchaseDate": serde_json::Value::Null,
        "source": { "siteId": site_id, "productId": product_id },
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
    builder.build(None, true).unwrap()
}

/// pack だけで復元した本は `books.site_id` / `books.tbf_product_id` に乗る
/// （重複抑止 `find_by_source` とサイト絞り込みが効くようになる）。
#[test]
fn pack_restore_restores_source_from_metadata() {
    let mut env = TestEnv::new("pack-source");
    let mut drive = FakeDrive::new();
    let bytes = metadata_pack_with_source("source 付き", "fanza", "d_777");
    drive.seed("pack-src.opfspack", &bytes);
    sync_env(&mut env, &mut drive).unwrap();

    let book = db::books::get(&env.pool, "pack-src").unwrap().unwrap();
    assert_eq!(book.site_id.as_deref(), Some("fanza"));
    assert_eq!(book.tbf_product_id.as_deref(), Some("d_777"));
    // 重複抑止（同じ source の本を探す）に乗る。
    let found = db::books::find_by_source(&env.pool, "fanza", "d_777").unwrap();
    assert_eq!(found.len(), 1, "source で引ける: {found:?}");
    assert_eq!(found[0].0, "pack-src");
}

/// 知らないサイト id を持つ pack でも、ページの復元は止めない（source は副次的）。
#[test]
fn pack_restore_ignores_unknown_site_id() {
    let mut env = TestEnv::new("pack-unknown-site");
    let mut drive = FakeDrive::new();
    let bytes = metadata_pack_with_source("知らないサイト", "unknown-site", "d_1");
    drive.seed("pack-unknown.opfspack", &bytes);
    sync_env(&mut env, &mut drive).unwrap();

    let book = db::books::get(&env.pool, "pack-unknown").unwrap().unwrap();
    assert_eq!(book.site_id, None, "site 行が無い id は入れない");
    assert_eq!(book.tbf_product_id, None);
    assert!(
        db::documents::get_document_by_book_id(&env.pool, "pack-unknown")
            .unwrap()
            .is_some(),
        "ページは復元される"
    );
}

/// `source` を持たない pack（旧形式 / Web 版が書いた pack）は今までどおり復元でき、
/// source は未設定のままになる（回帰）。
#[test]
fn pack_restore_without_source_leaves_it_unset() {
    let mut env = TestEnv::new("pack-no-source");
    let mut drive = FakeDrive::new();
    let bytes = metadata_pack("メタ本", "", "", "2026-08-21");
    drive.seed("pack-old.opfspack", &bytes);
    sync_env(&mut env, &mut drive).unwrap();

    let book = db::books::get(&env.pool, "pack-old").unwrap().unwrap();
    assert_eq!(book.title, "メタ本");
    assert_eq!(book.site_id, None);
    assert_eq!(book.tbf_product_id, None);
    assert!(
        db::documents::get_document_by_book_id(&env.pool, "pack-old")
            .unwrap()
            .is_some(),
        "本文エントリが無くてもページは復元される"
    );
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

/// 進捗コールバックは転送中の pack ごとに呼ばれ、名前・何件目 / 全件・バイト数を伝える。
///
/// 分母（`count`）は「今回の同期で転送する pack の総数」なので、md5 一致でスキップした
/// pack は数えない（表示が「2/5」のまま終わらない）。
#[test]
fn sync_with_progress_reports_each_pack_download() {
    let mut env = TestEnv::new("progress");
    let mut drive = FakeDrive::new();
    let pack_1 = plain_pack("pages/page_0001.webp", b"AAAA");
    let pack_2 = plain_pack("pages/page_0001.webp", b"BBBBBBBB");
    drive.seed("pack-1.opfspack", &pack_1);
    drive.seed("pack-2.opfspack", &pack_2);

    let mut events: Vec<SyncProgress> = Vec::new();
    let outcome = sync_env_with_progress(&mut env, &mut drive, &mut |progress| {
        events.push(progress.clone());
        true
    })
    .unwrap();

    assert_eq!(outcome.downloaded.len(), 2);
    assert_eq!(events.len(), 2, "pack ごとに進捗を報告する");
    assert!(
        events.iter().all(|e| e.phase == SyncPhase::Download),
        "取得中はダウンロードとして報告する（アップロード・バックアップは走らない構成）"
    );
    let mut indexes: Vec<usize> = events.iter().map(|e| e.index).collect();
    indexes.sort_unstable();
    assert_eq!(indexes, vec![1, 2], "何件目は 1 始まりの連番");
    assert!(
        events.iter().all(|e| e.count == 2),
        "分母は転送対象の総数: {events:?}"
    );
    let event = events
        .iter()
        .find(|e| e.name == "pack-1.opfspack")
        .expect("ファイル名を報告する");
    assert_eq!(event.bytes, pack_1.len() as u64);
    assert_eq!(event.total_bytes, Some(pack_1.len() as u64));
}

/// 進捗コールバックが `false` を返したら、その場で中止する（書かない・取り込まない・
/// 次の pack へ進まない）。
#[test]
fn sync_with_progress_stops_when_the_callback_asks_to_cancel() {
    let mut env = TestEnv::new("cancel");
    let mut drive = FakeDrive::new();
    drive.seed(
        "pack-1.opfspack",
        &metadata_pack("中止される本", "", "", "2026-08-21"),
    );
    drive.seed(
        "pack-2.opfspack",
        &metadata_pack("まだ進まない本", "", "", "2026-08-21"),
    );

    let mut seen = 0usize;
    let error = sync_env_with_progress(&mut env, &mut drive, &mut |_| {
        seen += 1;
        false
    })
    .unwrap_err();

    assert!(
        matches!(error, SyncError::Cancelled),
        "中止は Cancelled で返す: {error}"
    );
    assert_eq!(seen, 1, "最初の進捗で止める");
    assert_eq!(drive.download_count(), 1, "次の pack を取りに行かない");
    assert!(
        !env.packs().join("pack-1.opfspack").exists(),
        "中止した pack を保存しない"
    );
    assert!(
        db::books::get(&env.pool, "pack-1").unwrap().is_none(),
        "中止した pack を取り込まない"
    );
}

/// ローカルに pack を置き、`test-sub` が所有する本として登録する（＝アップロード対象）。
/// 戻り値は所有者列の暗号鍵。
fn seed_owned_local_pack(env: &TestEnv, pack_id: &str, bytes: &[u8]) -> [u8; 32] {
    let key = [11u8; 32];
    std::fs::write(env.packs().join(format!("{pack_id}.opfspack")), bytes).unwrap();
    db::books::insert(
        &env.pool,
        &db::books::Book {
            id: pack_id.into(),
            title: pack_id.into(),
            author: String::new(),
            circle_name: String::new(),
            purchase_date: None,
            file_name: format!("{pack_id}.opfspack"),
            file_size: bytes.len() as i64,
            opfs_path: format!("{pack_id}.opfspack"),
            cover_thumbnail: None,
            tbf_product_id: None,
            site_id: None,
            tags_fetched: 1,
            pack_id: Some(pack_id.into()),
            is_favorite: 0,
            is_hidden: 0,
            created_at: "2026-08-21 00:00:00".into(),
            updated_at: "2026-08-21 00:00:00".into(),
            media_category: None,
            ai_type: None,
            is_drm: 0,
            release_date: None,
            description: None,
            theme: None,
            maker_id: None,
            page_count: None,
            age_rating: None,
            series_name: None,
        },
    )
    .unwrap();
    db::books::set_owner_sub(
        &env.pool,
        pack_id,
        Some(thundoku_core::owner::encrypt(&key, "test-sub")),
    )
    .unwrap();
    key
}

/// 所有者つき（アップロード・DB バックアップも走る）同期を進捗つきで実行する。
fn owned_sync_with_progress(
    env: &TestEnv,
    drive: &mut dyn DriveApi,
    key: &[u8; 32],
    db_path: Option<&std::path::Path>,
    on_progress: &mut dyn FnMut(&SyncProgress) -> bool,
) -> Result<thundoku_core::drive::sync::SyncOutcome, SyncError> {
    sync_with_progress(
        thundoku_core::drive::sync::SyncRequest {
            pool: &env.pool,
            drive,
            packs_dir: &env.packs(),
            downloads_dir: &env.downloads(),
            identity_sub: Some("test-sub"),
            pack_root_key: None,
            owner_key: Some(key),
            folder_id: "folder-1",
            sync_books: true,
            db_path,
            thumbnails_dir: None,
        },
        on_progress,
    )
}

/// アップロードは「ファイルごとの開始」で進捗を報告し、そこで中止できる
/// （転送そのものに進捗 API が無いので、開始前に止める）。
#[test]
fn sync_with_progress_stops_before_uploading_when_the_callback_asks_to_cancel() {
    let env = TestEnv::new("cancel-upload");
    let mut drive = FakeDrive::new();
    let bytes = plain_pack("pages/page_0001.webp", b"LOCAL-ONLY");
    let key = seed_owned_local_pack(&env, "pack-9", &bytes);

    let mut phases: Vec<SyncPhase> = Vec::new();
    let error = owned_sync_with_progress(&env, &mut drive, &key, None, &mut |progress| {
        phases.push(progress.phase);
        false
    })
    .unwrap_err();

    assert!(matches!(error, SyncError::Cancelled), "中止を返す: {error}");
    assert_eq!(
        phases,
        vec![SyncPhase::Upload],
        "アップロードの開始で中止を効かせる"
    );
    assert_eq!(drive.upload_count(), 0, "中止したファイルを上げない");
    assert!(
        db::sync_state::get(&env.pool, "pack-9").unwrap().is_none(),
        "中止したファイルの同期状態を残さない"
    );
}

/// DB バックアップの書き出し前も中止できる（`false` なら上げない）。
#[test]
fn sync_with_progress_stops_before_backing_up_the_database() {
    let env = TestEnv::new("cancel-backup");
    let mut drive = FakeDrive::new();
    let key = [11u8; 32];
    let db_path = env.root.join("thundoku-shelf.db");

    let mut phases: Vec<SyncPhase> = Vec::new();
    let error = owned_sync_with_progress(
        &env,
        &mut drive,
        &key,
        Some(&db_path),
        &mut |progress| {
            phases.push(progress.phase);
            false
        },
    )
    .unwrap_err();

    assert!(matches!(error, SyncError::Cancelled), "中止を返す: {error}");
    assert_eq!(phases, vec![SyncPhase::Backup], "バックアップの前で止める");
    assert_eq!(drive.upload_count(), 0, "バックアップを上げない");
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
            media_category: None,
            ai_type: None,
            is_drm: 0,
            release_date: None,
            description: None,
            theme: None,
            maker_id: None,
            page_count: None,
            age_rating: None,
            series_name: None,
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
    let env = TestEnv::new("upload-new");
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
            media_category: None,
            ai_type: None,
            is_drm: 0,
            release_date: None,
            description: None,
            theme: None,
            maker_id: None,
            page_count: None,
            age_rating: None,
            series_name: None,
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

    let outcome = sync(thundoku_core::drive::sync::SyncRequest {
        pool: &env.pool,
        drive: &mut drive,
        packs_dir: &env.packs(),
        downloads_dir: &env.downloads(),
        identity_sub: Some("test-sub"),
        pack_root_key: None,
        owner_key: Some(&key),
        folder_id: "folder-1",
        sync_books: true,
        db_path: None,
        thumbnails_dir: None,
    })
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
    let outcome = sync(thundoku_core::drive::sync::SyncRequest {
        pool: &env.pool,
        drive: &mut drive,
        packs_dir: &env.packs(),
        downloads_dir: &env.downloads(),
        identity_sub: Some("test-sub"),
        pack_root_key: None,
        owner_key: Some(&key),
        folder_id: "folder-1",
        sync_books: true,
        db_path: None,
        thumbnails_dir: None,
    })
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
fn encrypted_pack_without_key_is_rejected() {
    let mut env = TestEnv::new("encrypted-no-id");
    let mut drive = FakeDrive::new();
    let root = PackRootKey::generate();
    let bytes = encrypted_pack(&root, "pack-e", b"SECRET");
    drive.seed("pack-e.opfspack", &bytes);

    let err = sync_env(&mut env, &mut drive).unwrap_err();
    assert!(matches!(&err, SyncError::PackKeyRequired(id) if id == "pack-e"));
    // nothing imported
    assert!(db::books::get(&env.pool, "pack-e").unwrap().is_none());
}

#[test]
fn encrypted_pack_imports_with_the_root_key() {
    let env = TestEnv::new("encrypted-with-id");
    let mut drive = FakeDrive::new();
    let root = PackRootKey::generate();
    let bytes = encrypted_pack(&root, "pack-e", b"SECRET");
    drive.seed("pack-e.opfspack", &bytes);

    let outcome = sync(thundoku_core::drive::sync::SyncRequest {
        pool: &env.pool,
        drive: &mut drive,
        packs_dir: &env.packs(),
        downloads_dir: &env.downloads(),
        identity_sub: Some("test-sub"),
        pack_root_key: Some(&root),
        owner_key: None,
        folder_id: "folder-1",
        sync_books: true,
        db_path: None,
        thumbnails_dir: None,
    })
    .unwrap();
    assert_eq!(outcome.downloaded, vec!["pack-e"]);
    let book = db::books::get(&env.pool, "pack-e").unwrap().unwrap();
    assert_eq!(book.title, "pack-e"); // no metadata.json in encrypted fixture
    // decryption round-trip via opfspack reader (鍵は PRK + pack id から導出)
    let key = root.derive_pack_key("pack-e");
    let local = std::fs::read(env.packs().join("pack-e.opfspack")).unwrap();
    let reader = opfspack::PackReader::open(&local).unwrap();
    assert_eq!(
        reader
            .read_entry("pages/page_0001.webp", Some(&key))
            .unwrap(),
        b"SECRET"
    );
}

/// ログイン中（鍵あり）でも平文 pack はそのまま取り込める
/// （暗号化の判定を `ENCRYPTED` にしたので、平文に鍵を要求しない）。
#[test]
fn plaintext_pack_downloads_while_logged_in() {
    let env = TestEnv::new("plaintext-logged-in");
    let mut drive = FakeDrive::new();
    let bytes = metadata_pack("平文本", "著者X", "サークルY", "2026-08-21");
    drive.seed("pack-plain.opfspack", &bytes);
    let root = PackRootKey::generate();

    let outcome = sync(thundoku_core::drive::sync::SyncRequest {
        pool: &env.pool,
        drive: &mut drive,
        packs_dir: &env.packs(),
        downloads_dir: &env.downloads(),
        identity_sub: Some("test-sub"),
        pack_root_key: Some(&root),
        owner_key: None,
        folder_id: "folder-1",
        sync_books: true,
        db_path: None,
        thumbnails_dir: None,
    })
    .unwrap();
    assert_eq!(outcome.downloaded, vec!["pack-plain"]);
    let book = db::books::get(&env.pool, "pack-plain").unwrap().unwrap();
    assert_eq!(book.title, "平文本");
    assert_eq!(book.author, "著者X");
}

/// v2 以前の pack は v3 では開けない。**再取り込み**を促すメッセージで伝える
/// （黙ってスキップして利用者に伝わらない、を避ける）。
#[test]
fn v2_pack_is_reported_as_unsupported() {
    let mut env = TestEnv::new("v2-pack");
    let mut drive = FakeDrive::new();
    // v3 の pack の version フィールドだけを 2 にしたもの（header CRC は計算し直す）。
    let mut bytes = plain_pack("pages/page_0001.webp", b"OLD-V2");
    bytes[4..8].copy_from_slice(&2u32.to_le_bytes());
    let crc = crc32(&bytes[..60]);
    bytes[60..64].copy_from_slice(&crc.to_le_bytes());
    drive.seed("pack-v2.opfspack", &bytes);

    let err = sync_env(&mut env, &mut drive).unwrap_err();
    match &err {
        SyncError::UnsupportedPackVersion { pack_id, version } => {
            assert_eq!(pack_id, "pack-v2");
            assert_eq!(*version, 2);
        }
        other => panic!("旧形式は UnsupportedPackVersion として伝える: {other:?}"),
    }
    let message = err.to_string();
    assert!(message.contains("取り込み直"), "{message}");
    assert!(db::books::get(&env.pool, "pack-v2").unwrap().is_none());
}

/// 鍵 bundle の未アップロード（初回作成時に失敗）は同期の最後に再試行される。
#[test]
fn sync_retries_pending_key_bundle_upload() {
    use thundoku_core::pack_keys::{KEY_BUNDLE_NAME, PENDING_UPLOAD_KEY};
    use thundoku_core::secrets::SecretStore;

    let env = TestEnv::new("pending-keys");
    let mut drive = FakeDrive::new();
    let sub = "sub-drive-sync-pending";
    let root = PackRootKey::generate();
    SecretStore::use_memory_backend();
    SecretStore::new()
        .save_pack_root_key(&opfspack::derive_owner_id(sub), &root.to_base64())
        .unwrap();
    db::settings::set(
        &env.pool,
        PENDING_UPLOAD_KEY,
        &opfspack::derive_owner_id(sub),
    )
    .unwrap();

    let outcome = sync(thundoku_core::drive::sync::SyncRequest {
        pool: &env.pool,
        drive: &mut drive,
        packs_dir: &env.packs(),
        downloads_dir: &env.downloads(),
        identity_sub: Some(sub),
        pack_root_key: Some(&root),
        owner_key: None,
        folder_id: "folder-1",
        sync_books: true,
        db_path: None,
        thumbnails_dir: None,
    })
    .unwrap();
    assert!(outcome.downloaded.is_empty());
    // bundle が Drive に上がり、印が消える。
    assert!(drive.files.contains_key(&format!("id-{KEY_BUNDLE_NAME}")));
    assert!(
        db::settings::get(&env.pool, PENDING_UPLOAD_KEY)
            .unwrap()
            .is_none(),
        "再試行できたら印を消す"
    );
    let bytes = &drive
        .files
        .get(&format!("id-{KEY_BUNDLE_NAME}"))
        .unwrap()
        .bytes;
    let bundle = opfspack::PackKeyBundle::from_json(bytes).unwrap();
    assert_eq!(
        bundle.unwrap_with_sub(sub).unwrap().as_bytes(),
        root.as_bytes()
    );
}

/// 所有者フィルタを構成できない（未ログイン等）ときは DB バックアップを
/// アップロードしない。空の所有集合でエクスポートすると、Drive 上の既存
/// バックアップを「本を含まない内容」で置き換えて復元手段を失うため。
#[test]
fn db_backup_is_not_replaced_when_owner_filter_is_unavailable() {
    let env = TestEnv::new("backup-guard");
    let mut drive = FakeDrive::new();
    let original = br#"{"books":[{"id":"keep-me"}]}"#.to_vec();
    let file_id = drive.seed("thundoku-backup.json", &original);
    let db_path = env.packs().join("thundoku-shelf.db");

    let outcome = sync(thundoku_core::drive::sync::SyncRequest {
        pool: &env.pool,
        drive: &mut drive,
        packs_dir: &env.packs(),
        downloads_dir: &env.downloads(),
        identity_sub: None,
        pack_root_key: None,
        owner_key: None,
        folder_id: "folder-1",
        sync_books: true,
        db_path: Some(&db_path),
        thumbnails_dir: None,
    })
    .unwrap();

    assert!(
        !outcome.database_backed_up,
        "所有者不明のときは DB バックアップを上げない"
    );
    assert_eq!(drive.upload_count(), 0, "アップロードしないこと");
    let kept = drive
        .files
        .get(&file_id)
        .expect("既存バックアップが残ること");
    assert_eq!(kept.bytes, original, "既存バックアップが置換されないこと");
}

/// 所有者フィルタが構成できるときは DB バックアップがアップロードされること
/// （上のガードが通常経路を壊していないことの確認）。
#[test]
fn db_backup_is_uploaded_when_owner_filter_is_available() {
    let env = TestEnv::new("backup-upload");
    let mut drive = FakeDrive::new();
    let key = [9u8; 32];
    // 現在の sub に所有される本を 1 冊用意する
    thundoku_core::db::books::insert(
        &env.pool,
        &thundoku_core::db::books::Book {
            id: "b1".into(),
            title: "自分の本".into(),
            author: String::new(),
            circle_name: String::new(),
            purchase_date: None,
            file_name: "b1.pdf".into(),
            file_size: 1,
            opfs_path: "b1.opfspack".into(),
            cover_thumbnail: None,
            tbf_product_id: None,
            site_id: None,
            tags_fetched: 0,
            pack_id: Some("b1".into()),
            is_favorite: 0,
            is_hidden: 0,
            created_at: "2026-08-01 00:00:00".into(),
            updated_at: "2026-08-01 00:00:00".into(),
            media_category: None,
            ai_type: None,
            is_drm: 0,
            release_date: None,
            description: None,
            theme: None,
            maker_id: None,
            page_count: None,
            age_rating: None,
            series_name: None,
        },
    )
    .unwrap();
    thundoku_core::db::books::set_owner_sub(
        &env.pool,
        "b1",
        Some(thundoku_core::owner::encrypt(&key, "sub-1")),
    )
    .unwrap();
    let db_path = env.packs().join("thundoku-shelf.db");

    let outcome = sync(thundoku_core::drive::sync::SyncRequest {
        pool: &env.pool,
        drive: &mut drive,
        packs_dir: &env.packs(),
        downloads_dir: &env.downloads(),
        identity_sub: Some("sub-1"),
        pack_root_key: None,
        owner_key: Some(&key),
        folder_id: "folder-1",
        sync_books: true,
        db_path: Some(&db_path),
        thumbnails_dir: None,
    })
    .unwrap();

    assert!(
        outcome.database_backed_up,
        "バックアップがアップロードされること"
    );
    let uploaded = drive
        .files
        .get("id-thundoku-backup.json")
        .expect("バックアップが存在すること");
    let json = String::from_utf8(uploaded.bytes.clone()).unwrap();
    assert!(json.contains("自分の本"), "所有する本が含まれること");
}

/// アップロードした DB バックアップの内容が基準値として保存され、
/// 直後の起動時チェックで「Drive が変わった（＝復元しますか）」と言わないこと。
#[test]
fn db_backup_upload_records_the_baseline_for_the_next_check() {
    let env = TestEnv::new("backup-baseline");
    let mut drive = FakeDrive::new();
    let key = [21u8; 32];
    thundoku_core::db::books::insert(
        &env.pool,
        &thundoku_core::db::books::Book {
            id: "b1".into(),
            title: "自分の本".into(),
            author: String::new(),
            circle_name: String::new(),
            purchase_date: None,
            file_name: "b1.pdf".into(),
            file_size: 1,
            opfs_path: "b1.opfspack".into(),
            cover_thumbnail: None,
            tbf_product_id: None,
            site_id: None,
            tags_fetched: 0,
            pack_id: Some("b1".into()),
            is_favorite: 0,
            is_hidden: 0,
            created_at: "2026-08-01 00:00:00".into(),
            updated_at: "2026-08-01 00:00:00".into(),
            media_category: None,
            ai_type: None,
            is_drm: 0,
            release_date: None,
            description: None,
            theme: None,
            maker_id: None,
            page_count: None,
            age_rating: None,
            series_name: None,
        },
    )
    .unwrap();
    thundoku_core::db::books::set_owner_sub(
        &env.pool,
        "b1",
        Some(thundoku_core::owner::encrypt(&key, "sub-1")),
    )
    .unwrap();
    let db_path = env.packs().join("thundoku-shelf.db");

    let outcome = sync(thundoku_core::drive::sync::SyncRequest {
        pool: &env.pool,
        drive: &mut drive,
        packs_dir: &env.packs(),
        downloads_dir: &env.downloads(),
        identity_sub: Some("sub-1"),
        pack_root_key: None,
        owner_key: Some(&key),
        folder_id: "folder-1",
        sync_books: true,
        db_path: Some(&db_path),
        thumbnails_dir: None,
    })
    .unwrap();
    assert!(outcome.database_backed_up);

    let baseline = thundoku_core::db::settings::get(
        &env.pool,
        thundoku_core::drive::sync::BACKUP_BASELINE_KEY,
    )
    .unwrap();
    let owned = thundoku_core::db::books::owned_book_ids(&env.pool, &key, Some("sub-1")).unwrap();
    let owner_filter = thundoku_core::db::backup::OwnerFilter {
        key: &key,
        sub: Some("sub-1"),
    };
    let status = thundoku_core::drive::sync::inspect_drive_backup(
        &env.pool,
        &mut drive,
        "folder-1",
        Some(&owned),
        Some(&owner_filter),
        baseline.as_deref(),
    )
    .unwrap()
    .expect("バックアップが存在すること");

    assert!(
        !status.should_offer_restore(),
        "アップロード直後の起動で復元確認を出してはいけない"
    );
}

// ---- R06: DB バックアップの暗号化（v3 の封筒。docs/spec/10-pack-keys.md §11） ----

/// keyring（テストではメモリ）に PRK を保存する。
fn save_root_key(sub: &str, root: &PackRootKey) {
    thundoku_core::secrets::SecretStore::use_memory_backend();
    thundoku_core::secrets::SecretStore::new()
        .save_pack_root_key(&opfspack::derive_owner_id(sub), &root.to_base64())
        .unwrap();
}

/// keyring から PRK を消す（鍵の無い端末を再現する）。
fn delete_root_key(sub: &str) {
    thundoku_core::secrets::SecretStore::use_memory_backend();
    let _ = thundoku_core::secrets::SecretStore::new()
        .delete_pack_root_key(&opfspack::derive_owner_id(sub));
}

/// 現在の sub が所有する本を 1 冊入れる（DB バックアップの対象）。
fn insert_owned_book(
    pool: &thundoku_core::db::SqlitePool,
    id: &str,
    title: &str,
    sub: &str,
    key: &[u8; 32],
) {
    db::books::insert(
        pool,
        &db::books::Book {
            id: id.into(),
            title: title.into(),
            author: String::new(),
            circle_name: String::new(),
            purchase_date: None,
            file_name: format!("{id}.opfspack"),
            file_size: 1,
            opfs_path: format!("{id}.opfspack"),
            cover_thumbnail: None,
            tbf_product_id: None,
            site_id: None,
            tags_fetched: 0,
            pack_id: Some(id.into()),
            is_favorite: 0,
            is_hidden: 0,
            created_at: "2026-09-01 00:00:00".into(),
            updated_at: "2026-09-01 00:00:00".into(),
            media_category: None,
            ai_type: None,
            is_drm: 0,
            release_date: None,
            description: None,
            theme: None,
            maker_id: None,
            page_count: None,
            age_rating: None,
            series_name: None,
        },
    )
    .unwrap();
    db::books::set_owner_sub(pool, id, Some(thundoku_core::owner::encrypt(key, sub))).unwrap();
}

/// DB バックアップ付きの同期（所有者と鍵を明示する）。
fn sync_with_backup(
    env: &TestEnv,
    drive: &mut dyn DriveApi,
    sub: &str,
    root: Option<&PackRootKey>,
    owner_key: &[u8; 32],
    db_path: &std::path::Path,
) -> Result<thundoku_core::drive::sync::SyncOutcome, SyncError> {
    sync(thundoku_core::drive::sync::SyncRequest {
        pool: &env.pool,
        drive,
        packs_dir: &env.packs(),
        downloads_dir: &env.downloads(),
        identity_sub: Some(sub),
        pack_root_key: root,
        owner_key: Some(owner_key),
        folder_id: "folder-1",
        sync_books: true,
        db_path: Some(db_path),
        thumbnails_dir: None,
    })
}

/// 所有者フィルタ付きの起動時チェック。
fn inspect_owned(
    env: &TestEnv,
    drive: &mut FakeDrive,
    sub: &str,
    owner_key: &[u8; 32],
    baseline: Option<&str>,
) -> Option<thundoku_core::drive::sync::BackupStatus> {
    let owned = db::books::owned_book_ids(&env.pool, owner_key, Some(sub)).unwrap();
    let filter = thundoku_core::db::backup::OwnerFilter {
        key: owner_key,
        sub: Some(sub),
    };
    thundoku_core::drive::sync::inspect_drive_backup(
        &env.pool,
        drive,
        "folder-1",
        Some(&owned),
        Some(&filter),
        baseline,
    )
    .unwrap()
}

/// バックアップファイルの中身を返す。
fn uploaded_backup(drive: &FakeDrive) -> Vec<u8> {
    drive
        .files
        .get("id-thundoku-backup.json")
        .expect("バックアップが存在すること")
        .bytes
        .clone()
}

fn baseline_of(pool: &thundoku_core::db::SqlitePool) -> Option<String> {
    db::settings::get(pool, BACKUP_BASELINE_KEY).unwrap()
}

/// PRK があれば `thundoku-backup.json` は**暗号化された封筒（v3）**として上がる。
#[test]
fn db_backup_is_encrypted_when_the_root_key_is_available() {
    let env = TestEnv::new("backup-v3");
    let mut drive = FakeDrive::new();
    let sub = "sub-backup-v3";
    let owner_key = [42u8; 32];
    let root = PackRootKey::generate();
    save_root_key(sub, &root);
    insert_owned_book(&env.pool, "b1", "自分の本", sub, &owner_key);
    let db_path = env.packs().join("thundoku-shelf.db");

    let outcome =
        sync_with_backup(&env, &mut drive, sub, Some(&root), &owner_key, &db_path).unwrap();
    assert!(outcome.database_backed_up, "初回はアップロードすること");

    let uploaded = uploaded_backup(&drive);
    let raw = String::from_utf8_lossy(&uploaded);
    assert!(
        !raw.contains("自分の本"),
        "平文（本のタイトル）が Drive のファイルに残っている"
    );
    let owner_id = opfspack::derive_owner_id(sub);
    let envelope =
        SealedEnvelope::from_json(&BACKUP_LABEL, &uploaded).expect("v3 の封筒として読める");
    assert_eq!(envelope.format_version(), 3);
    assert_eq!(envelope.owner_id(), owner_id);
    let plaintext = envelope.open(&root, &owner_id).expect("PRK で復号できる");
    assert!(
        String::from_utf8(plaintext).unwrap().contains("自分の本"),
        "封筒の中身は元のバックアップ JSON"
    );
    // 基準値は封筒の content_hmac（暗号文の md5 ではない）
    assert_eq!(
        baseline_of(&env.pool).as_deref(),
        Some(envelope.content_hmac_hex().as_str())
    );
}

/// 同じ内容を再度同期しても上げ直さない（暗号文は毎回変わるので md5 では判定できない）。
/// ローカルのユーザーデータ（お気に入り・付箋・所有情報）が空のときは、Drive に既存
/// バックアップがあっても **DB バックアップを上書きしない**。
///
/// 実測でこの事故が起きた（この端末が 0 件のまま 2 回アップロードし、他端末が入れた
/// お気に入り・付箋を失った）。門は `sync_with_progress` の 1 箇所にあり、設定の同期 /
/// 終了時アップロード / 自動同期のすべてがそこを通る。
#[test]
fn db_backup_upload_is_blocked_when_local_user_data_is_empty() {
    let env = TestEnv::new("backup-guard");
    let mut drive = FakeDrive::new();
    let sub = "sub-backup-guard";
    let owner_key = [44u8; 32];
    let root = PackRootKey::generate();
    save_root_key(sub, &root);
    insert_owned_book(&env.pool, "b1", "自分の本", sub, &owner_key);
    let db_path = env.packs().join("thundoku-shelf.db");

    // ① ユーザーデータがある状態で 1 回目 → アップロードされる
    let first =
        sync_with_backup(&env, &mut drive, sub, Some(&root), &owner_key, &db_path).unwrap();
    assert!(first.database_backed_up, "通常時はアップロードされる");
    let uploaded = uploaded_backup(&drive);

    // ② ローカルのユーザーデータを空にして（＝別端末の内容をまだ取り込んでいない状態）
    //    もう一度同期する → 門が働いて上書きしない
    thundoku_core::db::block_on(async {
        sqlx::query("DELETE FROM favorite_tags").execute(&env.pool).await.unwrap();
        sqlx::query("UPDATE books SET owner_sub = NULL").execute(&env.pool).await.unwrap();
    });
    let second =
        sync_with_backup(&env, &mut drive, sub, Some(&root), &owner_key, &db_path).unwrap();
    assert!(
        !second.database_backed_up,
        "ローカルのユーザーデータが空なら上書きしない（Drive の控えを保護）"
    );
    assert_eq!(
        uploaded_backup(&drive),
        uploaded,
        "Drive 上のバックアップは置き換わらない"
    );
}

#[test]
fn db_backup_is_not_reuploaded_when_the_content_is_unchanged() {
    let env = TestEnv::new("backup-v3-skip");
    let mut drive = FakeDrive::new();
    let sub = "sub-backup-v3-skip";
    let owner_key = [43u8; 32];
    let root = PackRootKey::generate();
    save_root_key(sub, &root);
    insert_owned_book(&env.pool, "b1", "自分の本", sub, &owner_key);
    let db_path = env.packs().join("thundoku-shelf.db");

    assert!(
        sync_with_backup(&env, &mut drive, sub, Some(&root), &owner_key, &db_path)
            .unwrap()
            .database_backed_up
    );
    let first = uploaded_backup(&drive);

    let outcome =
        sync_with_backup(&env, &mut drive, sub, Some(&root), &owner_key, &db_path).unwrap();
    assert!(
        !outcome.database_backed_up,
        "内容が同じなら再アップロードしない（content_hmac が一致する）"
    );
    assert_eq!(uploaded_backup(&drive), first, "ファイルは置き換わらない");

    // 参考: 同じ平文を封印し直すと暗号文（とファイルの md5）は変わるが、
    // 変更検知に使う content_hmac は同じ（＝暗号文 md5 を基準にしてはいけない理由）
    let owner_id = opfspack::derive_owner_id(sub);
    let envelope = SealedEnvelope::from_json(&BACKUP_LABEL, &first).unwrap();
    let plaintext = envelope.open(&root, &owner_id).unwrap();
    let resealed = SealedEnvelope::seal(&BACKUP_LABEL, &plaintext, &root, &owner_id);
    assert_eq!(resealed.content_hmac_hex(), envelope.content_hmac_hex());
    assert_ne!(resealed.to_json().unwrap(), first);
    assert_ne!(
        format!("{:x}", md5::compute(resealed.to_json().unwrap())),
        format!("{:x}", md5::compute(&first))
    );
}

/// PRK が無いときは従来どおり平文（v2）で上げる（鍵が無いだけで控えを失わない）。
#[test]
fn db_backup_stays_plaintext_without_the_root_key() {
    let env = TestEnv::new("backup-v2-fallback");
    let mut drive = FakeDrive::new();
    let sub = "sub-backup-v2-fallback";
    let owner_key = [44u8; 32];
    delete_root_key(sub);
    insert_owned_book(&env.pool, "b1", "自分の本", sub, &owner_key);
    let db_path = env.packs().join("thundoku-shelf.db");

    let outcome =
        sync_with_backup(&env, &mut drive, sub, None, &owner_key, &db_path).unwrap();
    assert!(outcome.database_backed_up);

    let uploaded = uploaded_backup(&drive);
    let text = String::from_utf8(uploaded.clone()).expect("平文の JSON");
    assert!(text.contains("自分の本"), "平文なので内容が読める");
    let backup = thundoku_core::db::backup::DriveBackup::parse(&uploaded).unwrap();
    assert!(!backup.is_encrypted(), "鍵が無いときは v2 の平文");
    assert_eq!(backup.plaintext(None).unwrap(), text);
    // 基準値は従来どおり正規形 md5（v2 の比較方法を変えない）
    assert_eq!(
        baseline_of(&env.pool),
        Some(thundoku_core::db::backup::canonical_md5_str(&text, None).unwrap())
    );

    // 2 回目は md5 一致でスキップ（従来動作）
    let outcome =
        sync_with_backup(&env, &mut drive, sub, None, &owner_key, &db_path).unwrap();
    assert!(!outcome.database_backed_up);
    assert_eq!(uploaded_backup(&drive), uploaded);
}

/// アップロード直後の起動時チェックは静か（基準値 = 封筒の `content_hmac`）。
/// 他端末が別の内容を上げたときだけ復元確認を出す。
#[test]
fn inspect_drive_backup_handles_v3_envelopes() {
    let env = TestEnv::new("backup-v3-inspect");
    let mut drive = FakeDrive::new();
    let sub = "sub-backup-v3-inspect";
    let owner_key = [45u8; 32];
    let root = PackRootKey::generate();
    save_root_key(sub, &root);
    insert_owned_book(&env.pool, "b1", "自分の本", sub, &owner_key);
    let db_path = env.packs().join("thundoku-shelf.db");

    sync_with_backup(&env, &mut drive, sub, Some(&root), &owner_key, &db_path).unwrap();
    let baseline = baseline_of(&env.pool);

    let status = inspect_owned(&env, &mut drive, sub, &owner_key, baseline.as_deref())
        .expect("バックアップが存在すること");
    assert!(!status.drive_changed, "自分が上げた封筒を『動いた』としない");
    assert!(!status.local_differs, "内容も同じ");
    assert!(!status.should_offer_restore());

    // 他端末（同じ PRK）が別の内容を上げた
    let other = thundoku_core::db::test_pool();
    db::migrate(&other).unwrap();
    insert_owned_book(&other, "b9", "他端末の本", sub, &owner_key);
    let other_json = thundoku_core::db::backup::export_json(&other, None, None, false).unwrap();
    let other_envelope = SealedEnvelope::seal(
        &BACKUP_LABEL,
        other_json.as_bytes(),
        &root,
        &opfspack::derive_owner_id(sub),
    );
    drive.seed("thundoku-backup.json", &other_envelope.to_json().unwrap());

    let status = inspect_owned(&env, &mut drive, sub, &owner_key, baseline.as_deref())
        .expect("バックアップが存在すること");
    assert!(status.drive_changed, "別端末の封筒を検出する");
    assert!(status.local_differs, "内容が違う");
    assert!(status.should_offer_restore(), "復元確認を出す");
}

/// 鍵が無ければ内容は比較できない（復元もできない）ので、復元確認を出さない。
#[test]
fn inspect_drive_backup_without_the_key_does_not_offer_a_restore() {
    let env = TestEnv::new("backup-v3-nokey");
    let mut drive = FakeDrive::new();
    let sub = "sub-backup-v3-nokey";
    let owner_key = [46u8; 32];
    let root = PackRootKey::generate();
    let owner_id = opfspack::derive_owner_id(sub);
    insert_owned_book(&env.pool, "b1", "自分の本", sub, &owner_key);

    let source = thundoku_core::db::backup::export_json(&env.pool, None, None, false).unwrap();
    let envelope = SealedEnvelope::seal(&BACKUP_LABEL, source.as_bytes(), &root, &owner_id);
    drive.seed("thundoku-backup.json", &envelope.to_json().unwrap());
    delete_root_key(sub);

    let status = inspect_owned(&env, &mut drive, sub, &owner_key, Some("stale-baseline"))
        .expect("バックアップが存在すること");
    assert!(status.drive_changed, "基準値とは違う");
    assert!(!status.local_differs, "復号できないので内容の比較はしない");
    assert!(
        !status.should_offer_restore(),
        "復元できないバックアップで確認を出さない"
    );
}

/// v3 の封筒を復号して復元でき、基準値（content_hmac）も更新される。
#[test]
fn restore_decrypts_a_v3_envelope() {
    let env = TestEnv::new("backup-v3-restore");
    let mut drive = FakeDrive::new();
    let sub = "sub-backup-v3-restore";
    let owner_key = [47u8; 32];
    let root = PackRootKey::generate();
    save_root_key(sub, &root);
    let owner_id = opfspack::derive_owner_id(sub);

    // 別端末のバックアップ（同じアカウントの PRK）を Drive に置く
    let source = thundoku_core::db::test_pool();
    db::migrate(&source).unwrap();
    insert_owned_book(&source, "b1", "復元される本", sub, &owner_key);
    let json = thundoku_core::db::backup::export_json(&source, None, None, false).unwrap();
    let envelope = SealedEnvelope::seal(&BACKUP_LABEL, json.as_bytes(), &root, &owner_id);
    drive.seed("thundoku-backup.json", &envelope.to_json().unwrap());

    thundoku_core::drive::sync::restore_drive_backup(&mut drive, "folder-1", &env.pool).unwrap();

    let book = db::books::get(&env.pool, "b1")
        .unwrap()
        .expect("復元される");
    assert_eq!(book.title, "復元される本");
    // 復元後は基準値が封筒の content_hmac になり、次の起動で「動いた」と言わない
    assert_eq!(
        baseline_of(&env.pool).as_deref(),
        Some(envelope.content_hmac_hex().as_str())
    );
    let status = inspect_owned(
        &env,
        &mut drive,
        sub,
        &owner_key,
        baseline_of(&env.pool).as_deref(),
    )
    .expect("バックアップが存在すること");
    assert!(!status.drive_changed, "復元直後は静かであること");
}

/// 鍵が無ければ復元しない（暗号文を平文として取り込む経路は無い）。
#[test]
fn restore_without_the_key_is_rejected_and_imports_nothing() {
    let env = TestEnv::new("backup-v3-restore-nokey");
    let mut drive = FakeDrive::new();
    let sub = "sub-backup-v3-restore-nokey";
    let owner_key = [48u8; 32];
    let root = PackRootKey::generate();
    let owner_id = opfspack::derive_owner_id(sub);

    let source = thundoku_core::db::test_pool();
    db::migrate(&source).unwrap();
    insert_owned_book(&source, "b1", "復元される本", sub, &owner_key);
    let json = thundoku_core::db::backup::export_json(&source, None, None, false).unwrap();
    let envelope = SealedEnvelope::seal(&BACKUP_LABEL, json.as_bytes(), &root, &owner_id);
    drive.seed("thundoku-backup.json", &envelope.to_json().unwrap());
    delete_root_key(sub);

    let error = thundoku_core::drive::sync::restore_drive_backup(&mut drive, "folder-1", &env.pool)
        .expect_err("鍵が無ければ復元しない");
    assert!(
        matches!(&error, SyncError::BackupKeyRequired(id) if id == &owner_id),
        "{error:?}"
    );
    assert!(
        db::books::get(&env.pool, "b1").unwrap().is_none(),
        "暗号文を内容として取り込まない"
    );
}

/// 別アカウントの封筒は、そのアカウントの鍵が無ければ復号できない
/// （所有者の判定は封筒の `owner_id` と keyring のスロットで行う）。
#[test]
fn a_v3_envelope_from_another_account_is_not_restored() {
    let env = TestEnv::new("backup-v3-other-account");
    let mut drive = FakeDrive::new();
    let sub = "sub-backup-v3-other-account";
    let other_sub = "sub-backup-v3-other-account-2";
    let owner_key = [49u8; 32];
    let other_root = PackRootKey::generate();
    let other_owner = opfspack::derive_owner_id(other_sub);
    // この端末の keyring には自分の鍵だけがある（別アカウントの鍵は無い）
    save_root_key(sub, &PackRootKey::generate());
    delete_root_key(other_sub);

    let source = thundoku_core::db::test_pool();
    db::migrate(&source).unwrap();
    insert_owned_book(&source, "b1", "別アカウントの本", other_sub, &owner_key);
    let json = thundoku_core::db::backup::export_json(&source, None, None, false).unwrap();
    let envelope = SealedEnvelope::seal(&BACKUP_LABEL, json.as_bytes(), &other_root, &other_owner);
    drive.seed("thundoku-backup.json", &envelope.to_json().unwrap());

    // 封筒の owner_id に対応する鍵が keyring に無い → 復号も取り込みもしない
    let error = thundoku_core::drive::sync::restore_drive_backup(&mut drive, "folder-1", &env.pool)
        .expect_err("鍵が無ければ復元しない");
    assert!(
        matches!(&error, SyncError::BackupKeyRequired(id) if id == &other_owner),
        "{error:?}"
    );
    assert!(db::books::get(&env.pool, "b1").unwrap().is_none());

    // 鍵を持っていても、AAD の `owner_id` が違えば復号できない（封筒の差し替え検知）
    assert!(
        envelope
            .open(&other_root, &opfspack::derive_owner_id(sub))
            .is_err(),
        "owner_id が違えば AAD が合わず開けない"
    );
}


/// アップロードの進捗（送れたバイト数 / 全体）が `sync_with_progress` へ流れる。
///
/// 終了時のアップロードや「今すぐ同期」の画面が、どこまで送ったかを出せるようにする。
#[test]
fn upload_progress_reports_sent_bytes() {
    let env = TestEnv::new("upload-progress");
    let mut drive = FakeDrive::new();
    let bytes = plain_pack("pages/page_0001.webp", b"PROGRESS");
    std::fs::write(env.packs().join("pack-7.opfspack"), &bytes).unwrap();
    db::books::insert(
        &env.pool,
        &db::books::Book {
            id: "pack-7".into(),
            title: "pack-7".into(),
            author: String::new(),
            circle_name: String::new(),
            purchase_date: None,
            file_name: "pack-7.opfspack".into(),
            file_size: bytes.len() as i64,
            opfs_path: "pack-7.opfspack".into(),
            cover_thumbnail: None,
            tbf_product_id: None,
            site_id: None,
            tags_fetched: 1,
            pack_id: Some("pack-7".into()),
            is_favorite: 0,
            is_hidden: 0,
            created_at: "2026-08-21 00:00:00".into(),
            updated_at: "2026-08-21 00:00:00".into(),
            media_category: None,
            ai_type: None,
            is_drm: 0,
            release_date: None,
            description: None,
            theme: None,
            maker_id: None,
            page_count: None,
            age_rating: None,
            series_name: None,
        },
    )
    .unwrap();
    let key = [11u8; 32];
    db::books::set_owner_sub(
        &env.pool,
        "pack-7",
        Some(thundoku_core::owner::encrypt(&key, "test-sub")),
    )
    .unwrap();

    let mut uploads: Vec<SyncProgress> = Vec::new();
    let outcome = sync_with_progress(
        thundoku_core::drive::sync::SyncRequest {
            pool: &env.pool,
            drive: &mut drive,
            packs_dir: &env.packs(),
            downloads_dir: &env.downloads(),
            identity_sub: Some("test-sub"),
            pack_root_key: None,
            owner_key: Some(&key),
            folder_id: "folder-1",
            sync_books: true,
            db_path: None,
            thumbnails_dir: None,
        },
        &mut |progress| {
            if progress.phase == SyncPhase::Upload {
                uploads.push(progress.clone());
            }
            true
        },
    )
    .unwrap();
    assert_eq!(outcome.uploaded, vec!["pack-7"]);
    let total = bytes.len() as u64;
    let last = uploads
        .iter()
        .rfind(|progress| progress.bytes == total)
        .unwrap_or_else(|| panic!("100% の進捗が無い: {uploads:?}"));
    assert_eq!(last.total_bytes, Some(total));
    assert_eq!(last.name, "pack-7.opfspack");
    // 途中の経過も報告される（半分の位置）
    assert!(
        uploads.iter().any(|progress| progress.bytes == total / 2),
        "途中経過が無い: {uploads:?}"
    );
}

/// バックアップ対象外の本は**アップロードしない**（大きい pack / 右クリックで外した本）。
///
/// 終了時のアップロードも同じ経路を通るので、ここが効けば終了時も上がらない。
#[test]
fn excluded_book_is_not_uploaded() {
    let env = TestEnv::new("upload-excluded");
    let mut drive = FakeDrive::new();
    let bytes = plain_pack("pages/page_0001.webp", b"EXCLUDED");
    std::fs::write(env.packs().join("pack-8.opfspack"), &bytes).unwrap();
    db::books::insert(
        &env.pool,
        &db::books::Book {
            id: "pack-8".into(),
            title: "pack-8".into(),
            author: String::new(),
            circle_name: String::new(),
            purchase_date: None,
            file_name: "pack-8.opfspack".into(),
            file_size: bytes.len() as i64,
            opfs_path: "pack-8.opfspack".into(),
            cover_thumbnail: None,
            tbf_product_id: None,
            site_id: None,
            tags_fetched: 1,
            pack_id: Some("pack-8".into()),
            is_favorite: 0,
            is_hidden: 0,
            created_at: "2026-08-21 00:00:00".into(),
            updated_at: "2026-08-21 00:00:00".into(),
            media_category: None,
            ai_type: None,
            is_drm: 0,
            release_date: None,
            description: None,
            theme: None,
            maker_id: None,
            page_count: None,
            age_rating: None,
            series_name: None,
        },
    )
    .unwrap();
    let key = [11u8; 32];
    db::books::set_owner_sub(
        &env.pool,
        "pack-8",
        Some(thundoku_core::owner::encrypt(&key, "test-sub")),
    )
    .unwrap();
    // バックアップ対象外にする（右クリックメニューの「バックアップ対象外」と同じ）
    db::books::set_backup_excluded(&env.pool, "pack-8", true).unwrap();
    assert!(db::books::is_backup_excluded(&env.pool, "pack-8").unwrap());
    assert!(
        db::books::backup_excluded_ids(&env.pool)
            .unwrap()
            .contains("pack-8")
    );

    let outcome = sync(thundoku_core::drive::sync::SyncRequest {
        pool: &env.pool,
        drive: &mut drive,
        packs_dir: &env.packs(),
        downloads_dir: &env.downloads(),
        identity_sub: Some("test-sub"),
        pack_root_key: None,
        owner_key: Some(&key),
        folder_id: "folder-1",
        sync_books: true,
        db_path: None,
        thumbnails_dir: None,
    })
    .unwrap();
    assert_eq!(drive.upload_count(), 0, "対象外なのに上げている");
    assert!(outcome.uploaded.is_empty(), "{:?}", outcome.uploaded);
    assert!(outcome.skipped.contains(&"pack-8".to_string()), "{:?}", outcome.skipped);
    assert!(!drive.files.contains_key("id-pack-8.opfspack"));

    // 戻せば上げられる（ON/OFF が効く）
    db::books::set_backup_excluded(&env.pool, "pack-8", false).unwrap();
    sync(thundoku_core::drive::sync::SyncRequest {
        pool: &env.pool,
        drive: &mut drive,
        packs_dir: &env.packs(),
        downloads_dir: &env.downloads(),
        identity_sub: Some("test-sub"),
        pack_root_key: None,
        owner_key: Some(&key),
        folder_id: "folder-1",
        sync_books: true,
        db_path: None,
        thumbnails_dir: None,
    })
    .unwrap();
    assert_eq!(drive.upload_count(), 1, "戻したのに上げていない");
}

/// Drive から削除できる（ローカルの pack は残る）。同期の状態行も消える。
///
/// 「Drive の容量を空けたい」ための操作。消したあと（対象外でなければ）次に同期したら
/// 上げ直せるよう、状態行は残さない（消しっぱなしで「上げ済み」と誤判定させない）。
#[test]
fn pack_can_be_deleted_from_drive_keeping_the_local_file() {
    let env = TestEnv::new("drive-delete");
    let mut drive = FakeDrive::new();
    let bytes = plain_pack("pages/page_0001.webp", b"TO-DELETE");
    std::fs::write(env.packs().join("pack-5.opfspack"), &bytes).unwrap();
    // Drive に上がっていて、同期の状態行もある状態を作る
    drive.seed("pack-5.opfspack", &bytes);
    db::sync_state::upsert(
        &env.pool,
        &db::sync_state::DriveSyncState {
            pack_id: "pack-5".into(),
            drive_file_id: "id-pack-5.opfspack".into(),
            md5: format!("{:x}", md5::compute(&bytes)),
            modified_time: None,
            last_synced_at: "2026-08-21 00:00:00".into(),
        },
    )
    .unwrap();

    let deleted = thundoku_core::drive::sync::delete_pack_from_drive(
        &env.pool,
        &mut drive,
        "folder-1",
        "pack-5",
    )
    .unwrap();
    assert!(deleted, "Drive から消せていない");
    assert!(!drive.files.contains_key("id-pack-5.opfspack"), "Drive に残っている");
    assert!(
        env.packs().join("pack-5.opfspack").exists(),
        "ローカルの pack まで消している"
    );
    assert!(
        db::sync_state::get(&env.pool, "pack-5").unwrap().is_none(),
        "同期の状態行が残っている（上げ済みと誤判定する）"
    );

    // 2 回目は消すものが無い（false）
    let again = thundoku_core::drive::sync::delete_pack_from_drive(
        &env.pool,
        &mut drive,
        "folder-1",
        "pack-5",
    )
    .unwrap();
    assert!(!again, "無いのに true を返している");
}

// ---- 書籍のバックアップ ON/OFF（`drive.sync.books`） ----
//
// OFF は pack（本のファイル）だけを止める。DB バックアップ（`thundoku-backup.json`）と
// pack 鍵 bundle（`thundoku-keys.json`）は役割が別なので、これまでどおり同期する。

/// OFF では pack を**上げない**。ON に戻せば同じ経路で上がる。
#[test]
fn sync_books_off_does_not_upload_packs() {
    let env = TestEnv::new("books-off-upload");
    let mut drive = FakeDrive::new();
    let bytes = plain_pack("pages/page_0001.webp", b"BOOKS-OFF");
    let key = seed_owned_local_pack(&env, "pack-11", &bytes);

    let outcome = sync(thundoku_core::drive::sync::SyncRequest {
        pool: &env.pool,
        drive: &mut drive,
        packs_dir: &env.packs(),
        downloads_dir: &env.downloads(),
        identity_sub: Some("test-sub"),
        pack_root_key: None,
        owner_key: Some(&key),
        folder_id: "folder-1",
        sync_books: false,
        db_path: None,
        thumbnails_dir: None,
    })
    .unwrap();

    assert_eq!(drive.upload_count(), 0, "OFF なのに pack を上げている");
    assert!(outcome.uploaded.is_empty(), "{:?}", outcome.uploaded);
    assert!(!drive.files.contains_key("id-pack-11.opfspack"));
    assert!(
        db::sync_state::get(&env.pool, "pack-11").unwrap().is_none(),
        "上げていないのに同期の状態行を書いている"
    );

    // ON なら上がる（フラグが効いている = OFF だから止まった）
    let outcome = sync(thundoku_core::drive::sync::SyncRequest {
        pool: &env.pool,
        drive: &mut drive,
        packs_dir: &env.packs(),
        downloads_dir: &env.downloads(),
        identity_sub: Some("test-sub"),
        pack_root_key: None,
        owner_key: Some(&key),
        folder_id: "folder-1",
        sync_books: true,
        db_path: None,
        thumbnails_dir: None,
    })
    .unwrap();
    assert_eq!(outcome.uploaded, vec!["pack-11"]);
    assert_eq!(drive.upload_count(), 1);
}

/// OFF では Drive にある pack を**取得しない**（本の登録・置き場への書き込みもしない）。
#[test]
fn sync_books_off_does_not_download_packs() {
    let mut env = TestEnv::new("books-off-download");
    let mut drive = FakeDrive::new();
    let bytes = metadata_pack("メタ本", "著者X", "サークルY", "2026-08-21");
    drive.seed("pack-12.opfspack", &bytes);

    let outcome = sync_env_with_books(&mut env, &mut drive, false).unwrap();

    assert_eq!(drive.download_count(), 0, "OFF なのに pack を取得している");
    assert!(outcome.downloaded.is_empty(), "{:?}", outcome.downloaded);
    assert!(
        !env.packs().join("pack-12.opfspack").exists(),
        "OFF なのに置き場へ書いている"
    );
    assert!(
        db::books::get(&env.pool, "pack-12").unwrap().is_none(),
        "OFF なのに本を登録している"
    );

    // ON なら取得する（フラグが効いている = OFF だから止まった）
    let outcome = sync_env_with_books(&mut env, &mut drive, true).unwrap();
    assert_eq!(outcome.downloaded, vec!["pack-12"]);
    assert_eq!(drive.download_count(), 1);
    assert!(env.packs().join("pack-12.opfspack").exists());
}

/// OFF でも DB バックアップ（`thundoku-backup.json`）は今までどおり上がる。
///
/// 所有フィルタを空にしてしまうと Drive 上の控えを「本を含まない内容」で
/// 置き換えてしまうため、中身に所有する本が入っていることまで確かめる。
#[test]
fn sync_books_off_still_uploads_the_database_backup() {
    let env = TestEnv::new("books-off-backup");
    let mut drive = FakeDrive::new();
    let sub = "sub-books-off";
    let owner_key = [7u8; 32];
    let db_path = env.root.join("thundoku-shelf.db");
    insert_owned_book(&env.pool, "book-1", "自分の本", sub, &owner_key);
    // ローカルに pack も置く（OFF なので上げてはいけない）
    std::fs::write(
        env.packs().join("book-1.opfspack"),
        plain_pack("pages/page_0001.webp", b"LOCAL-ONLY"),
    )
    .unwrap();

    let outcome = sync(thundoku_core::drive::sync::SyncRequest {
        pool: &env.pool,
        drive: &mut drive,
        packs_dir: &env.packs(),
        downloads_dir: &env.downloads(),
        identity_sub: Some(sub),
        pack_root_key: None,
        owner_key: Some(&owner_key),
        folder_id: "folder-1",
        sync_books: false,
        db_path: Some(&db_path),
        thumbnails_dir: None,
    })
    .unwrap();

    assert!(
        outcome.database_backed_up,
        "OFF でも DB バックアップは上げる"
    );
    let json = String::from_utf8(uploaded_backup(&drive)).unwrap();
    assert!(json.contains("自分の本"), "所有する本が含まれること");
    assert!(outcome.uploaded.is_empty(), "pack を上げてはいけない");
    assert!(!drive.files.contains_key("id-book-1.opfspack"));
    assert_eq!(drive.upload_count(), 1, "上がったのは DB バックアップだけ");
}

/// 設定キー `drive.sync.books` は**行が無ければ ON**（既定で本をバックアップする）。
#[test]
fn books_backup_is_on_without_a_setting_row() {
    let pool = thundoku_core::db::test_pool();
    thundoku_core::db::migrate(&pool).unwrap();

    assert!(
        thundoku_core::drive::sync::books_backup_enabled(&pool),
        "未設定は ON（既定）"
    );
    thundoku_core::db::settings::set(&pool, "drive.sync.books", "false").unwrap();
    assert!(
        !thundoku_core::drive::sync::books_backup_enabled(&pool),
        "\"false\" は OFF"
    );
    thundoku_core::db::settings::set(&pool, "drive.sync.books", "true").unwrap();
    assert!(
        thundoku_core::drive::sync::books_backup_enabled(&pool),
        "\"true\" は ON"
    );
}

// ---- 表紙バンドル（`thundoku-thumbs.json`、仕様 §11.8）-----------------------

/// 表紙バンドル（と必要なら DB バックアップ）を上げる同期。
#[allow(clippy::too_many_arguments)]
fn sync_thumbnails(
    env: &TestEnv,
    drive: &mut dyn DriveApi,
    sub: &str,
    root: Option<&PackRootKey>,
    owner_key: Option<&[u8; 32]>,
    db_path: Option<&std::path::Path>,
    thumbnails_dir: Option<&std::path::Path>,
) -> Result<thundoku_core::drive::sync::SyncOutcome, SyncError> {
    sync(thundoku_core::drive::sync::SyncRequest {
        pool: &env.pool,
        drive,
        packs_dir: &env.packs(),
        downloads_dir: &env.downloads(),
        identity_sub: Some(sub),
        pack_root_key: root,
        owner_key,
        folder_id: "folder-1",
        sync_books: false,
        db_path,
        thumbnails_dir,
    })
}

/// 所有する本棚アイテムを作り、448px の表紙キャッシュを置く。
fn seed_owned_shelf_cover(
    env: &TestEnv,
    sub: &str,
    owner_key: &[u8; 32],
    site_id: &str,
    database_id: &str,
    thumbnails_dir: &std::path::Path,
) {
    thundoku_core::db::block_on(async {
        sqlx::query(
            "INSERT INTO bookshelf_items (site_id, database_id, title, owner_sub) \
             VALUES (?1, ?2, '本', ?3)",
        )
        .bind(site_id)
        .bind(database_id)
        .bind(thundoku_core::owner::encrypt(owner_key, sub))
        .execute(&env.pool)
        .await
        .unwrap();
    });
    std::fs::create_dir_all(thumbnails_dir).unwrap();
    let image = image::RgbaImage::from_fn(448, 672, |x, y| {
        image::Rgba([(x % 251) as u8, (y % 241) as u8, 96, 255])
    });
    image
        .save(thumbnails_dir.join(format!("{site_id}_{database_id}_448.png")))
        .unwrap();
}

fn uploaded_thumbnails(drive: &FakeDrive) -> Vec<u8> {
    drive
        .files
        .get("id-thundoku-thumbs.json")
        .expect("表紙バンドルが存在すること")
        .bytes
        .clone()
}

/// 表紙バンドルは PRK で封をして `thundoku-thumbs.json` として上がる。
#[test]
fn thumbnail_bundle_is_uploaded_as_an_encrypted_envelope() {
    let env = TestEnv::new("thumbs-v3");
    let mut drive = FakeDrive::new();
    let sub = "sub-thumbs-v3";
    let owner_key = [51u8; 32];
    let root = PackRootKey::generate();
    save_root_key(sub, &root);
    let dir = env.root.join("thumbnails");
    seed_owned_shelf_cover(&env, sub, &owner_key, "dlsite", "RJ1", &dir);

    let outcome = sync_thumbnails(
        &env,
        &mut drive,
        sub,
        Some(&root),
        Some(&owner_key),
        None,
        Some(&dir),
    )
    .unwrap();
    assert!(outcome.thumbnails_backed_up, "初回はアップロードすること");

    let uploaded = uploaded_thumbnails(&drive);
    let raw = String::from_utf8_lossy(&uploaded);
    assert!(
        !raw.contains("database_id") && !raw.contains("dlsite"),
        "平文（表紙のキー）が Drive のファイルに残っている"
    );
    let owner_id = opfspack::derive_owner_id(sub);
    let envelope = SealedEnvelope::from_json(&opfspack::THUMBS_LABEL, &uploaded)
        .expect("表紙バンドルの封筒として読める");
    assert_eq!(
        envelope.format_version(),
        1,
        "バックアップの封筒（3）と別の版"
    );
    assert_eq!(envelope.owner_id(), owner_id);
    let plaintext = envelope.open(&root, &owner_id).expect("PRK で復号できる");
    let value: serde_json::Value = serde_json::from_slice(&plaintext).unwrap();
    assert_eq!(value["format_version"], 1);
    let entries = value["entries"].as_array().unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0]["kind"], "shelf");
    assert_eq!(entries[0]["site_id"], "dlsite");
    assert_eq!(entries[0]["database_id"], "RJ1");
    assert_eq!(entries[0]["mime"], "image/webp");

    assert_eq!(
        db::settings::get(&env.pool, "drive.thumbs.hash")
            .unwrap()
            .as_deref(),
        Some(envelope.content_hmac_hex().as_str()),
        "変更検知の基準値は封筒の content_hmac"
    );
    assert_eq!(
        db::settings::get(&env.pool, "drive.thumbs.failed").unwrap(),
        None,
        "成功したら失敗の印を残さない"
    );
}

/// 同じ内容を再度同期しても上げ直さない（暗号文は毎回変わるので md5 では判定できない）。
#[test]
fn thumbnail_bundle_is_not_reuploaded_when_unchanged() {
    let env = TestEnv::new("thumbs-skip");
    let mut drive = FakeDrive::new();
    let sub = "sub-thumbs-skip";
    let owner_key = [52u8; 32];
    let root = PackRootKey::generate();
    save_root_key(sub, &root);
    let dir = env.root.join("thumbnails");
    seed_owned_shelf_cover(&env, sub, &owner_key, "dlsite", "RJ1", &dir);

    assert!(
        sync_thumbnails(
            &env,
            &mut drive,
            sub,
            Some(&root),
            Some(&owner_key),
            None,
            Some(&dir)
        )
        .unwrap()
        .thumbnails_backed_up
    );
    let first = uploaded_thumbnails(&drive);

    let outcome = sync_thumbnails(
        &env,
        &mut drive,
        sub,
        Some(&root),
        Some(&owner_key),
        None,
        Some(&dir),
    )
    .unwrap();
    assert!(!outcome.thumbnails_backed_up, "内容が同じなら上げ直さない");
    assert_eq!(
        uploaded_thumbnails(&drive),
        first,
        "ファイルは置き換わらない"
    );
    assert_eq!(drive.upload_count(), 1, "アップロードは 1 回だけ");
}

/// PRK が無いときは表紙バンドルを上げない（平文で蔵書を晒さない。
/// DB バックアップの平文フォールバックとは判断が違う — 表紙は再取得できる）。
#[test]
fn thumbnail_bundle_is_not_uploaded_without_the_root_key() {
    let env = TestEnv::new("thumbs-no-key");
    let mut drive = FakeDrive::new();
    let sub = "sub-thumbs-no-key";
    let owner_key = [53u8; 32];
    let dir = env.root.join("thumbnails");
    seed_owned_shelf_cover(&env, sub, &owner_key, "dlsite", "RJ1", &dir);

    let outcome = sync_thumbnails(
        &env,
        &mut drive,
        sub,
        None,
        Some(&owner_key),
        None,
        Some(&dir),
    )
    .unwrap();

    assert!(!outcome.thumbnails_backed_up);
    assert!(
        !drive.files.contains_key("id-thundoku-thumbs.json"),
        "鍵が無いときは平文でも上げない"
    );
    assert_eq!(drive.upload_count(), 0);
}

/// 表紙を 1 枚も作れないときは、Drive 上の既存バンドルを空で置き換えない
/// （表紙キャッシュを消した端末が、Web 側の表紙を全部消してしまうのを防ぐ）。
#[test]
fn an_empty_bundle_never_replaces_the_existing_one() {
    let env = TestEnv::new("thumbs-empty-guard");
    let mut drive = FakeDrive::new();
    let sub = "sub-thumbs-empty-guard";
    let owner_key = [54u8; 32];
    let root = PackRootKey::generate();
    save_root_key(sub, &root);
    let dir = env.root.join("thumbnails");
    // 本はあるが表紙キャッシュは無い（＝取得元が無い）
    seed_owned_shelf_cover(&env, sub, &owner_key, "dlsite", "RJ1", &dir);
    std::fs::remove_file(dir.join("dlsite_RJ1_448.png")).unwrap();
    let existing = b"{\"entries\":[{\"keep\":true}]}".to_vec();
    let file_id = drive.seed("thundoku-thumbs.json", &existing);

    let outcome = sync_thumbnails(
        &env,
        &mut drive,
        sub,
        Some(&root),
        Some(&owner_key),
        None,
        Some(&dir),
    )
    .unwrap();

    assert!(!outcome.thumbnails_backed_up);
    assert_eq!(drive.upload_count(), 0, "空のバンドルを上げない");
    assert_eq!(
        drive.files.get(&file_id).unwrap().bytes,
        existing,
        "既存のバンドルが残ること"
    );
}

/// 表紙バンドルの作成が失敗しても同期全体は失敗させない（再取得できる派生データ。
/// 次の同期でやり直す）。
#[test]
fn a_broken_thumbnail_store_does_not_fail_the_sync() {
    let env = TestEnv::new("thumbs-broken");
    let mut drive = FakeDrive::new();
    let sub = "sub-thumbs-broken";
    let owner_key = [55u8; 32];
    let root = PackRootKey::generate();
    save_root_key(sub, &root);
    let dir = env.root.join("thumbnails");
    seed_owned_shelf_cover(&env, sub, &owner_key, "dlsite", "RJ1", &dir);
    insert_owned_book(&env.pool, "b1", "自分の本", sub, &owner_key);
    let db_path = env.packs().join("thundoku-shelf.db");
    // 派生キャッシュのテーブルを壊す（バンドルの組み立てだけが失敗する）
    thundoku_core::db::block_on(async {
        sqlx::query("DROP TABLE thumbnail_share")
            .execute(&env.pool)
            .await
            .unwrap();
    });

    let outcome = sync_thumbnails(
        &env,
        &mut drive,
        sub,
        Some(&root),
        Some(&owner_key),
        Some(&db_path),
        Some(&dir),
    )
    .expect("表紙の失敗で同期全体を落とさない");

    assert!(outcome.database_backed_up, "DB バックアップは上がること");
    assert!(!outcome.thumbnails_backed_up);
    assert!(!drive.files.contains_key("id-thundoku-thumbs.json"));
    assert_eq!(
        db::settings::get(&env.pool, "drive.thumbs.failed")
            .unwrap()
            .as_deref(),
        Some("1"),
        "失敗の印を残して設定画面に出せるようにする"
    );
}

// ---- 終了時のアップロード確認（未アップロードの変更があるか）-----------------

/// 終了時に確認を出すかの判定（所有者・PRK・書籍バックアップの設定を渡す省略形）。
fn pending_backup_changes(
    env: &TestEnv,
    sub: Option<&str>,
    owner_key: Option<&[u8; 32]>,
    root: Option<&PackRootKey>,
    sync_books: bool,
) -> bool {
    thundoku_core::drive::sync::pending_backup_changes(
        &env.pool,
        &env.packs(),
        sub,
        owner_key,
        root,
        sync_books,
    )
    .unwrap()
}

/// 所有者が分からないとき（未ログイン）は確認を出さない（同期も何も上げない）。
#[test]
fn pending_backup_changes_is_false_without_an_owner() {
    let env = TestEnv::new("pending-no-owner");
    assert!(
        !pending_backup_changes(&env, None, None, None, true),
        "所有者不明で「変更あり」にすると、上げられないのに確認を出すことになる"
    );
}

/// 同期直後は「未アップロードの変更なし」（終了時に確認を出さない）。
#[test]
fn pending_backup_changes_is_false_right_after_a_sync() {
    let env = TestEnv::new("pending-none");
    let mut drive = FakeDrive::new();
    let sub = "sub-pending-none";
    let owner_key = [61u8; 32];
    let root = PackRootKey::generate();
    save_root_key(sub, &root);
    insert_owned_book(&env.pool, "b1", "自分の本", sub, &owner_key);
    std::fs::write(env.packs().join("b1.opfspack"), b"PACK").unwrap();
    let db_path = env.packs().join("thundoku-shelf.db");

    let outcome =
        sync_with_backup(&env, &mut drive, sub, Some(&root), &owner_key, &db_path).unwrap();
    assert!(
        outcome.database_backed_up,
        "前提: DB バックアップが上がること"
    );
    assert_eq!(outcome.uploaded, vec!["b1"], "前提: pack も上がること");

    assert!(
        !pending_backup_changes(&env, Some(sub), Some(&owner_key), Some(&root), true),
        "同期直後に確認を出すと、何も変えていない利用者に毎回訊くことになる"
    );
}

/// 同期のあとに取り込んだ本（pack も DB も新しい）は「変更あり」。
#[test]
fn pending_backup_changes_is_true_for_a_new_book() {
    let env = TestEnv::new("pending-new-book");
    let mut drive = FakeDrive::new();
    let sub = "sub-pending-new-book";
    let owner_key = [62u8; 32];
    let root = PackRootKey::generate();
    save_root_key(sub, &root);
    insert_owned_book(&env.pool, "b1", "自分の本", sub, &owner_key);
    std::fs::write(env.packs().join("b1.opfspack"), b"PACK").unwrap();
    let db_path = env.packs().join("thundoku-shelf.db");
    sync_with_backup(&env, &mut drive, sub, Some(&root), &owner_key, &db_path).unwrap();

    insert_owned_book(&env.pool, "b2", "新しい本", sub, &owner_key);
    std::fs::write(env.packs().join("b2.opfspack"), b"PACK2").unwrap();

    assert!(
        pending_backup_changes(&env, Some(sub), Some(&owner_key), Some(&root), true),
        "新しい本を上げそこねて終了する"
    );
}

/// DB のユーザーデータ（お気に入り）を変えたら「変更あり」。
#[test]
fn pending_backup_changes_is_true_when_user_data_changed() {
    let env = TestEnv::new("pending-user-data");
    let mut drive = FakeDrive::new();
    let sub = "sub-pending-user-data";
    let owner_key = [63u8; 32];
    let root = PackRootKey::generate();
    save_root_key(sub, &root);
    insert_owned_book(&env.pool, "b1", "自分の本", sub, &owner_key);
    std::fs::write(env.packs().join("b1.opfspack"), b"PACK").unwrap();
    let db_path = env.packs().join("thundoku-shelf.db");
    sync_with_backup(&env, &mut drive, sub, Some(&root), &owner_key, &db_path).unwrap();

    db::books::set_favorite(&env.pool, "b1", true).unwrap();

    assert!(
        pending_backup_changes(&env, Some(sub), Some(&owner_key), Some(&root), true),
        "お気に入りを上げそこねて終了する"
    );
}

/// pack が最後の同期より新しいときも「変更あり」。書籍バックアップ OFF なら見ない
/// （同期も pack を上げないため）。
#[test]
fn pending_backup_changes_notices_a_pack_newer_than_the_last_sync() {
    let env = TestEnv::new("pending-pack-newer");
    let mut drive = FakeDrive::new();
    let sub = "sub-pending-pack-newer";
    let owner_key = [64u8; 32];
    let root = PackRootKey::generate();
    save_root_key(sub, &root);
    insert_owned_book(&env.pool, "b1", "自分の本", sub, &owner_key);
    std::fs::write(env.packs().join("b1.opfspack"), b"PACK").unwrap();
    let db_path = env.packs().join("thundoku-shelf.db");
    sync_with_backup(&env, &mut drive, sub, Some(&root), &owner_key, &db_path).unwrap();

    // 同期の記帳を過去に倒す（mtime の比較だけで判定できる決定的な状態にする）
    let state = db::sync_state::get(&env.pool, "b1").unwrap().unwrap();
    db::sync_state::upsert(
        &env.pool,
        &db::sync_state::DriveSyncState {
            last_synced_at: "2000-01-01 00:00:00".into(),
            ..state
        },
    )
    .unwrap();

    assert!(
        pending_backup_changes(&env, Some(sub), Some(&owner_key), Some(&root), true),
        "更新された pack を上げそこねて終了する"
    );
    assert!(
        !pending_backup_changes(&env, Some(sub), Some(&owner_key), Some(&root), false),
        "書籍バックアップ OFF の pack は上げない（確認も出さない）"
    );
}
