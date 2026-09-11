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

/// 表示名の最大文字数（1 行に出すため。バイト数ではなく**文字数**）。
pub const MAX_DISPLAY_NAME_CHARS: usize = 60;

/// ユーザーが入力した表示名を整える。空白だけなら `None`（＝変更しない）。
///
/// - 前後の空白を落とす
/// - 連続する空白（改行・タブ含む）は 1 個の半角スペースに畳む（1 行表示のため）
/// - [`MAX_DISPLAY_NAME_CHARS`] 文字で切る（日本語を壊さないよう文字単位）
fn normalize_display_name(raw: &str) -> Option<String> {
    let collapsed = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.is_empty() {
        return None;
    }
    let out: String = collapsed.chars().take(MAX_DISPLAY_NAME_CHARS).collect();
    // 末尾が空白で終わらないように（切り詰めで語間の空白が残るのを防ぐ）
    let out = out.trim_end().to_string();
    (!out.is_empty()).then_some(out)
}

/// コンテンツの表示名を書き換える（ユーザーによる名前カスタム）。
///
/// 実際に値が変わったときだけ `true` を返す（空名・存在しないコンテンツ・
/// 同名への変更は `false`。呼び出し側が pack の書き換えを省けるようにするため）。
pub fn rename(
    pool: &SqlitePool,
    book_id: &str,
    content_id: &str,
    display_name: &str,
) -> Result<bool, sqlx::Error> {
    let Some(name) = normalize_display_name(display_name) else {
        return Ok(false);
    };
    crate::db::block_on(async {
        let result = sqlx::query(
            "UPDATE book_contents SET display_name = ?1 \
             WHERE book_id = ?2 AND content_id = ?3 AND display_name <> ?1",
        )
        .bind(&name)
        .bind(book_id)
        .bind(content_id)
        .execute(pool)
        .await?;
        Ok(result.rows_affected() > 0)
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
/// - PDF / EPUB: 種別名（`PDF` / `EPUB`。ファイル名は出さない）
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
    let rows: Vec<(String, String, String)> = sqlx::query_as(
        "SELECT f.format_id, f.format_kind, b.file_name \
         FROM content_formats f \
         JOIN book_contents c ON c.content_id = f.content_id \
         JOIN books b ON b.id = c.book_id \
         WHERE (f.format_kind IN ('pdf', 'epub') AND f.label NOT IN ('PDF', 'EPUB')) \
           OR (f.format_kind = 'image' AND f.label = '画像')",
    )
    .fetch_all(&mut *conn)
    .await?;
    let mut updated = 0;
    for (format_id, format_kind, book_file_name) in rows {
        let Some(label) = legacy_label(&format_kind, &book_file_name) else {
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
fn legacy_label(format_kind: &str, book_file_name: &str) -> Option<String> {
    match format_kind {
        "pdf" | "epub" => Some(format_kind.to_uppercase()),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// 名前カスタムのテスト用に本 1 冊 + コンテンツ 1 件を作る。
    fn seed(pool: &SqlitePool) {
        crate::db::block_on(async {
            sqlx::query(
                "INSERT INTO books (id, title, file_name, file_size, opfs_path) \
                 VALUES ('b1', 't', 't.pdf', 1, 'b1.opfspack')",
            )
            .execute(pool)
            .await
            .unwrap();
        });
        insert_batch(
            pool,
            &[BookContent {
                content_id: "c1".into(),
                book_id: "b1".into(),
                display_name: "本文".into(),
                media_kind: "image".into(),
                is_primary: 1,
                sort_order: 0,
                created_at: "2026-09-11 00:00:00".into(),
            }],
            &[],
        )
        .unwrap();
    }

    fn display_name(pool: &SqlitePool) -> String {
        list_for_book(pool, "b1").unwrap()[0].display_name.clone()
    }

    #[test]
    fn rename_trims_and_collapses_whitespace() {
        let pool = crate::db::test_pool();
        seed(&pool);
        assert!(rename(&pool, "b1", "c1", "  本編\n   改訂版  ").unwrap());
        assert_eq!(display_name(&pool), "本編 改訂版");
    }

    #[test]
    fn rename_ignores_blank_names_and_unknown_contents() {
        let pool = crate::db::test_pool();
        seed(&pool);
        // 空白だけの名前は変更しない（既存の名前を壊さない）
        assert!(!rename(&pool, "b1", "c1", " \t\n ").unwrap());
        // 存在しないコンテンツも変更なし
        assert!(!rename(&pool, "b1", "missing", "別冊").unwrap());
        assert_eq!(display_name(&pool), "本文");
    }

    #[test]
    fn rename_is_idempotent_and_caps_length() {
        let pool = crate::db::test_pool();
        seed(&pool);
        // 同じ名前への変更は「変わっていない」を返す
        assert!(!rename(&pool, "b1", "c1", "本文").unwrap());
        let long = "あ".repeat(MAX_DISPLAY_NAME_CHARS + 20);
        assert!(rename(&pool, "b1", "c1", &long).unwrap());
        assert_eq!(display_name(&pool).chars().count(), MAX_DISPLAY_NAME_CHARS);
    }
}
