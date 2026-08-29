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
    "bookshelf_items",
    "checked_items",
    "tbf_events",
    "reading_progress",
    "book_tags",
    "favorite_tags",
    "imported_documents",
    "document_images",
    "book_first_events",
    "zenn_tag_metadata",
];

/// 画像・バイナリとして除外するカラム名。
const EXCLUDED_COLUMNS: &[&str] = &["thumbnail_data", "image_data"];

/// 主要テーブルを JSON 文字列にエクスポートする。
#[allow(clippy::explicit_auto_deref)]
pub fn export_json(pool: &SqlitePool) -> Result<String, sqlx::Error> {
    crate::db::block_on(async {
        let mut conn = pool.acquire().await?;
        let mut payload = Map::new();
        for table in TABLES {
            let rows = table_rows(&mut *conn, table).await?;
            payload.insert((*table).to_string(), Value::Array(rows));
        }
        Ok(serde_json::to_string(&Value::Object(payload)).unwrap_or_default())
    })
}

#[allow(clippy::explicit_auto_deref)]
async fn table_rows(
    conn: &mut sqlx::SqliteConnection,
    table: &str,
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
    let rows = sqlx::query(&format!("SELECT {col_list} FROM {table}"))
        .fetch_all(&mut *conn)
        .await?;
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

        let json = export_json(&pool).unwrap();
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
        assert_eq!(items[0]["product_title"], "本");
    }
}
