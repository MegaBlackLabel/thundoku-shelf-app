//! Persistence of TBF sync results into the local DB (bookshelf_items,
//! tbf_events, checked_items). Mirrors the Web rows; timestamps are local
//! UTC strings in the same formats the Web uses.

use crate::db::{SqlitePool, bookshelf, checklist};
use crate::tbf::{SITE_ID_TECHBOOKFEST, TbfChecklistEntry, TbfEventInfo, TbfShelfItem};

fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// Ensure a `tbf_events` row exists for the given slug (used as the row id),
/// so `bookshelf_items.event_id` does not violate the FK. If the event is
/// already present, nothing changes.
fn ensure_event(
    pool: &SqlitePool,
    slug: &str,
    event_name: Option<&str>,
) -> Result<(), sqlx::Error> {
    let exists = crate::db::block_on(async {
        let exists = sqlx::query_scalar::<_, i64>("SELECT 1 FROM tbf_events WHERE id = ?1")
            .bind(slug)
            .fetch_optional(pool)
            .await?
            .is_some();
        Ok::<_, sqlx::Error>(exists)
    })?;
    if exists {
        return Ok(());
    }
    let timestamp = now();
    // 開催日は canonical イベントマスターから引く（Web のイベント同期相当。
    // 本棚の購入日と開催日のマッチに使う）
    let (event_start_date, event_end_date, event_format, is_featured) =
        crate::tbf::canonical_events()
            .into_iter()
            .find(|event| event.slug == slug)
            .map(|event| {
                (
                    event.event_start_date,
                    event.event_end_date,
                    event.event_format,
                    event.is_featured,
                )
            })
            .unwrap_or((None, None, "offline".to_string(), false));
    checklist::upsert_event(
        pool,
        &checklist::TbfEvent {
            id: slug.to_string(),
            site_id: SITE_ID_TECHBOOKFEST.into(),
            slug: Some(slug.to_string()),
            tbf_event_id: Some(format!("Event:{slug}")),
            event_name: event_name.unwrap_or(slug).to_string(),
            event_date: event_start_date.clone(),
            event_start_date,
            event_end_date,
            event_format,
            is_cancelled: 0,
            display_order: 0,
            is_featured: is_featured as i64,
            created_at: timestamp.clone(),
            updated_at: timestamp,
        },
    )
}

/// Upsert bookshelf items; returns the number of rows written.
pub fn save_bookshelf(pool: &SqlitePool, items: &[TbfShelfItem]) -> Result<usize, sqlx::Error> {
    let timestamp = now();
    for item in items {
        if let Some(slug) = item.event_slug.as_deref().filter(|slug| !slug.is_empty()) {
            ensure_event(pool, slug, item.event_name.as_deref())?;
        }
        let tags_json = item
            .tags
            .as_ref()
            .and_then(|tags| serde_json::to_string(tags).ok());
        bookshelf::upsert(
            pool,
            &bookshelf::BookshelfItem {
                site_id: SITE_ID_TECHBOOKFEST.into(),
                database_id: item.id.clone(),
                title: item.title.clone(),
                circle_name: item.circle_name.clone(),
                thumbnail_url: item.thumbnail_url.clone(),
                format: item.format.clone(),
                caused_at: item.caused_at.clone(),
                event_name: item.event_name.clone(),
                event_slug: item.event_slug.clone(),
                event_id: item
                    .event_slug
                    .as_deref()
                    .filter(|slug| !slug.is_empty())
                    .map(String::from),
                file_name: item.file_name.clone(),
                download_url: item.download_url.clone(),
                is_downloadable: item.is_downloadable as i64,
                is_checked: 1,
                is_purchased: 1,
                is_new: 0,
                is_active: 1,
                is_favorite: 0,
                is_hidden: 0,
                hidden_at: None,
                tags_json,
                synced_at: timestamp.clone(),
                created_at: timestamp.clone(),
                updated_at: timestamp.clone(),
            },
        )?;
    }
    Ok(items.len())
}

/// Upsert event masters; `id` = slug for stable upserts.
pub fn save_events(pool: &SqlitePool, events: &[TbfEventInfo]) -> Result<usize, sqlx::Error> {
    let timestamp = now();
    for event in events {
        checklist::upsert_event(
            pool,
            &checklist::TbfEvent {
                id: event.slug.clone(),
                site_id: SITE_ID_TECHBOOKFEST.into(),
                slug: Some(event.slug.clone()),
                tbf_event_id: Some(event.tbf_event_id.clone()),
                event_name: event.event_name.clone(),
                event_date: event.event_date.clone(),
                event_start_date: event.event_start_date.clone(),
                event_end_date: event.event_end_date.clone(),
                event_format: event.event_format.clone(),
                is_cancelled: event.is_cancelled as i64,
                display_order: event.display_order,
                is_featured: event.is_featured as i64,
                created_at: timestamp.clone(),
                updated_at: timestamp.clone(),
            },
        )?;
    }
    Ok(events.len())
}

/// Upsert checklist entries for one event. `is_checked` is preserved for
/// existing rows (local user state), everything else follows the server.
pub fn save_checklist(
    pool: &SqlitePool,
    event_slug: &str,
    entries: &[TbfChecklistEntry],
) -> Result<usize, sqlx::Error> {
    ensure_event(pool, event_slug, None)?;
    let timestamp = now();
    // このイベントの同期に含まれない既存行を削除する（イベントごとの
    // チェックリストを完全な状態にする。Web の markMissingRowsInactive 相当。
    // 試し読みページは FK で紐付くため一緒に消す）。
    for existing in crate::db::checklist::list_items(pool, event_slug)? {
        if !entries.iter().any(|entry| entry.id == existing.id) {
            let _ = crate::db::samples::delete_for_item(pool, &existing.id);
            let _ = crate::db::checklist::delete_item(pool, &existing.id);
        }
    }
    for (index, entry) in entries.iter().enumerate() {
        let memo = match &entry.product_id {
            Some(product_id) => format!("product:{product_id} {}", entry.product_title),
            None => String::new(),
        };
        checklist::upsert_item(
            pool,
            &checklist::CheckedItem {
                id: entry.id.clone(),
                event_id: event_slug.to_string(),
                circle_name: entry.circle_name.clone(),
                space_number: entry.space_number.clone(),
                memo,
                is_checked: 0,
                sort_order: index as i64,
                tbf_circle_id: entry.tbf_circle_id.clone(),
                product_id: entry.product_id.clone(),
                product_title: entry.product_title.clone(),
                thumbnail_url: entry.thumbnail_url.clone(),
                thumbnail_data: None,
                price: entry.price,
                is_purchased: entry.is_purchased as i64,
                sample_fetch_attempted_at: None,
                created_at: entry
                    .created_at
                    .clone()
                    .unwrap_or_else(|| timestamp.clone()),
            },
        )?;
    }
    Ok(entries.len())
}

#[cfg(test)]
mod tests {

    use crate::db::{checklist, migrate};

    use super::*;

    fn entry(id: &str, circle: &str) -> TbfChecklistEntry {
        TbfChecklistEntry {
            id: id.into(),
            circle_name: circle.into(),
            space_number: "あ-01".into(),
            tbf_circle_id: None,
            product_id: Some("p1".into()),
            product_title: "本".into(),
            thumbnail_url: None,
            price: Some(1000),
            is_purchased: true,
            created_at: None,
        }
    }

    #[test]
    fn save_checklist_removes_rows_not_in_sync() {
        let pool = crate::db::test_pool();
        migrate(&pool).unwrap();
        // 2 行保存
        save_checklist(
            &pool,
            "tbf20",
            &[entry("tbf20:p1", "A"), entry("tbf20:p2", "B")],
        )
        .unwrap();
        assert_eq!(checklist::list_items(&pool, "tbf20").unwrap().len(), 2);
        // p2 を含まない同期 → p2 が削除され、イベントのデータが完全になる
        save_checklist(&pool, "tbf20", &[entry("tbf20:p1", "A")]).unwrap();
        let items = checklist::list_items(&pool, "tbf20").unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id, "tbf20:p1");
    }

    #[test]
    fn save_checklist_keeps_other_events_rows() {
        let pool = crate::db::test_pool();
        migrate(&pool).unwrap();
        save_checklist(&pool, "tbf20", &[entry("tbf20:p1", "A")]).unwrap();
        save_checklist(&pool, "tbf19", &[entry("tbf19:p1", "B")]).unwrap();
        // tbf20 の再同期で tbf19 の行は消えない
        save_checklist(&pool, "tbf20", &[entry("tbf20:p1", "A")]).unwrap();
        assert_eq!(checklist::list_items(&pool, "tbf20").unwrap().len(), 1);
        assert_eq!(checklist::list_items(&pool, "tbf19").unwrap().len(), 1);
    }
}
