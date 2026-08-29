//! `app_settings` key/value store.

use std::collections::BTreeMap;

use crate::db::SqlitePool;

pub fn get(pool: &SqlitePool, key: &str) -> Result<Option<String>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, (String,)>("SELECT value FROM app_settings WHERE key = ?1")
            .bind(key)
            .fetch_optional(pool)
            .await
            .map(|row| row.map(|(value,)| value))
    })
}

pub fn set(pool: &SqlitePool, key: &str, value: &str) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "INSERT INTO app_settings (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value,
               updated_at = CURRENT_TIMESTAMP",
        )
        .bind(key)
        .bind(value)
        .execute(pool)
        .await?;
        Ok(())
    })
}

pub fn delete(pool: &SqlitePool, key: &str) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query("DELETE FROM app_settings WHERE key = ?1")
            .bind(key)
            .execute(pool)
            .await?;
        Ok(())
    })
}

pub fn all(pool: &SqlitePool) -> Result<BTreeMap<String, String>, sqlx::Error> {
    crate::db::block_on(async {
        let rows = sqlx::query_as::<_, (String, String)>("SELECT key, value FROM app_settings")
            .fetch_all(pool)
            .await?;
        let mut map = BTreeMap::new();
        for (k, v) in rows {
            map.insert(k, v);
        }
        Ok(map)
    })
}
