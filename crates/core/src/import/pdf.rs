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
///
/// **カレントディレクトリ（`./`）は候補にしない。** Windows では CWD からも DLL を
/// 探索するため、攻撃者が用意したディレクトリを作業ディレクトリにして起動させられると、
/// 正規 DLL が同梱されていても先に任意の DLL をロードさせられる（DLL 配置攻撃）。
/// 開発時（`cargo test` / `cargo run`）の探索は、ビルド時に確定する
/// `CARGO_MANIFEST_DIR` の**絶対パス**で満たす（`scripts/fetch-pdfium.sh` が
/// `crates/core/` にライブラリを置く）。
fn library_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![Pdfium::pdfium_platform_library_name_at_path(env!(
        "CARGO_MANIFEST_DIR"
    ))];
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

/// ページ画像の目標幅（px）。`set_target_width` と上限計算の両方で使う。
const PDF_TARGET_WIDTH: f32 = 1000.0;

/// 1 ページの描画で許す最大画素数。
///
/// `set_maximum_height` は縦長ページの幅を縮めて画質を落とすため使わず、
/// 幅 1000px へ縮めたときの高さから画素数を見積もって、常識的な範囲を超える
/// ページを描画前に失敗させる。1000 × 16000 px（= 16 MPix、生 RGB で約 48 MB）を
/// 上限に置く（実本の見開きは数 MPix）。
const MAX_PDF_PAGE_PIXELS: f64 = 16_000_000.0;

/// Render every page of a PDF to webp (q80) at a target width of 1000px
/// and extract the page text. `progress` receives 0..=1.
pub fn render_pdf_pages(
    bytes: &[u8],
    progress: &mut (dyn FnMut(f32) + Send),
) -> Result<Vec<PageImage>, ImportError> {
    let mut pages = Vec::new();
    render_pdf_pages_into(bytes, progress, &mut |page| {
        pages.push(page);
        Ok(())
    })?;
    Ok(pages)
}

/// ページを**1 件ずつ** `on_page` へ渡しながら描画する（全ページを溜めない）。
///
/// 戻り値は描画したページ数（＝ `on_page` を呼んだ回数）。エンコードに失敗したページが
/// あればそこでエラーを返す（呼び出し側が数えたページ番号と実際のページがずれないよう、
/// **失敗したページは飛ばさずエラーにする**）。進捗の規則は [`render_pdf_pages`] と同じ。
///
/// 大きい本（数百ページ）で `Vec<PageImage>` を作ると、レンダリング結果（webp）を
/// 全部同時に持つことになるため、取り込みはこちらを使う。
pub fn render_pdf_pages_into(
    bytes: &[u8],
    progress: &mut (dyn FnMut(f32) + Send),
    on_page: &mut dyn FnMut(PageImage) -> Result<(), ImportError>,
) -> Result<usize, ImportError> {
    render_pdf_source_into(PdfSource::Bytes(bytes), progress, on_page)
}

/// ファイルから読んでページを 1 件ずつ渡す（**ソース全体をメモリへ読まない**）。
///
/// 4 GiB 級の PDF を取り込む経路。`load_pdf_from_file` は PDFium にファイルを渡すので、
/// ここで `std::fs::read` する必要がない（読み込みは PDFium が面倒を見る）。
pub fn render_pdf_file_into(
    path: &std::path::Path,
    progress: &mut (dyn FnMut(f32) + Send),
    on_page: &mut dyn FnMut(PageImage) -> Result<(), ImportError>,
) -> Result<usize, ImportError> {
    render_pdf_source_into(PdfSource::File(path), progress, on_page)
}

/// 描画対象（バイト列かファイルか）。ライフタイムを素直に扱うため enum で分ける
/// （クロージャにすると、ドキュメントのライフタイムが「PDFium の借用」と「ソースの借用」の
/// 短い方になり、高階ライフタイムの境界と噛み合わない）。
enum PdfSource<'a> {
    Bytes(&'a [u8]),
    File(&'a std::path::Path),
}

/// 読み込み方（バイト列 / ファイル）だけが違う共通部分。
fn render_pdf_source_into(
    source: PdfSource<'_>,
    progress: &mut (dyn FnMut(f32) + Send),
    on_page: &mut dyn FnMut(PageImage) -> Result<(), ImportError>,
) -> Result<usize, ImportError> {
    log::info!("render_pdf_pages: 開始");
    let render_start = std::time::Instant::now();

    // PDFium の同時利用は未定義動作なので、初期化も含めてここから直列化する。
    let _pdfium_guard = PDFIUM_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let pdfium = pdfium_instance()?;
    let document = match source {
        PdfSource::Bytes(bytes) => pdfium.load_pdf_from_byte_slice(bytes, None),
        PdfSource::File(path) => pdfium.load_pdf_from_file(path, None),
    }
    .map_err(|e| ImportError::Pdf(e.to_string()))?;
    let total = document.pages().len().max(0) as usize;
    if total == 0 {
        return Ok(0);
    }
    // **描画の前**にページ数を見る（上限内のページが数万ある PDF で、レンダリングと
    // WebP エンコードを長時間走らせてから pack の上限で落ちるのを避ける。F06）。
    if total > crate::import::MAX_PDF_PAGES {
        return Err(ImportError::Pdf(format!(
            "PDF のページ数 {total} が上限 {} を超えています",
            crate::import::MAX_PDF_PAGES
        )));
    }

    // ページ幅を 1000px に（アスペクト比維持）。高さの上限は設けない
    // （縦長ページでも幅 1000px を維持する。`set_maximum_height` を入れると
    // 縦長ページで幅が縮み画質が落ちる）。
    let render_config = PdfRenderConfig::new().set_target_width(PDF_TARGET_WIDTH as i32);
    // 8 ページずつ「描画（直列）→ 並列エンコード」を回す。全ページの生 RGB を
    // 溜めてから一括でエンコードすると 1 ページ約 4MB × ページ数になり、
    // 200 ページ級の本で 1GB を超える（旧 mupdf 経路はページごとにエンコードして
    // webp しか保持していなかった）。窓を切れば常駐は数ページ分で済む。
    const WINDOW: usize = 8;
    let done = std::sync::atomic::AtomicUsize::new(0);
    let progress = std::sync::Mutex::new(progress);
    // 渡したページ数（`on_page` を呼んだ回数）。総ページ数と一致するのが正常。
    let mut emitted = 0usize;
    // エンコード済みページの累積バイト数（上限を超えたら打ち切る）。
    let mut output_bytes = 0u64;
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
            // 描画前に「幅 1000px へ縮めたときの画素数」を見積もる。極端な
            // アスペクト比のページ（例: 1pt × 100000pt）は描画後のビットマップが
            // 数 GB になり得るため、確保する前に弾く（生 RGB は窓 8 ページ分が
            // 同時に載る）。実本の見開きでも数 MPix なので上限は十分余裕がある。
            let page_width = f64::from(page.width().value).max(1.0);
            let page_height = f64::from(page.height().value);
            let target_width = f64::from(PDF_TARGET_WIDTH);
            let projected_pixels = target_width * page_height * target_width / page_width;
            if projected_pixels > MAX_PDF_PAGE_PIXELS {
                return Err(ImportError::Pdf(format!(
                    "ページ {} の描画サイズが上限を超えています（幅 {PDF_TARGET_WIDTH}px 換算で {:.0} 画素 > {:.0}）",
                    index + 1,
                    projected_pixels,
                    MAX_PDF_PAGE_PIXELS
                )));
            }
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
                Some(page) => {
                    output_bytes = output_bytes.saturating_add(page.data.len() as u64);
                    // 1 ページずつ渡す（呼び出し側は store へ入れて手放す）。
                    on_page(page)?;
                    emitted += 1;
                }
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
        // **累積の出力量**も見る（1 ページ 16MPix の検査は「1 ページ」の上限で、
        // 全体の量は制約しない。pack の上限（`MAX_TOTAL_SIZE`）を超える分を
        // エンコードし続けないよう、ここで打ち切る。F06）。
        if output_bytes > opfspack::MAX_TOTAL_SIZE {
            return Err(ImportError::Pdf(format!(
                "PDF の出力が上限を超えています（{output_bytes} バイト > {} バイト）",
                opfspack::MAX_TOTAL_SIZE
            )));
        }
    }

    if let Some(error) = first_error {
        return Err(error);
    }

    log::info!(
        "render_pdf_pages: 完了（{emitted}/{total} ページ / {:?}）",
        render_start.elapsed()
    );
    Ok(emitted)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 探索候補はすべて絶対パス（CWD 相対の候補が混ざると DLL 配置攻撃が成立する）。
    #[test]
    fn library_candidates_are_absolute() {
        for candidate in library_candidates() {
            assert!(
                candidate.is_absolute(),
                "カレントディレクトリ依存の候補が残っている: {}",
                candidate.display()
            );
        }
    }
}
