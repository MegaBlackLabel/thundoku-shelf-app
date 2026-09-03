//! Storage/DB tests: verbatim schema migration + repository CRUD.

use thundoku_core::db::{books, bookshelf, checklist, progress, settings, sync_state, tags};

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
            "book_first_events",
            "book_tags",
            "books",
            "bookshelf_items",
            "checked_items",
            "document_images",
            "document_text",
            "drive_sync_state",
            "favorite_tags",
            "imported_documents",
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
    assert_eq!(count, 2);
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
fn bookshelf_upsert_replaces_existing_row() {
    let pool = memory_db();
    let item = |title: &str| bookshelf::BookshelfItem {
        site_id: "techbookfest".into(),
        database_id: "db-1".into(),
        title: title.into(),
        circle_name: "circle".into(),
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
    };
    bookshelf::upsert(&pool, &item("古いタイトル")).unwrap();
    bookshelf::upsert(&pool, &item("新しいタイトル")).unwrap();
    let list = bookshelf::list(&pool, "techbookfest").unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(list[0].title, "新しいタイトル");
}

#[test]
fn bookshelf_list_orders_by_caused_at_desc() {
    let pool = memory_db();
    let item = |title: &str, caused_at: Option<&str>, database_id: &str| bookshelf::BookshelfItem {
        site_id: "techbookfest".into(),
        database_id: database_id.into(),
        title: title.into(),
        circle_name: "circle".into(),
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
