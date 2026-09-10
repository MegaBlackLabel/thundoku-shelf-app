//! Import pipeline: PDF / EPUB / image-ZIP -> .opfspack pack + DB rows
//! (books, imported_documents, document_images, document_text,
//! token_analysis, book_tags). Mirrors the Web `file-import.ts` rules.

pub mod pdf;

pub mod classify;
pub mod export_text;
mod zip_names;

use classify::{EntryKind, classify_entry, is_readable_kind};

use std::io::Read;
use std::path::Path;

use opfspack::{Identity, PackBuilder};
use sha2::{Digest, Sha256};

use crate::db::{SqlitePool, books, contents, documents, tags as tags_repo};

#[derive(Debug)]
pub struct ImportedBook {
    pub book: books::Book,
    pub document: documents::ImportedDocument,
    /// Tags written to `book_tags` (source=generated).
    pub tags: Vec<String>,
    /// 取り込み時に読み飛ばしたエントリ（壊れた画像など）。0 件なら空。
    pub warnings: Vec<String>,
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

/// 画像エントリ 1 件をページ用 WebP へ変換する
/// （元が小さい画像は Lanczos3 で 1000px 幅まで拡大。表示時のぼやけ軽減）。
fn render_page_image(data: &[u8]) -> Result<(Vec<u8>, u32, u32), ImportError> {
    let decoded = image::load_from_memory(data).map_err(|e| ImportError::Image(e.to_string()))?;
    let decoded = upscale_if_small(&decoded, 1000);
    let (width, height) = (decoded.width(), decoded.height());
    Ok((encode_webp(&decoded, 88)?, width, height))
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

/// ZIP エントリパスからファイル名部分を取り出す（`dir/book.pdf` → `book.pdf`）。
fn entry_file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// ZIP エントリの索引と名前（本体データは保持しない）。
struct EntryMeta {
    index: usize,
    name: String,
}

/// エントリの名前だけを集める（本体は伸長しない）。
///
/// `zip` crate の `name()` は UTF-8 フラグの無い名前を CP437 として復号するため、
/// 日本語（Shift-JIS / CP932）の名前が文字化けする。生バイトから自前でデコードする。
fn collect_entry_metas<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Result<Vec<EntryMeta>, ImportError> {
    let mut metas = Vec::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index_raw(index)
            .map_err(|e| ImportError::Zip(e.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        metas.push(EntryMeta {
            index,
            name: zip_names::decode_entry_name(entry.name_raw()),
        });
    }
    Ok(metas)
}

/// ZIP のエントリを 1 件読み出す。
/// 全エントリを同時にメモリへ載せないよう、呼び出しごとに 1 件だけ伸長する。
fn read_zip_entry<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    index: usize,
) -> Result<Vec<u8>, ImportError> {
    let mut entry = archive
        .by_index(index)
        .map_err(|e| ImportError::Zip(e.to_string()))?;
    let mut data = Vec::new();
    entry
        .read_to_end(&mut data)
        .map_err(|e| ImportError::Zip(e.to_string()))?;
    Ok(data)
}

/// コンテンツのメディア種別（`book_contents.media_kind` の下地）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Pdf,
    Epub,
    Audio,
    Video,
}

/// レンディション（切替可能な表示形態）。同じ内容の別形式・別バリアント
/// （`PDF版` / `画像版`、`文字あり` / `文字なし` など）をここに畳む。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedRendition {
    /// 切替 UI に出す名前（`画像` / `PDF` など）。
    pub label: String,
    /// メディア種別（`content_formats.format_kind` の元）。
    pub kind: MediaKind,
    /// エントリの並び順（`collect_entry_metas` が返す一覧の添字。ページ順）。
    pub entries: Vec<usize>,
}

/// 読む単位（`book_contents` の下地）。表紙・junk は含まない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedContent {
    pub display_name: String,
    pub media_kind: MediaKind,
    pub renditions: Vec<PlannedRendition>,
}

impl PlannedContent {
    /// 主レンディションのページ数の目安。
    /// PDF / EPUB はページ数が展開するまで不明なので 1 ファイル = 1 として数える。
    fn page_hint(&self) -> usize {
        self.renditions.first().map_or(0, |r| r.entries.len())
    }
}

/// 取り込む対象が無いときの理由（通知文言は UI 側で決める。§11.2 R3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// 画像 / PDF / EPUB / 音声 / 動画が 1 件も無い（txt のみ・ゲーム等）。
    NotAReadableWork,
}

/// ZIP の解析結果。DB にもディスクにも書かない。
#[derive(Debug, Clone)]
pub struct ImportPlan {
    /// 読む単位。`import_zip_bytes` が見るのは `primary` の 1 件だけ。
    pub contents: Vec<PlannedContent>,
    /// 既定で選ばれるコンテンツの添字（`contents` が空なら無意味）。
    pub primary: usize,
    /// `_export.txt` から読んだ `(page_number, text)`。
    pub export_text: Vec<(i64, String)>,
    /// 解析時に読み飛ばしたエントリ。
    pub warnings: Vec<String>,
    /// 取り込む対象が無いときの理由。
    pub skip_reason: Option<SkipReason>,
}

/// コンテンツごとのエントリを集めるための作業用バケット。
struct Group {
    /// 同一性のキー（フォルダのパス。直下のファイルは `""` かファイル名）。
    key: String,
    /// 表示名（フォルダ名 / ファイル名 / `本文`）。
    display_name: String,
    /// `(種別, メタ情報の並び順)`。
    entries: Vec<(EntryKind, usize)>,
}

/// パスの区切り（ZIP は `/`。全角 `／` で書き出す作品もあるため両方見る）。
const PATH_SEPARATORS: [char; 3] = ['/', '\\', '／'];

/// 形式だけを表すフォルダ名か（このフォルダ自体は読む単位にしない）。
/// `1.尻穴便女/jpg/…` のように形式フォルダが挟まる構造で、内容のフォルダ名を採るため。
fn is_format_folder(name: &str) -> bool {
    let lower = name.trim().to_lowercase();
    matches!(
        lower.as_str(),
        "jpg"
            | "jpeg"
            | "png"
            | "webp"
            | "gif"
            | "bmp"
            | "tif"
            | "tiff"
            | "pdf"
            | "epub"
            | "カラー"
            | "モノクロ"
            | "文字あり"
            | "文字なし"
            | "seあり"
            | "seなし"
    ) || lower.ends_with("版")
        || lower.starts_with("画像")
}

/// エントリが属する読む単位（コンテンツ）のフォルダ。
///
/// 末尾が形式フォルダなら 1 つ上を使う（`1.尻穴便女/jpg/001.jpg` → `1.尻穴便女`）。
/// 戻り値は `(キー, 表示名)`。直下のファイルは `None`。
fn content_folder(name: &str) -> Option<(String, String)> {
    let mut components: Vec<&str> = name.split(PATH_SEPARATORS).collect();
    components.pop(); // ファイル名を落とす
    while components.last().is_some_and(|last| is_format_folder(last)) {
        components.pop();
    }
    let display_name = components.last()?.to_string();
    Some((components.join("/"), display_name))
}

/// ファイル名から拡張子を除いた部分。
fn file_stem(name: &str) -> &str {
    let base = name.rsplit(PATH_SEPARATORS).next().unwrap_or(name);
    base.rsplit_once('.').map_or(base, |(stem, _)| stem)
}

/// エントリ一覧からコンテンツ（読む単位）を組み立てる。
///
/// - **ページを直接含むフォルダ**を 1 コンテンツにする（形式フォルダは飛ばす）。
///   入れ子の上位フォルダ（`総集編/1.話A/…` の `総集編`）は単位にしない。
/// - 直下の画像はまとめて 1 コンテンツ（`本文`）
/// - 直下の PDF / EPUB / 音声 / 動画はファイルごとに 1 コンテンツ。
///   同じ名前のフォルダがあればそのレンディションとして畳む（`PDF版` / `画像版`。§3.2）
fn build_contents(metas: &[EntryMeta]) -> Vec<PlannedContent> {
    let mut groups: Vec<Group> = Vec::new();
    let mut root_entries: Vec<(EntryKind, usize, String)> = Vec::new();

    // 索引はアーカイブ索引ではなく `metas` 内の**並び順**を使う
    // （ディレクトリエントリを除いた分だけ両者はずれる）。
    for (ordinal, meta) in metas.iter().enumerate() {
        let kind = classify_entry(&meta.name);
        if !is_readable_kind(kind) {
            continue;
        }
        match content_folder(&meta.name) {
            Some((key, display_name)) => {
                group_push(&mut groups, &key, &display_name, kind, ordinal)
            }
            None => root_entries.push((kind, ordinal, file_stem(&meta.name).to_string())),
        }
    }
    // 直下の画像は 1 つにまとめ、それ以外はファイル単位。PDF は同名コンテンツへ畳む。
    for (kind, ordinal, stem) in root_entries {
        match kind {
            EntryKind::Image => group_push(&mut groups, "", "本文", kind, ordinal),
            _ => match groups.iter_mut().find(|group| group.display_name == stem) {
                Some(group) => group.entries.push((kind, ordinal)),
                None => group_push(&mut groups, &stem, &stem, kind, ordinal),
            },
        }
    }

    // フォルダ名の数字接頭辞（`1.` / `2.`）は作者の意図的な順序（§5.1）なので自然順で並べる
    groups.sort_by(|a, b| natural_cmp(&a.display_name, &b.display_name));

    groups
        .into_iter()
        .filter_map(|group| plan_content(group, metas))
        .collect()
}

fn group_push(
    groups: &mut Vec<Group>,
    key: &str,
    display_name: &str,
    kind: EntryKind,
    ordinal: usize,
) {
    match groups.iter_mut().find(|group| group.key == key) {
        Some(group) => group.entries.push((kind, ordinal)),
        None => groups.push(Group {
            key: key.to_string(),
            display_name: display_name.to_string(),
            entries: vec![(kind, ordinal)],
        }),
    }
}

/// バケットを `PlannedContent` へ変換する。読める種別が無ければ `None`。
fn plan_content(group: Group, metas: &[EntryMeta]) -> Option<PlannedContent> {
    // レンディションの並び順（先頭が主）。画像を先頭にする。
    const ORDER: &[EntryKind] = &[
        EntryKind::Image,
        EntryKind::Pdf,
        EntryKind::Epub,
        EntryKind::Audio,
        EntryKind::Video,
    ];
    let mut renditions = Vec::new();
    let mut media_kind = None;
    for kind in ORDER {
        let mut ordinals: Vec<usize> = group
            .entries
            .iter()
            .filter(|(entry_kind, _)| entry_kind == kind)
            .map(|(_, ordinal)| *ordinal)
            .collect();
        if ordinals.is_empty() {
            continue;
        }
        ordinals.sort_by(|a, b| natural_cmp(&metas[*a].name, &metas[*b].name));
        if media_kind.is_none() {
            media_kind = media_kind_of(*kind);
        }
        renditions.push(PlannedRendition {
            label: rendition_label(*kind, &ordinals, metas),
            kind: media_kind_of(*kind)?,
            entries: ordinals,
        });
    }
    Some(PlannedContent {
        display_name: group.display_name,
        media_kind: media_kind?,
        renditions,
    })
}

/// レンディションの表示名。画像は実際の拡張子（`JPEG` / `PNG` …）、
/// PDF / EPUB は種別名（拡張子を出さない: メニューでは内容名と並べるため）。
fn rendition_label(kind: EntryKind, ordinals: &[usize], metas: &[EntryMeta]) -> String {
    let first = ordinals.first().and_then(|ordinal| metas.get(*ordinal));
    let extension = first
        .and_then(|meta| meta.name.rsplit_once('.'))
        .map(|(_, ext)| ext.to_lowercase());
    let fallback = || {
        media_kind_of(kind)
            .map(|media| media.label().to_string())
            .unwrap_or_default()
    };
    match kind {
        EntryKind::Image => extension
            .as_deref()
            .and_then(crate::db::contents::image_label_for_extension)
            .map(str::to_string)
            .unwrap_or_else(fallback),
        EntryKind::Pdf | EntryKind::Epub => fallback(),
        _ => fallback(),
    }
}

impl MediaKind {
    /// 切替 UI に出す名前。
    fn label(self) -> &'static str {
        match self {
            MediaKind::Image => "画像",
            MediaKind::Pdf => "PDF",
            MediaKind::Epub => "EPUB",
            MediaKind::Audio => "音声",
            MediaKind::Video => "動画",
        }
    }

    /// DB（`book_contents.media_kind` / `content_formats.format_kind`）に入れる値。
    fn as_str(self) -> &'static str {
        match self {
            MediaKind::Image => "image",
            MediaKind::Pdf => "pdf",
            MediaKind::Epub => "epub",
            MediaKind::Audio => "audio",
            MediaKind::Video => "video",
        }
    }
}

fn media_kind_of(kind: EntryKind) -> Option<MediaKind> {
    match kind {
        EntryKind::Image => Some(MediaKind::Image),
        EntryKind::Pdf => Some(MediaKind::Pdf),
        EntryKind::Epub => Some(MediaKind::Epub),
        EntryKind::Audio => Some(MediaKind::Audio),
        EntryKind::Video => Some(MediaKind::Video),
        EntryKind::Cover | EntryKind::ExportText | EntryKind::Junk => None,
    }
}

fn base_title(file_name: &str) -> String {
    file_name
        .rsplit_once('.')
        .map(|(stem, _)| stem.to_string())
        .unwrap_or_else(|| file_name.to_string())
}

fn metadata_entry(
    title: &str,
    total_pages: Option<i64>,
    contents: &[ContentSpec],
) -> (Vec<u8>, String) {
    let contents_json: Vec<serde_json::Value> = contents
        .iter()
        .map(|content| {
            serde_json::json!({
                "contentId": content.content_id,
                "displayName": content.display_name,
                "mediaKind": content.media_kind.as_str(),
                "isPrimary": content.is_primary,
                "sortOrder": content.sort_order,
                "formats": content
                    .formats
                    .iter()
                    .map(|format| {
                        serde_json::json!({
                            "formatId": format.format_id,
                            "label": format.label,
                            "formatKind": format.kind.as_str(),
                            "pageCount": format.page_count,
                            "packEntryPrefix": format.pack_entry_prefix,
                            "sortOrder": format.sort_order,
                        })
                    })
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    let metadata = serde_json::json!({
        "schemaVersion": 1,
        "title": title,
        "author": "",
        "circleName": "",
        "purchaseDate": null,
        "readingProgress": { "currentPage": 0, "totalPages": total_pages },
        "contents": contents_json,
    });
    (
        serde_json::to_vec(&metadata).expect("metadata json"),
        "metadata.json".to_string(),
    )
}

/// `book_contents` に書く 1 コンテンツ分の行。
#[derive(Clone)]
struct ContentSpec {
    content_id: String,
    display_name: String,
    media_kind: MediaKind,
    is_primary: bool,
    sort_order: i64,
    formats: Vec<FormatSpec>,
}

/// `content_formats` に書く 1 レンディション分の行。
#[derive(Clone)]
struct FormatSpec {
    format_id: String,
    label: String,
    kind: MediaKind,
    page_count: i64,
    /// pack 内でこの形式のページが置かれる接頭辞（`pages` / `contents/1/r0`）。
    pack_entry_prefix: Option<String>,
    sort_order: i64,
}

/// 単体ファイル取り込み用の「1 コンテンツ + 1 レンディション」を作る。
fn single_content(
    media_kind: MediaKind,
    format_label: &str,
    page_count: i64,
    pack_entry_prefix: Option<&str>,
) -> ContentSpec {
    ContentSpec {
        content_id: uuid::Uuid::new_v4().to_string(),
        display_name: "本文".to_string(),
        media_kind,
        is_primary: true,
        sort_order: 0,
        formats: vec![FormatSpec {
            format_id: uuid::Uuid::new_v4().to_string(),
            label: format_label.to_string(),
            kind: media_kind,
            page_count,
            pack_entry_prefix: pack_entry_prefix.map(str::to_string),
            sort_order: 0,
        }],
    }
}

struct PackSpec {
    /// (entry path, data, mime, compress)
    entries: Vec<(String, Vec<u8>, String, bool)>,
    page_rows: Vec<PageRow>,
    /// (page_number, text) — PDF の抽出テキストや `_export.txt` の中身。
    texts: Vec<(i64, String)>,
    /// 読み飛ばしたエントリの説明（壊れた画像など）。
    warnings: Vec<String>,
    /// 永続化するコンテンツ構造（フェーズ2）。
    contents: Vec<ContentSpec>,
    source_type: String,
    /// 既定表示（primary）コンテンツのページ数。
    total_pages: i64,
}

struct PageRow {
    content_id: Option<String>,
    format_id: Option<String>,
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
    let (metadata, metadata_path) = metadata_entry(&title, Some(spec.total_pages), &spec.contents);
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
    // 再ダウンロード時は同じ book_id を再利用する（重複本を作らない）。
    // 既存行がある場合はユーザー状態を引き継いで置き換え、古いページ行を消してから入れる。
    let previous = reuse_book_id.and_then(|id| books::get(pool, id).ok().flatten());
    let book = match &previous {
        Some(prev) => {
            let mut book = book;
            book.is_favorite = prev.is_favorite;
            book.is_hidden = prev.is_hidden;
            book.created_at = prev.created_at.clone();
            book
        }
        None => book,
    };
    if previous.is_some() {
        documents::delete_for_book(pool, &book_id)?;
        contents::delete_for_book(pool, &book_id)?;
        books::upsert(pool, &book)?;
        log::info!("finish_import: books 更新（再取り込み）");
    } else {
        books::insert(pool, &book)?;
        log::info!("finish_import: books 挿入完了");
    }

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

    // コンテンツ構造（book_contents / content_formats）を保存する（フェーズ2）。
    // 再取り込み時は `contents::delete_for_book` で消してから入れ直す。
    let content_rows: Vec<contents::BookContent> = spec
        .contents
        .iter()
        .map(|content| contents::BookContent {
            content_id: content.content_id.clone(),
            book_id: book_id.clone(),
            display_name: content.display_name.clone(),
            media_kind: content.media_kind.as_str().to_string(),
            is_primary: i64::from(content.is_primary),
            sort_order: content.sort_order,
            created_at: timestamp.clone(),
        })
        .collect();
    let format_rows: Vec<contents::ContentFormat> = spec
        .contents
        .iter()
        .flat_map(|content| {
            content
                .formats
                .iter()
                .map(|format| contents::ContentFormat {
                    format_id: format.format_id.clone(),
                    content_id: content.content_id.clone(),
                    label: format.label.clone(),
                    format_kind: format.kind.as_str().to_string(),
                    page_count: format.page_count,
                    pack_entry_prefix: format.pack_entry_prefix.clone(),
                    sort_order: format.sort_order,
                    created_at: timestamp.clone(),
                })
        })
        .collect();
    contents::insert_batch(pool, &content_rows, &format_rows)?;
    log::info!(
        "finish_import: contents 挿入完了（{} コンテンツ / {} レンディション）",
        content_rows.len(),
        format_rows.len()
    );

    // 画像・テキスト・トークンをバッチで一括 INSERT する
    // （1 件 1 クエリだと数百ページ × トークン数万件の block_on が重く、
    //   取り込みが遅くなるため。トランザクション + バッチに集約する）。
    let mut image_rows = Vec::with_capacity(spec.page_rows.len() + 1);
    for row in &spec.page_rows {
        image_rows.push(documents::DocumentImage {
            id: uuid::Uuid::new_v4().to_string(),
            document_id: document.id.clone(),
            content_id: row.content_id.clone(),
            format_id: row.format_id.clone(),
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
    // サムネイル行は既定表示コンテンツのものとして紐づける
    let primary_ids = spec
        .contents
        .iter()
        .find(|content| content.is_primary)
        .map(|content| {
            (
                content.content_id.clone(),
                content
                    .formats
                    .first()
                    .map(|format| format.format_id.clone()),
            )
        });
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
            content_id: primary_ids
                .as_ref()
                .map(|(content_id, _)| content_id.clone()),
            format_id: primary_ids
                .as_ref()
                .and_then(|(_, format_id)| format_id.clone()),
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
    for (page_number, text) in &spec.texts {
        text_rows.push(documents::DocumentText {
            id: uuid::Uuid::new_v4().to_string(),
            document_id: document.id.clone(),
            page_number: *page_number,
            text_content: text.clone(),
            created_at: timestamp.clone(),
        });
    }
    documents::insert_texts_batch(pool, &text_rows)?;
    log::info!("finish_import: texts バッチ挿入完了");

    // Token analysis rows (nouns per page).
    let mut token_rows = Vec::new();
    for (page_number, text) in &spec.texts {
        for (word, count) in crate::tags::extract_nouns(text, &[&title]) {
            token_rows.push(documents::TokenRow {
                id: uuid::Uuid::new_v4().to_string(),
                document_id: document.id.clone(),
                page_number: *page_number,
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
    let text_refs: Vec<&str> = spec.texts.iter().map(|(_, text)| text.as_str()).collect();
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
        warnings: spec.warnings,
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
        "pdf" => import_pdf_bytes(
            pool, &file_name, &bytes, packs_dir, identity, progress, None,
        ),
        "epub" => import_epub_bytes(pool, &file_name, &bytes, packs_dir, identity, None),
        "zip" => import_zip_bytes(
            pool, &file_name, &bytes, packs_dir, identity, progress, None,
        ),
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
    reuse_book_id: Option<&str>,
) -> Result<ImportedBook, ImportError> {
    let pages = pdf::render_pdf_pages(bytes, progress)?;
    import_rendered_pdf_pages(
        pool,
        file_name,
        bytes.len() as i64,
        pages,
        packs_dir,
        identity,
        reuse_book_id,
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
    reuse_book_id: Option<&str>,
) -> Result<ImportedBook, ImportError> {
    if pages.is_empty() {
        return Err(ImportError::Pdf("no pages rendered".into()));
    }
    let total_pages = pages.len() as i64;
    let content = single_content(MediaKind::Pdf, "PDF", total_pages, Some("pages"));
    let content_id = content.content_id.clone();
    let format_id = content.formats[0].format_id.clone();
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
            content_id: Some(content_id.clone()),
            format_id: Some(format_id.clone()),
            page_number: index as i64 + 1,
            width: page.width as i64,
            height: page.height as i64,
            entry_path,
            file_size: page.data.len() as i64,
            text: Some(page.text.clone()),
        });
        texts.push((index as i64 + 1, page.text.clone()));
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
            warnings: Vec::new(),
            contents: vec![content],
            source_type: "pdf".to_string(),
            total_pages,
        },
        reuse_book_id,
    )
}

/// Import an EPUB as a single raw entry (viewing not supported — same as Web).
pub fn import_epub_bytes(
    pool: &SqlitePool,
    file_name: &str,
    bytes: &[u8],
    packs_dir: &Path,
    identity: Option<&Identity>,
    reuse_book_id: Option<&str>,
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
            warnings: Vec::new(),
            contents: vec![single_content(MediaKind::Epub, "EPUB", 0, None)],
            source_type: "epub".to_string(),
            total_pages: 0,
        },
        reuse_book_id,
    )
}

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
            at.len().cmp(&bt.len()).then_with(|| at.cmp(bt))
        } else {
            ia.1.cmp(&ib.1)
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    ka.len().cmp(&kb.len())
}

/// 既定の優先コンテンツを選ぶ（表紙・junk は `build_contents` で除外済み）。
///
/// 1. 名前に「本文」系の語を含むもの
/// 2. ページ数の目安が最大のもの
/// 3. 同数なら PDF / EPUB を優先、最後は索引順
fn choose_primary(contents: &[PlannedContent]) -> usize {
    // ビューアで読める種別だけを候補にする（音声・動画は取り込めない）
    let candidates: Vec<usize> = contents
        .iter()
        .enumerate()
        .filter(|(_, content)| is_viewable_media(content.media_kind))
        .map(|(index, _)| index)
        .collect();
    let Some(&fallback) = candidates.first() else {
        return 0;
    };
    if let Some(&body) = candidates
        .iter()
        .find(|&&index| is_body_name(&contents[index].display_name))
    {
        return body;
    }
    let mut best = fallback;
    for &index in &candidates {
        let pages = contents[index].page_hint();
        let best_pages = contents[best].page_hint();
        let prefer_media = matches!(contents[index].media_kind, MediaKind::Pdf | MediaKind::Epub)
            && !matches!(contents[best].media_kind, MediaKind::Pdf | MediaKind::Epub);
        if pages > best_pages || (pages == best_pages && prefer_media) {
            best = index;
        }
    }
    best
}

/// 現行ビューアで読めるメディア種別か（コンテンツの候補・取り込み対象）。
fn is_viewable_media(kind: MediaKind) -> bool {
    matches!(kind, MediaKind::Image | MediaKind::Pdf | MediaKind::Epub)
}

/// PDF を描画する。`progress` が `Some` のときだけ進捗を流す
/// （`progress` の型がプラットフォームで違うため、ここで吸収する）。
fn render_pdf_with(
    bytes: &[u8],
    progress: Option<&mut (dyn FnMut(f32) + Send)>,
) -> Result<Vec<pdf::PageImage>, ImportError> {
    match progress {
        Some(progress) => pdf::render_pdf_pages(bytes, progress),
        None => pdf::render_pdf_pages(bytes, &mut |_| {}),
    }
}

/// 名前に「本文」系の語を含むか（docs/import-patterns.md §5.1 の語彙）。
fn is_body_name(name: &str) -> bool {
    name.contains("本文") || name.contains("本編")
}

/// ZIP を解析して取り込み計画を立てる。DB にもディスクにも書かない。
pub fn analyze_zip(bytes: &[u8]) -> Result<ImportPlan, ImportError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| ImportError::Zip(e.to_string()))?;
    // 1 周目: 名前だけを集める（ページ画像は伸長しない）。
    let metas = collect_entry_metas(&mut archive)?;
    if metas.is_empty() {
        return Err(ImportError::EmptyArchive);
    }
    let contents = build_contents(&metas);
    let primary = choose_primary(&contents);

    // `_export.txt` は本文テキストなので解析時に読む（小さく、画像の伸長は伴わない）。
    let mut warnings = Vec::new();
    let mut export_texts: Vec<(i64, String)> = Vec::new();
    for meta in metas
        .iter()
        .filter(|meta| classify_entry(&meta.name) == EntryKind::ExportText)
    {
        match read_zip_entry(&mut archive, meta.index) {
            Ok(data) => {
                let decoded = zip_names::decode_text_bytes(&data);
                export_texts.extend(export_text::parse_export_text(&decoded));
            }
            Err(error) => warnings.push(format!("{}: {error}", meta.name)),
        }
    }

    let skip_reason = contents.is_empty().then_some(SkipReason::NotAReadableWork);
    Ok(ImportPlan {
        contents,
        primary,
        export_text: export_texts,
        warnings,
        skip_reason,
    })
}

/// `ImportPlan` の全コンテンツ／全レンディションを実際に取り込み、pack と DB を作る。
///
/// pack 内のパスは、既定表示コンテンツの第 1 レンディションだけ従来どおり
/// `pages/page_NNNN.webp` に置く（既存 pack・カバー規約との互換）。それ以外は
/// `contents/{content}/{rendition}/...` に入れる。
// 引数は取り込みの文脈そのもの（plan を足すと 8 個になる）。分割すると呼び出し側で
// 束ね直すだけなので、そのまま受け取る。
#[allow(clippy::too_many_arguments)]
pub fn commit_zip(
    pool: &SqlitePool,
    file_name: &str,
    bytes: &[u8],
    packs_dir: &Path,
    identity: Option<&Identity>,
    progress: &mut (dyn FnMut(f32) + Send),
    reuse_book_id: Option<&str>,
    plan: &ImportPlan,
) -> Result<ImportedBook, ImportError> {
    let unsupported = || ImportError::UnsupportedType("zip without pdf/epub/images".into());
    let primary = plan.contents.get(plan.primary).ok_or_else(unsupported)?;
    if !is_viewable_media(primary.media_kind) {
        // 音声・動画は現行ビューアの対象外（docs/import-patterns.md §3.3）
        return Err(unsupported());
    }
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| ImportError::Zip(e.to_string()))?;
    let metas = collect_entry_metas(&mut archive)?;

    // 2 周目: 選んだエントリだけを 1 件ずつ読む。
    // 旧実装は全エントリを `Vec<(String, Vec<u8>)>` に読み込んでいたため、
    // 巨大 ZIP（実データ最大 1.19GB / 展開後 1.33GB）で展開後のデータを
    // 同時に保持していた。ここでは 1 件ずつ伸長して使い終わったら捨てる。
    let mut pack_entries: Vec<(String, Vec<u8>, String, bool)> = Vec::new();
    let mut page_rows: Vec<PageRow> = Vec::new();
    let mut warnings = plan.warnings.clone();
    let mut contents_spec: Vec<ContentSpec> = Vec::new();
    let mut primary_pages = 0i64;
    let mut primary_thumbnail: Option<Vec<u8>> = None;
    let mut primary_cover: Option<Vec<u8>> = None;
    // ZIP に PDF / EPUB が 1 つだけ入っている場合は、その中身を本の実体として
    // 扱う（題名・ファイル名を内側のエントリ名から採る。フェーズ1の仕様）。
    let mut book_file_name = file_name.to_string();

    for (content_index, content) in plan.contents.iter().enumerate() {
        let is_primary = content_index == plan.primary;
        let content_id = uuid::Uuid::new_v4().to_string();
        let mut formats = Vec::new();
        for (rendition_index, rendition) in content.renditions.iter().enumerate() {
            let format_id = uuid::Uuid::new_v4().to_string();
            // 既定表示コンテンツの第 1 レンディションだけ従来のパスに置く
            let legacy = is_primary && rendition_index == 0;
            let prefix = if legacy {
                "pages".to_string()
            } else {
                format!("contents/{content_index}/r{rendition_index}")
            };
            let page_count = match rendition.kind {
                MediaKind::Image => {
                    let start = page_rows.len();
                    for ordinal in &rendition.entries {
                        let meta = metas.get(*ordinal).ok_or_else(unsupported)?;
                        let data = read_zip_entry(&mut archive, meta.index)?;
                        // 壊れた画像 1 枚で全体を失敗させない（決定 D6）
                        let (webp, width, height) = match render_page_image(&data) {
                            Ok(rendered) => rendered,
                            Err(error) => {
                                warnings.push(format!("{}: {error}", meta.name));
                                continue;
                            }
                        };
                        if legacy && primary_thumbnail.is_none() {
                            primary_thumbnail = Some(webp.clone());
                        }
                        let page_number = (page_rows.len() - start) as i64 + 1;
                        let entry_path = format!("{prefix}/page_{page_number:04}.webp");
                        pack_entries.push((
                            entry_path.clone(),
                            webp.clone(),
                            "image/webp".to_string(),
                            false,
                        ));
                        page_rows.push(PageRow {
                            content_id: Some(content_id.clone()),
                            format_id: Some(format_id.clone()),
                            page_number,
                            width: width as i64,
                            height: height as i64,
                            entry_path,
                            file_size: webp.len() as i64,
                            text: None,
                        });
                    }
                    (page_rows.len() - start) as i64
                }
                MediaKind::Pdf => {
                    let ordinal = rendition.entries.first().ok_or_else(unsupported)?;
                    let meta = metas.get(*ordinal).ok_or_else(unsupported)?;
                    let data = read_zip_entry(&mut archive, meta.index)?;
                    // 進捗は既定表示コンテンツの PDF だけに流す（他は描画の副作用を避ける）
                    let pages = if legacy {
                        render_pdf_with(&data, Some(&mut *progress))?
                    } else {
                        render_pdf_with(&data, None)?
                    };
                    for (index, page) in pages.iter().enumerate() {
                        let page_number = index as i64 + 1;
                        let entry_path = format!("{prefix}/page_{page_number:04}.webp");
                        pack_entries.push((
                            entry_path.clone(),
                            page.data.clone(),
                            "image/webp".to_string(),
                            false,
                        ));
                        page_rows.push(PageRow {
                            content_id: Some(content_id.clone()),
                            format_id: Some(format_id.clone()),
                            page_number,
                            width: page.width as i64,
                            height: page.height as i64,
                            entry_path,
                            file_size: page.data.len() as i64,
                            text: Some(page.text.clone()),
                        });
                    }
                    if legacy {
                        book_file_name = entry_file_name(&meta.name).to_string();
                        primary_thumbnail = pages.first().map(|page| page.data.clone());
                        primary_cover = pages.first().map(|page| page.data.clone());
                    }
                    pages.len() as i64
                }
                MediaKind::Epub => {
                    let ordinal = rendition.entries.first().ok_or_else(unsupported)?;
                    let meta = metas.get(*ordinal).ok_or_else(unsupported)?;
                    let data = read_zip_entry(&mut archive, meta.index)?;
                    let entry_path = if legacy {
                        book_file_name = entry_file_name(&meta.name).to_string();
                        entry_file_name(&meta.name).to_string()
                    } else {
                        format!("{prefix}/{}", entry_file_name(&meta.name))
                    };
                    pack_entries.push((
                        entry_path,
                        data,
                        "application/epub+zip".to_string(),
                        false,
                    ));
                    0
                }
                // 音声・動画はページを持たない（構造だけ記録する）
                MediaKind::Audio | MediaKind::Video => 0,
            };
            if legacy {
                primary_pages = page_count;
            }
            formats.push(FormatSpec {
                format_id,
                label: rendition.label.clone(),
                kind: rendition.kind,
                page_count,
                pack_entry_prefix: Some(prefix),
                sort_order: rendition_index as i64,
            });
        }
        contents_spec.push(ContentSpec {
            content_id,
            display_name: content.display_name.clone(),
            media_kind: content.media_kind,
            is_primary,
            sort_order: content_index as i64,
            formats,
        });
    }

    // 既定表示コンテンツが読めるページを持っていること（EPUB はページ列を持たない）
    if primary_pages == 0 && primary.media_kind != MediaKind::Epub {
        return Err(ImportError::Image(format!(
            "primary content has no pages: {}",
            primary.display_name
        )));
    }
    if let Some(thumbnail_source) = &primary_thumbnail {
        if let Some(cover_source) = &primary_cover {
            pack_entries.push((
                "cover.webp".to_string(),
                cover_source.clone(),
                "image/webp".to_string(),
                false,
            ));
        }
        let (thumb, _, _) = thumbnail_of(thumbnail_source)?;
        pack_entries.push((
            "thumbnail.webp".to_string(),
            thumb,
            "image/webp".to_string(),
            false,
        ));
    }

    let source_type = match primary.media_kind {
        MediaKind::Pdf => "pdf",
        MediaKind::Epub => "epub",
        _ => "image-set",
    };
    finish_import(
        pool,
        packs_dir,
        identity,
        &book_file_name,
        bytes.len() as i64,
        PackSpec {
            entries: pack_entries,
            page_rows,
            texts: plan.export_text.clone(),
            warnings,
            contents: contents_spec,
            source_type: source_type.to_string(),
            total_pages: primary_pages,
        },
        reuse_book_id,
    )
}

/// Import a ZIP: `analyze_zip` で計画を立て、既定の優先コンテンツを取り込む。
pub fn import_zip_bytes(
    pool: &SqlitePool,
    file_name: &str,
    bytes: &[u8],
    packs_dir: &Path,
    identity: Option<&Identity>,
    progress: &mut (dyn FnMut(f32) + Send),
    reuse_book_id: Option<&str>,
) -> Result<ImportedBook, ImportError> {
    let plan = analyze_zip(bytes)?;
    commit_zip(
        pool,
        file_name,
        bytes,
        packs_dir,
        identity,
        progress,
        reuse_book_id,
        &plan,
    )
}

/// pack（`.opfspack`）から DB の取り込み状態（`imported_documents` / `book_contents` /
/// `content_formats` / `document_images`）を再構築する（Drive 復元用）。
///
/// - すでにその本のドキュメント行があるときは何もしない（ローカルの取り込みを壊さない）
/// - 構造は pack の `metadata.json` の `contents`（フェーズ2で書き出し）を使い、
///   無い場合はエントリから 1 コンテンツとして推定する
/// - ページ画像の寸法はエントリのヘッダから読む（画素デコードはしない）
/// - 戻り値は再構築したかどうか
pub fn rebuild_from_pack(
    pool: &SqlitePool,
    pack_id: &str,
    pack_bytes: &[u8],
    identity: Option<&Identity>,
) -> Result<bool, ImportError> {
    if documents::get_document_by_book_id(pool, pack_id)?.is_some() {
        return Ok(false);
    }
    let reader = opfspack::PackReader::open(pack_bytes)?;
    let timestamp = now();
    let entry_paths: Vec<String> = reader.entries().iter().map(|e| e.path.clone()).collect();

    // metadata.json から題名と構造を読む
    let metadata: Option<serde_json::Value> = reader
        .read_entry("metadata.json", identity)
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok());
    let title = metadata
        .as_ref()
        .and_then(|value| value.get("title").and_then(|v| v.as_str()))
        .unwrap_or(pack_id)
        .to_string();
    let mut contents: Vec<ContentSpec> = metadata
        .as_ref()
        .and_then(|value| value.get("contents"))
        .and_then(|value| value.as_array())
        .map(|list| list.iter().filter_map(content_from_metadata).collect())
        .unwrap_or_default();
    if contents.is_empty() {
        contents = infer_contents(&title, &entry_paths);
    }
    if contents.is_empty() {
        return Ok(false); // ページもコンテンツも無い pack は復元しない
    }
    let primary = contents
        .iter()
        .find(|content| content.is_primary)
        .cloned()
        .or_else(|| contents.first().cloned())
        .expect("contents is not empty");

    // 画像行（ページ + サムネイル）。document 行は最後に作るが、行の document_id は先に採番する
    let document_id = uuid::Uuid::new_v4().to_string();
    let mut image_rows: Vec<documents::DocumentImage> = Vec::new();
    let mut primary_pages = 0i64;
    for entry in reader.entries() {
        let path = &entry.path;
        if path == "metadata.json" {
            continue;
        }
        let Ok(data) = reader.read_entry(path, identity) else {
            continue;
        };
        let (width, height) = image_dimensions(&data);
        if path == "thumbnail.webp" {
            image_rows.push(document_image_row(
                &document_id,
                pack_id,
                &primary.content_id,
                primary
                    .formats
                    .first()
                    .map(|format| format.format_id.clone()),
                "thumbnail",
                1,
                path,
                width,
                height,
                data.len() as i64,
                timestamp.clone(),
            ));
            continue;
        }
        if path == "cover.webp" {
            continue; // カバーは pack 側のエントリで完結（行は作らない）
        }
        let Some((content, format)) = contents.iter().find_map(|content| {
            content
                .formats
                .iter()
                .find(|format| {
                    format
                        .pack_entry_prefix
                        .as_deref()
                        .is_some_and(|prefix| path.starts_with(prefix))
                })
                .map(|format| (content, format))
        }) else {
            continue; // EPUB の生エントリなど
        };
        let Some(page_number) = page_number_of(path) else {
            continue;
        };
        let is_primary_format = content.content_id == primary.content_id
            && primary
                .formats
                .first()
                .is_some_and(|first| first.format_id == format.format_id);
        if is_primary_format {
            primary_pages = primary_pages.max(page_number);
        }
        image_rows.push(document_image_row(
            &document_id,
            pack_id,
            &content.content_id,
            Some(format.format_id.clone()),
            "page",
            page_number,
            path,
            width,
            height,
            data.len() as i64,
            timestamp.clone(),
        ));
    }
    if primary_pages == 0 {
        return Ok(false);
    }

    let source_type = match primary.media_kind {
        MediaKind::Pdf => "pdf",
        MediaKind::Epub => "epub",
        _ => "image-set",
    };
    let document = documents::ImportedDocument {
        id: document_id.clone(),
        book_id: pack_id.to_string(),
        source_type: source_type.to_string(),
        file_hash: sha256_hex(pack_bytes),
        total_pages: primary_pages,
        metadata: None,
        status: "completed".to_string(),
        created_at: timestamp.clone(),
        updated_at: timestamp.clone(),
    };
    documents::insert_document(pool, &document)?;

    let content_rows: Vec<contents::BookContent> = contents
        .iter()
        .map(|content| contents::BookContent {
            content_id: content.content_id.clone(),
            book_id: pack_id.to_string(),
            display_name: content.display_name.clone(),
            media_kind: content.media_kind.as_str().to_string(),
            is_primary: i64::from(content.is_primary),
            sort_order: content.sort_order,
            created_at: timestamp.clone(),
        })
        .collect();
    let format_rows: Vec<contents::ContentFormat> = contents
        .iter()
        .flat_map(|content| {
            content
                .formats
                .iter()
                .map(|format| contents::ContentFormat {
                    format_id: format.format_id.clone(),
                    content_id: content.content_id.clone(),
                    label: format.label.clone(),
                    format_kind: format.kind.as_str().to_string(),
                    page_count: format.page_count,
                    pack_entry_prefix: format.pack_entry_prefix.clone(),
                    sort_order: format.sort_order,
                    created_at: timestamp.clone(),
                })
        })
        .collect();
    contents::insert_batch(pool, &content_rows, &format_rows)?;
    documents::insert_images_batch(pool, &image_rows)?;
    log::info!(
        "drive restore: pack から再構築（{pack_id}: {} コンテンツ / {} ページ）",
        content_rows.len(),
        primary_pages
    );
    Ok(true)
}

/// `metadata.json` の 1 コンテンツ分を復元する。
fn content_from_metadata(value: &serde_json::Value) -> Option<ContentSpec> {
    let content_id = value.get("contentId")?.as_str()?.to_string();
    let media_kind = media_kind_from_str(value.get("mediaKind")?.as_str()?)?;
    let formats = value
        .get("formats")
        .and_then(|value| value.as_array())
        .map(|formats| {
            formats
                .iter()
                .filter_map(|format| {
                    Some(FormatSpec {
                        format_id: format.get("formatId")?.as_str()?.to_string(),
                        label: format
                            .get("label")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        kind: media_kind_from_str(format.get("formatKind")?.as_str()?)?,
                        page_count: format
                            .get("pageCount")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0),
                        pack_entry_prefix: format
                            .get("packEntryPrefix")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        sort_order: format
                            .get("sortOrder")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Some(ContentSpec {
        content_id,
        display_name: value
            .get("displayName")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        media_kind,
        is_primary: value
            .get("isPrimary")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        sort_order: value.get("sortOrder").and_then(|v| v.as_i64()).unwrap_or(0),
        formats,
    })
}

/// `contents` を持たない古い pack から 1 コンテンツを推定する。
fn infer_contents(title: &str, entry_paths: &[String]) -> Vec<ContentSpec> {
    let pages: Vec<&String> = entry_paths
        .iter()
        .filter(|path| page_number_of(path).is_some())
        .collect();
    let raw = entry_paths
        .iter()
        .find(|path| path.ends_with(".epub") || (path.ends_with(".pdf") && !path.contains('/')));
    let (media_kind, prefix) = if !pages.is_empty() {
        (MediaKind::Image, "pages")
    } else if let Some(path) = raw {
        if path.ends_with(".epub") {
            (MediaKind::Epub, "")
        } else {
            (MediaKind::Pdf, "")
        }
    } else {
        return Vec::new();
    };
    let page_count = pages.len() as i64;
    vec![ContentSpec {
        content_id: uuid::Uuid::new_v4().to_string(),
        display_name: title.to_string(),
        media_kind,
        is_primary: true,
        sort_order: 0,
        formats: vec![FormatSpec {
            format_id: uuid::Uuid::new_v4().to_string(),
            label: media_kind.label().to_string(),
            kind: media_kind,
            page_count,
            pack_entry_prefix: if prefix.is_empty() {
                None
            } else {
                Some(prefix.to_string())
            },
            sort_order: 0,
        }],
    }]
}

fn media_kind_from_str(value: &str) -> Option<MediaKind> {
    match value {
        "image" => Some(MediaKind::Image),
        "pdf" => Some(MediaKind::Pdf),
        "epub" => Some(MediaKind::Epub),
        "audio" => Some(MediaKind::Audio),
        "video" => Some(MediaKind::Video),
        _ => None,
    }
}

/// `pages/page_0001.webp` / `contents/0/r1/page_0012.webp` からページ番号を取る。
fn page_number_of(path: &str) -> Option<i64> {
    let name = path.rsplit('/').next()?;
    let stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name);
    stem.strip_prefix("page_")?.parse().ok()
}

/// 画像の寸法をヘッダから読む（画素はデコードしない）。
fn image_dimensions(data: &[u8]) -> (i64, i64) {
    image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok())
        .map(|(width, height)| (width as i64, height as i64))
        .unwrap_or((0, 0))
}

/// 再構築した 1 行分（ページ / サムネイル）。
#[allow(clippy::too_many_arguments)]
fn document_image_row(
    document_id: &str,
    pack_id: &str,
    content_id: &str,
    format_id: Option<String>,
    image_type: &str,
    page_number: i64,
    entry_path: &str,
    width: i64,
    height: i64,
    file_size: i64,
    timestamp: String,
) -> documents::DocumentImage {
    documents::DocumentImage {
        id: uuid::Uuid::new_v4().to_string(),
        document_id: document_id.to_string(),
        content_id: Some(content_id.to_string()),
        format_id,
        page_number,
        image_type: image_type.to_string(),
        opfs_path: format!("{pack_id}.opfspack"),
        width,
        height,
        mime_type: "image/webp".to_string(),
        file_size,
        extracted_text: None,
        pack_entry_path: Some(entry_path.to_string()),
        created_at: timestamp,
    }
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
    let content = single_content(MediaKind::Image, "画像", 1, Some("pages"));
    let content_id = content.content_id.clone();
    let format_id = content.formats[0].format_id.clone();
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
                content_id: Some(content_id),
                format_id: Some(format_id),
                page_number: 1,
                width: width as i64,
                height: height as i64,
                entry_path: "pages/page_0001.webp".to_string(),
                file_size: webp.len() as i64,
                text: None,
            }],
            texts: Vec::new(),
            warnings: Vec::new(),
            contents: vec![content],
            source_type: "image".to_string(),
            total_pages: 1,
        },
        None,
    )
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
        assert_eq!(
            natural_key("page10.jpg"),
            vec![
                (false, "page".into()),
                (true, "10".into()),
                (false, ".jpg".into())
            ]
        );
    }
}
