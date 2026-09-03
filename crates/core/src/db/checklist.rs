//! `tbf_events` / `checked_items` repository (チェックリスト).

use crate::db::SqlitePool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct TbfEvent {
    pub id: String,
    pub site_id: String,
    pub slug: Option<String>,
    pub tbf_event_id: Option<String>,
    pub event_name: String,
    pub event_date: Option<String>,
    pub event_start_date: Option<String>,
    pub event_end_date: Option<String>,
    pub event_format: String,
    pub is_cancelled: i64,
    pub display_order: i64,
    pub is_featured: i64,
    /// 「このイベントについて、技術書典手から最新状況を同期する」トグル。
    /// 1 = ポーリング対象（サーバー同期で上書きしない）。
    pub poll_sync_enabled: i64,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct CheckedItem {
    pub id: String,
    pub event_id: String,
    pub circle_name: String,
    pub space_number: String,
    pub memo: String,
    pub is_checked: i64,
    pub sort_order: i64,
    pub tbf_circle_id: Option<String>,
    pub product_id: Option<String>,
    pub product_title: String,
    pub thumbnail_url: Option<String>,
    pub thumbnail_data: Option<String>,
    pub price: Option<i64>,
    pub is_purchased: i64,
    pub sample_fetch_attempted_at: Option<String>,
    #[sqlx(rename = "createdAt")]
    pub created_at: String,
}

pub fn upsert_event(pool: &SqlitePool, event: &TbfEvent) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "INSERT INTO tbf_events (id, site_id, slug, tbf_event_id, event_name, event_date, \
             event_start_date, event_end_date, event_format, is_cancelled, display_order, \
             is_featured, poll_sync_enabled, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
             ON CONFLICT(id) DO UPDATE SET
               site_id = excluded.site_id,
               slug = excluded.slug,
               tbf_event_id = excluded.tbf_event_id,
               event_name = excluded.event_name,
               event_date = excluded.event_date,
               event_start_date = excluded.event_start_date,
               event_end_date = excluded.event_end_date,
               event_format = excluded.event_format,
               is_cancelled = excluded.is_cancelled,
               display_order = excluded.display_order,
               is_featured = excluded.is_featured,
               poll_sync_enabled = tbf_events.poll_sync_enabled,
               updated_at = excluded.updated_at",
        )
        .bind(&event.id)
        .bind(&event.site_id)
        .bind(&event.slug)
        .bind(&event.tbf_event_id)
        .bind(&event.event_name)
        .bind(&event.event_date)
        .bind(&event.event_start_date)
        .bind(&event.event_end_date)
        .bind(&event.event_format)
        .bind(event.is_cancelled)
        .bind(event.display_order)
        .bind(event.is_featured)
        .bind(event.poll_sync_enabled)
        .bind(&event.created_at)
        .bind(&event.updated_at)
        .execute(pool)
        .await?;
        Ok(())
    })
}

pub fn list_events(pool: &SqlitePool) -> Result<Vec<TbfEvent>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, TbfEvent>(
            "SELECT id, site_id, slug, tbf_event_id, event_name, event_date, event_start_date, \
             event_end_date, event_format, is_cancelled, display_order, is_featured, \
             poll_sync_enabled, created_at, updated_at FROM tbf_events \
             ORDER BY display_order, created_at DESC",
        )
        .fetch_all(pool)
        .await
    })
}

/// 「このイベントについて、技術書典手から最新状況を同期する」トグルを保存する。
pub fn set_poll_enabled(
    pool: &SqlitePool,
    slug: &str,
    enabled: bool,
) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query("UPDATE tbf_events SET poll_sync_enabled = ?1 WHERE slug = ?2")
            .bind(enabled as i64)
            .bind(slug)
            .execute(pool)
            .await?;
        Ok(())
    })
}

/// ポーリング対象（poll_sync_enabled = 1）のイベント slug 一覧を返す。
pub fn list_enabled_slugs(pool: &SqlitePool) -> Result<Vec<String>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_scalar::<_, String>(
            "SELECT slug FROM tbf_events WHERE poll_sync_enabled = 1 AND slug IS NOT NULL \
             ORDER BY display_order",
        )
        .fetch_all(pool)
        .await
    })
}

pub fn upsert_item(pool: &SqlitePool, item: &CheckedItem) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "INSERT INTO checked_items (id, event_id, circle_name, space_number, memo, is_checked, \
             sort_order, tbf_circle_id, product_id, product_title, thumbnail_url, thumbnail_data, \
             price, is_purchased, sample_fetch_attempted_at, createdAt)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
             ON CONFLICT(id) DO UPDATE SET
               event_id = excluded.event_id,
               circle_name = excluded.circle_name,
               space_number = excluded.space_number,
               memo = excluded.memo,
               is_checked = checked_items.is_checked,
               sort_order = excluded.sort_order,
               tbf_circle_id = excluded.tbf_circle_id,
               product_id = excluded.product_id,
               product_title = excluded.product_title,
               thumbnail_url = excluded.thumbnail_url,
               thumbnail_data = excluded.thumbnail_data,
               price = excluded.price,
               is_purchased = excluded.is_purchased,
               sample_fetch_attempted_at = excluded.sample_fetch_attempted_at",
        )
        .bind(&item.id)
        .bind(&item.event_id)
        .bind(&item.circle_name)
        .bind(&item.space_number)
        .bind(&item.memo)
        .bind(item.is_checked)
        .bind(item.sort_order)
        .bind(&item.tbf_circle_id)
        .bind(&item.product_id)
        .bind(&item.product_title)
        .bind(&item.thumbnail_url)
        .bind(&item.thumbnail_data)
        .bind(item.price)
        .bind(item.is_purchased)
        .bind(&item.sample_fetch_attempted_at)
        .bind(&item.created_at)
        .execute(pool)
        .await?;
        Ok(())
    })
}

pub fn list_items(pool: &SqlitePool, event_id: &str) -> Result<Vec<CheckedItem>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, CheckedItem>(
            "SELECT id, event_id, circle_name, space_number, memo, is_checked, sort_order, \
             tbf_circle_id, product_id, product_title, thumbnail_url, thumbnail_data, price, \
             is_purchased, sample_fetch_attempted_at, createdAt FROM checked_items \
             WHERE event_id = ?1 ORDER BY sort_order, circle_name",
        )
        .bind(event_id)
        .fetch_all(pool)
        .await
    })
}

pub fn set_checked(pool: &SqlitePool, item_id: &str, checked: bool) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query("UPDATE checked_items SET is_checked = ?1 WHERE id = ?2")
            .bind(checked as i64)
            .bind(item_id)
            .execute(pool)
            .await?;
        Ok(())
    })
}

/// チェックリスト項目のサムネイル（base64 データ）を保存する。
pub fn update_thumbnail_data(
    pool: &SqlitePool,
    item_id: &str,
    thumbnail_data: &str,
) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query("UPDATE checked_items SET thumbnail_data = ?1 WHERE id = ?2")
            .bind(thumbnail_data)
            .bind(item_id)
            .execute(pool)
            .await?;
        Ok(())
    })
}

/// チェックリスト項目を削除する。
pub fn delete_item(pool: &SqlitePool, item_id: &str) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query("DELETE FROM checked_items WHERE id = ?1")
            .bind(item_id)
            .execute(pool)
            .await?;
        Ok(())
    })
}

pub fn get_item(pool: &SqlitePool, item_id: &str) -> Result<Option<CheckedItem>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, CheckedItem>(
            "SELECT id, event_id, circle_name, space_number, memo, is_checked, sort_order, \
             tbf_circle_id, product_id, product_title, thumbnail_url, thumbnail_data, price, \
             is_purchased, sample_fetch_attempted_at, createdAt FROM checked_items WHERE id = ?1",
        )
        .bind(item_id)
        .fetch_optional(pool)
        .await
    })
}

#[cfg(test)]
mod tests {
    use crate::tbf::SITE_ID_TECHBOOKFEST;
    use super::*;

    fn event(slug: &str) -> TbfEvent {
        let ts = "2026-01-01 00:00:00".to_string();
        TbfEvent {
            id: slug.to_string(),
            site_id: SITE_ID_TECHBOOKFEST.into(),
            slug: Some(slug.to_string()),
            tbf_event_id: Some(format!("Event:{slug}")),
            event_name: format!("イベント{slug}"),
            event_date: None,
            event_start_date: None,
            event_end_date: None,
            event_format: "offline".to_string(),
            is_cancelled: 0,
            display_order: 0,
            is_featured: 0,
            created_at: ts.clone(),
            updated_at: ts,
            poll_sync_enabled: 0,
        }
    }

    #[test]
    fn set_poll_enabled_saves() {
        let pool = crate::db::test_pool();
        upsert_event(&pool, &event("tbf30")).unwrap();
        set_poll_enabled(&pool, "tbf30", true).unwrap();
        let events = list_events(&pool).unwrap();
        assert_eq!(
            events
                .iter()
                .find(|e| e.slug.as_deref() == Some("tbf30"))
                .unwrap()
                .poll_sync_enabled,
            1
        );
        set_poll_enabled(&pool, "tbf30", false).unwrap();
        let events = list_events(&pool).unwrap();
        assert_eq!(
            events
                .iter()
                .find(|e| e.slug.as_deref() == Some("tbf30"))
                .unwrap()
                .poll_sync_enabled,
            0
        );
    }

    #[test]
    fn list_enabled_slugs_returns_only_enabled() {
        let pool = crate::db::test_pool();
        upsert_event(&pool, &event("tbf30")).unwrap();
        upsert_event(&pool, &event("tbf29")).unwrap();
        set_poll_enabled(&pool, "tbf30", true).unwrap();
        assert_eq!(list_enabled_slugs(&pool).unwrap(), vec!["tbf30".to_string()]);
    }

    #[test]
    fn upsert_event_preserves_poll_enabled() {
        let pool = crate::db::test_pool();
        upsert_event(&pool, &event("tbf30")).unwrap();
        set_poll_enabled(&pool, "tbf30", true).unwrap();
        // 再 upsert（poll_sync_enabled = 0 で来る）してもトグルは保持される
        let fresh = TbfEvent {
            poll_sync_enabled: 0,
            ..event("tbf30")
        };
        upsert_event(&pool, &fresh).unwrap();
        let events = list_events(&pool).unwrap();
        assert_eq!(
            events
                .iter()
                .find(|e| e.slug.as_deref() == Some("tbf30"))
                .unwrap()
                .poll_sync_enabled,
            1
        );
    }

    #[test]
    fn migrate_creates_poll_sync_enabled_column_default_zero() {
        let pool = crate::db::test_pool();
        // migrate() 適用後の tbf_events に、default 0 の poll_sync_enabled 列が存在する
        let dflt: Option<String> = crate::db::block_on(async {
            sqlx::query_scalar::<_, String>(
                "SELECT dflt_value FROM pragma_table_info('tbf_events') WHERE name = 'poll_sync_enabled'",
            )
            .fetch_optional(&pool)
            .await
        })
        .unwrap();
        assert_eq!(dflt.as_deref(), Some("0"));
    }
}
