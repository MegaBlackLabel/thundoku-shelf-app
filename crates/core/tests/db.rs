//! Storage/DB tests: verbatim schema migration + repository CRUD.

use thundoku_core::db::{
    books, bookshelf, checklist, contents, progress, settings, sync_state, tags,
};

fn memory_db() -> thundoku_core::db::SqlitePool {
    thundoku_core::db::test_pool()
}

#[test]
fn migrate_creates_all_schema_tables() {
    let pool = memory_db();
    let names: Vec<String> = thundoku_core::db::block_on(async {
        sqlx::query_scalar(
            "SELECT name FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",
        )
        .fetch_all(&pool)
        .await
    })
    .unwrap();
    let mut names: Vec<&str> = names.iter().map(String::as_str).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "_sqlx_migrations",
            "app_settings",
            "book_contents",
            "book_first_events",
            "book_tags",
            "books",
            "bookshelf_items",
            "checked_items",
            "content_formats",
            "document_images",
            "document_text",
            "drive_sync_state",
            "favorite_tags",
            "imported_documents",
            "page_views",
            "product_sample_pages",
            "reading_progress",
            "sites",
            "tbf_events",
            "token_analysis",
            "view_history",
            "zenn_tag_metadata",
        ]
    );
    // techbookfest seed present
    let count: i64 = thundoku_core::db::block_on(async {
        sqlx::query_scalar("SELECT COUNT(*) FROM sites WHERE id = 'techbookfest'")
            .fetch_one(&pool)
            .await
    })
    .unwrap();
    assert_eq!(count, 1);
    // booth seed present
    let count: i64 = thundoku_core::db::block_on(async {
        sqlx::query_scalar("SELECT COUNT(*) FROM sites WHERE id = 'booth'")
            .fetch_one(&pool)
            .await
    })
    .unwrap();
    assert_eq!(count, 1);
}

#[test]
fn migrate_is_idempotent() {
    let pool = thundoku_core::db::test_pool();
    thundoku_core::db::migrate(&pool).unwrap();
    thundoku_core::db::migrate(&pool).unwrap();
    let count: i64 = thundoku_core::db::block_on(async {
        sqlx::query_scalar("SELECT COUNT(*) FROM sites")
            .fetch_one(&pool)
            .await
    })
    .unwrap();
    assert_eq!(count, 4);
}

#[test]
fn settings_roundtrip() {
    let pool = memory_db();
    assert_eq!(settings::get(&pool, "drive.sync.folder_id").unwrap(), None);
    settings::set(&pool, "drive.sync.folder_id", "folder-123").unwrap();
    settings::set(&pool, "drive.sync.enabled", "true").unwrap();
    assert_eq!(
        settings::get(&pool, "drive.sync.folder_id")
            .unwrap()
            .as_deref(),
        Some("folder-123")
    );
    let all = settings::all(&pool).unwrap();
    assert_eq!(
        all.get("drive.sync.enabled").map(String::as_str),
        Some("true")
    );
    settings::delete(&pool, "drive.sync.enabled").unwrap();
    assert_eq!(settings::get(&pool, "drive.sync.enabled").unwrap(), None);
}

#[test]
fn book_insert_get_list_delete() {
    let pool = memory_db();
    let book = books::Book {
        id: "book-1".into(),
        title: "テスト本".into(),
        author: "著者A".into(),
        circle_name: "サークルA".into(),
        purchase_date: Some("2026-08-21".into()),
        file_name: "book.pdf".into(),
        file_size: 12345,
        opfs_path: "book-1.opfspack".into(),
        cover_thumbnail: None,
        tbf_product_id: None,
        site_id: None,
        tags_fetched: 1,
        pack_id: Some("book-1".into()),
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
    };
    books::insert(&pool, &book).unwrap();
    let loaded = books::get(&pool, "book-1").unwrap().unwrap();
    assert_eq!(loaded, book);
    assert_eq!(books::list(&pool).unwrap().len(), 1);
    assert!(books::get(&pool, "nope").unwrap().is_none());
    books::delete(&pool, "book-1").unwrap();
    assert!(books::list(&pool).unwrap().is_empty());
}

#[test]
fn set_tbf_product_id_links_downloaded_book_to_shelf() {
    let pool = memory_db();
    let book = books::Book {
        id: "book-1".into(),
        title: "テスト本".into(),
        author: "著者A".into(),
        circle_name: "サークルA".into(),
        purchase_date: Some("2026-08-21".into()),
        file_name: "book.pdf".into(),
        file_size: 12345,
        opfs_path: "book-1.opfspack".into(),
        cover_thumbnail: None,
        tbf_product_id: None,
        site_id: None,
        tags_fetched: 1,
        pack_id: Some("book-1".into()),
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
    };
    books::insert(&pool, &book).unwrap();
    // ダウンロード完了後に bookshelf_items.database_id との対応を付ける
    books::set_tbf_product_id(&pool, "book-1", "db-1").unwrap();
    let fetched = books::get(&pool, "book-1").unwrap().unwrap();
    assert_eq!(fetched.tbf_product_id.as_deref(), Some("db-1"));
    // 存在しない本はエラーにしない
    books::set_tbf_product_id(&pool, "missing", "db-9").unwrap();
}

#[test]
fn owner_sub_roundtrip() {
    let pool = memory_db();
    let book = books::Book {
        id: "book-1".into(),
        title: "テスト本".into(),
        author: "著者A".into(),
        circle_name: "サークルA".into(),
        purchase_date: None,
        file_name: "book.pdf".into(),
        file_size: 12345,
        opfs_path: "book-1.opfspack".into(),
        cover_thumbnail: None,
        tbf_product_id: None,
        site_id: None,
        tags_fetched: 1,
        pack_id: Some("book-1".into()),
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
    };
    books::insert(&pool, &book).unwrap();
    // 初期状態は NULL（未所属）
    assert_eq!(books::get_owner_sub(&pool, "book-1").unwrap(), None);
    // 暗号化済み blob（不透明な文字列）で所属をセット
    books::set_owner_sub(&pool, "book-1", Some("enc-blob".into())).unwrap();
    assert_eq!(
        books::get_owner_sub(&pool, "book-1").unwrap(),
        Some("enc-blob".into())
    );
    // list でも返る
    assert_eq!(
        books::list_owner_subs(&pool).unwrap(),
        vec![("book-1".to_string(), Some("enc-blob".into()))]
    );
    // NULL に戻す（未所属へ）
    books::set_owner_sub(&pool, "book-1", None).unwrap();
    assert_eq!(books::get_owner_sub(&pool, "book-1").unwrap(), None);
}

#[test]
fn resolve_reuse_id_returns_owned_match_only() {
    let pool = memory_db();
    let key = [5u8; 32];
    let mk = |id: &str| books::Book {
        id: id.into(),
        title: "本".into(),
        author: String::new(),
        circle_name: String::new(),
        purchase_date: None,
        file_name: "f.pdf".into(),
        file_size: 1,
        opfs_path: format!("{id}.opfspack"),
        cover_thumbnail: None,
        tbf_product_id: Some("db-1".into()),
        site_id: Some("techbookfest".into()),
        tags_fetched: 1,
        pack_id: Some(id.into()),
        is_favorite: 0,
        is_hidden: 0,
        created_at: "2026-01-01 00:00:00".into(),
        updated_at: "2026-01-01 00:00:00".into(),
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
    };
    // book-1: sub-A に所属(暗号化済み) / book-2: 未所属(NULL)
    books::insert(&pool, &mk("book-1")).unwrap();
    books::insert(&pool, &mk("book-2")).unwrap();
    books::set_owner_sub(
        &pool,
        "book-1",
        Some(thundoku_core::owner::encrypt(&key, "sub-A")),
    )
    .unwrap();
    // 同一 source + sub-A → book-1 を再利用
    assert_eq!(
        books::resolve_reuse_id(&pool, &key, "techbookfest", "db-1", Some("sub-A")).unwrap(),
        Some("book-1".into())
    );
    // 別の sub → 一致なし（Aの行は再利用せず、NULL行も再利用しない）
    assert_eq!(
        books::resolve_reuse_id(&pool, &key, "techbookfest", "db-1", Some("sub-B")).unwrap(),
        None
    );
    // source 不一致 → None
    assert_eq!(
        books::resolve_reuse_id(&pool, &key, "techbookfest", "db-9", Some("sub-A")).unwrap(),
        None
    );
    // 未ログイン（None）→ 未所属（NULL）の book-2 を再利用
    assert_eq!(
        books::resolve_reuse_id(&pool, &key, "techbookfest", "db-1", None).unwrap(),
        Some("book-2".into())
    );
}

#[test]
fn owned_book_ids_filters_by_owner() {
    let pool = memory_db();
    let key = [3u8; 32];
    let mk = |id: &str| books::Book {
        id: id.into(),
        title: "本".into(),
        author: String::new(),
        circle_name: String::new(),
        purchase_date: None,
        file_name: "f.pdf".into(),
        file_size: 1,
        opfs_path: format!("{id}.opfspack"),
        cover_thumbnail: None,
        tbf_product_id: None,
        site_id: None,
        tags_fetched: 1,
        pack_id: Some(id.into()),
        is_favorite: 0,
        is_hidden: 0,
        created_at: "2026-01-01 00:00:00".into(),
        updated_at: "2026-01-01 00:00:00".into(),
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
    };
    // book-1: A 所属 / book-2: 未所属(NULL) / book-3: B 所属
    books::insert(&pool, &mk("book-1")).unwrap();
    books::insert(&pool, &mk("book-2")).unwrap();
    books::insert(&pool, &mk("book-3")).unwrap();
    books::set_owner_sub(
        &pool,
        "book-1",
        Some(thundoku_core::owner::encrypt(&key, "A")),
    )
    .unwrap();
    books::set_owner_sub(
        &pool,
        "book-3",
        Some(thundoku_core::owner::encrypt(&key, "B")),
    )
    .unwrap();

    // A ログイン中 → A の本だけ
    let as_a = books::owned_book_ids(&pool, &key, Some("A")).unwrap();
    assert!(as_a.contains("book-1"));
    assert!(!as_a.contains("book-2"));
    assert!(!as_a.contains("book-3"));
    // B ログイン中 → B の本だけ
    let as_b = books::owned_book_ids(&pool, &key, Some("B")).unwrap();
    assert!(as_b.contains("book-3") && !as_b.contains("book-1") && !as_b.contains("book-2"));
    // 未ログイン → 未所属(NULL)だけ
    let logged_out = books::owned_book_ids(&pool, &key, None).unwrap();
    assert_eq!(
        logged_out,
        std::collections::HashSet::from(["book-2".to_string()])
    );
}

#[test]
fn clear_owner_model_first_run_wipes_and_sets_flag() {
    let pool = memory_db();
    let packs = std::env::temp_dir().join("thundoku-owner-packs");
    let thumbs = std::env::temp_dir().join("thundoku-owner-thumbs");
    let _ = std::fs::remove_dir_all(&packs);
    let _ = std::fs::remove_dir_all(&thumbs);
    std::fs::create_dir_all(&packs).unwrap();
    std::fs::create_dir_all(&thumbs).unwrap();
    std::fs::write(packs.join("b1.opfspack"), b"data").unwrap();
    let book = books::Book {
        id: "book-1".into(),
        title: "t".into(),
        author: String::new(),
        circle_name: String::new(),
        purchase_date: None,
        file_name: "f.pdf".into(),
        file_size: 1,
        opfs_path: "book-1.opfspack".into(),
        cover_thumbnail: None,
        tbf_product_id: None,
        site_id: None,
        tags_fetched: 1,
        pack_id: Some("book-1".into()),
        is_favorite: 0,
        is_hidden: 0,
        created_at: "2026-01-01 00:00:00".into(),
        updated_at: "2026-01-01 00:00:00".into(),
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
    };
    books::insert(&pool, &book).unwrap();
    // 初回 → クリア & フラグセット
    assert!(thundoku_core::db::clear_owner_model_if_first_run(&pool, &packs, &thumbs).unwrap());
    assert_eq!(books::list(&pool).unwrap().len(), 0);
    assert!(!packs.join("b1.opfspack").exists());
    assert_eq!(
        settings::get(&pool, "owner_sub_model.initialized").unwrap(),
        Some("1".into())
    );
    // 2 回目 → no-op（クリアしない）
    assert!(!thundoku_core::db::clear_owner_model_if_first_run(&pool, &packs, &thumbs).unwrap());
}

#[test]
fn bookshelf_upsert_replaces_existing_row() {
    let pool = memory_db();
    let item = |title: &str| bookshelf::BookshelfItem {
        site_id: "techbookfest".into(),
        database_id: "db-1".into(),
        title: title.into(),
        circle_name: "circle".into(),
        author: String::new(),
        thumbnail_url: None,
        format: "pdf".into(),
        caused_at: None,
        event_name: None,
        event_slug: None,
        event_id: None,
        file_name: None,
        download_url: None,
        is_downloadable: 1,
        is_checked: 0,
        is_purchased: 0,
        is_new: 1,
        is_active: 1,
        is_favorite: 0,
        is_hidden: 0,
        hidden_at: None,
        tags_json: None,
        synced_at: "2026-08-21 00:00:00".into(),
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
    };
    bookshelf::upsert(&pool, &item("古いタイトル")).unwrap();
    bookshelf::upsert(&pool, &item("新しいタイトル")).unwrap();
    let list = bookshelf::list(&pool, "techbookfest").unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "新しいタイトル");
}

#[test]
fn bookshelf_upsert_preserves_author() {
    let pool = memory_db();
    // 技術書典: author 空（非表示）、BOOTH 等: author に作成者名が入る
    let item = |author: &str, site_id: &str, database_id: &str| bookshelf::BookshelfItem {
        site_id: site_id.into(),
        database_id: database_id.into(),
        title: "本".into(),
        circle_name: "circle".into(),
        author: author.into(),
        thumbnail_url: None,
        format: "pdf".into(),
        caused_at: None,
        event_name: None,
        event_slug: None,
        event_id: None,
        file_name: None,
        download_url: None,
        is_downloadable: 1,
        is_checked: 0,
        is_purchased: 1,
        is_new: 0,
        is_active: 1,
        is_favorite: 0,
        is_hidden: 0,
        hidden_at: None,
        tags_json: None,
        synced_at: "2026-08-21 00:00:00".into(),
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
    };
    // BOOTH は shop = 作成者名を author に格納する
    bookshelf::upsert(&pool, &item("YORIMIYA STUDIO", "booth", "b-1")).unwrap();
    // 技術書典は author 空のまま（従来の非表示挙動を壊さない）
    bookshelf::upsert(&pool, &item("", "techbookfest", "db-1")).unwrap();
    let booth = bookshelf::list(&pool, "booth").unwrap();
    assert_eq!(booth[0].author, "YORIMIYA STUDIO");
    let tbf = bookshelf::list(&pool, "techbookfest").unwrap();
    assert_eq!(tbf[0].author, "");
}

#[test]
fn bookshelf_list_orders_by_caused_at_desc() {
    let pool = memory_db();
    let item = |title: &str, caused_at: Option<&str>, database_id: &str| bookshelf::BookshelfItem {
        site_id: "techbookfest".into(),
        database_id: database_id.into(),
        title: title.into(),
        circle_name: "circle".into(),
        author: String::new(),
        thumbnail_url: None,
        format: "pdf".into(),
        caused_at: caused_at.map(String::from),
        event_name: None,
        event_slug: None,
        event_id: None,
        file_name: None,
        download_url: None,
        is_downloadable: 1,
        is_checked: 0,
        is_purchased: 1,
        is_new: 0,
        is_active: 1,
        is_favorite: 0,
        is_hidden: 0,
        hidden_at: None,
        tags_json: None,
        synced_at: "2026-08-21 00:00:00".into(),
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
    };
    bookshelf::upsert(&pool, &item("古い", Some("2026-01-01 00:00:00"), "db-1")).unwrap();
    bookshelf::upsert(&pool, &item("新しい", Some("2026-06-01 00:00:00"), "db-2")).unwrap();
    bookshelf::upsert(&pool, &item("日付なし", None, "db-3")).unwrap();
    let list = bookshelf::list(&pool, "techbookfest").unwrap();
    let titles: Vec<&str> = list.iter().map(|i| i.title.as_str()).collect();
    // Web 版と同一: caused_at 新しい順、null は末尾
    assert_eq!(titles, vec!["新しい", "古い", "日付なし"]);
}

#[test]
fn bookshelf_list_all_returns_every_site() {
    let pool = memory_db();
    let mut tbf = bookshelf::BookshelfItem {
        site_id: "techbookfest".into(),
        database_id: "db-1".into(),
        title: "技術書典の本".into(),
        circle_name: "circle".into(),
        author: String::new(),
        thumbnail_url: None,
        format: "pdf".into(),
        caused_at: Some("2026-01-01 00:00:00".into()),
        event_name: None,
        event_slug: None,
        event_id: None,
        file_name: None,
        download_url: None,
        is_downloadable: 1,
        is_checked: 0,
        is_purchased: 1,
        is_new: 0,
        is_active: 1,
        is_favorite: 0,
        is_hidden: 0,
        hidden_at: None,
        tags_json: None,
        synced_at: "2026-08-21 00:00:00".into(),
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
    };
    bookshelf::upsert(&pool, &tbf).unwrap();
    tbf.site_id = "booth".into();
    tbf.database_id = "booth-1".into();
    tbf.title = "BOOTHの本".into();
    tbf.caused_at = Some("2026-06-01 00:00:00".into());
    bookshelf::upsert(&pool, &tbf).unwrap();
    // サイトフィルタなしで両方取得できる
    let all = bookshelf::list_all(&pool).unwrap();
    assert_eq!(all.len(), 2);
    assert!(
        all.iter()
            .any(|i| i.site_id == "booth" && i.title == "BOOTHの本")
    );
    assert!(
        all.iter()
            .any(|i| i.site_id == "techbookfest" && i.title == "技術書典の本")
    );
    // サイト指定は従来どおり絞り込む
    let tbf_only = bookshelf::list(&pool, "techbookfest").unwrap();
    assert_eq!(tbf_only.len(), 1);
}

#[test]
fn bookshelf_update_tags_writes_tags_json() {
    let pool = memory_db();
    let item = bookshelf::BookshelfItem {
        site_id: "techbookfest".into(),
        database_id: "db-1".into(),
        title: "本".into(),
        circle_name: "circle".into(),
        author: String::new(),
        thumbnail_url: None,
        format: "pdf".into(),
        caused_at: None,
        event_name: None,
        event_slug: None,
        event_id: None,
        file_name: None,
        download_url: None,
        is_downloadable: 1,
        is_checked: 0,
        is_purchased: 1,
        is_new: 0,
        is_active: 1,
        is_favorite: 0,
        is_hidden: 0,
        hidden_at: None,
        tags_json: None,
        synced_at: "2026-08-21 00:00:00".into(),
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
    };
    bookshelf::upsert(&pool, &item).unwrap();
    bookshelf::update_tags(
        &pool,
        "techbookfest",
        "db-1",
        &["react".into(), "rust".into()],
    )
    .unwrap();
    let list = bookshelf::list(&pool, "techbookfest").unwrap();
    assert_eq!(bookshelf::tags_of(&list[0]), vec!["react", "rust"]);
    // 対象外の site_id には書き込まれない
    bookshelf::update_tags(&pool, "other-site", "db-1", &["x".into()]).unwrap();
    let list = bookshelf::list(&pool, "techbookfest").unwrap();
    assert_eq!(bookshelf::tags_of(&list[0]), vec!["react", "rust"]);
}

#[test]
fn checklist_events_items_and_toggle() {
    let pool = memory_db();
    let event = checklist::TbfEvent {
        id: "event-1".into(),
        site_id: "techbookfest".into(),
        slug: Some("tbf18".into()),
        tbf_event_id: Some("tbf-18".into()),
        event_name: "技術書典18".into(),
        event_date: None,
        event_start_date: Some("2026-05-01".into()),
        event_end_date: None,
        event_format: "offline".into(),
        is_cancelled: 0,
        display_order: 0,
        is_featured: 1,
        poll_sync_enabled: 0,
        created_at: "2026-08-21 00:00:00".into(),
        updated_at: "2026-08-21 00:00:00".into(),
    };
    checklist::upsert_event(&pool, &event).unwrap();
    let events = checklist::list_events(&pool).unwrap();
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].event_name, "技術書典18");

    let item = checklist::CheckedItem {
        id: "item-1".into(),
        event_id: "event-1".into(),
        circle_name: "サークルB".into(),
        space_number: "あ-01".into(),
        memo: String::new(),
        is_checked: 0,
        sort_order: 0,
        tbf_circle_id: None,
        product_id: Some("product-1".into()),
        product_title: "見たい本".into(),
        thumbnail_url: None,
        thumbnail_data: None,
        price: Some(1000),
        is_purchased: 0,
        sample_fetch_attempted_at: None,
        created_at: "2026-08-21 00:00:00".into(),
    };
    checklist::upsert_item(&pool, &item).unwrap();
    let items = checklist::list_items(&pool, "event-1").unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].space_number, "あ-01");
    assert_eq!(items[0].is_checked, 0);

    checklist::set_checked(&pool, "item-1", true).unwrap();
    let items = checklist::list_items(&pool, "event-1").unwrap();
    assert_eq!(items[0].is_checked, 1);
}

#[test]
fn reading_progress_roundtrip() {
    let pool = memory_db();
    let book = books::Book {
        id: "book-1".into(),
        title: "t".into(),
        author: String::new(),
        circle_name: String::new(),
        purchase_date: None,
        file_name: "t.pdf".into(),
        file_size: 1,
        opfs_path: "book-1.opfspack".into(),
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
    };
    books::insert(&pool, &book).unwrap();
    let progress = progress::ReadingProgress {
        book_id: "book-1".into(),
        current_page: 7,
        total_pages: Some(42),
        finished_at: None,
        last_read_at: "2026-08-21 01:00:00".into(),
        scroll_position: 0.5,
    };
    progress::upsert(&pool, &progress).unwrap();
    let loaded = progress::get(&pool, "book-1").unwrap().unwrap();
    assert_eq!(loaded, progress);
}

#[test]
fn tags_set_list_and_favorites() {
    let pool = memory_db();
    let book = books::Book {
        id: "book-1".into(),
        title: "t".into(),
        author: String::new(),
        circle_name: String::new(),
        purchase_date: None,
        file_name: "t.pdf".into(),
        file_size: 1,
        opfs_path: "book-1.opfspack".into(),
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
    };
    books::insert(&pool, &book).unwrap();
    tags::set_for_book(
        &pool,
        "book-1",
        &[("react", "generated"), ("nextjs", "manual")],
    )
    .unwrap();
    let list = tags::list_for_book(&pool, "book-1").unwrap();
    let mut names: Vec<&str> = list.iter().map(|t| t.tag_name.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, vec!["nextjs", "react"]);

    tags::set_for_book(&pool, "book-1", &[("rust", "generated")]).unwrap();
    let list = tags::list_for_book(&pool, "book-1").unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].tag_name, "rust");

    tags::set_favorite(&pool, "rust", true).unwrap();
    tags::set_favorite(&pool, "typescript", true).unwrap();
    assert_eq!(
        tags::list_favorites(&pool).unwrap(),
        vec!["rust", "typescript"]
    );
    tags::set_favorite(&pool, "rust", false).unwrap();
    assert_eq!(tags::list_favorites(&pool).unwrap(), vec!["typescript"]);
}

#[test]
fn drive_sync_state_crud() {
    let pool = memory_db();
    let state = sync_state::DriveSyncState {
        pack_id: "pack-1".into(),
        drive_file_id: "file-1".into(),
        md5: "d41d8cd98f00b204e9800998ecf8427e".into(),
        modified_time: Some("2026-08-21T00:00:00.000Z".into()),
        last_synced_at: "2026-08-21 00:00:00".into(),
    };
    sync_state::upsert(&pool, &state).unwrap();
    let loaded = sync_state::get(&pool, "pack-1").unwrap().unwrap();
    assert_eq!(loaded, state);
    let list = sync_state::list(&pool).unwrap();
    assert_eq!(list.len(), 1);
    sync_state::delete(&pool, "pack-1").unwrap();
    assert!(sync_state::get(&pool, "pack-1").unwrap().is_none());
}

fn column_exists(pool: &thundoku_core::db::SqlitePool, table: &str, column: &str) -> bool {
    thundoku_core::db::block_on(async {
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2")
            .bind(table)
            .bind(column)
            .fetch_one(pool)
            .await
    })
    .unwrap()
        > 0
}

// FANZA同人 / DLsite の共有メタ列（media_category / ai_type / is_drm / release_date /
// description / theme / maker_id / page_count / age_rating / series_name）がマイグレーションで
// 追加されること、および両ソースの sites 行が存在することを検証する。
#[test]
fn source_metadata_columns_and_sites_exist() {
    let pool = memory_db();
    let cols = [
        "media_category",
        "ai_type",
        "is_drm",
        "release_date",
        "description",
        "theme",
        "maker_id",
        "page_count",
        "age_rating",
        "series_name",
    ];
    for c in cols {
        assert!(
            column_exists(&pool, "bookshelf_items", c),
            "bookshelf_items.{c} is missing"
        );
        assert!(column_exists(&pool, "books", c), "books.{c} is missing");
    }
    let sites: Vec<String> = thundoku_core::db::block_on(async {
        sqlx::query_scalar("SELECT id FROM sites ORDER BY id")
            .fetch_all(&pool)
            .await
    })
    .unwrap();
    assert!(
        sites.contains(&"fanza".to_string()),
        "site 'fanza' missing: {sites:?}"
    );
    assert!(
        sites.contains(&"dlsite".to_string()),
        "site 'dlsite' missing: {sites:?}"
    );
}

// FANZA同人 / DLsite の共有ソースメタ列が Book に入出力（insert/get/upsert）で
// roundtrip すること、および is_drm のデフォルト（0）を検証する。
#[test]
fn book_metadata_roundtrip() {
    let pool = memory_db();
    let book = books::Book {
        id: "book-meta".into(),
        title: "メタ本".into(),
        author: "作者".into(),
        circle_name: "サークル".into(),
        purchase_date: Some("2026-09-01".into()),
        file_name: "b.zip".into(),
        file_size: 100,
        opfs_path: "book-meta.opfspack".into(),
        cover_thumbnail: None,
        tbf_product_id: Some("RJ00000001".into()),
        site_id: Some("dlsite".into()),
        tags_fetched: 1,
        pack_id: Some("book-meta".into()),
        is_favorite: 0,
        is_hidden: 0,
        created_at: "2026-09-01 00:00:00".into(),
        updated_at: "2026-09-01 00:00:00".into(),
        media_category: Some("comic".into()),
        ai_type: Some("none".into()),
        is_drm: 0,
        release_date: Some("2025-06-17".into()),
        description: Some("あらすじ".into()),
        theme: Some("オリジナル".into()),
        maker_id: Some("RG01048868".into()),
        page_count: Some(40),
        age_rating: Some("全年齢".into()),
        series_name: Some("少年エルフ".into()),
    };
    books::insert(&pool, &book).unwrap();
    assert_eq!(books::get(&pool, "book-meta").unwrap().unwrap(), book);
    // upsert でも roundtrip（id 衝突で update）
    books::upsert(&pool, &book).unwrap();
    assert_eq!(books::get(&pool, "book-meta").unwrap().unwrap(), book);
}

// FANZA同人 / DLsite の共有ソースメタ列が BookshelfItem に入出力（upsert/list）で
// roundtrip することを検証する。
#[test]
fn bookshelf_metadata_roundtrip() {
    let pool = memory_db();
    let item = bookshelf::BookshelfItem {
        site_id: "fanza".into(),
        database_id: "d_123".into(),
        title: "タイトル".into(),
        circle_name: "サークル".into(),
        author: "作者".into(),
        thumbnail_url: Some("https://example.com/thumb.jpg".into()),
        format: "ZIP".into(),
        caused_at: Some("2026-09-01".into()),
        event_name: None,
        event_slug: None,
        event_id: None,
        file_name: Some("d_123.zip".into()),
        download_url: Some("https://example.com/dl".into()),
        is_downloadable: 1,
        is_checked: 0,
        is_purchased: 1,
        is_new: 0,
        is_active: 1,
        is_favorite: 0,
        is_hidden: 0,
        hidden_at: None,
        tags_json: None,
        synced_at: "2026-09-01 00:00:00".into(),
        created_at: "2026-09-01 00:00:00".into(),
        updated_at: "2026-09-01 00:00:00".into(),
        media_category: Some("comic".into()),
        ai_type: Some("none".into()),
        is_drm: 0,
        release_date: Some("2025-06-17".into()),
        description: Some("説明".into()),
        theme: Some("オリジナル".into()),
        maker_id: Some("RG01048868".into()),
        page_count: Some(40),
        age_rating: Some("全年齢".into()),
        series_name: Some("少年エルフ".into()),
    };
    bookshelf::upsert(&pool, &item).unwrap();
    // upsert の upsert でも roundtrip（コンフリクトで更新）
    bookshelf::upsert(&pool, &item).unwrap();
    assert_eq!(bookshelf::list(&pool, "fanza").unwrap(), vec![item]);
}

// フェーズ2: コンテンツ（読む単位）とレンディション（切替可能な表示形態）が
// DB に保存・取得・削除できること。`document_images` が content を指せること。
#[test]
fn book_contents_and_formats_roundtrip() {
    let pool = memory_db();
    let book = books::Book {
        id: "book-c".into(),
        title: "複数コンテンツ本".into(),
        author: String::new(),
        circle_name: String::new(),
        purchase_date: None,
        file_name: "multi.zip".into(),
        file_size: 10,
        opfs_path: "book-c.opfspack".into(),
        cover_thumbnail: None,
        tbf_product_id: None,
        site_id: None,
        tags_fetched: 1,
        pack_id: Some("book-c".into()),
        is_favorite: 0,
        is_hidden: 0,
        created_at: "2026-01-01 00:00:00".into(),
        updated_at: "2026-01-01 00:00:00".into(),
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
    };
    books::insert(&pool, &book).unwrap();

    // document_images が content / format を参照できる列を持つこと
    assert!(column_exists(&pool, "document_images", "content_id"));
    assert!(column_exists(&pool, "document_images", "format_id"));

    let content = contents::BookContent {
        content_id: "c1".into(),
        book_id: "book-c".into(),
        display_name: "本文".into(),
        media_kind: "image".into(),
        is_primary: 1,
        sort_order: 0,
        created_at: "2026-01-01 00:00:00".into(),
    };
    let format = contents::ContentFormat {
        format_id: "f1".into(),
        content_id: "c1".into(),
        label: "画像".into(),
        format_kind: "image".into(),
        page_count: 3,
        pack_entry_prefix: Some("pages".into()),
        sort_order: 0,
        created_at: "2026-01-01 00:00:00".into(),
    };
    contents::insert_batch(&pool, &[content], &[format]).unwrap();

    let loaded = contents::list_for_book(&pool, "book-c").unwrap();
    assert_eq!(loaded.len(), 1);
    assert_eq!(loaded[0].display_name, "本文");
    assert_eq!(loaded[0].is_primary, 1);
    assert_eq!(
        contents::primary_for_book(&pool, "book-c")
            .unwrap()
            .unwrap()
            .content_id,
        "c1"
    );
    let formats = contents::formats_for_content(&pool, "c1").unwrap();
    assert_eq!(formats.len(), 1);
    assert_eq!(formats[0].label, "画像");
    assert_eq!(formats[0].page_count, 3);
    assert_eq!(formats[0].pack_entry_prefix.as_deref(), Some("pages"));

    // 別 book のコンテンツは混ざらない
    assert!(
        contents::list_for_book(&pool, "book-other")
            .unwrap()
            .is_empty()
    );

    contents::delete_for_book(&pool, "book-c").unwrap();
    assert!(contents::list_for_book(&pool, "book-c").unwrap().is_empty());
    assert!(
        contents::formats_for_content(&pool, "c1")
            .unwrap()
            .is_empty()
    );
}
