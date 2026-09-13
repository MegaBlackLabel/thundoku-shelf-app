//! PDF rendering: mupdf (source-built) -> webp pages + text extraction.
//! Isolated behind `render_pdf_pages` so the backend can be swapped without
//! touching the import pipeline. (pdfium-render's prebuilt static library
//! failed to link on macOS — missing FPDF_FORMFILL symbols — so the planned
//! mupdf contingency is in use.)

#[cfg(not(windows))]
use mupdf::{Colorspace, Device, Document, Matrix, Pixmap, TextPageFlags};

use crate::import::ImportError;

pub struct PageImage {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub text: String,
}

/// Windows クロスビルド（xwin）では mupdf をビルドできないため PDF レンダリング不可。
/// pdfium-render はグローバルな `BINDINGS` をプロセスで 1 つだけ持つため、
/// `Pdfium::new` は一度しか呼べない。`LazyLock` で一度だけ初期化して再利用する
/// （並列テスト・複数 PDF の取り込みで 2 回目に `Pdfium::new` すると panic する）。
#[cfg(windows)]
fn pdfium_instance() -> Result<&'static pdfium_render::prelude::Pdfium, ImportError> {
    use pdfium_render::prelude::*;
    static PDFIUM: std::sync::LazyLock<Result<Pdfium, String>> = std::sync::LazyLock::new(|| {
        // exe と同じディレクトリ、次いで CWD（./）から pdfium.dll を動的ロードする。
        // 配布時は exe と同じフォルダに pdfium.dll を置く。ビルド時にはライブラリ不要
        // （mupdf の AGPL と違い Apache-2.0 で MIT と互換）。
        let candidates = [
            // テスト実行時（cargo test）は CWD が crate ルート。
            Pdfium::pdfium_platform_library_name_at_path("./"),
            // アプリ実行時は exe と同じフォルダからロードする（配布レイアウト）。
            Pdfium::pdfium_platform_library_name_at_path(
                std::env::current_exe()
                    .ok()
                    .as_deref()
                    .and_then(|p| p.parent())
                    .unwrap_or_else(|| std::path::Path::new(".")),
            ),
        ];
        candidates
            .iter()
            .find_map(|candidate| Pdfium::bind_to_library(candidate).ok())
            .map(Pdfium::new)
            .ok_or_else(|| {
                format!(
                    "pdfium.dll をロードできませんでした（検索: {} / {}）",
                    candidates[0].display(),
                    candidates[1].display()
                )
            })
    });
    PDFIUM.as_ref().map_err(|e| ImportError::Pdf(e.clone()))
}

/// PDFium はプロセスで 1 つのライブラリ状態（フォントキャッシュ等）を共有し、
/// **同時利用がスレッドセーフではない**。`pdfium-render` の `thread_safe` feature は
/// `unsafe impl Send/Sync for Pdfium` を足すだけでロックはしない（呼び出し側の責任）。
///
/// 実測（Windows / pdfium-render 0.9.3）: テキスト入りの 1 ページ PDF を 8 スレッドで
/// 同時にレンダリングすると `STATUS_ACCESS_VIOLATION` (0xc0000005) でプロセスが落ちる。
/// スレッドごとに `load_pdf_from_byte_slice` し直しても再現する（＝ドキュメントを分けても
/// ダメ）。そのため PDFium を使う区間はこの Mutex で直列化する。
/// 1 冊の中の描画は元々このループが逐次なので、単冊の速度は変わらない（並行に複数冊を
/// 取り込むときだけ待ち合う）。
#[cfg(windows)]
static PDFIUM_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[cfg(windows)]
pub fn render_pdf_pages(
    bytes: &[u8],
    progress: &mut dyn FnMut(f32),
) -> Result<Vec<PageImage>, ImportError> {
    use pdfium_render::prelude::*;

    // PDFium の同時利用は未定義動作なので、初期化も含めてここから直列化する。
    let _pdfium_guard = PDFIUM_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let pdfium = pdfium_instance()?;
    let document = pdfium
        .load_pdf_from_byte_slice(bytes, None)
        .map_err(|e| ImportError::Pdf(e.to_string()))?;
    let total = document.pages().len();
    let total = total.max(0) as usize;
    if total == 0 {
        return Ok(Vec::new());
    }
    // mupdf と同じ解像度: ページ幅を 1000px に（アスペクト比維持）。
    // pdfium のピクセルは既定で sRGB、webp q80 は mupdf 経路と同じ encode_webp を使う。
    // mupdf と同じく高さ制限は設けない（縦長ページでも幅 1000px を維持し、
    // アスペクト比に応じて高さが決まる。set_maximum_height を入れると
    // 縦長ページで幅が縮み画質が下がるため）。
    let render_config = PdfRenderConfig::new().set_target_width(1000);
    let mut out = Vec::with_capacity(total);
    for (index, page) in document.pages().iter().enumerate() {
        let bitmap = page
            .render_with_config(&render_config)
            .map_err(|e| ImportError::Pdf(e.to_string()))?;
        let image = bitmap
            .as_image()
            .map_err(|e| ImportError::Pdf(e.to_string()))?
            .into_rgb8();
        let (width, height) = image.dimensions();
        let webp = crate::import::encode_webp(&image::DynamicImage::ImageRgb8(image), 80)?;
        let text = page.text().map(|t| t.to_string()).unwrap_or_default();
        out.push(PageImage {
            data: webp,
            width,
            height,
            text,
        });
        progress((index + 1) as f32 / total as f32);
    }
    Ok(out)
}

/// Render every page of a PDF to webp (q80) at a target width of 800px
/// and extract the page text. `progress` receives 0..=1.
#[cfg(not(windows))]
pub fn render_pdf_pages(
    bytes: &[u8],
    progress: &mut (dyn FnMut(f32) + Send),
) -> Result<Vec<PageImage>, ImportError> {
    log::info!("render_pdf_pages: 開始");
    let render_start = std::time::Instant::now();
    let document = Document::from_bytes(bytes, "application/pdf")
        .map_err(|e| ImportError::Pdf(e.to_string()))?;
    let count = document
        .page_count()
        .map_err(|e| ImportError::Pdf(e.to_string()))?;
    let total = count.max(0) as usize;
    if total == 0 {
        return Ok(Vec::new());
    }
    // ページ範囲を 4 チャンクに分割し、各スレッドが 1 回だけ Document を
    // パースして担当範囲をレンダリングする（ページごとにパースし直すと
    // 200 ページ超の PDF でパースのオーバーヘッドが並列化を打ち消す）。
    let results: std::sync::Mutex<Vec<Option<Result<PageImage, String>>>> =
        std::sync::Mutex::new((0..total).map(|_| None).collect());
    let done = std::sync::atomic::AtomicUsize::new(0);
    let progress = std::sync::Arc::new(std::sync::Mutex::new(progress));
    let chunk_size = total.div_ceil(8);
    std::thread::scope(|s| {
        for chunk in 0..8 {
            let start = chunk * chunk_size;
            let end = ((chunk + 1) * chunk_size).min(total);
            if start >= end {
                continue;
            }
            let bytes = bytes.to_vec();
            let results = &results;
            let done = &done;
            let progress = progress.clone();
            s.spawn(move || {
                let Ok(document) = Document::from_bytes(&bytes, "application/pdf") else {
                    return;
                };
                for index in start..end {
                    let result = render_page(&document, index as i32).map_err(|e| e.to_string());
                    results.lock().unwrap()[index] = Some(result);
                    let finished = done.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    if let Ok(mut callback) = progress.lock() {
                        callback(finished as f32 / total as f32);
                    }
                }
            });
        }
    });
    let mut first_error: Option<ImportError> = None;
    let mut rendered = Vec::with_capacity(total);
    for (index, result) in results.into_inner().unwrap().into_iter().enumerate() {
        match result.unwrap_or_else(|| Err("page renderer did not produce a result".to_string())) {
            Ok(page_image) => rendered.push(page_image),
            Err(message) => {
                if first_error.is_none() {
                    first_error = Some(ImportError::Pdf(message));
                }
                log::error!("render_pdf_pages: ページ {index} のレンダリング失敗");
            }
        }
    }
    if let Some(error) = first_error {
        return Err(error);
    }
    log::info!(
        "render_pdf_pages: 完了（{total} ページ / {:?}）",
        render_start.elapsed()
    );
    Ok(rendered)
}

/// 1 ページをレンダリングする（並列ワーカー用 — Document は共有）。
#[cfg(not(windows))]
fn render_page(document: &Document, index: i32) -> Result<PageImage, ImportError> {
    let page = document
        .load_page(index)
        .map_err(|e| ImportError::Pdf(e.to_string()))?;
    let bounds = page.bounds().map_err(|e| ImportError::Pdf(e.to_string()))?;
    let width = (bounds.x1 - bounds.x0).max(1.0);
    let height = (bounds.y1 - bounds.y0).max(1.0);
    let scale = 1000.0f32 / width;
    let pixel_w = (width * scale).round().max(1.0) as i32;
    let pixel_h = (height * scale).round().max(1.0) as i32;
    let matrix = Matrix::new_scale(scale, scale);
    // mupdf の Pixmap::new はピクセルを初期化しない（"will contain crap
    // data"）。page.run はページの内容だけを描画するため、背景・余白部分に
    // 前のページの残骸（未初期化メモリ）が残り、画像が重なって見える。
    // 描画前に白でクリアしてこれを防ぐ。
    let mut pixmap = Pixmap::new(&Colorspace::device_rgb(), 0, 0, pixel_w, pixel_h, false)
        .map_err(|e| ImportError::Pdf(e.to_string()))?;
    pixmap
        .clear_with(255)
        .map_err(|e| ImportError::Pdf(e.to_string()))?;
    let device = Device::from_pixmap(&pixmap).map_err(|e| ImportError::Pdf(e.to_string()))?;
    page.run(&device, &matrix)
        .map_err(|e| ImportError::Pdf(e.to_string()))?;
    let rgb = pixmap.samples().to_vec();
    let rgb_image = image::RgbImage::from_raw(pixel_w as u32, pixel_h as u32, rgb)
        .ok_or_else(|| ImportError::Pdf("invalid pixmap dimensions".into()))?;
    let webp = crate::import::encode_webp(&image::DynamicImage::ImageRgb8(rgb_image), 80)?;

    let text = page
        .to_text_page(TextPageFlags::PRESERVE_WHITESPACE | TextPageFlags::PRESERVE_LIGATURES)
        .and_then(|text_page| text_page.to_text())
        .unwrap_or_default();

    Ok(PageImage {
        data: webp,
        // レンダリング後の実サイズ（スケール前のポイント値ではなく）
        width: pixel_w as u32,
        height: pixel_h as u32,
        text,
    })
}
