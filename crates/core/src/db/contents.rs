//! `book_contents` / `content_formats` repository.
//!
//! - コンテンツ = **読む単位**（本文・別冊・オマケ等。`docs/import-patterns.md` §3.3）
//! - レンディション = 同じ内容の**別形式・別バリアント**（`PDF版` / `画像版`、
//!   `文字あり` / `文字なし`、`MP3/WAV × SEあり/なし`）
//!
//! `document_images.content_id` / `format_id` がここを参照する。表紙・裏表紙は
//! コンテンツにしない（決定 D4）。

use crate::db::SqlitePool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct BookContent {
    pub content_id: String,
    pub book_id: String,
    pub display_name: String,
    /// `image` / `pdf` / `epub` / `audio` / `video`
    pub media_kind: String,
    /// 既定で表示するコンテンツかどうか（0 / 1）。
    pub is_primary: i64,
    pub sort_order: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ContentFormat {
    pub format_id: String,
    pub content_id: String,
    /// 切替 UI に出す名前（`画像` / `PDF` など）。
    pub label: String,
    /// `image` / `pdf` / `epub` / `audio` / `video`
    pub format_kind: String,
    pub page_count: i64,
    /// pack 内でこの形式のページが置かれる接頭辞（例 `pages` / `contents/1/r0`）。
    pub pack_entry_prefix: Option<String>,
    pub sort_order: i64,
    pub created_at: String,
}

/// コンテンツとレンディションを一括 INSERT する。
pub fn insert_batch(
    pool: &SqlitePool,
    contents: &[BookContent],
    formats: &[ContentFormat],
) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        let mut tx = pool.begin().await?;
        for content in contents {
            sqlx::query(
                "INSERT INTO book_contents (content_id, book_id, display_name, media_kind, \
                 is_primary, sort_order, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .bind(&content.content_id)
            .bind(&content.book_id)
            .bind(&content.display_name)
            .bind(&content.media_kind)
            .bind(content.is_primary)
            .bind(content.sort_order)
            .bind(&content.created_at)
            .execute(&mut *tx)
            .await?;
        }
        for format in formats {
            sqlx::query(
                "INSERT INTO content_formats (format_id, content_id, label, format_kind, \
                 page_count, pack_entry_prefix, sort_order, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )
            .bind(&format.format_id)
            .bind(&format.content_id)
            .bind(&format.label)
            .bind(&format.format_kind)
            .bind(format.page_count)
            .bind(&format.pack_entry_prefix)
            .bind(format.sort_order)
            .bind(&format.created_at)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    })
}

/// 本に紐づくコンテンツを表示順で返す。
pub fn list_for_book(pool: &SqlitePool, book_id: &str) -> Result<Vec<BookContent>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, BookContent>(
            "SELECT content_id, book_id, display_name, media_kind, is_primary, sort_order, \
             created_at FROM book_contents WHERE book_id = ?1 ORDER BY sort_order, content_id",
        )
        .bind(book_id)
        .fetch_all(pool)
        .await
    })
}

/// 既定表示のコンテンツ（無ければ先頭）。
pub fn primary_for_book(
    pool: &SqlitePool,
    book_id: &str,
) -> Result<Option<BookContent>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, BookContent>(
            "SELECT content_id, book_id, display_name, media_kind, is_primary, sort_order, \
             created_at FROM book_contents WHERE book_id = ?1 \
             ORDER BY is_primary DESC, sort_order, content_id LIMIT 1",
        )
        .bind(book_id)
        .fetch_optional(pool)
        .await
    })
}

/// コンテンツのレンディション一覧（表示順）。
pub fn formats_for_content(
    pool: &SqlitePool,
    content_id: &str,
) -> Result<Vec<ContentFormat>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, ContentFormat>(
            "SELECT format_id, content_id, label, format_kind, page_count, pack_entry_prefix, \
             sort_order, created_at FROM content_formats WHERE content_id = ?1 \
             ORDER BY sort_order, format_id",
        )
        .bind(content_id)
        .fetch_all(pool)
        .await
    })
}

/// 本に紐づくコンテンツ／レンディションを削除する（再取り込み用）。
/// 子（`content_formats`）→ 親（`book_contents`）の順で消す。
pub fn delete_for_book(pool: &SqlitePool, book_id: &str) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "DELETE FROM content_formats WHERE content_id IN \
             (SELECT content_id FROM book_contents WHERE book_id = ?1)",
        )
        .bind(book_id)
        .execute(pool)
        .await?;
        sqlx::query("DELETE FROM book_contents WHERE book_id = ?1")
            .bind(book_id)
            .execute(pool)
            .await?;
        Ok(())
    })
}
