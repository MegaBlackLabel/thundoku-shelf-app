//! `bookshelf_items` repository (技術書典本棚).
//! Note: the verbatim Web schema uses camelCase `causedAt`.

use crate::db::SqlitePool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct BookshelfItem {
    pub site_id: String,
    pub database_id: String,
    pub title: String,
    pub circle_name: String,
    /// 作者名（技術書典は提供されないため空。BOOTH 等は作成者名）
    pub author: String,
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
    // 共有ソースメタ列（FANZA同人 / DLsite）
    pub media_category: Option<String>,
    pub ai_type: Option<String>,
    pub is_drm: i64,
    pub release_date: Option<String>,
    pub description: Option<String>,
    pub theme: Option<String>,
    pub maker_id: Option<String>,
    pub page_count: Option<i64>,
    pub age_rating: Option<String>,
    pub series_name: Option<String>,
}

const COLUMNS: &str = "site_id, database_id, title, circle_name, author, thumbnail_url, format, \
     causedAt, event_name, event_slug, event_id, file_name, download_url, is_downloadable, \
     is_checked, is_purchased, is_new, is_active, is_favorite, is_hidden, hidden_at, tags_json, \
     synced_at, created_at, updated_at, media_category, ai_type, is_drm, release_date, \
     description, theme, maker_id, page_count, age_rating, series_name";

/// Upsert by (site_id, database_id).
///
/// 同期（`save_purchases` 等）は作品ページ由来の値を持たないため、
/// `tags_json = None` / `author = ""` を送ってくる。これを素通しすると
/// ダウンロード時に取得したジャンル・作者が消えるので、値が無いときは
/// 既存値を保持する（`is_favorite` / `is_hidden` と同じ扱い）。
pub fn upsert(pool: &SqlitePool, item: &BookshelfItem) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(&format!(
            "INSERT INTO bookshelf_items ({COLUMNS}) VALUES \
             (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, \
             ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35)
             ON CONFLICT(site_id, database_id) DO UPDATE SET
               title = excluded.title,
               circle_name = excluded.circle_name,
               author = excluded.author,
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
               tags_json = COALESCE(excluded.tags_json, bookshelf_items.tags_json),
               author = CASE
                 WHEN excluded.author = '' THEN bookshelf_items.author
                 ELSE excluded.author
               END,
               synced_at = excluded.synced_at,
               updated_at = excluded.updated_at,
               media_category = excluded.media_category,
               ai_type = excluded.ai_type,
               is_drm = excluded.is_drm,
               release_date = excluded.release_date,
               description = excluded.description,
               theme = excluded.theme,
               maker_id = excluded.maker_id,
               page_count = excluded.page_count,
               age_rating = excluded.age_rating,
               series_name = excluded.series_name"
        ))
        .bind(&item.site_id)
        .bind(&item.database_id)
        .bind(&item.title)
        .bind(&item.circle_name)
        .bind(&item.author)
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
        .bind(&item.media_category)
        .bind(&item.ai_type)
        .bind(item.is_drm)
        .bind(&item.release_date)
        .bind(&item.description)
        .bind(&item.theme)
        .bind(&item.maker_id)
        .bind(item.page_count)
        .bind(&item.age_rating)
        .bind(&item.series_name)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// 未所属（`owner_sub IS NULL`）の行に所有者（暗号化済み sub）を与える。
///
/// 本棚の行はストアのセッションで同期したデータなので、帰属は「そのとき
/// ログインしていた Google アカウント」で決める。**既に所有者が付いている行は
/// 書き換えない**（A の購入一覧が B の同期で B のものに化けて、B の
/// バックアップに混ざるのを防ぐ）。戻り値は更新した行数。
pub fn attribute_owner(
    pool: &SqlitePool,
    site_id: &str,
    owner: Option<&str>,
) -> Result<u64, sqlx::Error> {
    let Some(owner) = owner else {
        // 未ログイン: 未所属のままにする。
        return Ok(0);
    };
    crate::db::block_on(async {
        let result = sqlx::query(
            "UPDATE bookshelf_items SET owner_sub = ?1 \
             WHERE site_id = ?2 AND owner_sub IS NULL",
        )
        .bind(owner)
        .bind(site_id)
        .execute(pool)
        .await?;
        Ok(result.rows_affected())
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
            "UPDATE bookshelf_items SET tags_json = ?1, tags_fetched = 1, \
             updated_at = CURRENT_TIMESTAMP \
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

/// タグ未取得の**未ダウンロード**作品の件数（通知に残り件数を出すため）。
pub fn pending_tag_fetch_count(pool: &SqlitePool, site_id: &str) -> Result<i64, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM bookshelf_items b \
             WHERE b.site_id = ?1 AND b.is_active = 1 AND b.tags_fetched = 0 \
               AND NOT EXISTS (SELECT 1 FROM books WHERE books.tbf_product_id = b.database_id)",
        )
        .bind(site_id)
        .fetch_one(pool)
        .await
    })
}

/// タグ未取得の**未ダウンロード**作品の `database_id` を最大 `limit` 件返す。
///
/// ダウンロード済みの本は取り込み時にタグを取っているため対象外
/// （取り込み時に失敗したぶんは手動の「ジャンル再取得」で拾う）。
/// タグが 0 件でも取得済みの印は立つので、同じ作品を毎回叩き続けることはない。
pub fn pending_tag_fetch(
    pool: &SqlitePool,
    site_id: &str,
    limit: usize,
) -> Result<Vec<String>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_scalar(
            "SELECT b.database_id FROM bookshelf_items b \
             WHERE b.site_id = ?1 AND b.is_active = 1 AND b.tags_fetched = 0 \
               AND NOT EXISTS (SELECT 1 FROM books WHERE books.tbf_product_id = b.database_id) \
             ORDER BY b.causedAt DESC, b.database_id \
             LIMIT ?2",
        )
        .bind(site_id)
        .bind(limit as i64)
        .fetch_all(pool)
        .await
    })
}

/// Set the author name of a bookshelf item（取得元 = サイトの作品ページ）。
pub fn update_author(
    pool: &SqlitePool,
    site_id: &str,
    database_id: &str,
    author: &str,
) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "UPDATE bookshelf_items SET author = ?1, updated_at = CURRENT_TIMESTAMP \
             WHERE site_id = ?2 AND database_id = ?3",
        )
        .bind(author)
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
