//! `imported_documents` / `document_images` / `document_text` repository.

use sqlx::Row;

use crate::db::SqlitePool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct ImportedDocument {
    pub id: String,
    pub book_id: String,
    pub source_type: String,
    pub file_hash: String,
    pub total_pages: i64,
    pub metadata: Option<String>,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct DocumentImage {
    pub id: String,
    pub document_id: String,
    /// 所属コンテンツ（`book_contents.content_id`）。NULL = フェーズ2以前の旧データ
    /// （単一コンテンツ扱い）。
    pub content_id: Option<String>,
    /// 所属レンディション（`content_formats.format_id`）。NULL = 旧データ。
    pub format_id: Option<String>,
    pub page_number: i64,
    pub image_type: String,
    pub opfs_path: String,
    pub width: i64,
    pub height: i64,
    pub mime_type: String,
    pub file_size: i64,
    pub pack_entry_path: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct DocumentText {
    pub id: String,
    pub document_id: String,
    pub page_number: i64,
    pub text_content: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct TokenRow {
    pub id: String,
    pub document_id: String,
    pub page_number: i64,
    pub token: String,
    pub pos: String,
    pub base_form: Option<String>,
    pub reading: Option<String>,
    pub frequency: i64,
    pub created_at: String,
}

pub fn insert_document(pool: &SqlitePool, document: &ImportedDocument) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "INSERT INTO imported_documents (id, book_id, source_type, file_hash, total_pages, \
             metadata, status, created_at, updated_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .bind(&document.id)
        .bind(&document.book_id)
        .bind(&document.source_type)
        .bind(&document.file_hash)
        .bind(document.total_pages)
        .bind(&document.metadata)
        .bind(&document.status)
        .bind(&document.created_at)
        .bind(&document.updated_at)
        .execute(pool)
        .await?;
        Ok(())
    })
}

pub fn get_document(pool: &SqlitePool, id: &str) -> Result<Option<ImportedDocument>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, ImportedDocument>(
            "SELECT id, book_id, source_type, file_hash, total_pages, metadata, status, created_at, updated_at FROM imported_documents WHERE id = ?1",
        )
        .bind(id)
        .fetch_optional(pool)
        .await
    })
}

/// book_id でインポート済みドキュメントを取得する（ダウンロード直後の
/// ページ数表示などに使う）。
pub fn get_document_by_book_id(
    pool: &SqlitePool,
    book_id: &str,
) -> Result<Option<ImportedDocument>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, ImportedDocument>(
            "SELECT id, book_id, source_type, file_hash, total_pages, metadata, status, created_at, updated_at FROM imported_documents WHERE book_id = ?1 LIMIT 1",
        )
        .bind(book_id)
        .fetch_optional(pool)
        .await
    })
}

pub fn insert_image(pool: &SqlitePool, image: &DocumentImage) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "INSERT INTO document_images (id, document_id, content_id, format_id, page_number, \
             image_type, opfs_path, width, height, mime_type, file_size, \
             pack_entry_path, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        )
        .bind(&image.id)
        .bind(&image.document_id)
        .bind(&image.content_id)
        .bind(&image.format_id)
        .bind(image.page_number)
        .bind(&image.image_type)
        .bind(&image.opfs_path)
        .bind(image.width)
        .bind(image.height)
        .bind(&image.mime_type)
        .bind(image.file_size)
        .bind(&image.pack_entry_path)
        .bind(&image.created_at)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// 本のページ/サムネイル画像を返す。**既定表示（primary）コンテンツの第 1 レンディション**と、
/// コンテンツを持たない旧データ（`content_id IS NULL`）だけを対象にする。
pub fn images_for_book(
    pool: &SqlitePool,
    book_id: &str,
) -> Result<Vec<DocumentImage>, sqlx::Error> {
    images_for_selection(pool, book_id, None, None)
}

/// コンテンツ／レンディションの選択に応じたページ列を返す（フェーズ3）。
///
/// - `content_id` 未指定 → 既定表示コンテンツ（`is_primary DESC, sort_order` の先頭）
/// - `format_id` 未指定 → そのコンテンツの先頭レンディション（`sort_order` の先頭）
/// - ページ行は対象レンディション、サムネイル等の非ページ行は対象コンテンツのものを返す
/// - `content_id IS NULL`（フェーズ2以前の旧データ）は常に対象に含める
pub fn images_for_selection(
    pool: &SqlitePool,
    book_id: &str,
    content_id: Option<&str>,
    format_id: Option<&str>,
) -> Result<Vec<DocumentImage>, sqlx::Error> {
    crate::db::block_on(async {
        let content_id: Option<String> = match content_id {
            Some(id) => Some(id.to_string()),
            None => {
                sqlx::query_scalar(
                    "SELECT content_id FROM book_contents WHERE book_id = ?1 \
                     ORDER BY is_primary DESC, sort_order, content_id LIMIT 1",
                )
                .bind(book_id)
                .fetch_optional(pool)
                .await?
            }
        };
        let format_id: Option<String> = match format_id {
            Some(id) => Some(id.to_string()),
            None => match &content_id {
                Some(id) => {
                    sqlx::query_scalar(
                        "SELECT format_id FROM content_formats WHERE content_id = ?1 \
                         ORDER BY sort_order, format_id LIMIT 1",
                    )
                    .bind(id)
                    .fetch_optional(pool)
                    .await?
                }
                None => None,
            },
        };
        sqlx::query_as::<_, DocumentImage>(
            "SELECT di.id, di.document_id, di.content_id, di.format_id, di.page_number, \
             di.image_type, di.opfs_path, di.width, di.height, di.mime_type, di.file_size, \
             di.pack_entry_path, di.created_at
             FROM document_images di
             JOIN imported_documents d ON d.id = di.document_id
             WHERE d.book_id = ?1
               AND (di.content_id IS NULL
                    OR (di.image_type = 'page' AND di.format_id IS ?2)
                    OR (di.image_type <> 'page' AND di.content_id IS ?3))
             ORDER BY di.image_type DESC, di.page_number ASC",
        )
        .bind(book_id)
        .bind(&format_id)
        .bind(&content_id)
        .fetch_all(pool)
        .await
    })
}

/// 指定した本に紐づく取り込み済みドキュメント（画像・テキスト・トークン含む）を削除する。
/// 同じ book_id を再利用して再取り込みするときに、古いページ行が残って
/// ページが二重に並ぶのを防ぐ。
pub fn delete_for_book(pool: &SqlitePool, book_id: &str) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        let document_ids: Vec<String> =
            sqlx::query_scalar("SELECT id FROM imported_documents WHERE book_id = ?1")
                .bind(book_id)
                .fetch_all(pool)
                .await?;
        for document_id in document_ids {
            sqlx::query("DELETE FROM token_analysis WHERE document_id = ?1")
                .bind(&document_id)
                .execute(pool)
                .await?;
            sqlx::query("DELETE FROM document_text WHERE document_id = ?1")
                .bind(&document_id)
                .execute(pool)
                .await?;
            sqlx::query("DELETE FROM document_images WHERE document_id = ?1")
                .bind(&document_id)
                .execute(pool)
                .await?;
            sqlx::query("DELETE FROM imported_documents WHERE id = ?1")
                .bind(&document_id)
                .execute(pool)
                .await?;
        }
        Ok(())
    })
}

pub fn insert_text(pool: &SqlitePool, text: &DocumentText) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "INSERT INTO document_text (id, document_id, page_number, text_content, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(&text.id)
        .bind(&text.document_id)
        .bind(text.page_number)
        .bind(&text.text_content)
        .bind(&text.created_at)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// ページ画像を一括 INSERT する（取り込み時の負荷改善）。
/// 1 ページ 1 クエリだと数百ページで数百本のフル コミットごとの
/// block_on が走るため、トランザクション + バッチでまとめる。
pub fn insert_images_batch(pool: &SqlitePool, images: &[DocumentImage]) -> Result<(), sqlx::Error> {
    if images.is_empty() {
        return Ok(());
    }
    crate::db::block_on(async {
        let mut tx = pool.begin().await?;
        for chunk in images.chunks(200) {
            let mut sql = String::from(
                "INSERT INTO document_images (id, document_id, content_id, format_id, page_number, \
                 image_type, opfs_path, width, height, mime_type, file_size, \
                 pack_entry_path, created_at) VALUES ",
            );
            let mut values = Vec::with_capacity(chunk.len());
            for _ in 0..chunk.len() {
                values.push("(?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)".to_string());
            }
            sql.push_str(&values.join(", "));
            let mut q = sqlx::query(&sql);
            for img in chunk {
                q = q
                    .bind(&img.id)
                    .bind(&img.document_id)
                    .bind(&img.content_id)
                    .bind(&img.format_id)
                    .bind(img.page_number)
                    .bind(&img.image_type)
                    .bind(&img.opfs_path)
                    .bind(img.width)
                    .bind(img.height)
                    .bind(&img.mime_type)
                    .bind(img.file_size)
                    .bind(&img.pack_entry_path)
                    .bind(&img.created_at);
            }
            q.execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    })
}

/// ページテキストを一括 INSERT する（取り込み時の負荷改善）。
pub fn insert_texts_batch(pool: &SqlitePool, texts: &[DocumentText]) -> Result<(), sqlx::Error> {
    if texts.is_empty() {
        return Ok(());
    }
    crate::db::block_on(async {
        let mut tx = pool.begin().await?;
        for chunk in texts.chunks(200) {
            let mut sql = String::from(
                "INSERT INTO document_text (id, document_id, page_number, text_content, created_at) \
                 VALUES ",
            );
            let mut values = Vec::with_capacity(chunk.len());
            for _ in 0..chunk.len() {
                values.push("(?, ?, ?, ?, ?)".to_string());
            }
            sql.push_str(&values.join(", "));
            let mut q = sqlx::query(&sql);
            for t in chunk {
                q = q
                    .bind(&t.id)
                    .bind(&t.document_id)
                    .bind(t.page_number)
                    .bind(&t.text_content)
                    .bind(&t.created_at);
            }
            q.execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    })
}

/// トークンを一括 INSERT する（取り込み時の負荷改善）。
/// 抽出トークンは 1 ページ数百件 × 数百ページで数万行になるため、
/// 1 件 1 クエリだと取り込みが著しく遅くなる。
pub fn insert_tokens_batch(pool: &SqlitePool, tokens: &[TokenRow]) -> Result<(), sqlx::Error> {
    if tokens.is_empty() {
        return Ok(());
    }
    crate::db::block_on(async {
        let mut tx = pool.begin().await?;
        for chunk in tokens.chunks(500) {
            let mut sql = String::from(
                "INSERT INTO token_analysis (id, document_id, page_number, token, pos, base_form, \
                 reading, frequency, created_at) VALUES ",
            );
            let mut values = Vec::with_capacity(chunk.len());
            for _ in 0..chunk.len() {
                values.push("(?, ?, ?, ?, ?, ?, ?, ?, ?)".to_string());
            }
            sql.push_str(&values.join(", "));
            let mut q = sqlx::query(&sql);
            for t in chunk {
                q = q
                    .bind(&t.id)
                    .bind(&t.document_id)
                    .bind(t.page_number)
                    .bind(&t.token)
                    .bind(&t.pos)
                    .bind(&t.base_form)
                    .bind(&t.reading)
                    .bind(t.frequency)
                    .bind(&t.created_at);
            }
            q.execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    })
}

pub fn all_text_for_document(
    pool: &SqlitePool,
    document_id: &str,
) -> Result<Vec<String>, sqlx::Error> {
    crate::db::block_on(async {
        let rows = sqlx::query(
            "SELECT text_content FROM document_text WHERE document_id = ?1 ORDER BY page_number",
        )
        .bind(document_id)
        .fetch_all(pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(row.get::<String, _>("text_content"));
        }
        Ok(out)
    })
}

pub fn insert_token(pool: &SqlitePool, token: &TokenRow) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "INSERT INTO token_analysis (id, document_id, page_number, token, pos, base_form, \
             reading, frequency, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        )
        .bind(&token.id)
        .bind(&token.document_id)
        .bind(token.page_number)
        .bind(&token.token)
        .bind(&token.pos)
        .bind(&token.base_form)
        .bind(&token.reading)
        .bind(token.frequency)
        .bind(&token.created_at)
        .execute(pool)
        .await?;
        Ok(())
    })
}
