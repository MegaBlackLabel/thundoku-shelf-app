//! `page_views` repository: ページ毎の閲覧記録（累計表示回数・累計滞在時間）。
//!
//! 1 書籍 × 1 ページ = 1 行（`view_count` はそのページが現在ページになった回数、
//! `total_seconds` はそのページ上で過ごした合計秒数）。`view_history` は書籍単位の
//! セッションを記録するが、こちらは「どのページをどれだけ見たか」のスタッツを補完する。

use crate::db::SqlitePool;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct PageView {
    pub book_id: String,
    /// 1-indexed（リーダーの `current_page` は 0 始まりなので +1 して保存する。
    /// `reading_progress` や `document_images.page_number` と同じ規約）。
    pub page_number: i64,
    pub view_count: i64,
    pub total_seconds: f64,
    pub last_viewed_at: String,
}

/// ページが現在ページになったことを記録する（view_count を +1）。
pub fn record_view(pool: &SqlitePool, book_id: &str, page_number: i64) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "INSERT INTO page_views (book_id, page_number, view_count, total_seconds, last_viewed_at) \
             VALUES (?1, ?2, 1, 0, CURRENT_TIMESTAMP) \
             ON CONFLICT(book_id, page_number) DO UPDATE SET \
               view_count = view_count + 1, \
               last_viewed_at = CURRENT_TIMESTAMP",
        )
        .bind(book_id)
        .bind(page_number)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// ページでの滞在時間（秒）を加算する。
pub fn add_dwell(
    pool: &SqlitePool,
    book_id: &str,
    page_number: i64,
    seconds: f64,
) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "INSERT INTO page_views (book_id, page_number, view_count, total_seconds, last_viewed_at) \
             VALUES (?1, ?2, 0, ?3, CURRENT_TIMESTAMP) \
             ON CONFLICT(book_id, page_number) DO UPDATE SET \
               total_seconds = total_seconds + excluded.total_seconds, \
               last_viewed_at = CURRENT_TIMESTAMP",
        )
        .bind(book_id)
        .bind(page_number)
        .bind(seconds)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// 1 冊分のページ毎記録（ページ番号順）。
pub fn for_book(pool: &SqlitePool, book_id: &str) -> Result<Vec<PageView>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, PageView>(
            "SELECT book_id, page_number, view_count, total_seconds, last_viewed_at \
             FROM page_views WHERE book_id = ?1 ORDER BY page_number ASC",
        )
        .bind(book_id)
        .fetch_all(pool)
        .await
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_book(pool: &SqlitePool) {
        crate::db::books::insert(
            pool,
            &crate::db::books::Book {
                id: "b1".into(),
                title: "t".into(),
                author: String::new(),
                circle_name: String::new(),
                purchase_date: None,
                file_name: "t.pdf".into(),
                file_size: 1,
                opfs_path: "b1.opfspack".into(),
                cover_thumbnail: None,
                tbf_product_id: None,
                site_id: None,
                tags_fetched: 1,
                pack_id: None,
                is_favorite: 0,
                is_hidden: 0,
                created_at: "2026-08-21 00:00:00".into(),
                updated_at: "2026-08-21 00:00:00".into(),
            },
        )
        .unwrap();
    }

    #[test]
    fn record_view_and_add_dwell_accumulate_per_page() {
        let pool = crate::db::test_pool();
        seed_book(&pool);

        // 同じページの表示回数と滞在秒数が加算される
        record_view(&pool, "b1", 1).unwrap();
        record_view(&pool, "b1", 1).unwrap();
        add_dwell(&pool, "b1", 1, 3.5).unwrap();
        add_dwell(&pool, "b1", 1, 1.5).unwrap();

        let rows = for_book(&pool, "b1").unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].page_number, 1);
        assert_eq!(rows[0].view_count, 2);
        assert!((rows[0].total_seconds - 5.0).abs() < 1e-9);

        // 別ページは別行
        record_view(&pool, "b1", 2).unwrap();
        let rows = for_book(&pool, "b1").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[1].page_number, 2);
        assert_eq!(rows[1].view_count, 1);

        // 本を消すと FK ON DELETE CASCADE で一緒に消える
        crate::db::books::delete(&pool, "b1").unwrap();
        assert!(for_book(&pool, "b1").unwrap().is_empty());
    }
}
