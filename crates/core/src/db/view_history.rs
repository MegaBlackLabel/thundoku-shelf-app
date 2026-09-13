//! `view_history` repository: 閲覧履歴（開始・終了時刻）と集計。
//!
//! 1 回の閲覧 = 1 行（`started_at` は開始時刻、`ended_at` は終了時刻）。
//! 途中でアプリを落とした場合に備え、閲覧中は `ended_at` を頻繁に
//! 更新（touch）しておくことで、最後に読んでいた時刻まで記録される。

use sqlx::Row;

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
             SELECT hex(randomblob(16)), ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP \
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
        sqlx::query("UPDATE view_history SET ended_at = CURRENT_TIMESTAMP WHERE id = ?")
            .bind(session_id)
            .execute(pool)
            .await
            .map(|_| ())
    })
}

/// 閲覧セッションの ended_at を現在時刻に更新する（強制終了対策の heartbeat）。
/// 閲覧回数（セッション数）。
pub fn view_count(pool: &SqlitePool, book_id: &str) -> Result<i64, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_scalar("SELECT COUNT(*) FROM view_history WHERE book_id = ?")
            .bind(book_id)
            .fetch_one(pool)
            .await
    })
}

/// 1 冊ぶんの閲覧統計（本棚のソート用）。
#[derive(Debug, Clone, PartialEq)]
pub struct ViewStats {
    /// 閲覧回数（セッション数）
    pub count: i64,
    /// 累計閲覧時間（秒）
    pub total_seconds: i64,
    /// 最後に開いた時刻（ended_at が無ければ started_at）
    pub last_viewed_at: Option<String>,
}

/// 全書籍の閲覧統計を 1 クエリでまとめて取る（本棚のソート用。0 回の本は行が無い）。
pub fn view_stats(
    pool: &SqlitePool,
) -> Result<std::collections::HashMap<String, ViewStats>, sqlx::Error> {
    crate::db::block_on(async {
        let rows: Vec<(String, i64, i64, Option<String>)> = sqlx::query_as(
            "SELECT book_id, COUNT(*), \
             CAST(COALESCE(SUM(CAST(julianday(COALESCE(ended_at, started_at)) - \
               julianday(started_at) AS REAL) * 86400), 0) AS INTEGER), \
             MAX(COALESCE(ended_at, started_at)) \
             FROM view_history GROUP BY book_id",
        )
        .fetch_all(pool)
        .await?;
        Ok(rows
            .into_iter()
            .map(|(book_id, count, total_seconds, last_viewed_at)| {
                (
                    book_id,
                    ViewStats {
                        count,
                        total_seconds,
                        last_viewed_at,
                    },
                )
            })
            .collect())
    })
}

/// 累計閲覧時間（秒）。ended_at - started_at の合計。
pub fn total_duration_secs(pool: &SqlitePool, book_id: &str) -> Result<i64, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_scalar(
            "SELECT CAST(COALESCE(SUM(CAST(julianday(COALESCE(ended_at, started_at)) - \
             julianday(started_at) AS REAL) * 86400), 0) AS INTEGER) \
             FROM view_history WHERE book_id = ?",
        )
        .bind(book_id)
        .fetch_one(pool)
        .await
    })
}

/// 履歴画面の 1 行 = 1 日 1 本の集約。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DailyHistory {
    pub book_id: String,
    /// ローカル日付（`YYYY-MM-DD`）。
    pub day: String,
    /// その日の最後の閲覧開始時刻（ローカル `YYYY-MM-DD HH:MM:SS`）。
    pub last_started_at: String,
    /// その日の閲覧時間の合計（秒）。
    pub duration_secs: i64,
    /// その日のセッション数。
    pub sessions: i64,
}

/// 閲覧履歴を「1 日 1 本」に集約して、新しい日から順に返す。
///
/// `view_history` の時刻は `CURRENT_TIMESTAMP`（= UTC）なので、**ローカル時刻に直してから**
/// 日付でまとめる（UTC で日付を切ると日本時間の朝が前日に入ってしまう）。
/// 日をまたいだセッションは開始時刻の日に入れる。同時刻なら book_id 順で決定的に並ぶ。
pub fn list_daily(pool: &SqlitePool) -> Result<Vec<DailyHistory>, sqlx::Error> {
    use chrono::{Local, NaiveDateTime, TimeZone as _};
    use std::collections::HashMap;

    let rows = crate::db::block_on(async {
        sqlx::query("SELECT book_id, started_at, ended_at FROM view_history")
            .fetch_all(pool)
            .await
    })?;

    let parse = |text: &str| -> Option<chrono::DateTime<Local>> {
        NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M:%S")
            .ok()
            .map(|naive| Local.from_utc_datetime(&naive))
    };

    let mut by_day: HashMap<(String, String), DailyHistory> = HashMap::new();
    for row in rows {
        let book_id: String = row.get("book_id");
        let started_at: String = row.get("started_at");
        let ended_at: Option<String> = row.get("ended_at");
        let Some(started) = parse(&started_at) else {
            continue;
        };
        let day = started.format("%Y-%m-%d").to_string();
        let last_started_at = started.format("%Y-%m-%d %H:%M:%S").to_string();
        let duration = ended_at
            .as_deref()
            .and_then(parse)
            .map(|ended| (ended - started).num_seconds().max(0))
            .unwrap_or(0);
        let entry = by_day
            .entry((book_id.clone(), day.clone()))
            .or_insert_with(|| DailyHistory {
                book_id,
                day,
                last_started_at: last_started_at.clone(),
                duration_secs: 0,
                sessions: 0,
            });
        entry.duration_secs += duration;
        entry.sessions += 1;
        if last_started_at > entry.last_started_at {
            entry.last_started_at = last_started_at;
        }
    }

    let mut days: Vec<DailyHistory> = by_day.into_values().collect();
    days.sort_by(|a, b| {
        b.last_started_at
            .cmp(&a.last_started_at)
            .then_with(|| a.book_id.cmp(&b.book_id))
    });
    Ok(days)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 履歴画面用の集約: 1 日 1 本にまとめ、閲覧時間を合算し、新しい日から並べる。
    /// 時刻は UTC（`CURRENT_TIMESTAMP`）なので、**ローカル日付**で日を切ることを確認する
    /// （UTC 20:00 = 日本時間 翌 05:00 のような時刻を使う）。
    /// セッションを「途中で落ちた」状態にする（ended_at だけ更新）。
    /// アプリ本体では使わないため、テスト専用のヘルパとしてここに置く。
    fn touch_session(pool: &SqlitePool, session_id: &str) {
        crate::db::block_on(async {
            sqlx::query("UPDATE view_history SET ended_at = CURRENT_TIMESTAMP WHERE id = ?")
                .bind(session_id)
                .execute(pool)
                .await
        })
        .unwrap();
    }

    #[test]
    fn view_stats_returns_count_duration_and_last_view_per_book() {
        let pool = crate::db::test_pool();
        for id in ["b1", "b2", "b3"] {
            crate::db::books::insert(&pool, &test_book(id)).unwrap();
        }
        // b1 は 2 回、b2 は 1 回（touch のみ = ended_at あり）、b3 は 0 回
        for _ in 0..2 {
            let session = start(&pool, "b1").unwrap();
            end(&pool, &session.id).unwrap();
        }
        let session = start(&pool, "b2").unwrap();
        touch_session(&pool, &session.id);

        let stats = view_stats(&pool).unwrap();
        let b1 = stats.get("b1").expect("b1 の行");
        assert_eq!(b1.count, 2, "b1 は 2 回");
        assert!(b1.last_viewed_at.is_some(), "最終閲覧が入る");
        assert!(!b1.last_viewed_at.as_deref().unwrap().is_empty());
        let b2 = stats.get("b2").expect("b2 の行");
        assert_eq!(b2.count, 1, "b2 は 1 回");
        assert!(!stats.contains_key("b3"), "0 回の本は行を作らない");

        // 最終閲覧は新しいセッションで更新される（開始時刻で比較できる）
        let b1_last = b1.last_viewed_at.clone().unwrap();
        let session = start(&pool, "b1").unwrap();
        end(&pool, &session.id).unwrap();
        let stats = view_stats(&pool).unwrap();
        let b1 = stats.get("b1").expect("b1 の行");
        assert_eq!(b1.count, 3, "3 回目");
        assert!(
            b1.last_viewed_at.as_deref().unwrap() >= b1_last.as_str(),
            "最終閲覧が巻き戻った: {} < {b1_last}",
            b1.last_viewed_at.as_deref().unwrap()
        );
    }

    #[test]
    fn view_stats_sums_session_seconds() {
        let pool = crate::db::test_pool();
        crate::db::books::insert(&pool, &test_book("b1")).unwrap();
        let session = start(&pool, "b1").unwrap();
        // 10 分前 + 5 分前 のセッションを作る（ended_at を直接更新）
        crate::db::block_on(async {
            sqlx::query("UPDATE view_history SET started_at = ?1, ended_at = ?2 WHERE id = ?3")
                .bind("2026-09-01 10:00:00")
                .bind("2026-09-01 10:10:00")
                .bind(&session.id)
                .execute(&pool)
                .await
        })
        .unwrap();
        let stats = view_stats(&pool).unwrap();
        assert_eq!(
            stats.get("b1").unwrap().total_seconds,
            600,
            "10 分 = 600 秒"
        );
    }

    #[test]
    fn list_daily_groups_by_local_day_and_sums_durations() {
        let pool = crate::db::test_pool();
        for id in ["b1", "b2"] {
            crate::db::books::insert(&pool, &test_book(id)).unwrap();
        }
        let insert = |id: &str, book: &str, started: &str, ended: &str| {
            crate::db::block_on(async {
                sqlx::query(
                    "INSERT INTO view_history (id, book_id, started_at, ended_at) \
                     VALUES (?, ?, ?, ?)",
                )
                .bind(id)
                .bind(book)
                .bind(started)
                .bind(ended)
                .execute(&pool)
                .await
            })
            .unwrap();
        };
        // b1: 同じローカル日に 2 セッション（10 分 + 20 分 = 30 分）
        insert("s1", "b1", "2026-09-12 20:00:00", "2026-09-12 20:10:00");
        insert("s2", "b1", "2026-09-12 20:30:00", "2026-09-12 20:50:00");
        // b2: 別の日（ローカルでは前日）
        insert("s3", "b2", "2026-09-11 20:00:00", "2026-09-11 20:30:00");

        let days = list_daily(&pool).unwrap();
        assert_eq!(days.len(), 2, "1 日 1 本に集約されていない");
        assert_eq!(days[0].book_id, "b1", "新しい日が先頭に来ていない");
        assert_eq!(days[0].sessions, 2);
        assert_eq!(days[0].duration_secs, 1800, "閲覧時間が合算されていない");
        assert_eq!(days[1].book_id, "b2");
        assert_eq!(days[1].duration_secs, 1800);

        // 日付は「UTC 文字列をローカルに直した日」になる（テスト側で同じ変換をして比較）
        let local_day = |utc: &str| {
            chrono::NaiveDateTime::parse_from_str(utc, "%Y-%m-%d %H:%M:%S")
                .unwrap()
                .and_utc()
                .with_timezone(&chrono::Local)
                .format("%Y-%m-%d")
                .to_string()
        };
        assert_eq!(days[0].day, local_day("2026-09-12 20:30:00"));
        assert_eq!(days[1].day, local_day("2026-09-11 20:00:00"));
        assert!(
            days[0].day != "2026-09-12" || local_day("2026-09-12 20:30:00") == "2026-09-12",
            "ローカル日付に変換されていない（UTC の日付で切っている）"
        );
    }

    fn test_book(id: &str) -> crate::db::books::Book {
        crate::db::books::Book {
            id: id.into(),
            title: "t".into(),
            author: String::new(),
            circle_name: String::new(),
            purchase_date: None,
            file_name: "t.pdf".into(),
            file_size: 1,
            opfs_path: format!("{id}.opfspack"),
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
        }
    }

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

        let session = start(&pool, "b1").unwrap();
        assert_eq!(session.book_id, "b1");
        assert!(session.ended_at.is_some());

        // 1 回目の閲覧を終了
        end(&pool, &session.id).unwrap();
        assert_eq!(view_count(&pool, "b1").unwrap(), 1);

        // 2 回目の閲覧（touch のみ = 途中で落ちたケース）
        let session2 = start(&pool, "b1").unwrap();
        touch_session(&pool, &session2.id);
        assert_eq!(view_count(&pool, "b1").unwrap(), 2);
        assert!(total_duration_secs(&pool, "b1").unwrap() >= 0);
    }
}
