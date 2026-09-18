//! PDF rendering: PDFium（実行時ロードの dylib）-> webp pages + text extraction.
//!
//! 以前は macOS / Linux で mupdf をソースビルドして使っていた（Windows だけ
//! PDFium）。mupdf は **AGPL-3.0** で、MIT で配布するアプリに同梱すると
//! ライセンスが両立しないため、全プラットフォームで PDFium（BSD-3-Clause /
//! Apache-2.0）に統一した。
//!
//! PDFium は**実行時**にライブラリをロードする。ビルド時にリンクしないのは、
//! pdfium-binaries の prebuilt static ライブラリが macOS で `FPDF_FORMFILL` を
//! 欠いていてリンクできないため（この経路ならその失敗を踏まない）。
//! ライブラリの探索先はテスト実行（CWD）・実行ファイルの隣・macOS の
//! `.app/Contents/Frameworks`。
//!
//! PDFium はプロセスで 1 つのライブラリ状態（フォントキャッシュ等）を共有し、
//! **同時利用がスレッドセーフではない**。`pdfium-render` の `thread_safe` feature は
//! `unsafe impl Send/Sync for Pdfium` を足すだけでロックはしない（呼び出し側の責任）。
//! 実測: テキスト入りの PDF を複数スレッドで同時にレンダリングすると
//! `STATUS_ACCESS_VIOLATION` (0xc0000005) でプロセスが落ちる。スレッドごとに
//! `load_pdf_from_byte_slice` し直しても再現する（＝ドキュメントを分けてもダメ）。
//! そのため PDFium を使う区間はこの Mutex で直列化する。1 冊の中の描画は元々
//! 逐次なので、単冊の速度は変わらない（並行に複数冊を取り込むときだけ待ち合う）。

use std::path::PathBuf;
use std::sync::Mutex;

use pdfium_render::prelude::*;

use crate::import::ImportError;

pub struct PageImage {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub text: String,
}

/// PDFium のライブラリを探す候補（プラットフォームごとのファイル名は
/// `pdfium_platform_library_name_at_path` が解決する）。
fn library_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![
        // テスト実行時（cargo test）は CWD が crate ルート。
        Pdfium::pdfium_platform_library_name_at_path("./"),
    ];
    if let Ok(exe) = std::env::current_exe()
        && let Some(dir) = exe.parent()
    {
        // アプリ実行時は実行ファイルの隣（Windows の配布レイアウト）。
        candidates.push(Pdfium::pdfium_platform_library_name_at_path(dir));
        // macOS の .app は Contents/Frameworks に置く。
        candidates.push(Pdfium::pdfium_platform_library_name_at_path(
            &dir.join("../Frameworks"),
        ));
    }
    candidates
}

/// プロセスで 1 つだけ初期化して使い回す。
///
/// `pdfium-render` はグローバルな `BINDINGS` を 1 つしか持たず、`Pdfium::new` を
/// 2 回呼ぶと panic する（並列テスト・複数 PDF の取り込みで踏む）。
fn pdfium_instance() -> Result<&'static Pdfium, ImportError> {
    static PDFIUM: std::sync::LazyLock<Result<Pdfium, String>> = std::sync::LazyLock::new(|| {
        let candidates = library_candidates();
        candidates
            .iter()
            .find_map(|candidate| Pdfium::bind_to_library(candidate).ok())
            .map(Pdfium::new)
            .ok_or_else(|| {
                let list = candidates
                    .iter()
                    .map(|p| p.display().to_string())
                    .collect::<Vec<_>>()
                    .join(" / ");
                format!("PDFium のライブラリをロードできませんでした（検索: {list}）")
            })
    });
    PDFIUM.as_ref().map_err(|e| ImportError::Pdf(e.clone()))
}

/// PDFium のライブラリが実際に見つかっているか（テストの前提判定用）。
///
/// 見つからない環境（ライブラリをまだ取得していない開発機）では PDF を描画する
/// テストをスキップするために使う。取得は `mise run pdfium`。
pub fn pdfium_library_path() -> Option<PathBuf> {
    library_candidates().into_iter().find(|path| path.exists())
}

static PDFIUM_LOCK: Mutex<()> = Mutex::new(());

/// Render every page of a PDF to webp (q80) at a target width of 1000px
/// and extract the page text. `progress` receives 0..=1.
pub fn render_pdf_pages(
    bytes: &[u8],
    progress: &mut (dyn FnMut(f32) + Send),
) -> Result<Vec<PageImage>, ImportError> {
    log::info!("render_pdf_pages: 開始");
    let render_start = std::time::Instant::now();

    // PDFium の同時利用は未定義動作なので、初期化も含めてここから直列化する。
    let _pdfium_guard = PDFIUM_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let pdfium = pdfium_instance()?;
    let document = pdfium
        .load_pdf_from_byte_slice(bytes, None)
        .map_err(|e| ImportError::Pdf(e.to_string()))?;
    let total = document.pages().len().max(0) as usize;
    if total == 0 {
        return Ok(Vec::new());
    }

    // ページ幅を 1000px に（アスペクト比維持）。高さの上限は設けない
    // （縦長ページでも幅 1000px を維持する。`set_maximum_height` を入れると
    // 縦長ページで幅が縮み画質が落ちる）。
    let render_config = PdfRenderConfig::new().set_target_width(1000);
    // 8 ページずつ「描画（直列）→ 並列エンコード」を回す。全ページの生 RGB を
    // 溜めてから一括でエンコードすると 1 ページ約 4MB × ページ数になり、
    // 200 ページ級の本で 1GB を超える（旧 mupdf 経路はページごとにエンコードして
    // webp しか保持していなかった）。窓を切れば常駐は数ページ分で済む。
    const WINDOW: usize = 8;
    let done = std::sync::atomic::AtomicUsize::new(0);
    let progress = std::sync::Mutex::new(progress);
    let mut rendered = Vec::with_capacity(total);
    let mut first_error: Option<ImportError> = None;
    // ページのハンドルは 1 つ作って使い回す。`document.pages().iter().skip(n)` を窓ごとに
    // 作り直すと、手前のページを毎回ロードして捨てる（`PdfPagesIterator::next` が
    // `PdfPages::get` を呼ぶため）。ページ数 n に対して約 n²/2W 回の無駄ロードになる。
    let pages = document.pages();

    for window_start in (0..total).step_by(WINDOW) {
        let count = (window_start + WINDOW).min(total) - window_start;

        // 1) 描画は PDFium の制約で直列。生の RGB はこの窓の中だけ保持する。
        let mut raw: Vec<(u32, u32, Vec<u8>, String)> = Vec::with_capacity(count);
        for index in window_start..window_start + count {
            let page = pages
                .get(index as _)
                .map_err(|e| ImportError::Pdf(e.to_string()))?;
            let bitmap = page
                .render_with_config(&render_config)
                .map_err(|e| ImportError::Pdf(e.to_string()))?;
            let image = bitmap
                .as_image()
                .map_err(|e| ImportError::Pdf(e.to_string()))?
                .into_rgb8();
            let (width, height) = image.dimensions();
            let text = page.text().map(|t| t.to_string()).unwrap_or_default();
            raw.push((width, height, image.into_raw(), text));
        }

        // 2) WebP エンコードは PDFium を触らないので並列に流す（ここを直列にすると
        //    8 並列だった mupdf 経路より体感で数倍遅くなる）。生バッファは
        //    スレッドへ**所有権ごと**渡すので、追加のコピーは作らない。
        let results: std::sync::Mutex<Vec<Option<Result<PageImage, String>>>> =
            std::sync::Mutex::new((0..count).map(|_| None).collect());
        std::thread::scope(|scope| {
            for (offset, (width, height, pixels, text)) in raw.into_iter().enumerate() {
                let results = &results;
                let done = &done;
                let progress = &progress;
                scope.spawn(move || {
                    let result = image::RgbImage::from_raw(width, height, pixels)
                        .ok_or_else(|| "invalid rgb buffer".to_string())
                        .and_then(|rgb| {
                            crate::import::encode_webp(&image::DynamicImage::ImageRgb8(rgb), 80)
                                .map_err(|e| e.to_string())
                        })
                        .map(|data| PageImage {
                            data,
                            width,
                            height,
                            text,
                        });
                    results.lock().unwrap()[offset] = Some(result);
                    let finished = done.fetch_add(1, std::sync::atomic::Ordering::SeqCst) + 1;
                    if let Ok(mut callback) = progress.lock() {
                        callback(finished as f32 / total as f32);
                    }
                });
            }
        });

        for (offset, result) in results.into_inner().unwrap().into_iter().enumerate() {
            match result.and_then(|r| r.ok()) {
                Some(page) => rendered.push(page),
                None => {
                    let index = window_start + offset;
                    log::error!("render_pdf_pages: ページ {index} のエンコードに失敗");
                    if first_error.is_none() {
                        first_error = Some(ImportError::Pdf(format!(
                            "ページ {index} を画像に変換できませんでした"
                        )));
                    }
                }
            }
        }
        if first_error.is_some() {
            break;
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
