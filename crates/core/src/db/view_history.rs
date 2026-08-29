//! `view_history` repository: 閲覧履歴（開始・終了時刻）と集計。
//!
//! 1 回の閲覧 = 1 行（`started_at` は開始時刻、`ended_at` は終了時刻）。
//! 途中でアプリを落とした場合に備え、閲覧中は `ended_at` を頻繁に
//! 更新（touch）しておくことで、最後に読んでいた時刻まで記録される。

use crate::db::SqlitePool;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct ViewSession {
    /// セッション ID（hex(randomblob(16))）
    pub id: String,
    pub book_id: String,
    pub started_at: String,
    /// 終了時刻。閲覧中の途中では最後の操作時刻が入る（強制終了対策）。
    pub ended_at: Option<String>,
}

/// 閲覧セッションを開始する（ended_at は開始時刻で初期化）。
/// 戻り値の `ended_at` は必ず Some（初期化済み）。
pub fn start(pool: &SqlitePool, book_id: &str) -> Result<ViewSession, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, ViewSession>(
            "INSERT INTO view_history (id, book_id, started_at, ended_at) \
             SELECT hex(randomblob(16)), ?1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP \
             RETURNING id, book_id, started_at, ended_at",
        )
        .bind(book_id)
        .fetch_one(pool)
        .await
    })
}

/// 閲覧セッションを終了する（ended_at = 現在時刻）。
pub fn end(pool: &SqlitePool, session_id: &str) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query("UPDATE view_history SET ended_at = CURRENT_TIMESTAMP WHERE id = ?1")
            .bind(session_id)
            .execute(pool)
            .await
            .map(|_| ())
    })
}

/// 閲覧セッションの ended_at を現在時刻に更新する（強制終了対策の heartbeat）。
pub fn touch(pool: &SqlitePool, session_id: &str) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query("UPDATE view_history SET ended_at = CURRENT_TIMESTAMP WHERE id = ?1")
            .bind(session_id)
            .execute(pool)
            .await
            .map(|_| ())
    })
}

/// 閲覧回数（セッション数）。
pub fn view_count(pool: &SqlitePool, book_id: &str) -> Result<i64, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_scalar("SELECT COUNT(*) FROM view_history WHERE book_id = ?1")
            .bind(book_id)
            .fetch_one(pool)
            .await
    })
}

/// 累計閲覧時間（秒）。ended_at - started_at の合計。
pub fn total_duration_secs(pool: &SqlitePool, book_id: &str) -> Result<i64, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_scalar(
            "SELECT COALESCE(SUM(CAST(julianday(COALESCE(ended_at, started_at)) - \
             julianday(started_at) AS REAL) * 86400), 0)::INTEGER \
             FROM view_history WHERE book_id = ?1",
        )
        .bind(book_id)
        .fetch_one(pool)
        .await
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn start_end_and_stats() {
        let pool = crate::db::test_pool();
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
            },
        )
        .unwrap();

        let session = start(&pool, "b1").unwrap();
        assert_eq!(session.book_id, "b1");
        assert!(session.ended_at.is_some());

        // 1 回目の閲覧を終了
        end(&pool, &session.id).unwrap();
        assert_eq!(view_count(&pool, "b1").unwrap(), 1);

        // 2 回目の閲覧（touch のみ = 途中で落ちたケース）
        let session2 = start(&pool, "b1").unwrap();
        touch(&pool, &session2.id).unwrap();
        assert_eq!(view_count(&pool, "b1").unwrap(), 2);
        assert!(total_duration_secs(&pool, "b1").unwrap() >= 0);
    }
}
