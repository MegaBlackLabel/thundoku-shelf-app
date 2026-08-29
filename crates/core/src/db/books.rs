//! `books` repository.

use crate::db::SqlitePool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Book {
    pub id: String,
    pub title: String,
    pub author: String,
    pub circle_name: String,
    pub purchase_date: Option<String>,
    pub file_name: String,
    pub file_size: i64,
    pub opfs_path: String,
    pub cover_thumbnail: Option<String>,
    pub tbf_product_id: Option<String>,
    pub site_id: Option<String>,
    pub tags_fetched: i64,
    pub pack_id: Option<String>,
    pub is_favorite: i64,
    pub is_hidden: i64,
    pub created_at: String,
    pub updated_at: String,
}

const COLUMNS: &str = "id, title, author, circle_name, purchase_date, file_name, file_size, \
     opfs_path, cover_thumbnail, tbf_product_id, site_id, tags_fetched, pack_id, \
     is_favorite, is_hidden, created_at, updated_at";

pub fn insert(pool: &SqlitePool, book: &Book) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(&format!(
            "INSERT INTO books ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, \
             ?11, ?12, ?13, ?14, ?15, ?16, ?17)"
        ))
        .bind(&book.id)
        .bind(&book.title)
        .bind(&book.author)
        .bind(&book.circle_name)
        .bind(&book.purchase_date)
        .bind(&book.file_name)
        .bind(book.file_size)
        .bind(&book.opfs_path)
        .bind(&book.cover_thumbnail)
        .bind(&book.tbf_product_id)
        .bind(&book.site_id)
        .bind(book.tags_fetched)
        .bind(&book.pack_id)
        .bind(book.is_favorite)
        .bind(book.is_hidden)
        .bind(&book.created_at)
        .bind(&book.updated_at)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// Insert or replace an existing row by id (drive sync import uses this).
pub fn upsert(pool: &SqlitePool, book: &Book) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(&format!(
            "INSERT INTO books ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, \
             ?11, ?12, ?13, ?14, ?15, ?16, ?17)
             ON CONFLICT(id) DO UPDATE SET
               title = excluded.title,
               author = excluded.author,
               circle_name = excluded.circle_name,
               purchase_date = excluded.purchase_date,
               file_name = excluded.file_name,
               file_size = excluded.file_size,
               opfs_path = excluded.opfs_path,
               cover_thumbnail = excluded.cover_thumbnail,
               tbf_product_id = excluded.tbf_product_id,
               site_id = excluded.site_id,
               tags_fetched = excluded.tags_fetched,
               pack_id = excluded.pack_id,
               updated_at = excluded.updated_at"
        ))
        .bind(&book.id)
        .bind(&book.title)
        .bind(&book.author)
        .bind(&book.circle_name)
        .bind(&book.purchase_date)
        .bind(&book.file_name)
        .bind(book.file_size)
        .bind(&book.opfs_path)
        .bind(&book.cover_thumbnail)
        .bind(&book.tbf_product_id)
        .bind(&book.site_id)
        .bind(book.tags_fetched)
        .bind(&book.pack_id)
        .bind(book.is_favorite)
        .bind(book.is_hidden)
        .bind(&book.created_at)
        .bind(&book.updated_at)
        .execute(pool)
        .await?;
        Ok(())
    })
}

pub fn get(pool: &SqlitePool, id: &str) -> Result<Option<Book>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, Book>(&format!("SELECT {COLUMNS} FROM books WHERE id = ?1"))
            .bind(id)
            .fetch_optional(pool)
            .await
    })
}

pub fn list(pool: &SqlitePool) -> Result<Vec<Book>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, Book>(&format!(
            "SELECT {COLUMNS} FROM books ORDER BY created_at DESC"
        ))
        .fetch_all(pool)
        .await
    })
}

/// Link a locally downloaded book to its techbookfest shelf item
/// (`bookshelf_items.database_id`), so the shelf card resolves as downloaded.
pub fn set_tbf_product_id(
    pool: &SqlitePool,
    book_id: &str,
    tbf_product_id: &str,
) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query("UPDATE books SET tbf_product_id = ?1 WHERE id = ?2")
            .bind(tbf_product_id)
            .bind(book_id)
            .execute(pool)
            .await?;
        Ok(())
    })
}

pub fn delete(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query("DELETE FROM books WHERE id = ?1")
            .bind(id)
            .execute(pool)
            .await?;
        Ok(())
    })
}

/// Set the favorite flag on a downloaded book.
pub fn set_favorite(pool: &SqlitePool, id: &str, favorite: bool) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "UPDATE books SET is_favorite = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
        )
        .bind(favorite as i64)
        .bind(id)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// Set the hidden flag on a downloaded book.
pub fn set_hidden(pool: &SqlitePool, id: &str, hidden: bool) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "UPDATE books SET is_hidden = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
        )
        .bind(hidden as i64)
        .bind(id)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// ダウンロード元のサイト（techbookfest / booth）を books に記録する。
/// ビューアー設定のサイト別キー（viewer.mode.{site} 等）の解決に使う。
pub fn set_site_id(pool: &SqlitePool, id: &str, site_id: &str) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query("UPDATE books SET site_id = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2")
            .bind(site_id)
            .bind(id)
            .execute(pool)
            .await?;
        Ok(())
    })
}

/// List favorite books (for the auto-download on startup).
pub fn list_favorites(pool: &SqlitePool) -> Result<Vec<Book>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, Book>(&format!(
            "SELECT {COLUMNS} FROM books WHERE is_favorite = 1 ORDER BY created_at DESC"
        ))
        .fetch_all(pool)
        .await
    })
}
