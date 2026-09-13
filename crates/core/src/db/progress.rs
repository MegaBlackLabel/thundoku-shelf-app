//! `reading_progress` repository.

use crate::db::SqlitePool;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct ReadingProgress {
    pub book_id: String,
    /// コンテンツ（`book_contents.content_id`）。`''` = 未指定（旧データ / 単一コンテンツ）。
    pub content_id: String,
    pub current_page: i64,
    pub total_pages: Option<i64>,
    /// Set once the last page has been reached; never cleared afterwards.
    pub finished_at: Option<String>,
    pub last_read_at: String,
}

impl ReadingProgress {
    /// True when the last page has been reached.
    /// current_page はリーダーの保存形式と同じ 1-indexed（最終ページ = total_pages）。
    pub fn is_finished(&self) -> bool {
        self.total_pages
            .is_some_and(|total| self.current_page >= total)
    }
}

/// 本の読書状態。**表示・フィルタ・集計はこの 1 か所で判定する**
/// （本棚のバッジ / 行の状態チップ / 履歴 / 設定の冊数集計がすべてここを通る）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadingState {
    /// 進捗が無い / 1 ページ目も読んでいない。
    Unread,
    /// 途中まで読んだ（最終ページには達していない）。
    Reading,
    /// 最終ページまで読んだ。
    Read,
}

impl ReadingState {
    /// 進捗から読書状態を決める。
    ///
    /// - **読了**: `finished_at` が立っている（リーダーが最終ページ到達でセットする）、
    ///   または `current_page >= total_pages`（`finished_at` を持たない旧データの救済）
    /// - **読書中**: 1 ページ目以降を読んでいる（`current_page > 0`。1-indexed なので
    ///   本を開いただけでは行が作られず、未読のままになる）
    /// - **未読**: 進捗が無い / `current_page == 0`
    pub fn from_progress(progress: Option<&ReadingProgress>) -> Self {
        match progress {
            Some(p) if p.finished_at.is_some() || p.is_finished() => ReadingState::Read,
            Some(p) if p.current_page > 0 => ReadingState::Reading,
            _ => ReadingState::Unread,
        }
    }
}

/// **既定表示（優先）コンテンツ**の進捗を返す（本棚カード・未読数・統計用）。
/// コンテンツ情報を持たない旧データは `content_id = ''` の行を見る。
pub fn get(pool: &SqlitePool, book_id: &str) -> Result<Option<ReadingProgress>, sqlx::Error> {
    let content_id = crate::db::contents::primary_for_book(pool, book_id)?
        .map(|content| content.content_id)
        .unwrap_or_default();
    get_for(pool, book_id, &content_id)
}

/// 指定コンテンツの進捗を返す（リーダーが再開位置に使う）。
pub fn get_for(
    pool: &SqlitePool,
    book_id: &str,
    content_id: &str,
) -> Result<Option<ReadingProgress>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, ReadingProgress>(
            "SELECT book_id, content_id, current_page, total_pages, finished_at, last_read_at \
             FROM reading_progress WHERE book_id = ?1 AND content_id = ?2",
        )
        .bind(book_id)
        .bind(content_id)
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
            "INSERT INTO reading_progress (book_id, content_id, current_page, total_pages, \
             finished_at, last_read_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT(book_id, content_id) DO UPDATE SET
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
        .bind(&progress.content_id)
        .bind(progress.current_page)
        .bind(progress.total_pages)
        .bind(&finished_at)
        .bind(&progress.last_read_at)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// その本の全コンテンツ分の進捗を消す。
pub fn delete(pool: &SqlitePool, book_id: &str) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query("DELETE FROM reading_progress WHERE book_id = ?1")
            .bind(book_id)
            .execute(pool)
            .await?;
        Ok(())
    })
}

/// 指定コンテンツの進捗だけ消す（コンテンツ削除時など）。
pub fn delete_for_content(
    pool: &SqlitePool,
    book_id: &str,
    content_id: &str,
) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query("DELETE FROM reading_progress WHERE book_id = ?1 AND content_id = ?2")
            .bind(book_id)
            .bind(content_id)
            .execute(pool)
            .await?;
        Ok(())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 読書状態の判定（唯一の定義）。境界を全部押さえる。
    #[test]
    fn reading_state_covers_the_boundaries() {
        let progress = |current_page: i64, total: Option<i64>, finished: bool| ReadingProgress {
            book_id: "b1".into(),
            content_id: String::new(),
            current_page,
            total_pages: total,
            finished_at: finished.then(|| "2026-01-01 00:00:00".to_string()),
            last_read_at: "2026-01-01 00:00:00".into(),
        };

        // 進捗なし = 未読
        assert_eq!(ReadingState::from_progress(None), ReadingState::Unread);
        // 開いただけ（current_page = 0）は未読のまま
        assert_eq!(
            ReadingState::from_progress(Some(&progress(0, Some(10), false))),
            ReadingState::Unread
        );
        // 1 ページ目以降を読んだら読書中
        assert_eq!(
            ReadingState::from_progress(Some(&progress(1, Some(10), false))),
            ReadingState::Reading
        );
        // 最終ページの 1 つ手前はまだ読書中（旧・設定の集計はここを読了にしていた）
        assert_eq!(
            ReadingState::from_progress(Some(&progress(9, Some(10), false))),
            ReadingState::Reading
        );
        // 最終ページ到達 = 読了
        assert_eq!(
            ReadingState::from_progress(Some(&progress(10, Some(10), false))),
            ReadingState::Read
        );
        // finished_at があれば読了（ページ数が食い違っていても読了を優先）
        assert_eq!(
            ReadingState::from_progress(Some(&progress(1, Some(10), true))),
            ReadingState::Read
        );
        // 総ページ数が無い本（PDF 以外 / 旧データ）でも「読んだ」ことは分かる
        assert_eq!(
            ReadingState::from_progress(Some(&progress(3, None, false))),
            ReadingState::Reading
        );
    }

    /// §8.2: 優先コンテンツを変えると、カードが見る行が変わるので誤った読了バッジが残らない。
    /// （進捗がコンテンツ単位の行に分かれたことで自然に満たされる）
    #[test]
    fn changing_the_primary_content_does_not_keep_a_wrong_finished_flag() {
        let pool = crate::db::test_pool();
        crate::db::block_on(async {
            sqlx::query(
                "INSERT INTO books (id, title, file_name, file_size, opfs_path) \
                 VALUES ('b1', 't', 't.zip', 1, 'b1.opfspack')",
            )
            .execute(&pool)
            .await
            .unwrap();
        });
        let stamp = "2026-09-11 00:00:00".to_string();
        crate::db::contents::insert_batch(
            &pool,
            &[
                crate::db::contents::BookContent {
                    content_id: "c1".into(),
                    book_id: "b1".into(),
                    display_name: "本編".into(),
                    media_kind: "image".into(),
                    is_primary: 1,
                    sort_order: 0,
                    created_at: stamp.clone(),
                },
                crate::db::contents::BookContent {
                    content_id: "c2".into(),
                    book_id: "b1".into(),
                    display_name: "別冊".into(),
                    media_kind: "pdf".into(),
                    is_primary: 0,
                    sort_order: 1,
                    created_at: stamp.clone(),
                },
            ],
            &[],
        )
        .unwrap();
        // 本編を読み切る（読了）
        upsert(
            &pool,
            &ReadingProgress {
                book_id: "b1".into(),
                content_id: "c1".into(),
                current_page: 3,
                total_pages: Some(3),
                finished_at: Some("2026-09-11 00:00:00".into()),
                last_read_at: stamp.clone(),
            },
        )
        .unwrap();
        assert!(get(&pool, "b1").unwrap().unwrap().finished_at.is_some());

        // 既定表示を別冊に変える（別冊は途中まで）
        crate::db::contents::set_primary(&pool, "b1", "c2").unwrap();
        upsert(
            &pool,
            &ReadingProgress {
                book_id: "b1".into(),
                content_id: "c2".into(),
                current_page: 1,
                total_pages: Some(5),
                finished_at: None,
                last_read_at: stamp.clone(),
            },
        )
        .unwrap();

        // カードが見る行（優先コンテンツ）は別冊 → 読了バッジは出ない
        let card = get(&pool, "b1").unwrap().unwrap();
        assert_eq!(card.content_id, "c2");
        assert!(card.finished_at.is_none(), "別冊に切り替えたら読了にしない");
        // 本編の読了は保持される（行が分かれている）
        assert!(
            get_for(&pool, "b1", "c1")
                .unwrap()
                .unwrap()
                .finished_at
                .is_some(),
            "本編の読了は残る"
        );
    }

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
            content_id: String::new(),
            current_page: 0,
            total_pages: Some(10),
            finished_at: None,
            last_read_at: "2026-08-21 00:00:00".into(),
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
