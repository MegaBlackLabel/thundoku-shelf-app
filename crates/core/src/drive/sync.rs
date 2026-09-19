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

/// 同期 1 回分の入力。
pub struct SyncRequest<'a> {
    /// ローカル DB プール
    pub pool: &'a SqlitePool,
    /// Drive 接続
    pub drive: &'a mut dyn DriveApi,
    /// ローカルの pack 置き場（ダウンロード先／アップロード元）
    pub packs_dir: &'a Path,
    /// 衝突時のローカル退避などに使う作業ディレクトリ
    pub downloads_dir: &'a Path,
    /// ログイン中アカウントの sub（未ログインは None＝アップロード対象なし）
    pub identity_sub: Option<&'a str>,
    /// 所有者列（`owner_sub`）の復号鍵（未ログインは None）
    pub owner_key: Option<&'a [u8; 32]>,
    /// Drive 側の同期フォルダ id
    pub folder_id: &'a str,
    /// DB バックアップのローカルパス（None なら DB バックアップ／復元をしない）
    pub db_path: Option<&'a Path>,
}

/// Run one full sync pass.
pub fn sync(request: SyncRequest<'_>) -> Result<SyncOutcome, SyncError> {
    let SyncRequest {
        pool,
        drive,
        packs_dir,
        downloads_dir,
        identity_sub,
        owner_key,
        folder_id,
        db_path,
    } = request;
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

        // sync_state 行が無い場合（新規ダウンロード直後・state クリア後・DB 復元後）は
        // local_changed が false になるため、条件にするとローカル専有の内容を退避せずに
        // 上書きしてしまう。ローカルに実体がある限り必ず退避する。
        if local_path.exists() {
            let backup = packs_dir.join(format!("{pack_id}.conflict-local.{PACK_EXTENSION}"));
            std::fs::copy(&local_path, &backup)?;
            outcome.conflicts.push((*pack_id).to_string());
        }
        std::fs::create_dir_all(downloads_dir)?;
        std::fs::create_dir_all(packs_dir)?;
        let temp = downloads_dir.join(format!("{pack_id}.{PACK_EXTENSION}"));
        std::fs::write(&temp, &bytes)?;
        // 直接 write だと書き込み途中のクラッシュで pack が壊れるため、
        // 同一ファイルシステム内の rename で置換する（原子的）。
        std::fs::rename(&temp, &local_path)?;
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
    // 所有者フィルタ（identity_sub / owner_key）を構成できないときは DB バックアップを
    // 上げない。空の所有集合でエクスポートすると、Drive 上の既存バックアップを
    // 「本を含まない内容」で置き換えてしまい、復元手段を失うため。
    let can_backup_db = identity_sub.is_some() && owner_key.is_some();
    if db_path.is_some() && !can_backup_db {
        // 無言でスキップすると「アップロードしたつもり」のまま終了してしまう
        // （次回起動で毎回復元確認が出る原因になる）。
        log::warn!(
            "drive sync: 所有者（Google ログイン / 暗号鍵）が不明なため DB バックアップをスキップ"
        );
    }
    if db_path.is_some() && can_backup_db {
        let json = crate::db::backup::export_json(pool, Some(&upload_ids))?;
        let bytes = json.as_bytes();
        let local_md5 = format!("{:x}", md5::compute(bytes));
        let existing = files.iter().find(|f| f.name == DB_BACKUP_NAME);
        let needs_upload = existing
            .map(|f| f.md5_checksum.as_deref() != Some(&local_md5))
            .unwrap_or(true);
        if needs_upload {
            log::info!(
                "drive sync: uploading database backup ({} bytes)",
                bytes.len()
            );
            // 先に新しいバックアップを上げてから旧ファイルを消す。
            // 削除→アップロードの順だと、途中で失敗したときに Drive 上の
            // バックアップが消えたままになる（唯一のオフサイト退避を失う）。
            drive.upload_multipart(DB_BACKUP_NAME, folder_id, bytes)?;
            if let Some(file) = existing
                && let Err(e) = drive.delete(&file.id)
            {
                // 旧ファイルが残っても新バックアップは存在するので致命的ではない。
                log::warn!("drive sync: failed to delete previous backup: {e}");
            }
            outcome.database_backed_up = true;
        } else {
            // データが変わらなくても、バックアップの更新日時が古いままに
            // ならないよう modifiedTime だけ現在時刻に更新する
            log::info!("drive sync: database unchanged, touching modified time");
            if let Some(file) = existing {
                drive.touch(&file.id)?;
            }
        }
        // アップロードした場合も md5 一致でスキップした場合も、Drive 上の内容は
        // このエクスポートと一致している。起動時の判定に使う基準値をここで更新する
        // （更新しないと、次回起動で「Drive 側が動いた」と誤判定して復元確認が出る）。
        let baseline = crate::db::backup::canonical_md5_str(&json, None)?;
        if let Err(error) = crate::db::settings::set(pool, BACKUP_BASELINE_KEY, &baseline) {
            log::warn!("drive sync: failed to store the backup baseline: {error}");
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
    // 復元直後はローカルが Drive の内容を含んでいる。ここで基準値を更新しないと、
    // 次回起動でも「Drive が動いた」と見えて同じバックアップを復元し続けてしまう
    // （ローカルに Drive に無い行が残っている限り差分は消えないため）。
    let baseline = crate::db::backup::canonical_md5_str(&json, None)?;
    crate::db::settings::set(pool, BACKUP_BASELINE_KEY, &baseline)?;
    Ok(())
}

/// 最後にアップロードした DB バックアップの内容（正規形 md5）を保存する設定キー。
/// 起動時の判定で「Drive 側が動いたのか、ローカル側だけが動いたのか」を区別するために使う。
pub const BACKUP_BASELINE_KEY: &str = "drive.backup.md5";

/// Drive の DB バックアップとローカルの比較結果（起動時の復元確認の判定材料）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupStatus {
    pub info: DriveBackupInfo,
    /// Drive のバックアップが、最後にこの端末がアップロードした内容から変わっているか。
    /// 基準値が無い（まだ一度もバックアップしていない）ときは判定できないので false。
    pub drive_changed: bool,
    /// Drive のバックアップとローカルの内容が違うか。
    pub local_differs: bool,
}

impl BackupStatus {
    /// 復元確認を出すべきか。Drive 側が新しくなっていて、かつ内容が違うときだけ出す。
    ///
    /// 単に「ローカルと違う」だけで出すと、ローカル側だけが進んだ場合（他端末を
    /// 起動していない場合）にも「復元しますか」と聞き、新しいローカルを古い
    /// バックアップで上書きしてしまう。
    pub fn should_offer_restore(&self) -> bool {
        self.drive_changed && self.local_differs
    }
}

/// Drive の `thundoku-backup.json` とローカルを比較し、復元確認の判定材料を返す。
/// バックアップが無ければ `Ok(None)`。
///
/// 比較はどちらも [`crate::db::backup::canonicalize_json`] の正規形で行う:
/// - アプリ自身が同期のたびに書き換える揮発列を落とす
/// - 行を PK 順に並べる（Drive 側のファイルは過去の書き出し順のままでも比較できる）
/// - ローカルは Drive 側に存在するテーブルだけを比較する（`view_history` など
///   新しく追加されたテーブルが Drive 側にまだ無い場合に毎回復元確認が出るのを防ぐ）
///
/// `baseline_md5` は最後にアップロードした内容の正規形 md5（[`BACKUP_BASELINE_KEY`]）。
/// Drive 側の正規形 md5 がこれと違うときだけ「Drive が動いた」と判定する。
pub fn inspect_drive_backup(
    pool: &SqlitePool,
    drive: &mut dyn DriveApi,
    folder_id: &str,
    book_ids: Option<&std::collections::HashSet<String>>,
    baseline_md5: Option<&str>,
) -> Result<Option<BackupStatus>, SyncError> {
    let Some(info) = check_drive_backup(drive, folder_id)? else {
        return Ok(None);
    };
    let drive_bytes = drive.download(&info.file_id)?;
    let drive_json: serde_json::Value = serde_json::from_slice(&drive_bytes)
        .map_err(|e| SyncError::Db(sqlx::Error::Protocol(e.to_string())))?;
    let local_json: serde_json::Value =
        serde_json::from_str(&crate::db::backup::export_json(pool, book_ids)?)
            .map_err(|e| SyncError::Db(sqlx::Error::Protocol(e.to_string())))?;
    let table_names: Vec<String> = drive_json
        .as_object()
        .map(|object| object.keys().cloned().collect())
        .unwrap_or_default();
    let drive_md5 = crate::db::backup::canonical_md5(&drive_json, None);
    let local_md5 = crate::db::backup::canonical_md5(&local_json, Some(table_names.as_slice()));
    Ok(Some(BackupStatus {
        info,
        drive_changed: baseline_md5.is_some_and(|baseline| baseline != drive_md5),
        local_differs: local_md5 != drive_md5,
    }))
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
        BACKUP_BASELINE_KEY,
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
            super::BACKUP_BASELINE_KEY,
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
    fn inspect_drive_backup_reports_only_when_data_differs() {
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
            !inspect_backup(&same, &mut drive, None)
                .expect("backup exists")
                .local_differs,
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
            inspect_backup(&same, &mut empty, None).is_none(),
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
            },
        )
        .unwrap();
        assert!(
            inspect_backup(&same, &mut drive, None)
                .expect("backup exists")
                .local_differs,
            "changed data must report a diff"
        );
    }

    #[test]
    fn inspect_drive_backup_ignores_tables_not_in_drive() {
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
            !inspect_backup(&local, &mut drive, None)
                .expect("backup exists")
                .local_differs,
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
        let outcome = super::sync(super::SyncRequest {
            pool: &pool,
            drive: &mut drive,
            packs_dir: &packs,
            downloads_dir: &dl,
            identity_sub: Some("test-sub"),
            owner_key: Some(&key),
            folder_id: "folder",
            db_path: Some(&db_path),
        })
        .unwrap();
        assert!(
            outcome.database_backed_up,
            "first sync must upload the backup"
        );
        assert_eq!(drive.uploaded.borrow().len(), 1);
        assert_eq!(drive.uploaded.borrow()[0].0, "thundoku-backup.json");

        // 2 回目（同じデータ）: md5 一致でスキップ
        let outcome = super::sync(super::SyncRequest {
            pool: &pool,
            drive: &mut drive,
            packs_dir: &packs,
            downloads_dir: &dl,
            identity_sub: Some("test-sub"),
            owner_key: Some(&key),
            folder_id: "folder",
            db_path: Some(&db_path),
        })
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
        let outcome = super::sync(super::SyncRequest {
            pool: &pool,
            drive: &mut drive,
            packs_dir: &packs,
            downloads_dir: &dl,
            identity_sub: Some("test-sub"),
            owner_key: Some(&key),
            folder_id: "folder",
            db_path: Some(&db_path),
        })
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

    /// テスト用の最小の本。
    fn test_book(id: &str) -> crate::db::books::Book {
        crate::db::books::Book {
            id: id.into(),
            title: format!("本 {id}"),
            author: String::new(),
            circle_name: "サークル".into(),
            purchase_date: None,
            file_name: format!("{id}.pdf"),
            file_size: 1,
            opfs_path: format!("{id}.opfspack"),
            cover_thumbnail: None,
            tbf_product_id: None,
            site_id: None,
            tags_fetched: 0,
            pack_id: Some(id.into()),
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
        }
    }

    /// テスト用のイベント（`updated_at` だけ差し替える）。
    fn test_event(id: &str, updated_at: &str) -> crate::db::checklist::TbfEvent {
        crate::db::checklist::TbfEvent {
            id: id.into(),
            site_id: "techbookfest".into(),
            slug: Some(id.into()),
            tbf_event_id: Some(format!("Event:{id}")),
            event_name: "技術書典".into(),
            event_date: Some("2026-04-12".into()),
            event_start_date: Some("2026-04-12".into()),
            event_end_date: Some("2026-04-12".into()),
            event_format: "offline".into(),
            is_cancelled: 0,
            display_order: 0,
            is_featured: 1,
            poll_sync_enabled: 1,
            created_at: "2026-01-01 00:00:00".into(),
            updated_at: updated_at.into(),
        }
    }

    /// `thundoku-backup.json` を 1 つ置いた Drive。
    fn drive_with_backup(json: &str) -> MockDrive {
        let drive = MockDrive {
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
            .insert("backup-id".into(), json.as_bytes().to_vec());
        drive
    }

    /// 起動時チェック（所有者フィルタ無し）の判定。
    fn inspect_backup(
        pool: &crate::db::SqlitePool,
        drive: &mut MockDrive,
        baseline: Option<&str>,
    ) -> Option<super::BackupStatus> {
        super::inspect_drive_backup(pool, drive, "folder", None, baseline).unwrap()
    }

    fn progress(
        book_id: &str,
        current_page: i64,
        last_read_at: &str,
    ) -> crate::db::progress::ReadingProgress {
        crate::db::progress::ReadingProgress {
            book_id: book_id.into(),
            content_id: String::new(),
            current_page,
            total_pages: Some(100),
            finished_at: None,
            last_read_at: last_read_at.into(),
        }
    }

    /// 起動時の復元確認は「Drive のバックアップが最後のアップロードから動いた」ときだけ出す。
    /// ローカル側だけが進んだ場合（他の PC を起動していない）は出さない。
    #[test]
    fn inspect_drive_backup_offers_restore_only_when_the_drive_backup_moved() {
        let pool = crate::db::test_pool();
        migrate(&pool).unwrap();
        crate::db::books::insert(&pool, &test_book("book-1")).unwrap();
        let uploaded = crate::db::backup::export_json(&pool, None).unwrap();
        let baseline = crate::db::backup::canonical_md5_str(&uploaded, None).unwrap();
        let mut drive = drive_with_backup(&uploaded);

        // この端末だけで読書が進んだ（Drive のファイルは誰も触っていない）
        crate::db::progress::upsert(&pool, &progress("book-1", 10, "2026-09-19 00:00:00")).unwrap();

        let status = inspect_backup(&pool, &mut drive, Some(&baseline)).expect("backup exists");
        assert!(
            !status.drive_changed,
            "Drive は最後のアップロードから動いていない"
        );
        assert!(status.local_differs, "ローカルの変更自体は検出する");
        assert!(
            !status.should_offer_restore(),
            "ローカル先行で『復元しますか』を出してはいけない"
        );

        // 他端末が違う内容をアップロードした
        let other = crate::db::test_pool();
        migrate(&other).unwrap();
        crate::db::books::insert(&other, &test_book("book-1")).unwrap();
        crate::db::progress::upsert(&other, &progress("book-1", 42, "2026-09-19 01:00:00"))
            .unwrap();
        let moved = crate::db::backup::export_json(&other, None).unwrap();
        let mut drive = drive_with_backup(&moved);

        let status = inspect_backup(&pool, &mut drive, Some(&baseline)).expect("backup exists");
        assert!(status.drive_changed, "Drive が動いたことを検出する");
        assert!(
            status.should_offer_restore(),
            "Drive 側が新しいときは復元確認を出す"
        );
    }

    /// `tbf_events.updated_at` のような、アプリ自身が同期のたびに書き換える
    /// 時刻列（揮発列）は内容差分として扱わない。
    #[test]
    fn inspect_drive_backup_ignores_volatile_timestamps() {
        let pool = crate::db::test_pool();
        migrate(&pool).unwrap();
        crate::db::checklist::upsert_event(&pool, &test_event("tbf20", "2026-09-14 10:00:00"))
            .unwrap();
        let uploaded = crate::db::backup::export_json(&pool, None).unwrap();
        let baseline = crate::db::backup::canonical_md5_str(&uploaded, None).unwrap();
        let mut drive = drive_with_backup(&uploaded);

        // 起動直後のポーリング（save_events）相当: 内容は同じで updated_at だけ進む
        crate::db::checklist::upsert_event(&pool, &test_event("tbf20", "2026-09-19 00:00:00"))
            .unwrap();

        let status = inspect_backup(&pool, &mut drive, Some(&baseline)).expect("backup exists");
        assert!(
            !status.drive_changed,
            "時刻列だけの違いを『Drive が動いた』にしない"
        );
        assert!(!status.local_differs, "時刻列だけの違いを差分にしない");
        assert!(!status.should_offer_restore());
    }

    /// 行の並び順が違っても、内容が同じなら差分にしない。
    /// （Drive 側の JSON は過去の書き出し順のまま残っていることがある）
    #[test]
    fn inspect_drive_backup_ignores_row_order() {
        let pool = crate::db::test_pool();
        migrate(&pool).unwrap();
        for id in ["a", "b", "c"] {
            crate::db::checklist::upsert_event(&pool, &test_event(id, "2026-09-14 10:00:00"))
                .unwrap();
        }
        let uploaded = crate::db::backup::export_json(&pool, None).unwrap();
        let baseline = crate::db::backup::canonical_md5_str(&uploaded, None).unwrap();

        // Drive 側のファイルは行の並びが逆
        let mut value: serde_json::Value = serde_json::from_str(&uploaded).unwrap();
        for rows in value.as_object_mut().unwrap().values_mut() {
            if let Some(rows) = rows.as_array_mut() {
                rows.reverse();
            }
        }
        let reordered = serde_json::to_string(&value).unwrap();
        assert_ne!(uploaded, reordered, "前提: 生の JSON は行順で変わる");
        let mut drive = drive_with_backup(&reordered);

        let status = inspect_backup(&pool, &mut drive, Some(&baseline)).expect("backup exists");
        assert!(!status.local_differs, "行順だけの違いを差分にしない");
        assert!(
            !status.drive_changed,
            "行順だけの違いを『Drive が動いた』にしない"
        );
    }

    /// 基準値が無い（この機能でまだ一度もアップロードしていない）ときは
    /// 方向を判定できないので復元確認を出さない。
    #[test]
    fn inspect_drive_backup_without_a_baseline_does_not_offer_restore() {
        let pool = crate::db::test_pool();
        migrate(&pool).unwrap();
        crate::db::books::insert(&pool, &test_book("book-1")).unwrap();
        let uploaded = crate::db::backup::export_json(&pool, None).unwrap();
        let mut drive = drive_with_backup(&uploaded);
        // ローカルには Drive のバックアップに無い本がある
        crate::db::books::insert(&pool, &test_book("book-2")).unwrap();

        let status = inspect_backup(&pool, &mut drive, None).expect("backup exists");
        assert!(
            !status.drive_changed,
            "基準値が無いときは『動いた』と断定しない"
        );
        assert!(!status.should_offer_restore());
    }

    /// 同じ内容なら挿入順が違っても同じ JSON を書き出す
    /// （生の md5 比較で毎回再アップロードするのを防ぐ）。
    #[test]
    fn export_json_is_independent_of_row_insertion_order() {
        let a = crate::db::test_pool();
        migrate(&a).unwrap();
        let b = crate::db::test_pool();
        migrate(&b).unwrap();
        for id in ["c", "a", "b"] {
            crate::db::checklist::upsert_event(&a, &test_event(id, "2026-09-14 10:00:00")).unwrap();
        }
        for id in ["a", "b", "c"] {
            crate::db::checklist::upsert_event(&b, &test_event(id, "2026-09-14 10:00:00")).unwrap();
        }
        assert_eq!(
            crate::db::backup::export_json(&a, None).unwrap(),
            crate::db::backup::export_json(&b, None).unwrap(),
            "同じ内容の DB からは同じ JSON を書き出す"
        );
    }

    /// 復元したあとに同じバックアップを「復元しますか」と聞き続けない
    /// （基準値を復元した内容で更新する）。その後の Drive の更新は検出できる。
    #[test]
    fn restore_drive_backup_makes_the_next_check_quiet() {
        // ローカルには Drive のバックアップに無い本がある
        let pool = crate::db::test_pool();
        migrate(&pool).unwrap();
        crate::db::books::insert(&pool, &test_book("book-2")).unwrap();

        // Drive 側は book-1 だけを含むバックアップ
        let src = crate::db::test_pool();
        migrate(&src).unwrap();
        crate::db::books::insert(&src, &test_book("book-1")).unwrap();
        let backup = crate::db::backup::export_json(&src, None).unwrap();
        let mut drive = drive_with_backup(&backup);

        super::restore_drive_backup(&mut drive, "folder", &pool).unwrap();
        let baseline = settings::get(&pool, super::BACKUP_BASELINE_KEY).unwrap();

        let status = inspect_backup(&pool, &mut drive, baseline.as_deref()).expect("backup exists");
        assert!(
            status.local_differs,
            "ローカルに Drive に無い本が残っている"
        );
        assert!(
            !status.should_offer_restore(),
            "復元直後にまた復元を促してはいけない"
        );

        // 他端末が別の内容を上げたら検出できる（＝基準値が復元内容で更新されている）
        let other = crate::db::test_pool();
        migrate(&other).unwrap();
        crate::db::books::insert(&other, &test_book("book-1")).unwrap();
        crate::db::books::insert(&other, &test_book("book-3")).unwrap();
        let moved = crate::db::backup::export_json(&other, None).unwrap();
        let mut drive = drive_with_backup(&moved);

        let status = inspect_backup(&pool, &mut drive, baseline.as_deref()).expect("backup exists");
        assert!(
            status.should_offer_restore(),
            "Drive が動いたら復元確認を出す"
        );
    }
}
