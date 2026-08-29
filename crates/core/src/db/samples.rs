//! `product_sample_pages` repository (技術書典 試し読み画像).

use crate::db::SqlitePool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct SamplePageRow {
    pub id: String,
    pub checklist_item_id: String,
    pub product_id: Option<String>,
    pub page_number: i64,
    pub image_url: Option<String>,
    pub image_data: Option<String>,
    pub mime_type: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub file_size: Option<i64>,
    pub fetched_at: String,
}

pub fn insert_sample_page(pool: &SqlitePool, page: &SamplePageRow) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "INSERT INTO product_sample_pages (id, checklist_item_id, product_id, page_number, \
             image_url, image_data, mime_type, width, height, file_size, fetched_at, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, CURRENT_TIMESTAMP)",
        )
        .bind(&page.id)
        .bind(&page.checklist_item_id)
        .bind(&page.product_id)
        .bind(page.page_number)
        .bind(&page.image_url)
        .bind(&page.image_data)
        .bind(&page.mime_type)
        .bind(page.width)
        .bind(page.height)
        .bind(page.file_size)
        .bind(&page.fetched_at)
        .execute(pool)
        .await?;
        Ok(())
    })
}

pub fn delete_for_item(pool: &SqlitePool, checklist_item_id: &str) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query("DELETE FROM product_sample_pages WHERE checklist_item_id = ?1")
            .bind(checklist_item_id)
            .execute(pool)
            .await?;
        Ok(())
    })
}

pub fn list_for_item(
    pool: &SqlitePool,
    checklist_item_id: &str,
) -> Result<Vec<SamplePageRow>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, SamplePageRow>(
            "SELECT id, checklist_item_id, product_id, page_number, image_url, image_data, \
             mime_type, width, height, file_size, fetched_at FROM product_sample_pages \
             WHERE checklist_item_id = ?1 ORDER BY page_number",
        )
        .bind(checklist_item_id)
        .fetch_all(pool)
        .await
    })
}
