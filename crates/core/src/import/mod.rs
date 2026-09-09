//! Import pipeline: PDF / EPUB / image-ZIP -> .opfspack pack + DB rows
//! (books, imported_documents, document_images, document_text,
//! token_analysis, book_tags). Mirrors the Web `file-import.ts` rules.

pub mod pdf;

use std::io::Read;
use std::path::Path;

use opfspack::{Identity, PackBuilder};
use sha2::{Digest, Sha256};

use crate::db::{SqlitePool, books, documents, tags as tags_repo};

#[derive(Debug)]
pub struct ImportedBook {
    pub book: books::Book,
    pub document: documents::ImportedDocument,
    /// Tags written to `book_tags` (source=generated).
    pub tags: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("unsupported file type: {0}")]
    UnsupportedType(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("pack error: {0}")]
    Pack(#[from] opfspack::PackError),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("pdf error: {0}")]
    Pdf(String),
    #[error("image error: {0}")]
    Image(String),
    #[error("zip error: {0}")]
    Zip(String),
    #[error("empty archive")]
    EmptyArchive,
}

fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

/// 幅が `min_width` 未満の画像を Lanczos3 で拡大する（元が小さい画像の表示
/// ぼやけを軽減するため。真の解像度は増えないがピクセルの滑らかさは向上する）。
fn upscale_if_small(img: &image::DynamicImage, min_width: u32) -> image::DynamicImage {
    let width = img.width();
    if width >= min_width || width == 0 {
        return img.clone();
    }
    let scale = min_width as f32 / width as f32;
    let new_width = (width as f32 * scale).round().max(1.0) as u32;
    let new_height = (img.height() as f32 * scale).round().max(1.0) as u32;
    img.resize_exact(new_width, new_height, image::imageops::FilterType::Lanczos3)
}

/// Encode any dynamic image as lossy webp with the given quality (0-100).
/// `image` 0.25's own webp encoder is lossless-only, so lossy encoding goes
/// through the `webp` crate (bundled libwebp).
pub fn encode_webp(image: &image::DynamicImage, quality: u8) -> Result<Vec<u8>, ImportError> {
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let memory =
        webp::Encoder::from_rgba(&rgba, width, height).encode(quality.clamp(0, 100) as f32);
    Ok(memory.to_vec())
}

/// Thumbnail: first page scaled to 200px width.
fn thumbnail_of(page: &[u8]) -> Result<(Vec<u8>, u32, u32), ImportError> {
    let decoded = image::load_from_memory(page).map_err(|e| ImportError::Image(e.to_string()))?;
    let (width, height) = (decoded.width(), decoded.height());
    let thumb_width = 200u32;
    let thumb_height = ((height as f64 * thumb_width as f64) / width.max(1) as f64)
        .round()
        .max(1.0) as u32;
    let resized = decoded.resize_exact(
        thumb_width,
        thumb_height,
        image::imageops::FilterType::Triangle,
    );
    let data = encode_webp(&resized, 80)?;
    Ok((data, thumb_width, thumb_height))
}

fn book_id_for(identity: Option<&Identity>, reuse_book_id: Option<&str>) -> String {
    // 再ダウンロード時は既存本を再利用して重複を防ぐ（未ログイン＝identity None でも）。
    if let Some(reuse) = reuse_book_id {
        return reuse.to_string();
    }
    identity
        .map(|i| i.pack_id.clone())
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

fn base_title(file_name: &str) -> String {
    file_name
        .rsplit_once('.')
        .map(|(stem, _)| stem.to_string())
        .unwrap_or_else(|| file_name.to_string())
}

fn metadata_entry(title: &str, total_pages: Option<i64>) -> (Vec<u8>, String) {
    let metadata = serde_json::json!({
        "schemaVersion": 1,
        "title": title,
        "author": "",
        "circleName": "",
        "purchaseDate": null,
        "readingProgress": { "currentPage": 0, "totalPages": total_pages },
    });
    (
        serde_json::to_vec(&metadata).expect("metadata json"),
        "metadata.json".to_string(),
    )
}

struct PackSpec {
    /// (entry path, data, mime, compress)
    entries: Vec<(String, Vec<u8>, String, bool)>,
    page_rows: Vec<PageRow>,
    texts: Vec<String>,
    source_type: String,
    total_pages: i64,
}

struct PageRow {
    page_number: i64,
    width: i64,
    height: i64,
    entry_path: String,
    file_size: i64,
    text: Option<String>,
}

fn finish_import(
    pool: &SqlitePool,
    packs_dir: &Path,
    identity: Option<&Identity>,
    file_name: &str,
    source_bytes_len: i64,
    spec: PackSpec,
    reuse_book_id: Option<&str>,
) -> Result<ImportedBook, ImportError> {
    let book_id = book_id_for(identity, reuse_book_id);
    let title = base_title(file_name);
    let timestamp = now();
    log::info!("finish_import: 開始（{} ページ）", spec.total_pages);
    let save_start = std::time::Instant::now();

    // Build the pack first (metadata + pages), so document.file_hash can
    // reference the real pack bytes.
    let mut builder = PackBuilder::new(chrono::Utc::now().timestamp_millis() as u64);
    let (metadata, metadata_path) = metadata_entry(&title, Some(spec.total_pages));
    builder.add_entry(&metadata_path, metadata, "application/json", false);
    for (path, data, mime, compress) in &spec.entries {
        builder.add_entry(path, data.clone(), mime, *compress);
    }
    let pack_bytes = builder.build(identity, true)?;
    std::fs::create_dir_all(packs_dir)?;
    std::fs::write(packs_dir.join(format!("{book_id}.opfspack")), &pack_bytes)?;
    log::info!("finish_import: パック作成（{:?}）", save_start.elapsed());

    let book = books::Book {
        id: book_id.clone(),
        title: title.clone(),
        author: String::new(),
        circle_name: String::new(),
        purchase_date: None,
        file_name: file_name.to_string(),
        file_size: source_bytes_len,
        opfs_path: format!("{book_id}.opfspack"),
        cover_thumbnail: None,
        tbf_product_id: None,
        site_id: None,
        tags_fetched: 1,
        pack_id: Some(book_id.clone()),
        is_favorite: 0,
        is_hidden: 0,
        created_at: timestamp.clone(),
        updated_at: timestamp.clone(),
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
    books::insert(pool, &book)?;
    log::info!("finish_import: books 挿入完了");

    let document = documents::ImportedDocument {
        id: uuid::Uuid::new_v4().to_string(),
        book_id: book_id.clone(),
        source_type: spec.source_type.clone(),
        file_hash: sha256_hex(&pack_bytes),
        total_pages: spec.total_pages,
        metadata: None,
        status: "completed".to_string(),
        created_at: timestamp.clone(),
        updated_at: timestamp.clone(),
    };
    documents::insert_document(pool, &document)?;
    log::info!("finish_import: document 挿入完了");

    // 画像・テキスト・トークンをバッチで一括 INSERT する
    // （1 件 1 クエリだと数百ページ × トークン数万件の block_on が重く、
    //   取り込みが遅くなるため。トランザクション + バッチに集約する）。
    let mut image_rows = Vec::with_capacity(spec.page_rows.len() + 1);
    for row in &spec.page_rows {
        image_rows.push(documents::DocumentImage {
            id: uuid::Uuid::new_v4().to_string(),
            document_id: document.id.clone(),
            page_number: row.page_number,
            image_type: "page".to_string(),
            opfs_path: format!("{book_id}.opfspack"),
            width: row.width,
            height: row.height,
            mime_type: "image/webp".to_string(),
            file_size: row.file_size,
            extracted_text: row.text.clone(),
            pack_entry_path: Some(row.entry_path.clone()),
            created_at: timestamp.clone(),
        });
    }
    if let Some((_, data, _, _)) = spec
        .entries
        .iter()
        .find(|(path, ..)| path == "thumbnail.webp")
    {
        let (width, height) = image::load_from_memory(data)
            .map(|d| (d.width() as i64, d.height() as i64))
            .unwrap_or((0, 0));
        image_rows.push(documents::DocumentImage {
            id: uuid::Uuid::new_v4().to_string(),
            document_id: document.id.clone(),
            page_number: 1,
            image_type: "thumbnail".to_string(),
            opfs_path: format!("{book_id}.opfspack"),
            width,
            height,
            mime_type: "image/webp".to_string(),
            file_size: data.len() as i64,
            extracted_text: None,
            pack_entry_path: Some("thumbnail.webp".to_string()),
            created_at: timestamp.clone(),
        });
    }
    documents::insert_images_batch(pool, &image_rows)?;
    log::info!("finish_import: images バッチ挿入完了");

    let mut text_rows = Vec::with_capacity(spec.texts.len());
    for (page_number, text) in spec.texts.iter().enumerate() {
        text_rows.push(documents::DocumentText {
            id: uuid::Uuid::new_v4().to_string(),
            document_id: document.id.clone(),
            page_number: page_number as i64 + 1,
            text_content: text.clone(),
            created_at: timestamp.clone(),
        });
    }
    documents::insert_texts_batch(pool, &text_rows)?;
    log::info!("finish_import: texts バッチ挿入完了");

    // Token analysis rows (nouns per page).
    let mut token_rows = Vec::new();
    for (page_number, text) in spec.texts.iter().enumerate() {
        for (word, count) in crate::tags::extract_nouns(text, &[&title]) {
            token_rows.push(documents::TokenRow {
                id: uuid::Uuid::new_v4().to_string(),
                document_id: document.id.clone(),
                page_number: page_number as i64 + 1,
                token: word.clone(),
                pos: "名詞".to_string(),
                base_form: Some(word.clone()),
                reading: None,
                frequency: count as i64,
                created_at: timestamp.clone(),
            });
        }
    }
    documents::insert_tokens_batch(pool, &token_rows)?;

    log::info!(
        "finish_import: DB 書き込み完了（images/texts/tokens）（{:?}）",
        save_start.elapsed()
    );
    // Generated tags (Zenn matching with noun-only fallback).
    let zenn_tags = crate::tags::fetch_zenn_tags().unwrap_or_default();
    let text_refs: Vec<&str> = spec.texts.iter().map(String::as_str).collect();
    let generated = crate::tags::generate_tags(&text_refs, &[&title], &zenn_tags);
    let tag_pairs: Vec<(&str, &str)> = generated
        .iter()
        .map(|t| (t.as_str(), "generated"))
        .collect();
    tags_repo::set_for_book(pool, &book_id, &tag_pairs)?;

    log::info!("finish_import: 完了（{:?}）", save_start.elapsed());
    Ok(ImportedBook {
        book,
        document,
        tags: generated,
    })
}

/// Import a file by extension: pdf / epub / zip.
pub fn import_file(
    pool: &SqlitePool,
    source_path: &Path,
    packs_dir: &Path,
    identity: Option<&Identity>,
    progress: &mut (dyn FnMut(f32) + Send),
) -> Result<ImportedBook, ImportError> {
    let file_name = source_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("import")
        .to_string();
    let extension = source_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    let bytes = std::fs::read(source_path)?;
    match extension.as_str() {
        "pdf" => import_pdf_bytes(pool, &file_name, &bytes, packs_dir, identity, progress),
        "epub" => import_epub_bytes(pool, &file_name, &bytes, packs_dir, identity),
        "zip" => import_zip_bytes(pool, &file_name, &bytes, packs_dir, identity, progress, None),
        _ => Err(ImportError::UnsupportedType(extension)),
    }
}

/// Import a PDF from memory.
pub fn import_pdf_bytes(
    pool: &SqlitePool,
    file_name: &str,
    bytes: &[u8],
    packs_dir: &Path,
    identity: Option<&Identity>,
    progress: &mut (dyn FnMut(f32) + Send),
) -> Result<ImportedBook, ImportError> {
    let pages = pdf::render_pdf_pages(bytes, progress)?;
    import_rendered_pdf_pages(
        pool,
        file_name,
        bytes.len() as i64,
        pages,
        packs_dir,
        identity,
    )
}

/// Import already-rendered PDF pages. Rendering is expensive and must happen
/// **outside** the DB lock, so callers run `pdf::render_pdf_pages` first and
/// only take the connection for this final write step.
pub fn import_rendered_pdf_pages(
    pool: &SqlitePool,
    file_name: &str,
    file_size: i64,
    pages: Vec<pdf::PageImage>,
    packs_dir: &Path,
    identity: Option<&Identity>,
) -> Result<ImportedBook, ImportError> {
    if pages.is_empty() {
        return Err(ImportError::Pdf("no pages rendered".into()));
    }
    let total_pages = pages.len() as i64;
    let mut entries = Vec::new();
    let mut page_rows = Vec::new();
    let mut texts = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        let entry_path = format!("pages/page_{:04}.webp", index + 1);
        entries.push((
            entry_path.clone(),
            page.data.clone(),
            "image/webp".to_string(),
            false,
        ));
        page_rows.push(PageRow {
            page_number: index as i64 + 1,
            width: page.width as i64,
            height: page.height as i64,
            entry_path,
            file_size: page.data.len() as i64,
            text: Some(page.text.clone()),
        });
        texts.push(page.text.clone());
    }
    // cover = page 1; thumbnail = page 1 scaled to 200px width.
    entries.push((
        "cover.webp".to_string(),
        pages[0].data.clone(),
        "image/webp".to_string(),
        false,
    ));
    let (thumb, _, _) = thumbnail_of(&pages[0].data)?;
    entries.push((
        "thumbnail.webp".to_string(),
        thumb,
        "image/webp".to_string(),
        false,
    ));

    finish_import(
        pool,
        packs_dir,
        identity,
        file_name,
        file_size,
        PackSpec {
            entries,
            page_rows,
            texts,
            source_type: "pdf".to_string(),
            total_pages,
        },
        None,
    )
}

/// Import an EPUB as a single raw entry (viewing not supported — same as Web).
pub fn import_epub_bytes(
    pool: &SqlitePool,
    file_name: &str,
    bytes: &[u8],
    packs_dir: &Path,
    identity: Option<&Identity>,
) -> Result<ImportedBook, ImportError> {
    let entry_path = file_name.to_string();
    finish_import(
        pool,
        packs_dir,
        identity,
        file_name,
        bytes.len() as i64,
        PackSpec {
            entries: vec![(
                entry_path,
                bytes.to_vec(),
                "application/epub+zip".to_string(),
                false,
            )],
            page_rows: Vec::new(),
            texts: Vec::new(),
            source_type: "epub".to_string(),
            total_pages: 0,
        },
        None,
    )
}

const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif"];

/// ファイル名を (is_numeric, chunk) の列に分解する（自然順ソート用）。
fn natural_key(s: &str) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_num = false;
    for c in s.chars() {
        let is_num = c.is_ascii_digit();
        if cur.is_empty() || is_num == cur_num {
            cur.push(c);
        } else {
            out.push((cur_num, std::mem::take(&mut cur)));
            cur.push(c);
        }
        cur_num = is_num;
    }
    if !cur.is_empty() {
        out.push((cur_num, cur));
    }
    out
}

/// ファイル名の自然順比較（`1.jpg` < `2.jpg` < `10.jpg`）。数字の連続は数値として比較する。
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let ka = natural_key(a);
    let kb = natural_key(b);
    for (ia, ib) in ka.iter().zip(kb.iter()) {
        let ord = if ia.0 && ib.0 {
            let at = ia.1.trim_start_matches('0');
            let bt = ib.1.trim_start_matches('0');
            at.len()
                .cmp(&bt.len())
                .then_with(|| at.cmp(bt))
        } else {
            ia.1.cmp(&ib.1)
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    ka.len().cmp(&kb.len())
}

#[cfg(test)]
mod natural_sort_tests {
    use super::{natural_cmp, natural_key};
    use std::cmp::Ordering;

    #[test]
    fn natural_cmp_orders_numeric_sequences() {
        let mut v = vec!["2.jpg", "10.jpg", "1.jpg", "3.jpg"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["1.jpg", "2.jpg", "3.jpg", "10.jpg"]);
    }

    #[test]
    fn natural_cmp_matches_lexicographic_for_nonnumeric() {
        assert_eq!(natural_cmp("a.jpg", "b.jpg"), Ordering::Less);
        assert_eq!(natural_cmp("b.jpg", "a.jpg"), Ordering::Greater);
    }

    #[test]
    fn natural_key_splits_numeric_runs() {
        assert_eq!(natural_key("page10.jpg"), vec![(false, "page".into()), (true, "10".into()), (false, ".jpg".into())]);
    }
}

/// Import a ZIP: PDF > EPUB > image set (file-import.ts priority).
pub fn import_zip_bytes(
    pool: &SqlitePool,
    file_name: &str,
    bytes: &[u8],
    packs_dir: &Path,
    identity: Option<&Identity>,
    progress: &mut (dyn FnMut(f32) + Send),
    reuse_book_id: Option<&str>,
) -> Result<ImportedBook, ImportError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| ImportError::Zip(e.to_string()))?;
    let mut entries: Vec<(String, Vec<u8>)> = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive
            .by_index(index)
            .map_err(|e| ImportError::Zip(e.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        let name = entry.name().to_string();
        let mut data = Vec::new();
        entry
            .read_to_end(&mut data)
            .map_err(|e| ImportError::Zip(e.to_string()))?;
        entries.push((name, data));
    }
    if entries.is_empty() {
        return Err(ImportError::EmptyArchive);
    }
    if let Some((name, data)) = entries
        .iter()
        .find(|(name, _)| name.to_lowercase().ends_with(".pdf"))
    {
        return import_pdf_bytes(pool, name, data, packs_dir, identity, progress);
    }
    if let Some((name, data)) = entries
        .iter()
        .find(|(name, _)| name.to_lowercase().ends_with(".epub"))
    {
        return import_epub_bytes(pool, name, data, packs_dir, identity);
    }
    let mut images: Vec<&(String, Vec<u8>)> = entries
        .iter()
        .filter(|(name, _)| {
            name.rsplit_once('.')
                .is_some_and(|(_, ext)| IMAGE_EXTENSIONS.contains(&ext.to_lowercase().as_str()))
        })
        .collect();
    if images.is_empty() {
        return Err(ImportError::UnsupportedType(
            "zip without pdf/epub/images".into(),
        ));
    }
    images.sort_by(|a, b| natural_cmp(&a.0, &b.0));

    let mut pack_entries = Vec::new();
    let mut page_rows = Vec::new();
    let total_pages = images.len() as i64;
    for (index, (name, data)) in images.iter().enumerate() {
        let decoded = image::load_from_memory(data)
            .map_err(|e| ImportError::Image(format!("{name}: {e}")))?;
        // 元画像が小さい場合は Lanczos3 で 1000px 幅まで拡大してから保存する
        // （表示時のぼやけ軽減。RustMangaReader の Smart Scaling と同じ方式）
        let decoded = upscale_if_small(&decoded, 1000);
        let (width, height) = (decoded.width(), decoded.height());
        let webp = encode_webp(&decoded, 88)?;
        let entry_path = format!("pages/page_{:04}.webp", index + 1);
        pack_entries.push((
            entry_path.clone(),
            webp.clone(),
            "image/webp".to_string(),
            false,
        ));
        page_rows.push(PageRow {
            page_number: index as i64 + 1,
            width: width as i64,
            height: height as i64,
            entry_path,
            file_size: webp.len() as i64,
            text: None,
        });
    }
    let (thumb, _, _) = thumbnail_of(&pack_entries[0].1)?;
    pack_entries.push((
        "thumbnail.webp".to_string(),
        thumb,
        "image/webp".to_string(),
        false,
    ));

    finish_import(
        pool,
        packs_dir,
        identity,
        file_name,
        bytes.len() as i64,
        PackSpec {
            entries: pack_entries,
            page_rows,
            texts: Vec::new(),
            source_type: "image-set".to_string(),
            total_pages,
        },
        reuse_book_id,
    )
}

/// 単体画像（jpg / png / webp / gif 等）を 1 ページの本として取り込む。
/// BOOTH のダウンロードが PDF ではなく画像ファイルの場合に使う。
pub fn import_image_bytes(
    pool: &SqlitePool,
    file_name: &str,
    bytes: &[u8],
    packs_dir: &Path,
    identity: Option<&Identity>,
) -> Result<ImportedBook, ImportError> {
    let decoded = image::load_from_memory(bytes).map_err(|e| ImportError::Image(e.to_string()))?;
    // 元画像が小さい場合は Lanczos3 で 1000px 幅まで拡大してから保存する
    let decoded = upscale_if_small(&decoded, 1000);
    let (width, height) = (decoded.width(), decoded.height());
    let webp = encode_webp(&decoded, 88)?;
    let (thumb, _, _) = thumbnail_of(&webp)?;
    finish_import(
        pool,
        packs_dir,
        identity,
        file_name,
        bytes.len() as i64,
        PackSpec {
            entries: vec![
                (
                    "pages/page_0001.webp".to_string(),
                    webp.clone(),
                    "image/webp".to_string(),
                    false,
                ),
                (
                    "thumbnail.webp".to_string(),
                    thumb,
                    "image/webp".to_string(),
                    false,
                ),
            ],
            page_rows: vec![PageRow {
                page_number: 1,
                width: width as i64,
                height: height as i64,
                entry_path: "pages/page_0001.webp".to_string(),
                file_size: webp.len() as i64,
                text: None,
            }],
            texts: Vec::new(),
            source_type: "image".to_string(),
            total_pages: 1,
        },
        None,
    )
}
