//! `book_contents` / `content_formats` repository.
//!
//! - コンテンツ = **読む単位**（本文・別冊・オマケ等。`docs/import-patterns.md` §3.3）
//! - レンディション = 同じ内容の**別形式・別バリアント**（`PDF版` / `画像版`、
//!   `文字あり` / `文字なし`、`MP3/WAV × SEあり/なし`）
//!
//! `document_images.content_id` / `format_id` がここを参照する。表紙・裏表紙は
//! コンテンツにしない（決定 D4）。

use crate::db::SqlitePool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct BookContent {
    pub content_id: String,
    pub book_id: String,
    pub display_name: String,
    /// `image` / `pdf` / `epub` / `audio` / `video`
    pub media_kind: String,
    /// 既定で表示するコンテンツかどうか（0 / 1）。
    pub is_primary: i64,
    pub sort_order: i64,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ContentFormat {
    pub format_id: String,
    pub content_id: String,
    /// 切替 UI に出す名前（`画像` / `PDF` など）。
    pub label: String,
    /// `image` / `pdf` / `epub` / `audio` / `video`
    pub format_kind: String,
    pub page_count: i64,
    /// pack 内でこの形式のページが置かれる接頭辞（例 `pages` / `contents/1/r0`）。
    pub pack_entry_prefix: Option<String>,
    pub sort_order: i64,
    pub created_at: String,
}

/// コンテンツとレンディションを一括 INSERT する。
pub fn insert_batch(
    pool: &SqlitePool,
    contents: &[BookContent],
    formats: &[ContentFormat],
) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        let mut tx = pool.begin().await?;
        for content in contents {
            sqlx::query(
                "INSERT INTO book_contents (content_id, book_id, display_name, media_kind, \
                 is_primary, sort_order, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .bind(&content.content_id)
            .bind(&content.book_id)
            .bind(&content.display_name)
            .bind(&content.media_kind)
            .bind(content.is_primary)
            .bind(content.sort_order)
            .bind(&content.created_at)
            .execute(&mut *tx)
            .await?;
        }
        for format in formats {
            sqlx::query(
                "INSERT INTO content_formats (format_id, content_id, label, format_kind, \
                 page_count, pack_entry_prefix, sort_order, created_at) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            )
            .bind(&format.format_id)
            .bind(&format.content_id)
            .bind(&format.label)
            .bind(&format.format_kind)
            .bind(format.page_count)
            .bind(&format.pack_entry_prefix)
            .bind(format.sort_order)
            .bind(&format.created_at)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    })
}

/// 本に紐づくコンテンツを表示順で返す。
pub fn list_for_book(pool: &SqlitePool, book_id: &str) -> Result<Vec<BookContent>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, BookContent>(
            "SELECT content_id, book_id, display_name, media_kind, is_primary, sort_order, \
             created_at FROM book_contents WHERE book_id = ?1 ORDER BY sort_order, content_id",
        )
        .bind(book_id)
        .fetch_all(pool)
        .await
    })
}

/// 既定表示のコンテンツ（無ければ先頭）。
pub fn primary_for_book(
    pool: &SqlitePool,
    book_id: &str,
) -> Result<Option<BookContent>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, BookContent>(
            "SELECT content_id, book_id, display_name, media_kind, is_primary, sort_order, \
             created_at FROM book_contents WHERE book_id = ?1 \
             ORDER BY is_primary DESC, sort_order, content_id LIMIT 1",
        )
        .bind(book_id)
        .fetch_optional(pool)
        .await
    })
}

/// コンテンツのレンディション一覧（表示順）。
pub fn formats_for_content(
    pool: &SqlitePool,
    content_id: &str,
) -> Result<Vec<ContentFormat>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, ContentFormat>(
            "SELECT format_id, content_id, label, format_kind, page_count, pack_entry_prefix, \
             sort_order, created_at FROM content_formats WHERE content_id = ?1 \
             ORDER BY sort_order, format_id",
        )
        .bind(content_id)
        .fetch_all(pool)
        .await
    })
}

/// コンテンツとレンディションをまとめて返す（UI のページ一覧用）。
pub fn list_with_formats(
    pool: &SqlitePool,
    book_id: &str,
) -> Result<Vec<(BookContent, Vec<ContentFormat>)>, sqlx::Error> {
    let contents = list_for_book(pool, book_id)?;
    let mut out = Vec::with_capacity(contents.len());
    for content in contents {
        let formats = formats_for_content(pool, &content.content_id)?;
        out.push((content, formats));
    }
    Ok(out)
}

/// 既定表示コンテンツを付け替える（`is_primary` は常に 1 本だけ）。
/// ページ数の再計算や読了の再評価（§8.3）はフェーズ5で対応する。
pub fn set_primary(pool: &SqlitePool, book_id: &str, content_id: &str) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        let mut tx = pool.begin().await?;
        sqlx::query("UPDATE book_contents SET is_primary = 0 WHERE book_id = ?1")
            .bind(book_id)
            .execute(&mut *tx)
            .await?;
        sqlx::query(
            "UPDATE book_contents SET is_primary = 1 WHERE book_id = ?1 AND content_id = ?2",
        )
        .bind(book_id)
        .bind(content_id)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    })
}

/// ファイル名や拡張子から画像の表示名（`JPEG` / `PNG` …）を推定する。
/// 実データ（FANZA 290 件）では画像セットは JPEG が主流。
pub fn image_label_for_extension(extension: &str) -> Option<&'static str> {
    match extension.to_lowercase().as_str() {
        "jpg" | "jpeg" => Some("JPEG"),
        "png" => Some("PNG"),
        "webp" => Some("WEBP"),
        "gif" => Some("GIF"),
        "bmp" => Some("BMP"),
        "tif" | "tiff" => Some("TIFF"),
        _ => None,
    }
}

/// 旧ラベル（`画像` / `PDF` / `EPUB`）を実データに合わせて書き換えるデータ移行。
///
/// フェーズ2以前の取り込みでは `content_formats.label` が種別名のままだった。
/// Pack には元の拡張子が残らない（ページは webp 化されている）ため、
/// - PDF / EPUB: 本のファイル名（単体取り込み）→ コンテンツ名 + 拡張子 の順で推定
/// - 画像: 本のファイル名に画像拡張子があればそれ、無ければ `JPEG` とみなす
///
/// 対象は旧ラベルの行だけなので、繰り返し実行しても何もしない（冪等）。
pub fn run_legacy_label_migration(pool: &SqlitePool) -> Result<u64, sqlx::Error> {
    crate::db::block_on(async {
        let mut conn = pool.acquire().await?;
        migrate_legacy_labels(&mut conn).await
    })
}

/// `run_legacy_label_migration` の本体（`migrate()` から同じ接続で呼ぶ）。
pub(crate) async fn migrate_legacy_labels(
    conn: &mut sqlx::SqliteConnection,
) -> Result<u64, sqlx::Error> {
    let rows: Vec<(String, String, String, String)> = sqlx::query_as(
        "SELECT f.format_id, f.format_kind, c.display_name, b.file_name \
         FROM content_formats f \
         JOIN book_contents c ON c.content_id = f.content_id \
         JOIN books b ON b.id = c.book_id \
         WHERE f.label IN ('画像', 'PDF', 'EPUB')",
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut updated = 0;
    for (format_id, format_kind, display_name, book_file_name) in rows {
        let Some(label) = legacy_label(&format_kind, &display_name, &book_file_name) else {
            continue;
        };
        sqlx::query("UPDATE content_formats SET label = ?1 WHERE format_id = ?2")
            .bind(&label)
            .bind(&format_id)
            .execute(&mut *conn)
            .await?;
        updated += 1;
    }
    Ok(updated)
}

/// 旧ラベルから新しい表示名を推定する。対象外の種別は `None`。
fn legacy_label(format_kind: &str, display_name: &str, book_file_name: &str) -> Option<String> {
    match format_kind {
        "pdf" | "epub" => {
            let suffix = format!(".{format_kind}");
            if book_file_name.to_lowercase().ends_with(&suffix) {
                Some(book_file_name.to_string())
            } else if display_name.to_lowercase().ends_with(&suffix) {
                Some(display_name.to_string())
            } else {
                Some(format!("{display_name}{suffix}"))
            }
        }
        "image" => {
            let from_book = book_file_name
                .rsplit_once('.')
                .and_then(|(_, extension)| image_label_for_extension(extension));
            Some(from_book.unwrap_or("JPEG").to_string())
        }
        _ => None,
    }
}

/// コンテンツとレンディションを削除する（再取り込み用）。
/// 子（`content_formats`）→ 親（`book_contents`）の順で消す。
pub fn delete_for_book(pool: &SqlitePool, book_id: &str) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "DELETE FROM content_formats WHERE content_id IN \
             (SELECT content_id FROM book_contents WHERE book_id = ?1)",
        )
        .bind(book_id)
        .execute(pool)
        .await?;
        sqlx::query("DELETE FROM book_contents WHERE book_id = ?1")
            .bind(book_id)
            .execute(pool)
            .await?;
        Ok(())
    })
}
