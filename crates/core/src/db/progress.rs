//! `reading_progress` repository.

use crate::db::SqlitePool;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct ReadingProgress {
    pub book_id: String,
    pub current_page: i64,
    pub total_pages: Option<i64>,
    /// Set once the last page has been reached; never cleared afterwards.
    pub finished_at: Option<String>,
    pub last_read_at: String,
    pub scroll_position: f64,
}

impl ReadingProgress {
    /// True when the last page has been reached.
    /// current_page はリーダーの保存形式と同じ 1-indexed（最終ページ = total_pages）。
    pub fn is_finished(&self) -> bool {
        self.total_pages
            .is_some_and(|total| self.current_page >= total)
    }
}

pub fn get(pool: &SqlitePool, book_id: &str) -> Result<Option<ReadingProgress>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, ReadingProgress>(
            "SELECT book_id, current_page, total_pages, finished_at, last_read_at, scroll_position \
             FROM reading_progress WHERE book_id = ?1",
        )
        .bind(book_id)
        .fetch_optional(pool)
        .await
    })
}

pub fn upsert(pool: &SqlitePool, progress: &ReadingProgress) -> Result<(), sqlx::Error> {
    // Once finished, keep finished_at forever (even if the reader moves back).
    let finished_at = progress.finished_at.clone().or_else(|| {
        if progress.is_finished() {
            Some(progress.last_read_at.clone())
        } else {
            None
        }
    });
    crate::db::block_on(async {
        sqlx::query(
            "INSERT INTO reading_progress (book_id, current_page, total_pages, finished_at, \
             last_read_at, scroll_position) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(book_id) DO UPDATE SET
               current_page = excluded.current_page,
               total_pages = excluded.total_pages,
               finished_at = CASE
                 WHEN reading_progress.finished_at IS NOT NULL THEN reading_progress.finished_at
                 ELSE excluded.finished_at
               END,
               last_read_at = excluded.last_read_at,
               scroll_position = excluded.scroll_position",
        )
        .bind(&progress.book_id)
        .bind(progress.current_page)
        .bind(progress.total_pages)
        .bind(&finished_at)
        .bind(&progress.last_read_at)
        .bind(progress.scroll_position)
        .execute(pool)
        .await?;
        Ok(())
    })
}

pub fn delete(pool: &SqlitePool, book_id: &str) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query("DELETE FROM reading_progress WHERE book_id = ?1")
            .bind(book_id)
            .execute(pool)
            .await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finished_at_is_set_at_last_page_and_never_cleared() {
        let pool = crate::db::test_pool();
        // reading_progress.book_id の FK 用に books を 1 冊入れる
        crate::db::books::insert(
            &pool,
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
                media_category: None,
                ai_type: None,
                is_drm: 0,
                release_date: None,
                description: None,
                theme: None,
                maker_id: None,
                page_count: None,
                age_rating: None,
                series_name: None,
            },
        )
        .unwrap();
        let base = ReadingProgress {
            book_id: "b1".into(),
            current_page: 0,
            total_pages: Some(10),
            finished_at: None,
            last_read_at: "2026-08-21 00:00:00".into(),
            scroll_position: 0.0,
        };
        upsert(&pool, &base).unwrap();
        assert_eq!(get(&pool, "b1").unwrap().unwrap().finished_at, None);

        // 最終ページ（1-indexed の 10 = 10 ページ目）で保存 → finished_at が立つ
        let finished = ReadingProgress {
            current_page: 10,
            ..base.clone()
        };
        upsert(&pool, &finished).unwrap();
        assert!(get(&pool, "b1").unwrap().unwrap().finished_at.is_some());

        // 1 ページ目に戻しても finished_at は維持される
        let rewound = ReadingProgress {
            current_page: 1,
            ..base.clone()
        };
        upsert(&pool, &rewound).unwrap();
        assert!(
            get(&pool, "b1").unwrap().unwrap().finished_at.is_some(),
            "finished_at must survive rewinding to page 1"
        );
    }
}
