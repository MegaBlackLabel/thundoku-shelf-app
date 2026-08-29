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
#[cfg(windows)]
pub fn render_pdf_pages(
    _bytes: &[u8],
    _progress: &mut dyn FnMut(f32),
) -> Result<Vec<PageImage>, ImportError> {
    Err(ImportError::UnsupportedType(
        "pdf rendering is not available on the Windows build".into(),
    ))
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
