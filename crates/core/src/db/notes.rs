//! 付箋（ページ単位のメモ）。
//!
//! 1 ページ = 1 件（`book_id` + `content_id` + `page` で一意）。同じページに付箋アイコンから
//! 付け直すとメモを上書きする。`spread_side` は付けたときの見開きの左右（単一表示は `None`）で、
//! 「本を見る」から開くときに同じ側へ出すために使う。
//!
//! **付箋の ON / OFF はメモとは別**（`is_active`）。リーダーで付箋を外してもメモは残り、
//! もう一度付けるとメモが戻る（誤操作でメモを失わない）。
//!
//! メモ本文（`page_notes.memo`）は**平文で保存しない**。書き込みは [`column_crypto`] で
//! 暗号化し、読み出しは復号する（セキュリティ評価 F02）。

use sqlx::Row;

use crate::db::{SqlitePool, column_crypto};

/// 見開きのどちら側だったか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpreadSide {
    Left,
    Right,
}

impl SpreadSide {
    /// DB に保存する文字列。
    pub fn as_str(self) -> &'static str {
        match self {
            SpreadSide::Left => "left",
            SpreadSide::Right => "right",
        }
    }

    /// DB の文字列から復元する（未知の値は `None`）。
    pub fn parse(value: &str) -> Option<Self> {
        <SpreadSide as std::str::FromStr>::from_str(value).ok()
    }
}

impl std::str::FromStr for SpreadSide {
    type Err = ();

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "left" => Ok(SpreadSide::Left),
            "right" => Ok(SpreadSide::Right),
            _ => Err(()),
        }
    }
}

/// 付箋 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageNote {
    pub id: String,
    pub book_id: String,
    /// 表示中のコンテンツ（未指定 = `''`）。
    pub content_id: String,
    /// ページ番号（**1-indexed**。`reading_progress` と同じ）。
    pub page: i64,
    pub memo: String,
    /// 付けたときの見開きの左右（単一表示で付けたら `None`）。
    pub spread_side: Option<SpreadSide>,
    /// 付箋が付いているか。false = 外した状態（メモは残っている）。
    pub is_active: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl PageNote {
    /// DB 用のコンテンツキー（`content_id` が空なら `''`）。
    pub fn content_key(&self) -> &str {
        &self.content_id
    }
}

/// 追加 / 更新する内容。時刻は DB が UTC（`CURRENT_TIMESTAMP`）で入れる
/// （表示側でローカルに直す。`view_history` と同じ扱い）。
pub struct PageNoteInput<'a> {
    pub id: &'a str,
    pub book_id: &'a str,
    pub content_id: &'a str,
    /// 1-indexed。
    pub page: i64,
    pub memo: &'a str,
    pub spread_side: Option<SpreadSide>,
}

/// 付箋を追加 / 更新する（同じ本・コンテンツ・ページなら 1 件のまま。
/// 作成日はそのまま、メモと見開き側・更新日を書き換える）。
///
/// メモは暗号化して保存する。鍵が取れないときは**平文で書かず**にエラーにする
/// （fail-closed）。
pub fn upsert(pool: &SqlitePool, note: &PageNoteInput<'_>) -> Result<(), sqlx::Error> {
    let key = column_crypto::db_key()?;
    let memo = column_crypto::encrypt_str(
        &key,
        &column_crypto::aad_page_notes(note.book_id, note.content_id, note.page, "memo"),
        note.memo,
    )?;
    crate::db::block_on(async {
        sqlx::query(
            // 付け直しは「付箋を付ける」操作なので is_active を立てる（外していた
            // 場合もメモごと復帰する）。created_at（付箋登録日）は最初に付けた日を保つ。
            "INSERT INTO page_notes (id, book_id, content_id, page, memo, spread_side, is_active) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1) \
             ON CONFLICT(book_id, content_id, page) DO UPDATE SET \
               memo = excluded.memo, \
               spread_side = excluded.spread_side, \
               is_active = 1, \
               updated_at = CURRENT_TIMESTAMP",
        )
        .bind(note.id)
        .bind(note.book_id)
        .bind(note.content_id)
        .bind(note.page)
        .bind(&memo)
        .bind(note.spread_side.map(SpreadSide::as_str))
        .execute(pool)
        .await
    })?;
    Ok(())
}

/// 付箋を外す / 付け直す（**メモは消さない**）。外しても `get_for_page` でメモが読める。
pub fn set_active(
    pool: &SqlitePool,
    book_id: &str,
    content_id: &str,
    page: i64,
    active: bool,
) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "UPDATE page_notes SET is_active = ?4, updated_at = CURRENT_TIMESTAMP \
             WHERE book_id = ?1 AND content_id = ?2 AND page = ?3",
        )
        .bind(book_id)
        .bind(content_id)
        .bind(page)
        .bind(i64::from(active))
        .execute(pool)
        .await
    })?;
    Ok(())
}

/// 指定ページの付箋（無ければ `None`）。**外した付箋も返す**（メモの復帰用）。
pub fn get_for_page(
    pool: &SqlitePool,
    book_id: &str,
    content_id: &str,
    page: i64,
) -> Result<Option<PageNote>, sqlx::Error> {
    let key = column_crypto::db_key()?;
    crate::db::block_on(async {
        sqlx::query(
            "SELECT id, book_id, content_id, page, memo, spread_side, is_active, created_at, \
                    updated_at \
             FROM page_notes WHERE book_id = ?1 AND content_id = ?2 AND page = ?3",
        )
        .bind(book_id)
        .bind(content_id)
        .bind(page)
        .fetch_optional(pool)
        .await
    })
    .map(|row| row.map(|row| row_to_note(row, &key)))
}

/// 付箋を持つページ番号（0-indexed）の集合。リーダーがページ画像に印を出すために使う。
pub fn noted_pages(
    pool: &SqlitePool,
    book_id: &str,
    content_id: &str,
) -> Result<std::collections::HashSet<usize>, sqlx::Error> {
    let pages = crate::db::block_on(async {
        sqlx::query_scalar::<_, i64>(
            "SELECT page FROM page_notes \
             WHERE book_id = ?1 AND content_id = ?2 AND is_active = 1 ORDER BY page",
        )
        .bind(book_id)
        .bind(content_id)
        .fetch_all(pool)
        .await
    })?;
    Ok(pages
        .into_iter()
        .filter(|page| *page > 0)
        .map(|page| (page - 1) as usize)
        .collect())
}

/// 付箋を **追加が新しい順** に返す（付箋項目画面用）。メモは復号して返す。
pub fn list_newest_first(pool: &SqlitePool) -> Result<Vec<PageNote>, sqlx::Error> {
    let key = column_crypto::db_key()?;
    let rows = crate::db::block_on(async {
        sqlx::query(
            "SELECT id, book_id, content_id, page, memo, spread_side, is_active, created_at, \
                    updated_at \
             FROM page_notes WHERE is_active = 1 ORDER BY created_at DESC, rowid DESC",
        )
        .fetch_all(pool)
        .await
    })?;
    Ok(rows
        .into_iter()
        .map(|row| row_to_note(row, &key))
        .collect())
}

/// 付箋を消す。
pub fn delete(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query("DELETE FROM page_notes WHERE id = ?1")
            .bind(id)
            .execute(pool)
            .await
    })?;
    Ok(())
}

fn row_to_note(row: sqlx::sqlite::SqliteRow, key: &[u8; 32]) -> PageNote {
    let spread_side: Option<String> = row.get("spread_side");
    let id: String = row.get("id");
    let book_id: String = row.get("book_id");
    let content_id: String = row.get("content_id");
    let page: i64 = row.get("page");
    let stored: String = row.get("memo");
    let aad = column_crypto::aad_page_notes(&book_id, &content_id, page, "memo");
    // 復号できない値（鍵違い・改ざん・壊れた base64）は**空文字**にする。
    // **暗号文を画面に出さない**（fail-closed）。行そのものは残す（付箋の ON / OFF と
    // 見開き側はメモとは別に意味を持つ）。
    let memo = column_crypto::decrypt_str(key, &aad, &stored)
        .ok()
        .flatten()
        .unwrap_or_else(|| {
            log::warn!(
                "付箋メモを復号できないため空として扱います（鍵違い・改ざんの可能性）: \
                 {book_id}/{content_id}/{page}"
            );
            String::new()
        });
    PageNote {
        id,
        book_id,
        content_id,
        page,
        memo,
        spread_side: spread_side.as_deref().and_then(SpreadSide::parse),
        is_active: row.get::<i64, _>("is_active") != 0,
        created_at: row.get("created_at"),
        updated_at: row.get("updated_at"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn note<'a>(
        id: &'a str,
        book: &'a str,
        content: &'a str,
        page: i64,
        memo: &'a str,
    ) -> PageNoteInput<'a> {
        PageNoteInput {
            id,
            book_id: book,
            content_id: content,
            page,
            memo,
            spread_side: None,
        }
    }

    fn seed_book(pool: &SqlitePool, id: &str) {
        crate::db::books::insert(
            pool,
            &crate::db::books::Book {
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
            },
        )
        .unwrap();
    }

    /// 同じページへの付箋は 1 件のまま（メモを上書き、作成日は維持）。
    #[test]
    fn upsert_keeps_one_note_per_page() {
        let pool = crate::db::test_pool();
        seed_book(&pool, "b1");
        upsert(
            &pool,
            &PageNoteInput {
                spread_side: Some(SpreadSide::Right),
                ..note("n1", "b1", "", 5, "最初のメモ")
            },
        )
        .unwrap();
        let created_at = list_newest_first(&pool).unwrap()[0].created_at.clone();

        upsert(
            &pool,
            &PageNoteInput {
                id: "n2",
                spread_side: Some(SpreadSide::Left),
                ..note("n2", "b1", "", 5, "書き直したメモ")
            },
        )
        .unwrap();

        let notes = list_newest_first(&pool).unwrap();
        assert_eq!(notes.len(), 1, "同じページに 2 件できている");
        assert_eq!(notes[0].memo, "書き直したメモ");
        assert_eq!(
            notes[0].spread_side,
            Some(SpreadSide::Left),
            "見開き側が更新されていない"
        );
        assert_eq!(notes[0].created_at, created_at, "作成日が書き換わっている");
    }

    /// 付箋を外してもメモは残り、付け直すとメモが戻る（誤操作でメモを失わない）。
    #[test]
    fn detaching_keeps_the_memo_and_reattaching_restores_it() {
        let pool = crate::db::test_pool();
        seed_book(&pool, "b1");
        upsert(&pool, &note("n1", "b1", "", 3, "大事なメモ")).unwrap();

        // 外す: 一覧 / ページ印から消えるが、メモは読める
        set_active(&pool, "b1", "", 3, false).unwrap();
        assert!(
            list_newest_first(&pool).unwrap().is_empty(),
            "外した付箋が一覧に出ている"
        );
        assert!(
            noted_pages(&pool, "b1", "").unwrap().is_empty(),
            "外した付箋がページ印に残っている"
        );
        let detached = get_for_page(&pool, "b1", "", 3)
            .unwrap()
            .expect("外した付箋が消えている");
        assert_eq!(detached.memo, "大事なメモ", "外したときにメモが消えている");
        assert!(!detached.is_active);

        // 付け直す: メモが復帰して一覧 / ページ印に戻る
        upsert(&pool, &note("n1", "b1", "", 3, "大事なメモ")).unwrap();
        let reattached = get_for_page(&pool, "b1", "", 3).unwrap().unwrap();
        assert!(reattached.is_active, "付け直しても ON になっていない");
        assert_eq!(reattached.memo, "大事なメモ");
        assert_eq!(list_newest_first(&pool).unwrap().len(), 1);
        assert!(noted_pages(&pool, "b1", "").unwrap().contains(&2));
    }

    /// 追加が新しい順に並ぶ（同時刻は後から入れたものが先）。
    #[test]
    fn list_returns_newest_first() {
        let pool = crate::db::test_pool();
        seed_book(&pool, "b1");
        seed_book(&pool, "b2");
        upsert(&pool, &note("n1", "b1", "", 1, "古い")).unwrap();
        upsert(&pool, &note("n2", "b2", "", 3, "新しい")).unwrap();
        upsert(&pool, &note("n3", "b1", "", 2, "最後")).unwrap();

        let notes = list_newest_first(&pool).unwrap();
        let memos: Vec<&str> = notes.iter().map(|n| n.memo.as_str()).collect();
        assert_eq!(memos, vec!["最後", "新しい", "古い"]);
    }

    /// ページ指定の取得・リーダー用の印・削除。
    #[test]
    fn get_noted_pages_and_delete() {
        let pool = crate::db::test_pool();
        seed_book(&pool, "b1");
        upsert(&pool, &note("n1", "b1", "", 1, "1 ページ目")).unwrap();
        upsert(&pool, &note("n2", "b1", "", 4, "4 ページ目")).unwrap();

        let found = get_for_page(&pool, "b1", "", 4)
            .unwrap()
            .expect("4 ページ目の付箋");
        assert_eq!(found.memo, "4 ページ目");
        assert!(get_for_page(&pool, "b1", "", 2).unwrap().is_none());

        // ページ番号は 1-indexed で保存し、リーダーには 0-indexed で返す
        let pages = noted_pages(&pool, "b1", "").unwrap();
        assert!(
            pages.contains(&0) && pages.contains(&3),
            "印が合わない: {pages:?}"
        );

        delete(&pool, &found.id).unwrap();
        assert_eq!(list_newest_first(&pool).unwrap().len(), 1);
    }

    /// `spread_side` の保存文字列（"left" / "right"）はリーダーのアクションとも共有する。
    #[test]
    fn spread_side_round_trips() {
        for side in [SpreadSide::Left, SpreadSide::Right] {
            assert_eq!(SpreadSide::parse(side.as_str()), Some(side));
        }
        assert_eq!(SpreadSide::parse("up"), None);
    }

    /// コンテンツが違えば別の付箋（同じページ番号でも衝突しない）。
    #[test]
    fn notes_are_scoped_to_the_content() {
        let pool = crate::db::test_pool();
        seed_book(&pool, "b1");
        upsert(&pool, &note("n1", "b1", "", 2, "本文の 2 ページ目")).unwrap();
        upsert(&pool, &note("n2", "b1", "bonus", 2, "別冊の 2 ページ目")).unwrap();

        let notes = list_newest_first(&pool).unwrap();
        assert_eq!(notes.len(), 2, "コンテンツをまたいで 1 件に潰れている");
        assert_eq!(notes[0].content_key(), "bonus");
    }
}
