//! `drive_sync_state` repository — Google Drive sync bookkeeping.

use crate::db::SqlitePool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct DriveSyncState {
    pub pack_id: String,
    pub drive_file_id: String,
    pub md5: String,
    pub modified_time: Option<String>,
    pub last_synced_at: String,
}

pub fn get(pool: &SqlitePool, pack_id: &str) -> Result<Option<DriveSyncState>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, DriveSyncState>(
            "SELECT pack_id, drive_file_id, md5, modified_time, last_synced_at \
             FROM drive_sync_state WHERE pack_id = ?1",
        )
        .bind(pack_id)
        .fetch_optional(pool)
        .await
    })
}

pub fn upsert(pool: &SqlitePool, state: &DriveSyncState) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "INSERT INTO drive_sync_state (pack_id, drive_file_id, md5, modified_time, \
             last_synced_at) VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT(pack_id) DO UPDATE SET
               drive_file_id = excluded.drive_file_id,
               md5 = excluded.md5,
               modified_time = excluded.modified_time,
               last_synced_at = excluded.last_synced_at",
        )
        .bind(&state.pack_id)
        .bind(&state.drive_file_id)
        .bind(&state.md5)
        .bind(&state.modified_time)
        .bind(&state.last_synced_at)
        .execute(pool)
        .await?;
        Ok(())
    })
}

pub fn list(pool: &SqlitePool) -> Result<Vec<DriveSyncState>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, DriveSyncState>(
            "SELECT pack_id, drive_file_id, md5, modified_time, last_synced_at \
             FROM drive_sync_state ORDER BY pack_id",
        )
        .fetch_all(pool)
        .await
    })
}

pub fn delete(pool: &SqlitePool, pack_id: &str) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query("DELETE FROM drive_sync_state WHERE pack_id = ?1")
            .bind(pack_id)
            .execute(pool)
            .await?;
        Ok(())
    })
}
