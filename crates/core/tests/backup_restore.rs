//! バックアップのリストアに関する回帰テスト。
//!
//! Drive バックアップ（JSON）を新規 DB に復元する経路は「別 PC への復元」の
//! 主経路なので、FK 順序・原子性・スキーマ差異・ユーザーデータの網羅を検証する。

use thundoku_core::db::{backup, block_on, books};

fn book(id: &str) -> books::Book {
    books::Book {
        id: id.into(),
        title: format!("本 {id}"),
        author: String::new(),
        circle_name: String::new(),
        purchase_date: None,
        file_name: format!("{id}.pdf"),
        file_size: 1,
        opfs_path: format!("{id}.opfspack"),
        cover_thumbnail: None,
        tbf_product_id: None,
        site_id: None,
        tags_fetched: 0,
        pack_id: Some(id.into()),
        is_favorite: 0,
        is_hidden: 0,
        created_at: "2026-08-01 00:00:00".into(),
        updated_at: "2026-08-01 00:00:00".into(),
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

/// 技術書典のイベントに紐づく本棚アイテムとチェックリスト項目を持つ DB を組む。
fn seed_source_pool() -> sqlx::SqlitePool {
    let pool = thundoku_core::db::test_pool();
    block_on(async {
        sqlx::query(
            "INSERT INTO tbf_events (id, site_id, event_name) \
             VALUES ('e1', 'techbookfest', '技術書典 15')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO bookshelf_items (site_id, database_id, title, event_id) \
             VALUES ('techbookfest', 'd1', 'イベント本', 'e1')",
        )
        .execute(&pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO checked_items (id, event_id, circle_name, space_number) \
             VALUES ('ci1', 'e1', 'サークルA', 'あ-01')",
        )
        .execute(&pool)
        .await
        .unwrap();
    });
    pool
}

/// 新規（空の）DB へ復元しても、FK の親子順序で失敗しないこと。
///
/// 親（tbf_events）より先に子（bookshelf_items / checked_items）を INSERT すると
/// `foreign_keys(true)` の下で FK 違反になり、復元が中断して**部分復元**が残る。
#[test]
fn restore_into_fresh_db_keeps_event_children() {
    let src = seed_source_pool();
    let json = backup::export_json(&src, None, None).unwrap();

    let dst = thundoku_core::db::test_pool(); // 空の新規 DB（sites のみ seed 済み）
    backup::import_json(&dst, &json).expect("restore into a fresh DB should succeed");

    let count =
        |sql: &'static str| -> i64 { block_on(sqlx::query_scalar(sql).fetch_one(&dst)).unwrap() };
    assert_eq!(
        count("SELECT COUNT(*) FROM tbf_events WHERE id = 'e1'"),
        1,
        "tbf_events が復元されること"
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM bookshelf_items WHERE database_id = 'd1'"),
        1,
        "bookshelf_items が復元されること"
    );
    assert_eq!(
        count("SELECT COUNT(*) FROM checked_items WHERE id = 'ci1'"),
        1,
        "checked_items が復元されること"
    );
}

/// ユーザーが書いた付箋メモ（page_notes）がバックアップ対象に含まれること。
///
/// 付箋は再生成できないユーザーデータなので、復元で消えてはならない。
#[test]
fn restore_keeps_user_page_notes() {
    let src = thundoku_core::db::test_pool();
    books::insert(&src, &book("b1")).unwrap();
    thundoku_core::db::notes::upsert(
        &src,
        &thundoku_core::db::notes::PageNoteInput {
            id: "n1",
            book_id: "b1",
            content_id: "",
            page: 3,
            memo: "ここ重要",
            spread_side: None,
        },
    )
    .unwrap();

    let json = backup::export_json(&src, None, None).unwrap();
    assert!(
        json.contains("page_notes"),
        "エクスポートに page_notes が含まれること"
    );

    let dst = thundoku_core::db::test_pool();
    backup::import_json(&dst, &json).unwrap();
    let notes = thundoku_core::db::notes::list_newest_first(&dst).unwrap();
    assert_eq!(notes.len(), 1, "付箋が復元されること");
    assert_eq!(notes[0].memo, "ここ重要");
}

/// 古いバックアップ（新しい列を含まない）を復元しても、既存のローカル値が
/// NULL で上書きされないこと。
#[test]
fn old_backup_does_not_null_out_newer_columns() {
    let src = thundoku_core::db::test_pool();
    books::insert(&src, &book("b1")).unwrap();
    let mut json: serde_json::Value =
        serde_json::from_str(&backup::export_json(&src, None, None).unwrap()).unwrap();
    // 旧バージョンのバックアップを模す: 後から追加された列を落とす
    for row in json["books"].as_array_mut().unwrap() {
        let obj = row.as_object_mut().unwrap();
        obj.remove("release_date");
        obj.remove("owner_sub");
        obj.remove("is_drm");
    }
    let old_json = serde_json::to_string(&json).unwrap();

    // 復元先には既にローカルの新しい値がある
    let dst = thundoku_core::db::test_pool();
    let mut local = book("b1");
    local.release_date = Some("2020-01-01".into());
    books::insert(&dst, &local).unwrap();
    books::set_owner_sub(
        &dst,
        "b1",
        Some(thundoku_core::owner::encrypt(&[7u8; 32], "sub-1")),
    )
    .unwrap();

    backup::import_json(&dst, &old_json).expect("old backup should import");

    let after = books::get(&dst, "b1").unwrap().unwrap();
    assert_eq!(
        after.release_date.as_deref(),
        Some("2020-01-01"),
        "バックアップに無い列は既存値を保つこと"
    );
    let owner_sub: Option<String> =
        block_on(sqlx::query_scalar("SELECT owner_sub FROM books WHERE id = 'b1'").fetch_one(&dst))
            .unwrap();
    assert!(
        owner_sub.is_some(),
        "バックアップに無い owner_sub が NULL 上書きされないこと"
    );
}

/// 古いバックアップが NOT NULL 列（is_drm 等）を含まなくても、DEFAULT で
/// 補われて復元が失敗しないこと。
#[test]
fn old_backup_missing_not_null_column_still_imports() {
    let src = thundoku_core::db::test_pool();
    books::insert(&src, &book("b1")).unwrap();
    let mut json: serde_json::Value =
        serde_json::from_str(&backup::export_json(&src, None, None).unwrap()).unwrap();
    for row in json["books"].as_array_mut().unwrap() {
        row.as_object_mut().unwrap().remove("is_drm");
    }
    let old_json = serde_json::to_string(&json).unwrap();

    let dst = thundoku_core::db::test_pool();
    backup::import_json(&dst, &old_json).expect("old backup should import with DEFAULT");
    let after = books::get(&dst, "b1").unwrap().unwrap();
    assert_eq!(after.is_drm, 0, "DEFAULT 値で復元されること");
}

/// 途中で失敗した復元は全体が巻き戻り、部分復元が残らないこと。
#[test]
fn failed_restore_rolls_back_completely() {
    let src = seed_source_pool();
    let mut json: serde_json::Value =
        serde_json::from_str(&backup::export_json(&src, None, None).unwrap()).unwrap();
    // 存在しないサイトを参照させて FK 違反を起こす（books の後で失敗する）
    for row in json["bookshelf_items"].as_array_mut().unwrap() {
        row.as_object_mut()
            .unwrap()
            .insert("site_id".into(), serde_json::Value::String("nope".into()));
    }
    let broken = serde_json::to_string(&json).unwrap();

    let dst = thundoku_core::db::test_pool();
    let err = backup::import_json(&dst, &broken);
    assert!(err.is_err(), "FK 違反で復元は失敗すること");

    // books は bookshelf_items より先に処理されるが、巻き戻されるので 0 件のはず
    let books_count: i64 =
        block_on(sqlx::query_scalar("SELECT COUNT(*) FROM books").fetch_one(&dst)).unwrap();
    assert_eq!(books_count, 0, "部分復元が残らないこと（ロールバック）");
}

/// 別端末で同じページに付いた付箋（id が異なる）を復元しても、自然キー
/// `UNIQUE(book_id, content_id, page)` 違反で復元全体が失敗しないこと。
///
/// 付箋の同一性はアプリ自身も `ON CONFLICT(book_id, content_id, page)` で
/// 扱っている（db/notes.rs）。復元が id だけで競合判定すると、同じページの
/// 付箋を二重登録しようとして UNIQUE 違反 → トランザクション全体がロールバックする。
#[test]
fn restore_merges_page_notes_that_differ_only_by_id() {
    let src = thundoku_core::db::test_pool();
    books::insert(&src, &book("b1")).unwrap();
    thundoku_core::db::notes::upsert(
        &src,
        &thundoku_core::db::notes::PageNoteInput {
            id: "note-from-device-a",
            book_id: "b1",
            content_id: "",
            page: 3,
            memo: "端末Aのメモ",
            spread_side: None,
        },
    )
    .unwrap();
    let json = backup::export_json(&src, None, None).unwrap();

    // 復元先には同じページの付箋が別 id で存在する
    let dst = thundoku_core::db::test_pool();
    books::insert(&dst, &book("b1")).unwrap();
    thundoku_core::db::notes::upsert(
        &dst,
        &thundoku_core::db::notes::PageNoteInput {
            id: "note-from-device-b",
            book_id: "b1",
            content_id: "",
            page: 3,
            memo: "端末Bのメモ",
            spread_side: None,
        },
    )
    .unwrap();

    backup::import_json(&dst, &json).expect("restore should merge page notes by natural key");

    let notes = thundoku_core::db::notes::list_newest_first(&dst).unwrap();
    assert_eq!(notes.len(), 1, "同じページの付箋は 1 件に統合されること");
    assert_eq!(
        notes[0].memo, "端末Aのメモ",
        "Drive 側の内容が優先されること"
    );
}
