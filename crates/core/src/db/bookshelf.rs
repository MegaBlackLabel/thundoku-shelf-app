//! `bookshelf_items` repository (技術書典本棚).
//! Note: the verbatim Web schema uses camelCase `causedAt`.

use crate::db::SqlitePool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct BookshelfItem {
    pub site_id: String,
    pub database_id: String,
    pub title: String,
    pub circle_name: String,
    pub thumbnail_url: Option<String>,
    pub format: String,
    #[sqlx(rename = "causedAt")]
    pub caused_at: Option<String>,
    pub event_name: Option<String>,
    pub event_slug: Option<String>,
    pub event_id: Option<String>,
    pub file_name: Option<String>,
    pub download_url: Option<String>,
    pub is_downloadable: i64,
    pub is_checked: i64,
    pub is_purchased: i64,
    pub is_new: i64,
    pub is_active: i64,
    pub is_favorite: i64,
    pub is_hidden: i64,
    /// 非表示にした日時（非表示ログイン時のみ）
    pub hidden_at: Option<String>,
    pub tags_json: Option<String>,
    pub synced_at: String,
    pub created_at: String,
    pub updated_at: String,
}

const COLUMNS: &str = "site_id, database_id, title, circle_name, thumbnail_url, format, \
     causedAt, event_name, event_slug, event_id, file_name, download_url, is_downloadable, \
     is_checked, is_purchased, is_new, is_active, is_favorite, is_hidden, hidden_at, tags_json, \
     synced_at, created_at, updated_at";

/// Upsert by (site_id, database_id).
pub fn upsert(pool: &SqlitePool, item: &BookshelfItem) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(&format!(
            "INSERT INTO bookshelf_items ({COLUMNS}) VALUES \
             (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, \
             ?19, ?20, ?21, ?22, ?23, ?24)
             ON CONFLICT(site_id, database_id) DO UPDATE SET
               title = excluded.title,
               circle_name = excluded.circle_name,
               thumbnail_url = excluded.thumbnail_url,
               format = excluded.format,
               causedAt = excluded.causedAt,
               event_name = excluded.event_name,
               event_slug = excluded.event_slug,
               event_id = excluded.event_id,
               file_name = excluded.file_name,
               download_url = excluded.download_url,
               is_downloadable = excluded.is_downloadable,
               is_checked = excluded.is_checked,
               is_purchased = excluded.is_purchased,
               is_new = excluded.is_new,
               is_active = excluded.is_active,
               is_hidden = bookshelf_items.is_hidden,
               hidden_at = bookshelf_items.hidden_at,
               tags_json = excluded.tags_json,
               synced_at = excluded.synced_at,
               updated_at = excluded.updated_at"
        ))
        .bind(&item.site_id)
        .bind(&item.database_id)
        .bind(&item.title)
        .bind(&item.circle_name)
        .bind(&item.thumbnail_url)
        .bind(&item.format)
        .bind(&item.caused_at)
        .bind(&item.event_name)
        .bind(&item.event_slug)
        .bind(&item.event_id)
        .bind(&item.file_name)
        .bind(&item.download_url)
        .bind(item.is_downloadable)
        .bind(item.is_checked)
        .bind(item.is_purchased)
        .bind(item.is_new)
        .bind(item.is_active)
        .bind(item.is_favorite)
        .bind(item.is_hidden)
        .bind(&item.hidden_at)
        .bind(&item.tags_json)
        .bind(&item.synced_at)
        .bind(&item.created_at)
        .bind(&item.updated_at)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// bookshelf_items の event_name を更新する（同期で空になったイベント名を
/// tbf_events から補完するときに使う）。
pub fn set_event_name(
    pool: &SqlitePool,
    site_id: &str,
    database_id: &str,
    event_name: &str,
) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "UPDATE bookshelf_items SET event_name = ?1, updated_at = CURRENT_TIMESTAMP          WHERE site_id = ?2 AND database_id = ?3",
        )
        .bind(event_name)
        .bind(site_id)
        .bind(database_id)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// 全サイトの本棚アイテムを取得する（BOOTH と技術書典をまとめて扱う）。
pub fn list_all(pool: &SqlitePool) -> Result<Vec<BookshelfItem>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, BookshelfItem>(&format!(
            "SELECT {COLUMNS} FROM bookshelf_items              ORDER BY causedAt IS NULL, causedAt DESC, title ASC"
        ))
        .fetch_all(pool)
        .await
    })
}

pub fn list(pool: &SqlitePool, site_id: &str) -> Result<Vec<BookshelfItem>, sqlx::Error> {
    // Web 版と同一: 購入日（causedAt）が新しい順、null は末尾、同列はタイトル順。
    crate::db::block_on(async {
        sqlx::query_as::<_, BookshelfItem>(&format!(
            "SELECT {COLUMNS} FROM bookshelf_items WHERE site_id = ?1 \
             ORDER BY causedAt IS NULL, causedAt DESC, title ASC"
        ))
        .bind(site_id)
        .fetch_all(pool)
        .await
    })
}

/// Replace the manual tags for a bookshelf item (`tags_json`), mirroring the
/// Web `updateBookTags` fallback for not-yet-downloaded books.
pub fn update_tags(
    pool: &SqlitePool,
    site_id: &str,
    database_id: &str,
    tags: &[String],
) -> Result<(), sqlx::Error> {
    let json = serde_json::to_string(tags).unwrap_or_else(|_| "[]".to_string());
    crate::db::block_on(async {
        sqlx::query(
            "UPDATE bookshelf_items SET tags_json = ?1, updated_at = CURRENT_TIMESTAMP \
             WHERE site_id = ?2 AND database_id = ?3",
        )
        .bind(json)
        .bind(site_id)
        .bind(database_id)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// Parse `tags_json` into a tag list (empty when absent/invalid).
pub fn tags_of(item: &BookshelfItem) -> Vec<String> {
    item.tags_json
        .as_deref()
        .and_then(|json| serde_json::from_str::<Vec<String>>(json).ok())
        .unwrap_or_default()
}

/// Set the favorite flag on a shelf item (not-yet-downloaded books included).
pub fn set_favorite(
    pool: &SqlitePool,
    site_id: &str,
    database_id: &str,
    favorite: bool,
) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "UPDATE bookshelf_items SET is_favorite = ?1, updated_at = CURRENT_TIMESTAMP \
             WHERE site_id = ?2 AND database_id = ?3",
        )
        .bind(favorite as i64)
        .bind(site_id)
        .bind(database_id)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// Set the hidden flag on a shelf item.
pub fn set_hidden(
    pool: &SqlitePool,
    site_id: &str,
    database_id: &str,
    hidden: bool,
) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        let hidden_at = if hidden {
            "CURRENT_TIMESTAMP".to_string()
        } else {
            "NULL".to_string()
        };
        sqlx::query(&format!(
            "UPDATE bookshelf_items SET is_hidden = ?1, hidden_at = {hidden_at}, \
             updated_at = CURRENT_TIMESTAMP \
             WHERE site_id = ?2 AND database_id = ?3"
        ))
        .bind(hidden as i64)
        .bind(site_id)
        .bind(database_id)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// 非表示にした本の一覧（非表示にした日時付き。日時がない場合は現在日時で埋める）。
pub fn list_hidden(pool: &SqlitePool) -> Result<Vec<BookshelfItem>, sqlx::Error> {
    crate::db::block_on(async {
        // hidden_at が無い（既存データ）場合は現在日時を登録する
        sqlx::query(
            "UPDATE bookshelf_items SET hidden_at = CURRENT_TIMESTAMP \
             WHERE is_hidden = 1 AND hidden_at IS NULL",
        )
        .execute(pool)
        .await?;
        sqlx::query_as::<_, BookshelfItem>(&format!(
            "SELECT {COLUMNS} FROM bookshelf_items WHERE is_hidden = 1 \
             ORDER BY hidden_at DESC, title ASC"
        ))
        .fetch_all(pool)
        .await
    })
}

/// Favorite shelf items (for the auto-download on startup).
pub fn list_favorites(pool: &SqlitePool) -> Result<Vec<BookshelfItem>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, BookshelfItem>(&format!(
            "SELECT {COLUMNS} FROM bookshelf_items WHERE is_favorite = 1 \
             ORDER BY causedAt IS NULL, causedAt DESC, title ASC"
        ))
        .fetch_all(pool)
        .await
    })
}
