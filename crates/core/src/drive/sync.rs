//! Bidirectional sync engine between the local library and the
//! `thundoku-shelf/` folder in My Drive.
//!
//! Rules (mirrors the plan):
//! - Drive-side: `*.opfspack` files whose md5 differs from the recorded
//!   state are downloaded, validated and imported.
//! - Local-side: packs with no state row (new) or a local mtime newer than
//!   `last_synced_at` are uploaded.
//! - Conflict (both changed, different content): Drive wins; the local pack
//!   is backed up to `{packId}.conflict-local.opfspack`.
//! - Deletions are never propagated in either direction.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use opfspack::{Identity, PackError, PackReader, entry_flags};

use crate::db::{SqlitePool, books, sync_state};
use crate::drive::{DriveApi, DriveError, DriveFile};

pub const PACK_EXTENSION: &str = "opfspack";
const META_ENTRY: &str = "metadata.json";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct SyncOutcome {
    pub downloaded: Vec<String>,
    pub uploaded: Vec<String>,
    pub skipped: Vec<String>,
    pub conflicts: Vec<String>,
    /// Drive フォルダ内の同期対象（.opfspack）ファイル数
    pub file_count: usize,
    /// Drive フォルダ内の同期対象ファイルの合計サイズ（bytes）
    pub total_bytes: u64,
    /// DB（thundoku-shelf.db）を Drive にバックアップしたか
    pub database_backed_up: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum SyncError {
    #[error("drive error: {0}")]
    Drive(#[from] DriveError),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("pack error: {0}")]
    Pack(#[from] PackError),
    #[error("invalid pack: {0}")]
    InvalidPack(String),
    #[error("identity required to decrypt pack: {0}")]
    IdentityRequired(String),
}

fn pack_id_from_name(name: &str) -> Option<&str> {
    name.strip_suffix(&format!(".{PACK_EXTENSION}"))
        .filter(|id| {
            !id.is_empty()
                && !id.contains('/')
                && !id.contains('\\')
                && !id.contains("..")
                && !id.contains(':')
                && !id.starts_with('.')
                && !id.chars().any(char::is_control)
        })
}

fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Local pack file mtime (seconds) strictly newer than `last_synced_at`
/// ("%Y-%m-%d %H:%M:%S"). Any parse failure means "not changed".
fn local_newer_than(path: &Path, last_synced_at: &str) -> bool {
    if !path.exists() {
        return false;
    }
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    let Ok(modified) = metadata.modified() else {
        return false;
    };
    let Ok(elapsed) = modified.duration_since(UNIX_EPOCH) else {
        return false;
    };
    let mtime = elapsed.as_secs() as i64;
    let Ok(synced) = chrono::NaiveDateTime::parse_from_str(last_synced_at, "%Y-%m-%d %H:%M:%S")
    else {
        return false;
    };
    mtime > synced.and_utc().timestamp()
}

/// Import a downloaded pack into `books`, preferring `metadata.json` fields.
fn import_book(
    pool: &SqlitePool,
    reader: &PackReader,
    pack_id: &str,
    identity_sub: Option<&str>,
    file_size: i64,
) -> Result<(), SyncError> {
    let mut title = pack_id.to_string();
    let mut author = String::new();
    let mut circle_name = String::new();
    let mut purchase_date: Option<String> = None;
    let identity = identity_sub.map(|sub| Identity {
        sub: sub.to_string(),
        pack_id: pack_id.to_string(),
    });
    if let Ok(data) = reader.read_entry(META_ENTRY, identity.as_ref())
        && let Ok(metadata) = serde_json::from_slice::<serde_json::Value>(&data)
    {
        if let Some(value) = metadata.get("title").and_then(serde_json::Value::as_str) {
            title = value.to_string();
        }
        if let Some(value) = metadata.get("author").and_then(serde_json::Value::as_str) {
            author = value.to_string();
        }
        if let Some(value) = metadata
            .get("circleName")
            .and_then(serde_json::Value::as_str)
        {
            circle_name = value.to_string();
        }
        if let Some(value) = metadata
            .get("purchaseDate")
            .and_then(serde_json::Value::as_str)
        {
            purchase_date = Some(value.to_string());
        }
    }

    let timestamp = now();
    books::upsert(
        pool,
        &books::Book {
            id: pack_id.to_string(),
            title,
            author,
            circle_name,
            purchase_date,
            file_name: format!("{pack_id}.{PACK_EXTENSION}"),
            file_size,
            opfs_path: format!("{pack_id}.{PACK_EXTENSION}"),
            cover_thumbnail: None,
            tbf_product_id: None,
            site_id: None,
            tags_fetched: 1,
            pack_id: Some(pack_id.to_string()),
            is_favorite: 0,
            is_hidden: 0,
            created_at: timestamp.clone(),
            updated_at: timestamp,
        },
    )?;
    Ok(())
}

/// Run one full sync pass.
pub fn sync(
    pool: &SqlitePool,
    drive: &mut dyn DriveApi,
    packs_dir: &Path,
    downloads_dir: &Path,
    identity_sub: Option<&str>,
    folder_id: &str,
    db_path: Option<&Path>,
) -> Result<SyncOutcome, SyncError> {
    log::info!("drive sync: list_files start");
    let files = drive.list_files(folder_id)?;
    log::info!("drive sync: list_files -> {} files", files.len());
    let mut outcome = SyncOutcome::default();

    let mut drive_pack_by_id: HashMap<&str, &DriveFile> = HashMap::new();
    for file in &files {
        if let Some(pack_id) = pack_id_from_name(&file.name) {
            drive_pack_by_id.entry(pack_id).or_insert(file);
        }
    }
    // バックアップ対象（.opfspack + DB JSON）を含むフォルダ全体の集計
    outcome.file_count = files.len();
    outcome.total_bytes = files
        .iter()
        .filter_map(|f| f.size)
        .map(|size| size.max(0) as u64)
        .sum();

    log::info!("drive sync: download direction start");
    // -- download direction -------------------------------------------------
    for (pack_id, file) in &drive_pack_by_id {
        let state = sync_state::get(pool, pack_id)?;
        let drive_md5 = file.md5_checksum.clone().unwrap_or_default();
        if let Some(state) = &state
            && !drive_md5.is_empty()
            && drive_md5 == state.md5
        {
            outcome.skipped.push((*pack_id).to_string());
            continue;
        }
        let local_path = packs_dir.join(format!("{pack_id}.{PACK_EXTENSION}"));
        let local_changed = state
            .as_ref()
            .is_some_and(|s| local_newer_than(&local_path, &s.last_synced_at));

        let bytes = drive.download(&file.id)?;
        let reader = PackReader::open(&bytes)
            .map_err(|e| SyncError::InvalidPack(format!("{pack_id}: {e}")))?;
        let needs_identity = reader
            .entries()
            .iter()
            .any(|entry| entry.flags & entry_flags::IDENTITY_BOUND != 0);
        if needs_identity && identity_sub.is_none() {
            return Err(SyncError::IdentityRequired((*pack_id).to_string()));
        }

        if local_changed && local_path.exists() {
            let backup = packs_dir.join(format!("{pack_id}.conflict-local.{PACK_EXTENSION}"));
            std::fs::copy(&local_path, &backup)?;
            outcome.conflicts.push((*pack_id).to_string());
        }
        std::fs::create_dir_all(downloads_dir)?;
        std::fs::create_dir_all(packs_dir)?;
        let temp = downloads_dir.join(format!("{pack_id}.{PACK_EXTENSION}"));
        std::fs::write(&temp, &bytes)?;
        std::fs::write(&local_path, &bytes)?;
        import_book(pool, &reader, pack_id, identity_sub, bytes.len() as i64)?;
        sync_state::upsert(
            pool,
            &sync_state::DriveSyncState {
                pack_id: (*pack_id).to_string(),
                drive_file_id: file.id.clone(),
                md5: drive_md5.clone(),
                modified_time: file.modified_time.clone(),
                last_synced_at: now(),
            },
        )?;
        outcome.downloaded.push((*pack_id).to_string());
    }

    log::info!("drive sync: download direction done, upload direction start");
    // -- upload direction ---------------------------------------------------
    let drive_ids: Vec<&str> = files.iter().map(|file| file.id.as_str()).collect();
    for book in books::list(pool)? {
        let pack_id = book
            .pack_id
            .clone()
            .or_else(|| Some(book.id.clone()))
            .unwrap_or_default();
        if pack_id.is_empty() {
            continue;
        }
        let local_path: PathBuf = packs_dir.join(format!("{pack_id}.{PACK_EXTENSION}"));
        if !local_path.exists() {
            continue;
        }
        let state = sync_state::get(pool, &pack_id)?;
        let should_upload = match &state {
            None => true,
            Some(state) => {
                // Deletion non-propagation: if the drive file disappeared,
                // leave everything as is.
                let drive_has = drive_ids.iter().any(|id| **id == state.drive_file_id);
                drive_has && local_newer_than(&local_path, &state.last_synced_at)
            }
        };
        if should_upload {
            let bytes = std::fs::read(&local_path)?;
            let file_id = drive.upload_multipart(
                &format!("{pack_id}.{PACK_EXTENSION}"),
                folder_id,
                &bytes,
            )?;
            sync_state::upsert(
                pool,
                &sync_state::DriveSyncState {
                    pack_id: pack_id.clone(),
                    drive_file_id: file_id,
                    md5: format!("{:x}", md5::compute(&bytes)),
                    modified_time: None,
                    last_synced_at: now(),
                },
            )?;
            outcome.uploaded.push(pack_id);
        }
    }

    log::info!("drive sync: upload direction done, database backup start");
    // -- database backup ---------------------------------------------------
    // DB のテキストデータを JSON にまとめて `thundoku-backup.json` として
    // Drive にバックアップする。画像 base64 は含めず、md5 が変わったとき
    // だけアップロードする（200MB 級の DB ファイル全体は上げない）。
    if db_path.is_some() {
        let json = crate::db::backup::export_json(pool)?;
        let bytes = json.into_bytes();
        let local_md5 = format!("{:x}", md5::compute(&bytes));
        let existing = files.iter().find(|f| f.name == DB_BACKUP_NAME);
        let needs_upload = existing
            .map(|f| f.md5_checksum.as_deref() != Some(&local_md5))
            .unwrap_or(true);
        if needs_upload {
            log::info!(
                "drive sync: uploading database backup ({} bytes)",
                bytes.len()
            );
            if let Some(file) = existing {
                drive.delete(&file.id)?;
            }
            drive.upload_multipart(DB_BACKUP_NAME, folder_id, &bytes)?;
            outcome.database_backed_up = true;
        } else {
            // データが変わらなくても、バックアップの更新日時が古いままに
            // ならないよう modifiedTime だけ現在時刻に更新する
            log::info!("drive sync: database unchanged, touching modified time");
            if let Some(file) = existing {
                drive.touch(&file.id)?;
            }
        }
    }

    Ok(outcome)
}

/// Drive 上の DB バックアップのファイル名。
const DB_BACKUP_NAME: &str = "thundoku-backup.json";

/// Drive 同期の状態（drive_sync_state の行と drive.* 設定）をクリアする。
/// 次回同期時に全ファイルが再アップロード/再ダウンロードの対象になる。
pub fn clear_sync_state(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query("DELETE FROM drive_sync_state")
            .execute(pool)
            .await?;
        Ok::<(), sqlx::Error>(())
    })?;
    for key in [
        "drive.sync.enabled",
        "drive.sync.folder_id",
        "drive.last_sync_at",
        "drive.file_count",
        "drive.total_bytes",
    ] {
        let _ = crate::db::settings::delete(pool, key);
    }
    Ok(())
}

#[cfg(test)]
mod tests {

    use crate::db::{migrate, settings, sync_state};

    #[test]
    fn clear_sync_state_removes_rows_and_drive_settings() {
        let pool = crate::db::test_pool();
        migrate(&pool).unwrap();
        // 同期状態と設定を入れる
        sync_state::upsert(
            &pool,
            &sync_state::DriveSyncState {
                pack_id: "pack1".into(),
                drive_file_id: "file1".into(),
                md5: "md5".into(),
                modified_time: None,
                last_synced_at: "2026-08-23 10:00:00".into(),
            },
        )
        .unwrap();
        settings::set(&pool, "drive.sync.enabled", "1").unwrap();
        settings::set(&pool, "drive.sync.folder_id", "folder123").unwrap();
        settings::set(&pool, "drive.last_sync_at", "2026-08-23 10:00:00").unwrap();
        settings::set(&pool, "drive.file_count", "3").unwrap();
        settings::set(&pool, "drive.total_bytes", "1024").unwrap();
        // 他機能の設定は残すべき
        settings::set(&pool, "api.last_sync_at", "2026-08-23 09:00:00").unwrap();

        super::clear_sync_state(&pool).unwrap();

        assert!(sync_state::list(&pool).unwrap().is_empty(), "rows cleared");
        for key in [
            "drive.sync.enabled",
            "drive.sync.folder_id",
            "drive.last_sync_at",
            "drive.file_count",
            "drive.total_bytes",
        ] {
            assert!(
                settings::get(&pool, key).unwrap().is_none(),
                "{key} must be cleared"
            );
        }
        assert!(
            settings::get(&pool, "api.last_sync_at").unwrap().is_some(),
            "non-drive settings must survive"
        );
    }

    /// Drive API のモック（アップロードを files に反映して md5 比較できるようにする）。
    struct MockDrive {
        files: Vec<crate::drive::DriveFile>,
        uploaded: std::cell::RefCell<Vec<(String, Vec<u8>)>>,
        deleted: std::cell::RefCell<Vec<String>>,
        touched: std::cell::RefCell<Vec<String>>,
    }

    impl crate::drive::DriveApi for MockDrive {
        fn list_files(
            &mut self,
            _folder_id: &str,
        ) -> Result<Vec<crate::drive::DriveFile>, crate::drive::DriveError> {
            Ok(self.files.clone())
        }
        fn download(&mut self, _file_id: &str) -> Result<Vec<u8>, crate::drive::DriveError> {
            Ok(Vec::new())
        }
        fn upload_multipart(
            &mut self,
            name: &str,
            _folder_id: &str,
            bytes: &[u8],
        ) -> Result<String, crate::drive::DriveError> {
            self.uploaded
                .borrow_mut()
                .push((name.to_string(), bytes.to_vec()));
            self.files.push(crate::drive::DriveFile {
                id: format!("id-{name}"),
                name: name.to_string(),
                size: Some(bytes.len() as i64),
                md5_checksum: Some(format!("{:x}", md5::compute(bytes))),
                modified_time: None,
            });
            Ok(format!("id-{name}"))
        }
        fn create_folder(&mut self, _name: &str) -> Result<String, crate::drive::DriveError> {
            Ok("folder".into())
        }
        fn delete(&mut self, file_id: &str) -> Result<(), crate::drive::DriveError> {
            self.deleted.borrow_mut().push(file_id.to_string());
            Ok(())
        }
        fn touch(&mut self, file_id: &str) -> Result<(), crate::drive::DriveError> {
            self.touched.borrow_mut().push(file_id.to_string());
            Ok(())
        }
    }

    #[test]
    fn sync_backs_up_database_json_when_changed() {
        let pool = crate::db::test_pool();
        migrate(&pool).unwrap();
        // データを入れるとエクスポート内容が変わる
        crate::db::books::insert(
            &pool,
            &crate::db::books::Book {
                id: "book-1".into(),
                title: "本".into(),
                author: String::new(),
                circle_name: "サークル".into(),
                purchase_date: None,
                file_name: "b.pdf".into(),
                file_size: 1,
                opfs_path: "b.opfspack".into(),
                cover_thumbnail: None,
                tbf_product_id: None,
                site_id: None,
                tags_fetched: 0,
                pack_id: Some("book-1".into()),
                is_favorite: 0,
                is_hidden: 0,
                created_at: "2026-08-23 00:00:00".into(),
                updated_at: "2026-08-23 00:00:00".into(),
            },
        )
        .unwrap();
        let packs = std::env::temp_dir().join("thundoku-sync-test-packs");
        std::fs::create_dir_all(&packs).unwrap();
        let dl = std::env::temp_dir().join("thundoku-sync-test-dl");
        std::fs::create_dir_all(&dl).unwrap();
        let db_path = std::env::temp_dir().join("thundoku-sync-test.db");
        let mut drive = MockDrive {
            files: Vec::new(),
            uploaded: Default::default(),
            deleted: Default::default(),
            touched: Default::default(),
        };

        // 初回: JSON バックアップがアップロードされる
        let outcome = super::sync(
            &pool,
            &mut drive,
            &packs,
            &dl,
            None,
            "folder",
            Some(&db_path),
        )
        .unwrap();
        assert!(
            outcome.database_backed_up,
            "first sync must upload the backup"
        );
        assert_eq!(drive.uploaded.borrow().len(), 1);
        assert_eq!(drive.uploaded.borrow()[0].0, "thundoku-backup.json");

        // 2 回目（同じデータ）: md5 一致でスキップ
        let outcome = super::sync(
            &pool,
            &mut drive,
            &packs,
            &dl,
            None,
            "folder",
            Some(&db_path),
        )
        .unwrap();
        assert!(
            !outcome.database_backed_up,
            "unchanged backup must be skipped"
        );
        assert!(
            !drive.touched.borrow().is_empty(),
            "unchanged backup must still touch modified time"
        );

        // データが変わったら再アップロード
        crate::db::books::insert(
            &pool,
            &crate::db::books::Book {
                id: "book-2".into(),
                title: "本2".into(),
                author: String::new(),
                circle_name: "サークル".into(),
                purchase_date: None,
                file_name: "b2.pdf".into(),
                file_size: 1,
                opfs_path: "b2.opfspack".into(),
                cover_thumbnail: None,
                tbf_product_id: None,
                site_id: None,
                tags_fetched: 0,
                pack_id: Some("book-2".into()),
                is_favorite: 0,
                is_hidden: 0,
                created_at: "2026-08-23 00:00:00".into(),
                updated_at: "2026-08-23 00:00:00".into(),
            },
        )
        .unwrap();
        let outcome = super::sync(
            &pool,
            &mut drive,
            &packs,
            &dl,
            None,
            "folder",
            Some(&db_path),
        )
        .unwrap();
        assert!(
            outcome.database_backed_up,
            "changed data must be re-uploaded"
        );
        assert!(
            !drive.deleted.borrow().is_empty(),
            "old file must be deleted"
        );
    }
}
