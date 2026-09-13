//! バックアップ用の JSON エクスポート。
//!
//! DB ファイル全体（画像 base64 を含み 200MB 級になる）をそのまま
//! Drive に上げるのは重いため、主要テーブルのテキストデータだけを
//! JSON にまとめる。画像データ（`thumbnail_data` / `image_data`）と
//! 環境依存の設定（`drive.*` / `api.last_sync_at` 等）は含めない:
//! サイズを抑え、同期のたびに内容が変わって毎回アップロードされるのを防ぐ。

use serde_json::{Map, Number, Value};
use sqlx::{Row, TypeInfo, ValueRef};

use crate::db::SqlitePool;

/// エクスポート対象のテーブル（テキストデータのみ）。
const TABLES: &[&str] = &[
    "books",
    // tbf_events は bookshelf_items.event_id / checked_items.event_id から
    // 参照されるため、必ず参照元より先に INSERT する（FK 順序）。
    "tbf_events",
    "bookshelf_items",
    "checked_items",
    "book_contents",
    "content_formats",
    "reading_progress",
    "page_views",
    "book_tags",
    "favorite_tags",
    "favorite_entities",
    "imported_documents",
    "document_images",
    "book_first_events",
    // 付箋メモ（ユーザーが書いたデータ）。ページ本文や解析結果と違い
    // 復元できないため、バックアップ対象に含める。
    "page_notes",
    "zenn_tag_metadata",
    "view_history",
];
/// 画像・バイナリとして除外するカラム名。
const EXCLUDED_COLUMNS: &[&str] = &["thumbnail_data", "image_data"];

/// 主要テーブルを JSON 文字列にエクスポートする。
/// `book_ids` が `Some(ids)` のとき、所有者（本の id 集合）に連動して
/// `books` とその下位テーブルだけをエクスポートする（P3）。`None` は全件。
#[allow(clippy::explicit_auto_deref)]
pub fn export_json(
    pool: &SqlitePool,
    book_ids: Option<&std::collections::HashSet<String>>,
) -> Result<String, sqlx::Error> {
    crate::db::block_on(async {
        // 複数テーブルを跨いで読み出すため、トランザクションで一貫した
        // スナップショットから取り出す（並行書き込みでテーブル間が
        // 食い違ったバックアップを作らない）。
        let mut tx = pool.begin().await?;
        let mut payload = Map::new();
        for table in TABLES {
            let rows = table_rows(&mut *tx, table, book_ids).await?;
            payload.insert((*table).to_string(), Value::Array(rows));
        }
        tx.commit().await?;
        // 直列化に失敗したら空文字を返さない（空バックアップで Drive 上の
        // 既存バックアップを置き換えてしまう事故を防ぐ）。
        serde_json::to_string(&Value::Object(payload))
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))
    })
}

/// `export_json` で書き出したバックアップを DB に反映する。
///
/// 各テーブルを PK 競合時に `DO UPDATE` する UPSERT でマージする（Drive 優先）。
/// SQLite の `INSERT ... ON CONFLICT DO UPDATE` は DELETE を伴わないため、
/// FK の `ON DELETE CASCADE`（例: `books` → `view_history`）を発火させない。
/// テーブルは `TABLES` の順（FK 参照元が先）で処理する。
pub fn import_json(pool: &SqlitePool, json: &str) -> Result<(), sqlx::Error> {
    let payload: serde_json::Value =
        serde_json::from_str(json).map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
    crate::db::block_on(async {
        // 1 行でも失敗したら全て巻き戻す（部分復元を残さない）。
        let mut tx = pool.begin().await?;
        for table in TABLES {
            let Some(rows) = payload.get(*table).and_then(serde_json::Value::as_array) else {
                continue; // バックアップに無いテーブルはスキップ
            };
            let Some(pk) = pk_columns(table) else {
                log::warn!("drive restore: no PK mapping for {table}, skipping");
                continue;
            };
            // 競合判定は自然キーがあればそちらを使う（PK と別の UNIQUE 制約を
            // 持つ表で、もう片方の制約違反により復元が失敗するのを防ぐ）。
            let conflict = conflict_columns(table).unwrap_or(pk);
            upsert_rows(&mut tx, table, pk, conflict, rows).await?;
        }
        tx.commit().await?;
        Ok(())
    })
}

/// UPSERT の競合判定に使う列（既定は PRIMARY KEY）。
///
/// `PRIMARY KEY` とは別に自然キーの UNIQUE 制約を持つテーブルは、そちらを
/// 同一性として扱う。片方だけで競合判定すると、もう片方の UNIQUE 違反で
/// INSERT が失敗し（トランザクションのため復元全体がロールバックする）、
/// 復元できなくなる。
fn conflict_columns(table: &str) -> Option<&'static [&'static str]> {
    match table {
        // 付箋の同一性は (book, content, page)。id は端末ごとに生成され得るため
        // （アプリ自身の upsert も `ON CONFLICT(book_id, content_id, page)` を使う）、
        // id を競合判定に使うと別端末の同じ付箋を二重登録しようとして失敗する。
        "page_notes" => Some(&["book_id", "content_id", "page"]),
        other => pk_columns(other),
    }
}

/// テーブルごとの PRIMARY KEY カラム（`TABLES` の定義と一致させる）。
fn pk_columns(table: &str) -> Option<&'static [&'static str]> {
    Some(match table {
        "books" => &["id"],
        "bookshelf_items" => &["site_id", "database_id"],
        "checked_items" => &["id"],
        "tbf_events" => &["id"],
        "book_contents" => &["content_id"],
        "content_formats" => &["format_id"],
        "reading_progress" => &["book_id", "content_id"],
        "page_views" => &["book_id", "content_id", "page_number"],
        "book_tags" => &["id"],
        "favorite_tags" => &["tag_name"],
        "favorite_entities" => &["entity_kind", "entity_name"],
        "imported_documents" => &["id"],
        "document_images" => &["id"],
        "book_first_events" => &["site_id", "database_id"],
        "page_notes" => &["id"],
        "zenn_tag_metadata" => &["tag_name"],
        "view_history" => &["id"],
        _ => return None,
    })
}

#[allow(clippy::explicit_auto_deref)]
async fn table_rows(
    conn: &mut sqlx::SqliteConnection,
    table: &str,
    book_ids: Option<&std::collections::HashSet<String>>,
) -> Result<Vec<Value>, sqlx::Error> {
    // 画像（blob）カラムは SELECT から除外して読み込みコストを削る
    let cols: Vec<String> = {
        let info = sqlx::query(&format!("PRAGMA table_info({table})"))
            .fetch_all(&mut *conn)
            .await?;
        let mut names = Vec::new();
        for row in info {
            let name: String = row.get(1);
            if !EXCLUDED_COLUMNS.contains(&name.as_str()) {
                names.push(name);
            }
        }
        names
    };
    let col_list = cols.join(", ");
    // 所有者（本の id 集合）に連動して下位テーブルを絞る（P3）。json_each で IN を組む。
    let mut where_sql = String::new();
    let mut bind_json: Option<String> = None;
    if let Some(ids) = book_ids {
        let json = serde_json::to_string(ids).unwrap_or_else(|_| "[]".into());
        match table {
            "document_images" => {
                where_sql = " WHERE document_id IN (SELECT id FROM imported_documents \
                             WHERE book_id IN (SELECT value FROM json_each(?)))"
                    .into();
                bind_json = Some(json);
            }
            "books" => {
                where_sql = " WHERE id IN (SELECT value FROM json_each(?))".into();
                bind_json = Some(json);
            }
            "reading_progress" | "page_views" | "book_tags" | "view_history"
            | "imported_documents" | "book_contents" | "page_notes" => {
                where_sql = " WHERE book_id IN (SELECT value FROM json_each(?))".into();
                bind_json = Some(json);
            }
            "content_formats" => {
                where_sql = " WHERE content_id IN (SELECT content_id FROM book_contents \
                             WHERE book_id IN (SELECT value FROM json_each(?)))"
                    .into();
                bind_json = Some(json);
            }
            _ => {}
        }
    }
    let sql = format!("SELECT {col_list} FROM {table}{where_sql}");
    let mut query = sqlx::query(&sql);
    if let Some(json) = &bind_json {
        query = query.bind(json);
    }
    let rows = query.fetch_all(&mut *conn).await?;
    let mut out = Vec::new();
    for row in rows {
        let mut obj = Map::new();
        for (index, column) in cols.iter().enumerate() {
            let raw = row.try_get_raw(index)?;
            // NULL はストレージクラスが NULL（declared type は TEXT 等に
            // フォールバックされる）なので、先に is_null で判定する。
            // それ以外は値のストレージクラス（TEXT / INTEGER / REAL / BLOB）
            // で rusqlite の ValueRef 相当に振り分ける。BLOB は null 扱い。
            let value = if raw.is_null() {
                Value::Null
            } else {
                match raw.type_info().name() {
                    "TEXT" => Value::String(row.try_get::<String, _>(index)?),
                    "INTEGER" => Value::Number(Number::from(row.try_get::<i64, _>(index)?)),
                    "REAL" => Number::from_f64(row.try_get::<f64, _>(index)?)
                        .map(Value::Number)
                        .unwrap_or(Value::Null),
                    _ => Value::Null,
                }
            };
            obj.insert(column.clone(), value);
        }
        out.push(Value::Object(obj));
    }
    Ok(out)
}

async fn upsert_rows(
    conn: &mut sqlx::SqliteConnection,
    table: &str,
    pk: &[&str],
    conflict_cols: &[&str],
    rows: &[serde_json::Value],
) -> Result<(), sqlx::Error> {
    // 画像（blob）カラムは対象外（エクスポート時と同じ除外リスト）
    let table_cols: Vec<String> = {
        let info = sqlx::query(&format!("PRAGMA table_info({table})"))
            .fetch_all(&mut *conn)
            .await?;
        let mut names = Vec::new();
        for row in info {
            let name: String = row.get(1);
            if !EXCLUDED_COLUMNS.contains(&name.as_str()) {
                names.push(name);
            }
        }
        names
    };
    if table_cols.is_empty() {
        return Ok(());
    }
    // 競合判定に使う列は UPDATE 対象から外す（同一性そのものなので書き換えない）
    let conflict_set: std::collections::HashSet<&str> = conflict_cols.iter().copied().collect();
    let conflict = conflict_cols.join(", ");
    for row in rows {
        let Some(obj) = row.as_object() else {
            continue;
        };
        // PK / 競合判定列が欠けている行は INSERT しない（NULL を作らない）
        if let Some(missing) = pk
            .iter()
            .chain(conflict_cols.iter())
            .find(|k| !obj.contains_key(**k))
        {
            log::warn!("drive restore: {table} の行に必須列 {missing} が無いためスキップ");
            continue;
        }
        // バックアップに含まれる列だけを書き込み対象にする。バックアップに
        // 無い列を NULL で書くと、既存値の消失（NULL 上書き）や NOT NULL
        // 制約違反になるため、欠落列はスキーマの DEFAULT に任せる。
        let cols: Vec<&str> = table_cols
            .iter()
            .map(String::as_str)
            .filter(|c| obj.contains_key(*c))
            .collect();
        if cols.is_empty() {
            continue;
        }
        let update_cols: Vec<&str> = cols
            .iter()
            .copied()
            .filter(|c| !conflict_set.contains(c))
            .collect();
        let col_list = cols.join(", ");
        let placeholders = cols
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = if update_cols.is_empty() {
            format!(
                "INSERT INTO {table} ({col_list}) VALUES ({placeholders}) \
                 ON CONFLICT ({conflict}) DO NOTHING"
            )
        } else {
            let update_set = update_cols
                .iter()
                .map(|c| format!("{c} = excluded.{c}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "INSERT INTO {table} ({col_list}) VALUES ({placeholders}) \
                 ON CONFLICT ({conflict}) DO UPDATE SET {update_set}"
            )
        };
        let mut query = sqlx::query(&sql);
        for col in &cols {
            let v = obj.get(*col).cloned().unwrap_or(Value::Null);
            match v {
                Value::Null => {
                    query = query.bind(None::<String>);
                }
                Value::String(s) => {
                    query = query.bind(s);
                }
                Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        query = query.bind(i);
                    } else {
                        query = query.bind(n.as_f64().unwrap_or(0.0));
                    }
                }
                Value::Bool(b) => {
                    query = query.bind(b as i64);
                }
                _ => {
                    query = query.bind(None::<String>);
                }
            }
        }
        query.execute(&mut *conn).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_json_contains_tables_without_images() {
        let pool = crate::db::test_pool();
        // 本とチェックリスト項目を 1 件ずつ入れる
        crate::db::books::insert(
            &pool,
            &crate::db::books::Book {
                id: "book-1".into(),
                title: "テスト本".into(),
                author: "著者".into(),
                circle_name: "サークル".into(),
                purchase_date: None,
                file_name: "book-1.pdf".into(),
                file_size: 10,
                opfs_path: "book-1.opfspack".into(),
                cover_thumbnail: None,
                tbf_product_id: None,
                site_id: None,
                tags_fetched: 0,
                pack_id: Some("book-1".into()),
                is_favorite: 0,
                is_hidden: 0,
                created_at: "2026-08-23 00:00:00".into(),
                updated_at: "2026-08-23 00:00:00".into(),
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
        // checked_items の FK 用にイベントを seed する
        crate::db::checklist::upsert_event(
            &pool,
            &crate::db::checklist::TbfEvent {
                id: "tbf20".into(),
                site_id: "techbookfest".into(),
                slug: Some("tbf20".into()),
                tbf_event_id: Some("Event:tbf20".into()),
                event_name: "技術書典20".into(),
                event_date: Some("2026-04-11".into()),
                event_start_date: Some("2026-04-11".into()),
                event_end_date: Some("2026-04-26".into()),
                event_format: "hybrid".into(),
                is_cancelled: 0,
                display_order: 0,
                is_featured: 1,
                poll_sync_enabled: 0,
                created_at: "2026-08-23 00:00:00".into(),
                updated_at: "2026-08-23 00:00:00".into(),
            },
        )
        .unwrap();
        crate::db::checklist::upsert_item(
            &pool,
            &crate::db::checklist::CheckedItem {
                id: "tbf20:p1".into(),
                event_id: "tbf20".into(),
                circle_name: "サークルA".into(),
                space_number: "あ-01".into(),
                memo: String::new(),
                is_checked: 0,
                sort_order: 0,
                tbf_circle_id: None,
                product_id: Some("p1".into()),
                product_title: "本".into(),
                thumbnail_url: Some("https://example.com/img.png".into()),
                thumbnail_data: Some("aGVsbG8=".into()),
                price: Some(1000),
                is_purchased: 0,
                sample_fetch_attempted_at: None,
                created_at: "2026-08-23 00:00:00".into(),
            },
        )
        .unwrap();

        let json = export_json(&pool, None).unwrap();
        let payload: Value = serde_json::from_str(&json).unwrap();
        // books テーブルに 1 件
        let books = payload["books"].as_array().unwrap();
        assert_eq!(books.len(), 1);
        assert_eq!(books[0]["title"], "テスト本");
        // checked_items に thumbnail_data が含まれない（画像は除外）
        let items = payload["checked_items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert!(
            items[0].get("thumbnail_data").is_none(),
            "image columns must be excluded"
        );
    }
    #[test]
    fn import_json_round_trips_main_tables_and_updates() {
        use crate::db::progress::ReadingProgress;
        // ソース DB に本・進捗・閲覧履歴を入れてエクスポートする
        let src = crate::db::test_pool();
        crate::db::books::insert(
            &src,
            &crate::db::books::Book {
                id: "book-1".into(),
                title: "テスト本".into(),
                author: "著者".into(),
                circle_name: "サークル".into(),
                purchase_date: None,
                file_name: "book-1.pdf".into(),
                file_size: 10,
                opfs_path: "book-1.opfspack".into(),
                cover_thumbnail: None,
                tbf_product_id: None,
                site_id: None,
                tags_fetched: 0,
                pack_id: Some("book-1".into()),
                is_favorite: 0,
                is_hidden: 0,
                created_at: "2026-08-23 00:00:00".into(),
                updated_at: "2026-08-23 00:00:00".into(),
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
        // フェーズ2: コンテンツ構造もバックアップ／復元の対象
        crate::db::contents::insert_batch(
            &src,
            &[crate::db::contents::BookContent {
                content_id: "c1".into(),
                book_id: "book-1".into(),
                display_name: "本文".into(),
                media_kind: "image".into(),
                is_primary: 1,
                sort_order: 0,
                created_at: "2026-08-23 00:00:00".into(),
            }],
            &[crate::db::contents::ContentFormat {
                format_id: "f1".into(),
                content_id: "c1".into(),
                label: "画像".into(),
                format_kind: "image".into(),
                page_count: 2,
                pack_entry_prefix: Some("pages".into()),
                sort_order: 0,
                created_at: "2026-08-23 00:00:00".into(),
            }],
        )
        .unwrap();
        crate::db::progress::upsert(
            &src,
            &ReadingProgress {
                book_id: "book-1".into(),
                content_id: "c1".into(),
                current_page: 12,
                total_pages: Some(120),
                finished_at: None,
                last_read_at: "2026-08-23 09:00:00".into(),
            },
        )
        .unwrap();
        crate::db::view_history::start(&src, "book-1").unwrap();
        assert_eq!(
            crate::db::view_history::view_count(&src, "book-1").unwrap(),
            1
        );
        // ページ毎閲覧記録を入れる（バックアップ対象に含まれるべき）
        crate::db::page_views::record_view(&src, "book-1", "c1", 1).unwrap();
        crate::db::page_views::record_view(&src, "book-1", "c1", 2).unwrap();
        crate::db::page_views::add_dwell(&src, "book-1", "c1", 1, 3.5).unwrap();
        crate::db::page_views::add_dwell(&src, "book-1", "c1", 2, 1.25).unwrap();

        let json = export_json(&src, None).unwrap();
        // view_history がバックアップに含まれる
        let payload: Value = serde_json::from_str(&json).unwrap();
        let vh = payload["view_history"].as_array().unwrap();
        assert_eq!(vh.len(), 1, "view_history must be in the backup");
        // page_views もバックアップに含まれる
        let pv = payload["page_views"].as_array().unwrap();
        assert_eq!(pv.len(), 2, "page_views must be in the backup");
        let p1 = pv.iter().find(|r| r["page_number"] == 1).unwrap();
        assert_eq!(p1["view_count"], 1);
        assert!((p1["total_seconds"].as_f64().unwrap() - 3.5).abs() < 1e-9);
        // コンテンツ構造もバックアップに含まれる
        assert_eq!(payload["book_contents"].as_array().unwrap().len(), 1);
        assert_eq!(payload["content_formats"].as_array().unwrap().len(), 1);

        // 空の DB にインポートすると本・進捗・閲覧履歴が復元される
        let dst = crate::db::test_pool();
        import_json(&dst, &json).unwrap();
        let restored = crate::db::books::get(&dst, "book-1").unwrap().unwrap();
        assert_eq!(restored.title, "テスト本");
        // コンテンツ構造も復元される（FK 順が正しくないと失敗する）
        let restored_contents = crate::db::contents::list_for_book(&dst, "book-1").unwrap();
        assert_eq!(restored_contents.len(), 1);
        assert_eq!(restored_contents[0].display_name, "本文");
        let restored_formats = crate::db::contents::formats_for_content(&dst, "c1").unwrap();
        assert_eq!(restored_formats.len(), 1);
        assert_eq!(restored_formats[0].page_count, 2);
        let progress = crate::db::progress::get(&dst, "book-1").unwrap().unwrap();
        assert_eq!(progress.current_page, 12);
        assert_eq!(
            crate::db::view_history::view_count(&dst, "book-1").unwrap(),
            1
        );
        // page_views も復元される
        let rows = crate::db::page_views::for_book(&dst, "book-1").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].page_number, 1);
        assert_eq!(rows[0].view_count, 1);
        assert!((rows[0].total_seconds - 3.5).abs() < 1e-9);
        assert!((rows[1].total_seconds - 1.25).abs() < 1e-9);
    }

    #[test]
    fn export_json_filters_by_owner() {
        let pool = crate::db::test_pool();
        crate::db::migrate(&pool).unwrap();
        let key = [17u8; 32];
        let mk = |id: &str| crate::db::books::Book {
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
        crate::db::books::insert(&pool, &mk("book-A")).unwrap();
        crate::db::books::insert(&pool, &mk("book-B")).unwrap();
        crate::db::books::set_owner_sub(&pool, "book-A", Some(crate::owner::encrypt(&key, "A")))
            .unwrap();
        crate::db::books::set_owner_sub(&pool, "book-B", Some(crate::owner::encrypt(&key, "B")))
            .unwrap();
        // 進捗も入れる
        use crate::db::progress::ReadingProgress;
        for (id, page) in [("book-A", 5), ("book-B", 9)] {
            crate::db::progress::upsert(
                &pool,
                &ReadingProgress {
                    book_id: id.into(),
                    content_id: String::new(),
                    current_page: page,
                    total_pages: Some(10),
                    finished_at: None,
                    last_read_at: "2026-01-01 00:00:00".into(),
                },
            )
            .unwrap();
        }

        // A の所有のみ → book-A とその進捗だけ
        let owned_a: std::collections::HashSet<String> = ["book-A".into()].into();
        let json = export_json(&pool, Some(&owned_a)).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        let books = v["books"].as_array().unwrap();
        assert_eq!(books.len(), 1);
        assert_eq!(books[0]["id"], "book-A");
        let prog = v["reading_progress"].as_array().unwrap();
        assert_eq!(prog.len(), 1);
        assert_eq!(prog[0]["book_id"], "book-A");

        // 全件（None）→ 両方
        let json_full = export_json(&pool, None).unwrap();
        let vf: Value = serde_json::from_str(&json_full).unwrap();
        assert_eq!(vf["books"].as_array().unwrap().len(), 2);
        assert_eq!(vf["reading_progress"].as_array().unwrap().len(), 2);
    }
}
