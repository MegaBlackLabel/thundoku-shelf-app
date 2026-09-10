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
    /// Drive の `thundoku-backup.json` から DB へ復元したか
    pub database_restored: bool,
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
    pack_bytes: &[u8],
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
    )?;
    // pack から取り込み状態（ドキュメント・コンテンツ・ページ行）を再構築する。
    // DB だけ失った / 別端末での復元用。すでに取り込み済みなら何もしない。
    if let Err(error) =
        crate::import::rebuild_from_pack(pool, pack_id, pack_bytes, identity.as_ref())
    {
        log::warn!("drive restore: pack からの再構築に失敗 ({pack_id}): {error}");
    }
    Ok(())
}

/// Run one full sync pass.
pub fn sync(
    pool: &SqlitePool,
    drive: &mut dyn DriveApi,
    packs_dir: &Path,
    downloads_dir: &Path,
    identity_sub: Option<&str>,
    owner_key: Option<&[u8; 32]>,
    folder_id: &str,
    db_path: Option<&Path>,
) -> Result<SyncOutcome, SyncError> {
    log::info!("drive sync: list_files start");
    let files = drive.list_files(folder_id)?;
    log::info!("drive sync: list_files -> {} files", files.len());
    let mut outcome = SyncOutcome::default();
    // アップロード / バックアップ対象の所有者フィルタ（P2/P3）。
    // ログイン中は現在 sub の本のみ、未ログインは何も上げない（未所属はアップロードしない）。
    let upload_ids: std::collections::HashSet<String> = match identity_sub {
        Some(sub) => owner_key
            .map(|k| books::owned_book_ids(pool, k, Some(sub)))
            .transpose()?
            .unwrap_or_default(),
        None => std::collections::HashSet::new(),
    };

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
        import_book(
            pool,
            &reader,
            pack_id,
            identity_sub,
            bytes.len() as i64,
            &bytes,
        )?;
        // ダウンロードした pack は現在 sub の所有として記録する（フォルダ分離前提で帰属を信頼）。
        if let (Some(sub), Some(key)) = (identity_sub, owner_key) {
            books::set_owner_sub(pool, pack_id, Some(crate::owner::encrypt(key, sub)))?;
        }
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
        // 所有者フィルタ：現在 sub の本だけアップロード（未所属・他アカウントは上げない）。
        if !upload_ids.contains(&book.id) {
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
        let json = crate::db::backup::export_json(pool, Some(&upload_ids))?;
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

/// Drive 上の `thundoku-backup.json` の要約（復元確認ダイアログの表示に使う）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveBackupInfo {
    pub file_id: String,
    pub md5: Option<String>,
    pub size: Option<i64>,
    pub modified_time: Option<String>,
}

/// Drive の `thundoku-backup.json` が存在するか確認し、要約を返す。
/// バックアップが無ければ `Ok(None)`。起動時の復元確認で使う。
pub fn check_drive_backup(
    drive: &mut dyn DriveApi,
    folder_id: &str,
) -> Result<Option<DriveBackupInfo>, SyncError> {
    let files = drive.list_files(folder_id)?;
    Ok(files
        .into_iter()
        .find(|f| f.name == DB_BACKUP_NAME)
        .map(|f| DriveBackupInfo {
            file_id: f.id,
            md5: f.md5_checksum,
            size: f.size,
            modified_time: f.modified_time,
        }))
}

/// Drive の `thundoku-backup.json` をダウンロードして DB に反映する。
/// UPSERT でマージするため既存のローカル行は残り、Drive 側の値が優先される。
pub fn restore_drive_backup(
    drive: &mut dyn DriveApi,
    folder_id: &str,
    pool: &SqlitePool,
) -> Result<(), SyncError> {
    let info = check_drive_backup(drive, folder_id)?
        .ok_or_else(|| SyncError::Db(sqlx::Error::Protocol("no drive backup found".into())))?;
    let bytes = drive.download(&info.file_id)?;
    let json = String::from_utf8(bytes)
        .map_err(|e| SyncError::Db(sqlx::Error::Protocol(e.to_string())))?;
    log::info!(
        "drive sync: restoring database backup ({} bytes, md5={:?})",
        json.len(),
        info.md5
    );
    crate::db::backup::import_json(pool, &json)?;
    Ok(())
}

/// ローカル DB と Drive の DB バックアップに差分があるかを判定する。
/// バックアップが無ければ `Ok(false)`（復元対象なし）。
/// ローカルは `backup::export_json` の md5 で比較する（DB ファイル全体ではなく
/// バックアップと同じ生データで比較するため、毎回アップロードされるのを防ぐ）。
pub fn backup_has_diff(
    pool: &SqlitePool,
    drive: &mut dyn DriveApi,
    folder_id: &str,
    book_ids: Option<&std::collections::HashSet<String>>,
) -> Result<bool, SyncError> {
    let Some(info) = check_drive_backup(drive, folder_id)? else {
        return Ok(false);
    };
    // Drive のバックアップ JSON をダウンロードして、そこに含まれるテーブルだけを
    // 比較対象にする。`view_history` など新しく追加されたテーブルが Drive 側に
    // まだ無い場合、その差分で毎回復元確認が出るのを防ぐ。
    let drive_bytes = drive.download(&info.file_id)?;
    let drive_json: serde_json::Value = serde_json::from_slice(&drive_bytes)
        .map_err(|e| SyncError::Db(sqlx::Error::Protocol(e.to_string())))?;
    let local_json: serde_json::Value =
        serde_json::from_str(&crate::db::backup::export_json(pool, book_ids)?)
            .map_err(|e| SyncError::Db(sqlx::Error::Protocol(e.to_string())))?;
    // 両方を「Drive に存在するテーブルだけ」に絞って正規化し、決定的な順序で比較する。
    let drive_obj = drive_json.as_object();
    let table_names: Vec<&String> = drive_obj.map(|o| o.keys().collect()).unwrap_or_default();
    let norm = |value: &serde_json::Value| -> serde_json::Value {
        let obj = value.as_object().cloned().unwrap_or_default();
        let mut filtered = serde_json::Map::new();
        for name in &table_names {
            if let Some(v) = obj.get(*name) {
                filtered.insert((*name).clone(), v.clone());
            }
        }
        serde_json::Value::Object(filtered)
    };
    let drive_norm = norm(&drive_json);
    let local_norm = norm(&local_json);
    let drive_md5 = format!(
        "{:x}",
        md5::compute(serde_json::to_string(&drive_norm).unwrap().as_bytes())
    );
    let local_md5 = format!(
        "{:x}",
        md5::compute(serde_json::to_string(&local_norm).unwrap().as_bytes())
    );
    Ok(local_md5 != drive_md5)
}

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

    #[test]
    fn check_drive_backup_reports_present_or_absent() {
        let mut drive = MockDrive {
            files: vec![crate::drive::DriveFile {
                id: "backup-id".into(),
                name: "thundoku-backup.json".into(),
                size: Some(123),
                md5_checksum: Some("abc123".into()),
                modified_time: Some("2026-08-23T00:00:00Z".into()),
            }],
            uploaded: Default::default(),
            deleted: Default::default(),
            touched: Default::default(),
            downloads: Default::default(),
        };
        let info = super::check_drive_backup(&mut drive, "folder").unwrap();
        let info = info.expect("backup must be found");
        assert_eq!(info.file_id, "backup-id");
        assert_eq!(info.md5.as_deref(), Some("abc123"));

        // バックアップが無いフォルダでは None
        let empty = MockDrive {
            files: Vec::new(),
            uploaded: Default::default(),
            deleted: Default::default(),
            touched: Default::default(),
            downloads: Default::default(),
        };
        let mut empty = empty;
        assert!(
            super::check_drive_backup(&mut empty, "folder")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn restore_drive_backup_imports_db_json() {
        use crate::db::progress::ReadingProgress;

        // ソース DB でバックアップを生成
        let src = crate::db::test_pool();
        migrate(&src).unwrap();
        crate::db::books::insert(
            &src,
            &crate::db::books::Book {
                id: "book-1".into(),
                title: "復元される本".into(),
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
        crate::db::progress::upsert(
            &src,
            &ReadingProgress {
                book_id: "book-1".into(),
                content_id: String::new(),
                current_page: 42,
                total_pages: Some(200),
                finished_at: None,
                last_read_at: "2026-08-23 09:00:00".into(),
                scroll_position: 0.0,
            },
        )
        .unwrap();
        let json = crate::db::backup::export_json(&src, None).unwrap();

        let mut drive = MockDrive {
            files: vec![crate::drive::DriveFile {
                id: "backup-id".into(),
                name: "thundoku-backup.json".into(),
                size: Some(json.len() as i64),
                md5_checksum: Some(format!("{:x}", md5::compute(json.as_bytes()))),
                modified_time: None,
            }],
            uploaded: Default::default(),
            deleted: Default::default(),
            touched: Default::default(),
            downloads: Default::default(),
        };
        drive
            .downloads
            .borrow_mut()
            .insert("backup-id".into(), json.into_bytes());

        // 空の DB に復元する
        let dst = crate::db::test_pool();
        migrate(&dst).unwrap();
        super::restore_drive_backup(&mut drive, "folder", &dst).unwrap();

        let restored = crate::db::books::get(&dst, "book-1").unwrap().unwrap();
        assert_eq!(restored.title, "復元される本");
        let progress = crate::db::progress::get(&dst, "book-1").unwrap().unwrap();
        assert_eq!(progress.current_page, 42);
    }

    #[test]
    fn backup_has_diff_reports_only_when_data_differs() {
        use crate::db::progress::ReadingProgress;

        // ソース DB でバックアップを生成し、その md5 を Drive に置く
        let src = crate::db::test_pool();
        migrate(&src).unwrap();
        crate::db::books::insert(
            &src,
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
        crate::db::progress::upsert(
            &src,
            &ReadingProgress {
                book_id: "book-1".into(),
                content_id: String::new(),
                current_page: 42,
                total_pages: Some(200),
                finished_at: None,
                last_read_at: "2026-08-23 09:00:00".into(),
                scroll_position: 0.0,
            },
        )
        .unwrap();
        let json = crate::db::backup::export_json(&src, None).unwrap();
        let json_md5 = format!("{:x}", md5::compute(json.as_bytes()));

        // 同一データの DB: md5 一致 -> 差分なし
        let same = crate::db::test_pool();
        migrate(&same).unwrap();
        crate::db::books::insert(
            &same,
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
        crate::db::progress::upsert(
            &same,
            &ReadingProgress {
                book_id: "book-1".into(),
                content_id: String::new(),
                current_page: 42,
                total_pages: Some(200),
                finished_at: None,
                last_read_at: "2026-08-23 09:00:00".into(),
                scroll_position: 0.0,
            },
        )
        .unwrap();
        let mut drive = MockDrive {
            files: vec![crate::drive::DriveFile {
                id: "backup-id".into(),
                name: "thundoku-backup.json".into(),
                size: Some(json.len() as i64),
                md5_checksum: Some(json_md5.clone()),
                modified_time: None,
            }],
            uploaded: Default::default(),
            deleted: Default::default(),
            touched: Default::default(),
            downloads: Default::default(),
        };
        drive
            .downloads
            .borrow_mut()
            .insert("backup-id".into(), json.as_bytes().to_vec());
        assert!(
            !super::backup_has_diff(&same, &mut drive, "folder", None).unwrap(),
            "identical data must not report a diff"
        );

        // バックアップが無い -> 差分なし
        let empty = MockDrive {
            files: Vec::new(),
            uploaded: Default::default(),
            deleted: Default::default(),
            touched: Default::default(),
            downloads: Default::default(),
        };
        let mut empty = empty;
        assert!(
            !super::backup_has_diff(&same, &mut empty, "folder", None).unwrap(),
            "no backup must not report a diff"
        );

        // データを変えた DB -> 差分あり
        crate::db::progress::upsert(
            &same,
            &ReadingProgress {
                book_id: "book-1".into(),
                content_id: String::new(),
                current_page: 100,
                total_pages: Some(200),
                finished_at: None,
                last_read_at: "2026-08-23 10:00:00".into(),
                scroll_position: 0.0,
            },
        )
        .unwrap();
        assert!(
            super::backup_has_diff(&same, &mut drive, "folder", None).unwrap(),
            "changed data must report a diff"
        );
    }

    #[test]
    fn backup_has_diff_ignores_tables_not_in_drive() {
        use crate::db::progress::ReadingProgress;

        // ローカル DB に本・進捗・閲覧履歴を入れる
        let local = crate::db::test_pool();
        migrate(&local).unwrap();
        crate::db::books::insert(
            &local,
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
        crate::db::progress::upsert(
            &local,
            &ReadingProgress {
                book_id: "book-1".into(),
                content_id: String::new(),
                current_page: 42,
                total_pages: Some(200),
                finished_at: None,
                last_read_at: "2026-08-23 09:00:00".into(),
                scroll_position: 0.0,
            },
        )
        .unwrap();
        crate::db::view_history::start(&local, "book-1").unwrap();

        // Drive のバックアップは古い形式: view_history キーを含まない
        let local_json: serde_json::Value =
            serde_json::from_str(&crate::db::backup::export_json(&local, None).unwrap()).unwrap();
        let mut drive_obj = local_json.as_object().cloned().unwrap();
        drive_obj.remove("view_history");
        let drive_bytes = serde_json::to_vec(&serde_json::Value::Object(drive_obj)).unwrap();

        let mut drive = MockDrive {
            files: vec![crate::drive::DriveFile {
                id: "backup-id".into(),
                name: "thundoku-backup.json".into(),
                size: Some(drive_bytes.len() as i64),
                md5_checksum: None,
                modified_time: None,
            }],
            uploaded: Default::default(),
            deleted: Default::default(),
            touched: Default::default(),
            downloads: Default::default(),
        };
        drive
            .downloads
            .borrow_mut()
            .insert("backup-id".into(), drive_bytes);

        // view_history は Drive 側に無いため比較対象から外れ、データが一致していれば差分なし
        assert!(
            !super::backup_has_diff(&local, &mut drive, "folder", None).unwrap(),
            "tables missing in drive must be ignored when other data matches"
        );
    }

    /// Drive API のモック（アップロードを files に反映して md5 比較できるようにする）。
    struct MockDrive {
        files: Vec<crate::drive::DriveFile>,
        uploaded: std::cell::RefCell<Vec<(String, Vec<u8>)>>,
        deleted: std::cell::RefCell<Vec<String>>,
        touched: std::cell::RefCell<Vec<String>>,
        downloads: std::cell::RefCell<std::collections::HashMap<String, Vec<u8>>>,
    }

    impl crate::drive::DriveApi for MockDrive {
        fn list_files(
            &mut self,
            _folder_id: &str,
        ) -> Result<Vec<crate::drive::DriveFile>, crate::drive::DriveError> {
            Ok(self.files.clone())
        }
        fn download(&mut self, file_id: &str) -> Result<Vec<u8>, crate::drive::DriveError> {
            Ok(self
                .downloads
                .borrow()
                .get(file_id)
                .cloned()
                .unwrap_or_default())
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
        let key = [13u8; 32];
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
        // 所有者ベースのバックアップ（P3）で、test-sub の本だけを出す。
        crate::db::books::set_owner_sub(
            &pool,
            "book-1",
            Some(crate::owner::encrypt(&key, "test-sub")),
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
            downloads: Default::default(),
        };
        // 初回: JSON バックアップがアップロードされる
        let outcome = super::sync(
            &pool,
            &mut drive,
            &packs,
            &dl,
            Some("test-sub"),
            Some(&key),
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
            Some("test-sub"),
            Some(&key),
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
        // book-2 も test-sub の所有にして、バックアップ内容を変える（再アップロード判定）。
        crate::db::books::set_owner_sub(
            &pool,
            "book-2",
            Some(crate::owner::encrypt(&key, "test-sub")),
        )
        .unwrap();
        let outcome = super::sync(
            &pool,
            &mut drive,
            &packs,
            &dl,
            Some("test-sub"),
            Some(&key),
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
