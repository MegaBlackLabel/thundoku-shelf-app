//! Regression tests: syncing bookshelf / checklist items with event slugs
//! that do not yet exist in `tbf_events` must not raise FK constraint errors.
//! (The Web resolves the event first; the desktop now ensures the row.)

use thundoku_core::db;
use thundoku_core::tbf::sync::{save_bookshelf, save_checklist};
use thundoku_core::tbf::{TbfChecklistEntry, TbfShelfItem};

fn db() -> thundoku_core::db::SqlitePool {
    thundoku_core::db::test_pool()
}

#[test]
fn bookshelf_sync_with_unknown_event_slug_does_not_violate_fk() {
    let pool = db();
    let items = vec![TbfShelfItem {
        id: "db-1".into(),
        title: "React 本".into(),
        circle_name: "サークルA".into(),
        thumbnail_url: None,
        format: "PDF".into(),
        caused_at: Some("2026-04-12T09:16:36.410Z".into()),
        event_name: Some("技術書典21".into()),
        event_slug: Some("tbf21".into()),
        file_name: None,
        download_url: None,
        is_downloadable: true,
        tags: None,
    }];

    // tbf_events contains only canonical events (tbf1..tbf20) — tbf21 absent.
    let count = save_bookshelf(&pool, &items).unwrap();
    assert_eq!(count, 1);

    // bookshelf row persisted with the FK target auto-created
    let rows = db::bookshelf::list(&pool, "techbookfest").unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].event_id.as_deref(), Some("tbf21"));

    // the event row was created so the FK holds
    let events = db::checklist::list_events(&pool).unwrap();
    assert!(events.iter().any(|e| e.id == "tbf21"));
}

#[test]
fn checklist_sync_with_unknown_event_slug_does_not_violate_fk() {
    let pool = db();
    let entries = vec![TbfChecklistEntry {
        id: "tbf22:p1".into(),
        circle_name: "サークルB".into(),
        space_number: "あ-01".into(),
        tbf_circle_id: Some("c1".into()),
        product_id: Some("p1".into()),
        product_title: "本".into(),
        thumbnail_url: None,
        price: Some(1000),
        is_purchased: true,
        created_at: Some("2026-04-12T09:16:36.410Z".into()),
    }];

    // event tbf22 not yet in tbf_events.
    let count = save_checklist(&pool, "tbf22", &entries).unwrap();
    assert_eq!(count, 1);

    let items = db::checklist::list_items(&pool, "tbf22").unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].event_id, "tbf22");

    // FK target row was created
    let events = db::checklist::list_events(&pool).unwrap();
    assert!(events.iter().any(|e| e.id == "tbf22"));
}

#[test]
fn bookshelf_sync_keeps_existing_event_row() {
    let pool = db();
    // pre-insert a tbf_events row with a custom display_order
    db::checklist::upsert_event(
        &pool,
        &db::checklist::TbfEvent {
            id: "tbf20".into(),
            site_id: "techbookfest".into(),
            slug: Some("tbf20".into()),
            tbf_event_id: Some("Event:tbf20".into()),
            event_name: "技術書典20".into(),
            event_date: None,
            event_start_date: Some("2026-04-11".into()),
            event_end_date: None,
            event_format: "offline".into(),
            is_cancelled: 0,
            display_order: 5,
            is_featured: 1,
            created_at: "2026-01-01 00:00:00".into(),
            updated_at: "2026-01-01 00:00:00".into(),
        },
    )
    .unwrap();

    let items = vec![TbfShelfItem {
        id: "db-2".into(),
        title: "t".into(),
        circle_name: "c".into(),
        thumbnail_url: None,
        format: "PDF".into(),
        caused_at: None,
        event_name: Some("技術書典20".into()),
        event_slug: Some("tbf20".into()),
        file_name: None,
        download_url: None,
        is_downloadable: false,
        tags: None,
    }];
    save_bookshelf(&pool, &items).unwrap();

    let event = db::checklist::list_events(&pool)
        .unwrap()
        .into_iter()
        .find(|e| e.id == "tbf20")
        .unwrap();
    // existing row untouched (display_order preserved, no overwrite)
    assert_eq!(event.display_order, 5);
    assert_eq!(event.is_featured, 1);
}
