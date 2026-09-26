//! Import pipeline tests: PDF / EPUB / image ZIP -> .opfspack packs, DB rows
//! and generated tags.

use std::path::PathBuf;

use opfspack::{PackBuilder, PackReader, PackRootKey};
use thundoku_core::db;
use thundoku_core::import::{
    ImportError, MAX_NESTED_ENTRIES, MediaKind, analyze_zip, import_file, import_pdf_bytes,
};
use thundoku_core::tags;

const PDF_FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/tests/fixtures/2018-04-22_libraries_of_react_with_cover.pdf"
);

struct TestEnv {
    root: PathBuf,
    pool: thundoku_core::db::SqlitePool,
}

impl TestEnv {
    fn new(tag: &str) -> Self {
        let root =
            std::env::temp_dir().join(format!("thundoku-import-test-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("packs")).unwrap();
        let pool = thundoku_core::db::test_pool();
        thundoku_core::db::migrate(&pool).unwrap();
        Self { root, pool }
    }

    fn packs(&self) -> PathBuf {
        self.root.join("packs")
    }
}

impl Drop for TestEnv {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn no_progress(_: f32) {}

/// オブジェクト列から最小 PDF を組み立てる（xref / trailer 付き）。
fn build_pdf(objects: &[Vec<u8>]) -> Vec<u8> {
    let mut pdf: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        pdf.extend_from_slice(object);
        pdf.extend_from_slice(b"\nendobj\n");
    }
    let xref_offset = pdf.len();
    pdf.extend_from_slice(
        format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes(),
    );
    for offset in &offsets {
        pdf.extend_from_slice(format!("{:010} 00000 n \n", offset).as_bytes());
    }
    pdf.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            xref_offset
        )
        .as_bytes(),
    );
    pdf
}

fn content_stream(content: &str) -> Vec<u8> {
    format!(
        "<< /Length {} >>\nstream\n{}endstream",
        content.len(),
        content
    )
    .into_bytes()
}

/// 指定コンテンツ（塗りオペレータ）の 200x200 単一ページ PDF を生成する。
fn solid_pdf_with_content(content: &str) -> Vec<u8> {
    build_pdf(&[
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>".to_vec(),
        content_stream(content),
    ])
}

/// Helvetica のテキストを描く 200x200 単一ページ PDF。PDFium のフォントキャッシュを
/// 使わせるため、並列レンダリングの回帰テストに使う。
fn text_pdf() -> Vec<u8> {
    build_pdf(&[
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R \
           /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_vec(),
        content_stream("BT /F1 24 Tf 20 150 Td (Hello PDFium 0123456789) Tj ET\n"),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
    ])
}

/// 指定色で塗りつぶした 200x200 の単一ページ PDF を生成する（最小 PDF）。
fn solid_color_pdf(r: u8, g: u8, b: u8) -> Vec<u8> {
    let content = format!(
        "1 0 0 1 0 0 cm\n{} {} {} rg\n0 0 200 200 re\nf\n",
        r as f32 / 255.0,
        g as f32 / 255.0,
        b as f32 / 255.0
    );
    solid_pdf_with_content(&content)
}

/// PDF を描画するテストの前提。PDFium のライブラリが無い環境ではスキップする
/// （`mise run pdfium` で取得できる。CI は取得してからテストする）。
fn pdfium_ready() -> bool {
    if thundoku_core::import::pdf::pdfium_library_path().is_some() {
        return true;
    }
    eprintln!("skip: PDFium のライブラリがありません（`mise run pdfium` で取得してください）");
    false
}

fn center_rgb(pdf: &[u8]) -> (u8, u8, u8) {
    let pages = thundoku_core::import::pdf::render_pdf_pages(pdf, &mut no_progress).unwrap();
    assert_eq!(pages.len(), 1);
    let img = image::load_from_memory(&pages[0].data).unwrap();
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    let p = rgba.get_pixel(w / 2, h / 2);
    (p[0], p[1], p[2])
}

/// PDFium はグローバルなフォントキャッシュを持ち、**同時利用が安全ではない**
/// （`pdfium-render` の `thread_safe` feature は `unsafe impl Send/Sync` を足すだけで
/// ロックはしない）。8 スレッドで同時にレンダリングしても落ちず、内容も正しいこと。
/// 修正前はこの形で `STATUS_ACCESS_VIOLATION` によりプロセスが落ちていた。
#[test]
fn pdf_rendering_is_safe_from_multiple_threads() {
    if !pdfium_ready() {
        return;
    }
    let bytes = text_pdf();
    let texts = std::sync::Mutex::new(Vec::new());
    std::thread::scope(|scope| {
        for _ in 0..8 {
            let bytes = &bytes;
            let texts = &texts;
            scope.spawn(move || {
                for _ in 0..5 {
                    let mut progress = no_progress;
                    let pages = thundoku_core::import::pdf::render_pdf_pages(bytes, &mut progress)
                        .expect("render");
                    assert_eq!(pages.len(), 1);
                    assert!(
                        pages[0].text.contains("Hello PDFium"),
                        "テキストが取れていない: {:?}",
                        pages[0].text
                    );
                    let image = image::load_from_memory(&pages[0].data).expect("webp をデコード");
                    assert!(image.width() > 0 && image.height() > 0);
                    texts.lock().unwrap().push(pages[0].text.clone());
                }
            });
        }
    });
    assert_eq!(
        texts.lock().unwrap().len(),
        40,
        "8 スレッド × 5 回すべて結果が返る"
    );
}

#[test]
fn pdf_rendering_converts_cmyk_red_correctly() {
    if !pdfium_ready() {
        return;
    }
    // 回帰: 技術書典の PDF は CMYK や ICC ベースの色を使うことが多い。
    // CMYK 赤 (C=0, M=1, Y=1, K=0) が RGB の赤 (255, 0, 0) に変換されること。
    let pdf = solid_pdf_with_content("1 0 0 1 0 0 cm\n0 1 1 0 k\n0 0 200 200 re\nf\n");
    let (r, g, b) = center_rgb(&pdf);
    assert!(
        r > 200,
        "CMYK red should convert to strong red, got r={r} g={g} b={b}"
    );
    assert!(
        g < 60 && b < 60,
        "CMYK red should have near-zero green/blue, got r={r} g={g} b={b}"
    );
}

#[test]
fn pdf_rendering_preserves_mid_gray_gamma() {
    if !pdfium_ready() {
        return;
    }
    // 回帰: mupdf がリニア RGB で出力すると中間グレーが暗くなり
    // 「色がおかしい」（赤が濁る等）ように見える。sRGB 128 が
    // ほぼ 128 で出力されること（±20 の許容）を検証する。
    let pdf = solid_color_pdf(128, 128, 128);
    let (r, g, b) = center_rgb(&pdf);
    for (name, value) in [("r", r), ("g", g), ("b", b)] {
        assert!(
            (value as i16 - 128).abs() < 20,
            "gray {name} channel should be ~128 (sRGB), got {value} \
             (linear output would be ~55)"
        );
    }
}

#[test]
fn webp_keeps_dark_red_stable() {
    // 回帰: 技術書典 PDF の実ページの暗い赤 (183, 42, 39) が webp q80
    // エンコードで大きく変化しないことを検証する。
    let mut img = image::RgbImage::new(100, 100);
    for pixel in img.pixels_mut() {
        *pixel = image::Rgb([183, 42, 39]);
    }
    let webp =
        thundoku_core::import::encode_webp(&image::DynamicImage::ImageRgb8(img), 80).unwrap();
    let decoded = image::load_from_memory(&webp).unwrap().to_rgb8();
    let p = decoded.get_pixel(50, 50);
    for (name, original, got) in [("r", 183, p[0]), ("g", 42, p[1]), ("b", 39, p[2])] {
        assert!(
            (got as i16 - original as i16).abs() < 30,
            "dark red {name} channel drifted: expected ~{original}, got {got}"
        );
    }
}

#[test]
fn webp_encode_preserves_vivid_red() {
    // 回帰: webp エンコード（lossy q80）で鮮やかな赤が暗くならないことを検証。
    let mut img = image::RgbImage::new(100, 100);
    for pixel in img.pixels_mut() {
        *pixel = image::Rgb([255, 0, 0]);
    }
    let webp =
        thundoku_core::import::encode_webp(&image::DynamicImage::ImageRgb8(img), 80).unwrap();
    let decoded = image::load_from_memory(&webp).unwrap().to_rgb8();
    let p = decoded.get_pixel(50, 50);
    assert!(
        p[0] > 200,
        "webp should keep red vivid, got r={} g={} b={}",
        p[0],
        p[1],
        p[2]
    );
    assert!(
        p[1] < 60 && p[2] < 60,
        "webp should keep green/blue low, got r={} g={} b={}",
        p[0],
        p[1],
        p[2]
    );
}

#[test]
fn pdf_rendering_preserves_red_color() {
    if !pdfium_ready() {
        return;
    }
    // 回帰: 赤い矩形の PDF をレンダリングし、中心ピクセルの RGB が
    // 赤（R 優位・G/B ほぼゼロ）であることを検証する。
    let pdf = solid_color_pdf(255, 0, 0);
    let pages = thundoku_core::import::pdf::render_pdf_pages(&pdf, &mut no_progress).unwrap();
    assert_eq!(pages.len(), 1);
    let img = image::load_from_memory(&pages[0].data).unwrap();
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    let p = rgba.get_pixel(w / 2, h / 2);
    assert!(
        p[0] > 200,
        "red channel should dominate, got r={} g={} b={}",
        p[0],
        p[1],
        p[2]
    );
    assert!(
        p[1] < 60 && p[2] < 60,
        "green/blue channels should be near zero, got r={} g={} b={}",
        p[0],
        p[1],
        p[2]
    );
}

#[test]
fn pdf_pages_have_clean_white_backgrounds() {
    if !pdfium_ready() {
        return;
    }
    // 回帰: mupdf の Pixmap::new はピクセルを初期化しないため、page.run が
    // ページ内容だけを描画すると背景・余白に前ページの残骸（未初期化メモリ）
    // が残り「画像が重なって見える」。描画前の白クリアを検証する。
    let bytes = std::fs::read(PDF_FIXTURE).unwrap();
    let pages = thundoku_core::import::pdf::render_pdf_pages(&bytes, &mut no_progress).unwrap();
    assert!(pages.len() >= 2, "fixture should have multiple pages");
    for (index, page) in pages.iter().enumerate() {
        let img = image::load_from_memory(&page.data).unwrap();
        let rgba = img.to_rgba8();
        let (w, h) = (rgba.width(), rgba.height());
        for (x, y) in [
            (0, 0),
            (w - 1, 0),
            (0, h - 1),
            (w - 1, h - 1),
            (w / 2, 0),
            (w / 2, h - 1),
            (0, h / 2),
            (w - 1, h / 2),
        ] {
            let p = rgba.get_pixel(x, y);
            assert_eq!(
                (p[0], p[1], p[2]),
                (255, 255, 255),
                "page {index} background at ({x},{y}) must be white \
                 (previous page residue detected)"
            );
        }
    }
}

#[test]
fn pdf_fixture_imports_pages_text_and_db_rows() {
    if !pdfium_ready() {
        return;
    }
    let env = TestEnv::new("pdf");
    let mut progress_calls: Vec<f32> = Vec::new();
    let imported = import_file(
        &env.pool,
        std::path::Path::new(PDF_FIXTURE),
        &env.packs(),
        None,
        &mut |p| progress_calls.push(p),
    )
    .unwrap();

    // book row
    let book = db::books::get(&env.pool, &imported.book.id)
        .unwrap()
        .unwrap();
    assert_eq!(book.title, "2018-04-22_libraries_of_react_with_cover");
    assert_eq!(book.pack_id.as_deref(), Some(imported.book.id.as_str()));

    // document row
    assert_eq!(imported.document.source_type, "pdf");
    assert_eq!(imported.document.file_hash.len(), 64);
    assert!(imported.document.total_pages > 0);

    // pack written and readable; pages + thumbnail + cover + metadata
    let pack_bytes =
        std::fs::read(env.packs().join(format!("{}.opfspack", imported.book.id))).unwrap();
    let reader = PackReader::open(&pack_bytes).unwrap();
    let paths: Vec<&str> = reader.entries().iter().map(|e| e.path.as_str()).collect();
    assert!(paths.contains(&"thumbnail.webp"));
    assert!(paths.contains(&"cover.webp"));
    assert!(paths.contains(&"metadata.json"));
    let page_count = paths.iter().filter(|p| p.starts_with("pages/")).count();
    assert!(page_count > 0);
    assert_eq!(page_count, imported.document.total_pages as usize);

    // page entries are webp
    let page0 = reader.read_entry("pages/page_0001.webp", None).unwrap();
    assert_eq!(&page0[..4], b"RIFF");
    assert_eq!(&page0[8..12], b"WEBP");

    // document_images rows match pack entries
    let image_count: i64 = thundoku_core::db::block_on(async {
        sqlx::query_scalar("SELECT COUNT(*) FROM document_images WHERE document_id = ?1")
            .bind(&imported.document.id)
            .fetch_one(&env.pool)
            .await
    })
    .unwrap();
    assert_eq!(image_count as usize, page_count + 1); // pages + thumbnail

    // text extraction produced rows
    let text_count: i64 = thundoku_core::db::block_on(async {
        sqlx::query_scalar("SELECT COUNT(*) FROM document_text WHERE document_id = ?1")
            .bind(&imported.document.id)
            .fetch_one(&env.pool)
            .await
    })
    .unwrap();
    assert_eq!(text_count as usize, page_count);

    // progress callback fired at least twice and ended at 1.0
    assert!(progress_calls.len() >= 2);
    assert_eq!(*progress_calls.last().unwrap(), 1.0);
}

#[test]
fn epub_imports_as_single_raw_entry() {
    let env = TestEnv::new("epub");
    let epub_path = env.root.join("sample-book.epub");
    let epub_bytes: Vec<u8> = (0u8..=255).cycle().take(4096).collect();
    std::fs::write(&epub_path, &epub_bytes).unwrap();

    let imported =
        import_file(&env.pool, &epub_path, &env.packs(), None, &mut no_progress).unwrap();
    assert_eq!(imported.document.source_type, "epub");
    assert_eq!(imported.document.total_pages, 0);

    let pack_bytes =
        std::fs::read(env.packs().join(format!("{}.opfspack", imported.book.id))).unwrap();
    let reader = PackReader::open(&pack_bytes).unwrap();
    let entry = reader.entry("sample-book.epub").unwrap();
    assert_eq!(entry.size, 4096);
    assert_eq!(
        reader.read_entry("sample-book.epub", None).unwrap(),
        epub_bytes
    );
}

fn make_png(width: u32, height: u32, color: [u8; 3]) -> Vec<u8> {
    let mut image = image::RgbImage::new(width, height);
    for pixel in image.pixels_mut() {
        *pixel = image::Rgb(color);
    }
    let mut out = Vec::new();
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
        .unwrap();
    out
}

#[test]
fn image_zip_imports_sorted_pages() {
    let env = TestEnv::new("image-zip");
    let zip_path = env.root.join("photo-book.zip");
    let file = std::fs::File::create(&zip_path).unwrap();
    let mut zip = zip::ZipWriter::new(file);
    let options = zip::write::SimpleFileOptions::default();
    // deliberately out of order to verify name sorting
    for (name, color) in [
        ("img-003.png", [0, 0, 255]),
        ("img-001.png", [255, 0, 0]),
        ("img-002.png", [0, 255, 0]),
    ] {
        zip.start_file(name, options).unwrap();
        use std::io::Write;
        zip.write_all(&make_png(64, 96, color)).unwrap();
    }
    zip.finish().unwrap();

    let imported = import_file(&env.pool, &zip_path, &env.packs(), None, &mut no_progress).unwrap();
    assert_eq!(imported.document.source_type, "image-set");
    assert_eq!(imported.document.total_pages, 3);

    let pack_bytes =
        std::fs::read(env.packs().join(format!("{}.opfspack", imported.book.id))).unwrap();
    let reader = PackReader::open(&pack_bytes).unwrap();
    let paths: Vec<&str> = reader.entries().iter().map(|e| e.path.as_str()).collect();
    assert!(paths.contains(&"pages/page_0001.webp"));
    assert!(paths.contains(&"pages/page_0003.webp"));
    assert!(paths.contains(&"thumbnail.webp"));
    // first page is the red one (img-001)
    let first = reader.read_entry("pages/page_0001.webp", None).unwrap();
    assert_eq!(&first[8..12], b"WEBP");
}

#[test]
fn zip_with_pdf_reuses_book_id_without_duplicate() {
    let env = TestEnv::new("zip-pdf-reuse");
    let zip_path = env.root.join("mixed.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    // 実データ同様、ZIP 内のフォルダに PDF が入っている形
    zip.start_file("inner/inside.pdf", options).unwrap();
    {
        use std::io::Write;
        zip.write_all(&solid_pdf_with_content(
            "1 0 0 1 0 0 cm\n0 1 1 0 k\n0 0 200 200 re\nf\n",
        ))
        .unwrap();
    }
    zip.finish().unwrap();
    let bytes = std::fs::read(&zip_path).unwrap();

    let first = thundoku_core::import::import_zip_bytes(
        &env.pool,
        "mixed.zip",
        &bytes,
        &env.packs(),
        None,
        &mut no_progress,
        None,
    )
    .unwrap();
    // 題名・file_name は ZIP 内パスではなくファイル名を使う（"inner/inside.pdf" ではなく "inside.pdf"）
    assert_eq!(first.book.file_name, "inside.pdf");
    assert_eq!(first.book.title, "inside");
    // PDF レンディションの表示名は種別名（拡張子は出さない）
    let stored = db::contents::list_with_formats(&env.pool, &first.book.id).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].1[0].label, "PDF");

    db::books::set_favorite(&env.pool, &first.book.id, true).unwrap();

    // 再ダウンロード（同一 book_id の再利用）で重複本を作らない
    let second = thundoku_core::import::import_zip_bytes(
        &env.pool,
        "mixed.zip",
        &bytes,
        &env.packs(),
        None,
        &mut no_progress,
        Some(&first.book.id),
    )
    .unwrap();
    assert_eq!(second.book.id, first.book.id);

    let all = db::books::list(&env.pool).unwrap();
    assert_eq!(all.len(), 1, "再取り込みで本が増えないこと");
    assert_eq!(
        all[0].is_favorite, 1,
        "ユーザー状態（お気に入り）が維持されること"
    );

    // ページは置き換わる（旧ドキュメントが残って二重に並ばない）
    let images = db::documents::images_for_book(&env.pool, &first.book.id).unwrap();
    let pages = images.iter().filter(|i| i.image_type == "page").count();
    assert_eq!(pages, 1);
}

#[test]
fn zip_entry_names_are_decoded_from_cp932() {
    let env = TestEnv::new("zip-cp932");
    let zip_path = env.root.join("cp932.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    // プレースホルダは CP932 の「表紙.epub」と同じ 9 バイトにしておく
    zip.start_file("zzzz.epub", options).unwrap();
    {
        use std::io::Write;
        zip.write_all(b"epub-bytes").unwrap();
    }
    zip.finish().unwrap();

    // 名前バイトを CP932 の「表紙.epub」へ差し替える（長さが同じなのでオフセットも CRC も変わらない）
    let bytes = std::fs::read(&zip_path).unwrap();
    let cp932 = [0x95u8, 0x5c, 0x8e, 0x86, 0x2e, 0x65, 0x70, 0x75, 0x62];
    let mut patched = Vec::with_capacity(bytes.len());
    let mut hits = 0;
    let mut i = 0;
    while i < bytes.len() {
        if i + 9 <= bytes.len() && &bytes[i..i + 9] == b"zzzz.epub".as_slice() {
            patched.extend_from_slice(&cp932);
            hits += 1;
            i += 9;
        } else {
            patched.push(bytes[i]);
            i += 1;
        }
    }
    assert!(hits > 0, "placeholder name not found in the zip");
    std::fs::write(&zip_path, &patched).unwrap();

    let imported = import_file(&env.pool, &zip_path, &env.packs(), None, &mut no_progress).unwrap();
    // 文字化けせず CP932 の名前が復号されること
    assert_eq!(imported.book.file_name, "表紙.epub");
    assert_eq!(imported.book.title, "表紙");
}

#[test]
fn single_image_imports_as_one_page_book() {
    let env = TestEnv::new("single-image");
    // BOOTH のダウンロードが画像ファイル（イラスト等）の場合
    let png = make_png(64, 96, [200, 100, 50]);
    let imported = thundoku_core::import::import_image_bytes(
        &env.pool,
        "illust.png",
        &png,
        &env.packs(),
        None,
        None,
    )
    .unwrap();
    assert_eq!(imported.document.source_type, "image");
    assert_eq!(imported.document.total_pages, 1);

    let pack_bytes =
        std::fs::read(env.packs().join(format!("{}.opfspack", imported.book.id))).unwrap();
    let reader = PackReader::open(&pack_bytes).unwrap();
    let paths: Vec<&str> = reader.entries().iter().map(|e| e.path.as_str()).collect();
    assert!(paths.contains(&"pages/page_0001.webp"));
    assert!(paths.contains(&"thumbnail.webp"));
    let page = reader.read_entry("pages/page_0001.webp", None).unwrap();
    assert_eq!(&page[8..12], b"WEBP");
}

#[test]
fn image_reuses_book_id_without_duplicate() {
    let env = TestEnv::new("image-reuse");
    let png = make_png(64, 96, [200, 100, 50]);

    let first = thundoku_core::import::import_image_bytes(
        &env.pool,
        "illust.png",
        &png,
        &env.packs(),
        None,
        None,
    )
    .unwrap();
    db::books::set_favorite(&env.pool, &first.book.id, true).unwrap();

    // 再ダウンロード（同一 book_id の再利用）で重複本を作らない
    let second = thundoku_core::import::import_image_bytes(
        &env.pool,
        "illust.png",
        &png,
        &env.packs(),
        None,
        Some(&first.book.id),
    )
    .unwrap();
    assert_eq!(second.book.id, first.book.id);

    let all = db::books::list(&env.pool).unwrap();
    assert_eq!(all.len(), 1, "再取り込みで本が増えないこと");
    assert_eq!(
        all[0].is_favorite, 1,
        "ユーザー状態（お気に入り）が維持されること"
    );

    // ページは置き換わる（旧ドキュメントが残って二重に並ばない）
    let images = db::documents::images_for_book(&env.pool, &first.book.id).unwrap();
    let pages = images.iter().filter(|i| i.image_type == "page").count();
    assert_eq!(pages, 1);
}

/// 24bit 非圧縮 BMP を組み立てる（テスト用。`image` のエンコーダ機能に依存しない）。
fn make_bmp(width: u32, height: u32, color: [u8; 3]) -> Vec<u8> {
    let row_bytes = width * 3;
    let padding = (4 - (row_bytes % 4)) % 4;
    let pixel_len = (row_bytes + padding) * height;
    let mut out = Vec::new();
    out.extend_from_slice(b"BM");
    out.extend_from_slice(&(54 + pixel_len).to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&54u32.to_le_bytes());
    out.extend_from_slice(&40u32.to_le_bytes());
    out.extend_from_slice(&(width as i32).to_le_bytes());
    out.extend_from_slice(&(height as i32).to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes());
    out.extend_from_slice(&24u16.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&pixel_len.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&2835u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    out.extend_from_slice(&0u32.to_le_bytes());
    for _ in 0..height {
        for _ in 0..width {
            out.extend_from_slice(&[color[2], color[1], color[0]]);
        }
        out.extend(std::iter::repeat_n(0u8, padding as usize));
    }
    out
}

#[test]
fn bmp_only_zip_imports() {
    // 実データに `.bmp` のみの作品が存在する（docs/import-patterns.md §2.3 G1）。
    let env = TestEnv::new("bmp-zip");
    let zip_path = env.root.join("bmp-book.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    for (name, color) in [("1.bmp", [255, 0, 0]), ("2.bmp", [0, 255, 0])] {
        zip.start_file(name, options).unwrap();
        use std::io::Write;
        zip.write_all(&make_bmp(64, 96, color)).unwrap();
    }
    zip.finish().unwrap();

    let imported = import_file(&env.pool, &zip_path, &env.packs(), None, &mut no_progress).unwrap();
    assert_eq!(imported.document.source_type, "image-set");
    assert_eq!(imported.document.total_pages, 2);
}

#[test]
fn image_zip_with_export_text_imports_page_text() {
    // `_export.txt`（ページ別セリフ本文）を持つ画像セット作品（docs/import-patterns.md §4）。
    let env = TestEnv::new("export-text-zip");
    let zip_path = env.root.join("export.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    for (name, color) in [("001.jpg", [255, 0, 0]), ("002.jpg", [0, 255, 0])] {
        zip.start_file(name, options).unwrap();
        use std::io::Write;
        zip.write_all(&make_png(64, 96, color)).unwrap();
    }
    zip.start_file("壊れた姉弟と壊れる僕_export.txt", options)
        .unwrap();
    {
        use std::io::Write;
        zip.write_all(
            "\u{feff}<<1Page>>\r\n壊れた姉弟と壊れる僕\r\n<<3Page>>\r\n三人目\r\n".as_bytes(),
        )
        .unwrap();
    }
    zip.finish().unwrap();

    let imported = import_file(&env.pool, &zip_path, &env.packs(), None, &mut no_progress).unwrap();
    assert_eq!(imported.document.total_pages, 2);

    // マーカーのページ番号がそのまま document_text に入る（2 ページ目は欠番）。
    // 本文は平文で保存されない（セキュリティ評価 F02）ので、生の列を復号して確かめる。
    let rows: Vec<(String, i64, String)> = thundoku_core::db::block_on(async {
        sqlx::query_as(
            "SELECT id, page_number, text_content FROM document_text \
             WHERE document_id = ?1 ORDER BY page_number",
        )
        .bind(&imported.document.id)
        .fetch_all(&env.pool)
        .await
    })
    .unwrap();
    let key = thundoku_core::db::column_crypto::db_key().unwrap();
    let rows: Vec<(i64, String)> = rows
        .into_iter()
        .map(|(id, page_number, stored)| {
            assert!(
                stored.starts_with(thundoku_core::db::column_crypto::PREFIX),
                "本文が平文で保存されている: {stored}"
            );
            let text = thundoku_core::db::column_crypto::decrypt(
                &key,
                &thundoku_core::db::column_crypto::aad_document_text(&id, "text_content"),
                &stored,
            )
            .expect("復号できること");
            (page_number, text)
        })
        .collect();
    assert_eq!(
        rows,
        vec![
            (1, "壊れた姉弟と壊れる僕".to_string()),
            (3, "三人目".to_string()),
        ]
    );
}

#[test]
fn corrupted_image_is_skipped_with_warning() {
    // 壊れた画像 1 枚で全体を失敗させない（docs/import-patterns.md §11.1 D6）。
    let env = TestEnv::new("corrupt-image-zip");
    let zip_path = env.root.join("corrupt.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    for (name, color) in [("001.png", [255, 0, 0]), ("002.png", [0, 255, 0])] {
        zip.start_file(name, options).unwrap();
        use std::io::Write;
        zip.write_all(&make_png(64, 96, color)).unwrap();
    }
    zip.start_file("003.png", options).unwrap();
    {
        use std::io::Write;
        zip.write_all(b"not a png at all").unwrap();
    }
    zip.finish().unwrap();

    let imported = import_file(&env.pool, &zip_path, &env.packs(), None, &mut no_progress).unwrap();
    assert_eq!(imported.document.total_pages, 2);
    assert_eq!(imported.warnings.len(), 1);
    assert!(
        imported.warnings[0].contains("003.png"),
        "warning should name the broken entry: {:?}",
        imported.warnings
    );

    // ページ番号は連番のまま（欠番を作らない）
    let pack_bytes =
        std::fs::read(env.packs().join(format!("{}.opfspack", imported.book.id))).unwrap();
    let reader = PackReader::open(&pack_bytes).unwrap();
    let paths: Vec<&str> = reader.entries().iter().map(|e| e.path.as_str()).collect();
    assert!(paths.contains(&"pages/page_0001.webp"));
    assert!(paths.contains(&"pages/page_0002.webp"));
    assert!(!paths.contains(&"pages/page_0003.webp"));
}

#[test]
fn analyze_zip_plans_without_writing() {
    let env = TestEnv::new("analyze-no-write");
    let zip_path = env.root.join("plan.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    {
        use std::io::Write;
        for (name, color) in [
            ("001.jpg", [255, 0, 0]),
            ("002.jpg", [0, 255, 0]),
            // 表紙と junk はコンテンツにしない（決定 D4 / junk 判定）
            ("表紙.jpg", [200, 0, 0]),
        ] {
            zip.start_file(name, options).unwrap();
            zip.write_all(&make_png(64, 96, color)).unwrap();
        }
        zip.start_file("Thumbs.db", options).unwrap();
        zip.write_all(b"junk").unwrap();
        zip.start_file("本文_export.txt", options).unwrap();
        zip.write_all("<<1Page>>\n本文\n<<2Page>>\n続き\n".as_bytes())
            .unwrap();
    }
    zip.finish().unwrap();

    let bytes = std::fs::read(&zip_path).unwrap();
    let plan = analyze_zip(&bytes).unwrap();

    assert_eq!(plan.contents.len(), 1, "表紙と junk はコンテンツにならない");
    assert_eq!(plan.contents[0].display_name, "本文");
    assert_eq!(plan.contents[0].media_kind, MediaKind::Image);
    assert_eq!(plan.contents[0].renditions[0].entries.len(), 2);
    assert_eq!(plan.export_text.len(), 2, "{plan:#?}");
    assert!(plan.warnings.is_empty());
    assert!(plan.skip_reason.is_none());

    // DB には何も書かない
    let books: i64 = thundoku_core::db::block_on(async {
        sqlx::query_scalar("SELECT COUNT(*) FROM books")
            .fetch_one(&env.pool)
            .await
    })
    .unwrap();
    let documents: i64 = thundoku_core::db::block_on(async {
        sqlx::query_scalar("SELECT COUNT(*) FROM imported_documents")
            .fetch_one(&env.pool)
            .await
    })
    .unwrap();
    assert_eq!((books, documents), (0, 0));
    // ディスクにも pack を書かない
    assert_eq!(std::fs::read_dir(env.packs()).unwrap().count(), 0);
}

#[test]
fn zip_with_directory_entries_imports_folder_contents() {
    // 実 ZIP はフォルダエントリを含む。旧実装は「メタ情報の並び」を
    // アーカイブ索引で引いていたため索引がずれて panic していた。
    let env = TestEnv::new("zip-dir-entries");
    let zip_path = env.root.join("folder.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    zip.add_directory("book/", options).unwrap();
    for (name, color) in [("book/002.jpg", [0, 255, 0]), ("book/001.jpg", [255, 0, 0])] {
        zip.start_file(name, options).unwrap();
        use std::io::Write;
        zip.write_all(&make_png(64, 96, color)).unwrap();
    }
    zip.finish().unwrap();

    let bytes = std::fs::read(&zip_path).unwrap();
    let plan = analyze_zip(&bytes).unwrap();
    assert_eq!(plan.contents.len(), 1);
    assert_eq!(plan.contents[0].display_name, "book");
    assert_eq!(plan.contents[0].renditions[0].entries.len(), 2);

    let imported = import_file(&env.pool, &zip_path, &env.packs(), None, &mut no_progress).unwrap();
    assert_eq!(imported.document.source_type, "image-set");
    assert_eq!(imported.document.total_pages, 2);

    // 名前順に並ぶ（001 が 1 ページ目）
    let pack_bytes =
        std::fs::read(env.packs().join(format!("{}.opfspack", imported.book.id))).unwrap();
    let reader = PackReader::open(&pack_bytes).unwrap();
    assert!(
        reader
            .entries()
            .iter()
            .any(|e| e.path == "pages/page_0001.webp")
    );
}

/// 再取得相当（同じ book_id で再取り込み）でも、**ユーザーが付けたコンテンツ名と
/// content_id を引き継ぐ**こと。content_id を引き継ぐことで、content_id に紐づく
/// 進捗（`reading_progress`）・ページ毎記録（`page_views`）も維持される。
#[test]
fn reimport_preserves_custom_content_name_and_id() {
    let env = TestEnv::new("reimport-rename");
    let zip_path = env.root.join("multi.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    for (name, color) in [("001.jpg", [255, 0, 0]), ("002.jpg", [0, 255, 0])] {
        zip.start_file(format!("本編/{name}"), options).unwrap();
        use std::io::Write;
        zip.write_all(&make_png(64, 96, color)).unwrap();
    }
    zip.start_file("別冊/001.jpg", options).unwrap();
    {
        use std::io::Write;
        zip.write_all(&make_png(64, 96, [0, 0, 255])).unwrap();
    }
    zip.finish().unwrap();
    let bytes = std::fs::read(&zip_path).unwrap();

    let first = thundoku_core::import::import_zip_bytes(
        &env.pool,
        "multi.zip",
        &bytes,
        &env.packs(),
        None,
        &mut no_progress,
        None,
    )
    .unwrap();
    let before = db::contents::list_for_book(&env.pool, &first.book.id).unwrap();
    assert_eq!(before.len(), 2);
    let primary_before = before.iter().find(|c| c.is_primary == 1).unwrap();
    let primary_id = primary_before.content_id.clone();
    assert_eq!(primary_before.display_name, "本編");
    // ビューアーで「本編」→「総集編」にリネームする（DB 側）
    assert!(
        db::contents::rename(&env.pool, &first.book.id, &primary_id, "総集編").unwrap(),
        "リネームが反映される"
    );

    // 再取り込み（同じ book_id = 再取得相当）
    let again = thundoku_core::import::import_zip_bytes(
        &env.pool,
        "multi.zip",
        &bytes,
        &env.packs(),
        None,
        &mut no_progress,
        Some(&first.book.id),
    )
    .unwrap();
    assert_eq!(again.book.id, first.book.id);

    let after = db::contents::list_for_book(&env.pool, &first.book.id).unwrap();
    let primary_after = after.iter().find(|c| c.is_primary == 1).unwrap();
    assert_eq!(
        primary_after.display_name, "総集編",
        "再取り込みでカスタム名が戻らないこと"
    );
    assert_eq!(
        primary_after.content_id, primary_id,
        "content_id を引き継ぐこと（進捗・ページ毎記録の紐付けを維持）"
    );
    let secondary_after = after.iter().find(|c| c.is_primary == 0).unwrap();
    assert_eq!(secondary_after.display_name, "別冊");

    // pack の metadata.json もカスタム名になっている（Drive 復元でも戻らない）
    let pack_bytes =
        std::fs::read(env.packs().join(format!("{}.opfspack", first.book.id))).unwrap();
    let reader = PackReader::open(&pack_bytes).unwrap();
    let meta: serde_json::Value =
        serde_json::from_slice(&reader.read_entry("metadata.json", None).unwrap()).unwrap();
    assert!(
        meta["contents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|content| content["displayName"] == "総集編"),
        "pack の metadata もカスタム名: {meta}"
    );
}

/// 再取り込みしてもコンテンツが二重化しない
#[test]
fn zip_with_two_folder_contents_persists_structure() {
    // フェーズ2: 解析したコンテンツ／レンディションを DB と pack に保存する。
    let env = TestEnv::new("multi-content");
    let zip_path = env.root.join("multi.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    for (name, color) in [("001.jpg", [255, 0, 0]), ("002.jpg", [0, 255, 0])] {
        zip.start_file(format!("本編/{name}"), options).unwrap();
        use std::io::Write;
        zip.write_all(&make_png(64, 96, color)).unwrap();
    }
    zip.start_file("別冊/001.jpg", options).unwrap();
    {
        use std::io::Write;
        zip.write_all(&make_png(64, 96, [0, 0, 255])).unwrap();
    }
    zip.finish().unwrap();

    let imported = import_file(&env.pool, &zip_path, &env.packs(), None, &mut no_progress).unwrap();
    // 本編（2 ページ）が既定表示
    assert_eq!(imported.document.source_type, "image-set");
    assert_eq!(imported.document.total_pages, 2);

    // 2 コンテンツが保存され、本編が primary
    let stored = db::contents::list_for_book(&env.pool, &imported.book.id).unwrap();
    assert_eq!(stored.len(), 2, "2 コンテンツが保存される");
    let primary = db::contents::primary_for_book(&env.pool, &imported.book.id)
        .unwrap()
        .unwrap();
    assert_eq!(primary.display_name, "本編");
    assert_eq!(primary.is_primary, 1);
    assert_eq!(primary.media_kind, "image");

    let primary_formats =
        db::contents::formats_for_content(&env.pool, &primary.content_id).unwrap();
    assert_eq!(primary_formats.len(), 1);
    assert_eq!(primary_formats[0].page_count, 2);
    assert_eq!(
        primary_formats[0].label, "JPEG",
        "画像レンディションは拡張子を表示名にする"
    );
    assert_eq!(
        primary_formats[0].pack_entry_prefix.as_deref(),
        Some("pages")
    );

    // リーダー向け（images_for_book）は既定表示コンテンツのページだけを返す
    let images = db::documents::images_for_book(&env.pool, &imported.book.id).unwrap();
    let pages: Vec<&db::documents::DocumentImage> = images
        .iter()
        .filter(|image| image.image_type == "page")
        .collect();
    assert_eq!(pages.len(), 2, "別冊のページは混ざらない");
    assert!(
        pages
            .iter()
            .all(|page| page.content_id.as_deref() == Some(primary.content_id.as_str()))
    );

    // 別冊のページも pack に入っている（切り替え用）
    let secondary = stored
        .iter()
        .find(|content| content.content_id != primary.content_id)
        .unwrap();
    let secondary_formats =
        db::contents::formats_for_content(&env.pool, &secondary.content_id).unwrap();
    let prefix = secondary_formats[0].pack_entry_prefix.clone().unwrap();
    let pack_bytes =
        std::fs::read(env.packs().join(format!("{}.opfspack", imported.book.id))).unwrap();
    let reader = PackReader::open(&pack_bytes).unwrap();
    let paths: Vec<&str> = reader.entries().iter().map(|e| e.path.as_str()).collect();
    assert!(paths.contains(&"pages/page_0001.webp"));
    assert!(paths.contains(&format!("{prefix}/page_0001.webp").as_str()));

    // metadata.json に contents が入る（同期復元用）
    let meta_bytes = reader.read_entry("metadata.json", None).unwrap();
    let meta: serde_json::Value = serde_json::from_slice(&meta_bytes).unwrap();
    let meta_contents = meta["contents"].as_array().unwrap();
    assert_eq!(meta_contents.len(), 2);
    assert!(
        meta_contents
            .iter()
            .any(|content| content["displayName"] == "本編" && content["isPrimary"] == true)
    );

    // 再取り込みしてもコンテンツが二重化しない
    let again = thundoku_core::import::import_zip_bytes(
        &env.pool,
        "multi.zip",
        &std::fs::read(&zip_path).unwrap(),
        &env.packs(),
        None,
        &mut no_progress,
        Some(&imported.book.id),
    )
    .unwrap();
    assert_eq!(again.book.id, imported.book.id);
    let stored_again = db::contents::list_for_book(&env.pool, &imported.book.id).unwrap();
    assert_eq!(stored_again.len(), 2, "再取り込みでコンテンツが増えない");

    // 選択に応じたページ列（フェーズ3）。再取り込みでは content_id を引き継ぐが、
    // ここは最新の行を引き直して検証する。
    let default_pages =
        db::documents::images_for_selection(&env.pool, &imported.book.id, None, None).unwrap();
    assert_eq!(
        default_pages
            .iter()
            .filter(|image| image.image_type == "page")
            .count(),
        2,
        "未指定なら既定表示コンテンツのページ"
    );
    let primary_again = db::contents::primary_for_book(&env.pool, &imported.book.id)
        .unwrap()
        .unwrap();
    assert_eq!(primary_again.display_name, "本編");
    let secondary_again = stored_again
        .iter()
        .find(|content| content.content_id != primary_again.content_id)
        .unwrap();
    assert_eq!(secondary_again.display_name, "別冊");
    let secondary_formats_again =
        db::contents::formats_for_content(&env.pool, &secondary_again.content_id).unwrap();
    let secondary_pages = db::documents::images_for_selection(
        &env.pool,
        &imported.book.id,
        Some(&secondary_again.content_id),
        None,
    )
    .unwrap();
    let secondary_only: Vec<&db::documents::DocumentImage> = secondary_pages
        .iter()
        .filter(|image| image.image_type == "page")
        .collect();
    assert_eq!(secondary_only.len(), 1, "別冊は 1 ページ");
    assert!(
        secondary_only
            .iter()
            .all(|page| page.format_id.as_deref()
                == Some(secondary_formats_again[0].format_id.as_str())),
        "別冊の選択で本編のページが混ざらない"
    );
    assert!(
        secondary_pages
            .iter()
            .all(|image| image.content_id.as_deref() == Some(secondary_again.content_id.as_str()))
    );
}

#[test]
fn zip_nested_folders_become_separate_contents() {
    // 実データ（d_614383 尻穴便女 総集編）は「総集編フォルダ / 1.話A / 2.話B …」の形。
    // **ページを直接含むフォルダ**が読む単位になり、上位フォルダは単位にしない。
    let env = TestEnv::new("nested-folders");
    let zip_path = env.root.join("nested.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    for (name, color) in [
        ("総集編/1.話A/001.jpg", [255, 0, 0]),
        ("総集編/1.話A/002.jpg", [0, 255, 0]),
        ("総集編/2.話B/001.jpg", [0, 0, 255]),
        // 形式フォルダを挟む場合は、その上のフォルダ名を採る
        ("総集編/3.話C/jpg/001.jpg", [255, 255, 0]),
        ("総集編/4.オマケ漫画/001.jpg", [128, 0, 128]),
    ] {
        zip.start_file(name, options).unwrap();
        use std::io::Write;
        zip.write_all(&make_png(64, 96, color)).unwrap();
    }
    zip.finish().unwrap();

    let imported = import_file(&env.pool, &zip_path, &env.packs(), None, &mut no_progress).unwrap();
    let stored = db::contents::list_with_formats(&env.pool, &imported.book.id).unwrap();
    let names: Vec<&str> = stored
        .iter()
        .map(|(content, _)| content.display_name.as_str())
        .collect();
    assert_eq!(
        names,
        vec!["1.話A", "2.話B", "3.話C", "4.オマケ漫画"],
        "ページを含むフォルダが単位になる（自然順）"
    );
    // 形式フォルダ（jpg）ではなくその上のフォルダ名を使う
    let talk_c = stored
        .iter()
        .find(|(content, _)| content.display_name == "3.話C")
        .unwrap();
    assert_eq!(talk_c.1.len(), 1);
    assert_eq!(talk_c.1[0].page_count, 1);
    assert_eq!(talk_c.1[0].label, "JPEG");
    assert_eq!(
        talk_c.1[0].pack_entry_prefix.as_deref(),
        Some("contents/2/r0")
    );
    // 既定表示はページ数最多（1.話A = 2 ページ）
    let primary = db::contents::primary_for_book(&env.pool, &imported.book.id)
        .unwrap()
        .unwrap();
    assert_eq!(primary.display_name, "1.話A");
    assert_eq!(imported.document.total_pages, 2);
}

#[test]
fn import_with_root_key_writes_an_encrypted_v3_pack() {
    // pdfium に依存しない経路（画像 1 枚）で、鍵ありの取り込みを検証する。
    let env = TestEnv::new("encrypted-import");
    let root = PackRootKey::generate();
    let png = make_png(64, 96, [10, 20, 30]);
    let imported = thundoku_core::import::import_image_bytes(
        &env.pool,
        "locked.png",
        &png,
        &env.packs(),
        Some(&root),
        None,
    )
    .unwrap();

    let pack_bytes =
        std::fs::read(env.packs().join(format!("{}.opfspack", imported.book.id))).unwrap();
    let reader = PackReader::open(&pack_bytes).unwrap();
    assert_eq!(reader.header().version, 3, "v3 で書き出す");
    assert!(
        reader.header().flags & opfspack::pack_flags::ENCRYPTED != 0,
        "鍵ありの取り込みは暗号化する"
    );
    // metadata.json もページも鍵なしでは読めない（平文で書かれていない）。
    assert!(reader.read_entry("metadata.json", None).is_err());
    assert!(reader.read_entry("pages/page_0001.webp", None).is_err());
    // 同じ PRK + 同じ book id から導出した鍵では読める。
    let key = root.derive_pack_key(&imported.book.id);
    let page = reader.read_entry("pages/page_0001.webp", Some(&key)).unwrap();
    assert_eq!(&page[8..12], b"WEBP");
    let meta: serde_json::Value =
        serde_json::from_slice(&reader.read_entry("metadata.json", Some(&key)).unwrap()).unwrap();
    assert_eq!(meta["title"], "locked");
}

#[test]
fn unsupported_extension_is_rejected() {
    let env = TestEnv::new("unsupported");
    let path = env.root.join("notes.txt");
    std::fs::write(&path, b"hello").unwrap();
    let err = import_file(&env.pool, &path, &env.packs(), None, &mut no_progress).unwrap_err();
    assert!(matches!(err, ImportError::UnsupportedType(_)));
}

#[test]
fn pdf_import_binds_identity_when_provided() {
    if !pdfium_ready() {
        return;
    }
    let env = TestEnv::new("pdf-identity");
    let pdf_bytes = std::fs::read(PDF_FIXTURE).unwrap();
    let book_id = uuid::Uuid::new_v4().to_string();
    let root = PackRootKey::generate();
    let imported = import_pdf_bytes(
        &env.pool,
        "bound.pdf",
        &pdf_bytes,
        &env.packs(),
        Some(&root),
        &mut no_progress,
        Some(&book_id),
    )
    .unwrap();
    assert_eq!(imported.book.id, book_id);
    let pack_bytes = std::fs::read(env.packs().join(format!("{book_id}.opfspack"))).unwrap();
    let reader = PackReader::open(&pack_bytes).unwrap();
    assert_eq!(reader.header().version, 3, "v3 で書き出す");
    assert!(reader.header().flags & opfspack::pack_flags::ENCRYPTED != 0);
    // 鍵が無い / 違う鍵では読めない（PRK から冊ごとに導出した鍵でのみ読める）
    assert!(reader.read_entry("pages/page_0001.webp", None).is_err());
    let wrong = PackRootKey::generate().derive_pack_key(&book_id);
    assert!(reader.read_entry("pages/page_0001.webp", Some(&wrong)).is_err());
    let key = root.derive_pack_key(&book_id);
    let page = reader.read_entry("pages/page_0001.webp", Some(&key)).unwrap();
    assert_eq!(&page[..4], b"RIFF");
}

#[test]
fn tags_extract_nouns_with_exclusions_and_length_cap() {
    let text = "Reactの本でNext.jsについて学ぶ。Reactはライブラリで、Next.jsはフレームワークです。";
    let nouns = tags::extract_nouns(text, &["React", "Next.js"]);
    // 名詞が取れる
    assert!(!nouns.is_empty());
    let word_map: std::collections::HashMap<String, usize> = nouns.into_iter().collect();
    assert!(!word_map.is_empty());
    // 除外語は含まれない
    assert!(!word_map.contains_key("React"));
}

#[test]
fn tags_generate_falls_back_to_nouns_when_zenn_unavailable() {
    let tags = tags::generate_tags(
        &["技術書典でReactの本を買った。技術書典は楽しい。"],
        &[],
        &[],
    );
    assert!(!tags.is_empty());
    assert!(tags.len() <= 10);
    for tag in &tags {
        assert!(tag.chars().count() <= 20);
    }
}

// ---- フェーズ7: 名前カスタム（pack の metadata.json 書き換え） ----

/// 2 コンテンツ（本文 / 別冊）を持つ metadata.json。
fn metadata_with_two_contents() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({
        "schemaVersion": 1,
        "title": "テスト本",
        "readingProgress": { "currentPage": 0, "totalPages": 1 },
        "contents": [
            {
                "contentId": "c1",
                "displayName": "本文",
                "mediaKind": "image",
                "isPrimary": true,
                "sortOrder": 0,
                "formats": []
            },
            {
                "contentId": "c2",
                "displayName": "別冊",
                "mediaKind": "pdf",
                "isPrimary": false,
                "sortOrder": 1,
                "formats": []
            }
        ]
    }))
    .unwrap()
}

#[test]
fn rename_content_in_pack_rewrites_only_that_content() {
    let mut builder = PackBuilder::new(1_700_000_000_000);
    builder.add_entry(
        "pages/page_0001.webp",
        b"page-1".to_vec(),
        "image/webp",
        false,
    );
    builder.add_entry(
        "metadata.json",
        metadata_with_two_contents(),
        "application/json",
        false,
    );
    let pack = builder.build(None, false).unwrap();

    let renamed = thundoku_core::import::rename_content_in_pack(&pack, "b1", "c2", "続編", None)
        .unwrap()
        .expect("metadata が変わった pack は Some を返す");

    let reader = PackReader::open(&renamed).unwrap();
    let meta: serde_json::Value =
        serde_json::from_slice(&reader.read_entry("metadata.json", None).unwrap()).unwrap();
    assert_eq!(meta["contents"][1]["displayName"], "続編");
    assert_eq!(meta["contents"][0]["displayName"], "本文");
    assert_eq!(meta["title"], "テスト本");
    // ページとそれ以外のエントリはそのまま読める
    assert_eq!(
        reader.read_entry("pages/page_0001.webp", None).unwrap(),
        b"page-1"
    );
    assert_eq!(reader.entries().len(), 2);
    // 元の pack は変更しない
    let original: serde_json::Value = serde_json::from_slice(
        &PackReader::open(&pack)
            .unwrap()
            .read_entry("metadata.json", None)
            .unwrap(),
    )
    .unwrap();
    assert_eq!(original["contents"][1]["displayName"], "別冊");
}

#[test]
fn rename_content_in_pack_keeps_identity_binding() {
    let pack_id = "b1";
    let root = PackRootKey::generate();
    let key = root.derive_pack_key(pack_id);
    let mut builder = PackBuilder::new(7);
    builder.add_entry(
        "pages/page_0001.webp",
        b"page-1".to_vec(),
        "image/webp",
        true,
    );
    builder.add_entry(
        "metadata.json",
        metadata_with_two_contents(),
        "application/json",
        true,
    );
    let pack = builder.build(Some(&key), true).unwrap();

    let renamed = thundoku_core::import::rename_content_in_pack(
        &pack,
        pack_id,
        "c1",
        "本編",
        Some(&root),
    )
    .unwrap()
    .unwrap();

    let reader = PackReader::open(&renamed).unwrap();
    // 暗号化は維持される（鍵無しでは読めない）
    assert!(reader.read_entry("pages/page_0001.webp", None).is_err());
    assert_eq!(
        reader
            .read_entry("pages/page_0001.webp", Some(&key))
            .unwrap(),
        b"page-1"
    );
    let meta: serde_json::Value =
        serde_json::from_slice(&reader.read_entry("metadata.json", Some(&key)).unwrap()).unwrap();
    assert_eq!(meta["contents"][0]["displayName"], "本編");
}

#[test]
fn rename_content_in_pack_returns_none_when_unknown_or_missing() {
    // 対象の content_id が無ければ None（pack を作り直さない）
    let mut builder = PackBuilder::new(1);
    builder.add_entry(
        "metadata.json",
        metadata_with_two_contents(),
        "application/json",
        false,
    );
    let pack = builder.build(None, false).unwrap();
    assert!(
        thundoku_core::import::rename_content_in_pack(&pack, "b1", "missing", "x", None)
            .unwrap()
            .is_none()
    );

    // metadata.json を持たない pack も None（エラーにしない）
    let mut bare = PackBuilder::new(1);
    bare.add_entry("pages/page_0001.webp", b"p".to_vec(), "image/webp", false);
    let bare = bare.build(None, false).unwrap();
    assert!(
        thundoku_core::import::rename_content_in_pack(&bare, "b1", "c1", "x", None)
            .unwrap()
            .is_none()
    );
}

// ---- フェーズ8（§11.2 R1）: 入れ子アーカイブの上限付き再帰展開 ----

/// ZIP をメモリ上で組み立てる（入れ子アーカイブの fixture 用）。
fn build_zip(entries: impl IntoIterator<Item = (String, Vec<u8>)>) -> Vec<u8> {
    use std::io::Write;
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options = zip::write::SimpleFileOptions::default();
    for (name, data) in entries {
        writer.start_file(name, options).unwrap();
        writer.write_all(&data).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

fn planned_content<'a>(
    plan: &'a thundoku_core::import::ImportPlan,
    display_name: &str,
) -> &'a thundoku_core::import::PlannedContent {
    plan.contents
        .iter()
        .find(|content| content.display_name == display_name)
        .unwrap_or_else(|| {
            panic!(
                "content {display_name:?} not found in {:?}",
                plan.contents
                    .iter()
                    .map(|content| content.display_name.as_str())
                    .collect::<Vec<_>>()
            )
        })
}

#[test]
fn nested_zip_contents_are_imported() {
    // ZIP の中の ZIP（決定 D5）。入れ子内の画像が 1 コンテンツとして取り込まれる。
    let env = TestEnv::new("nested-zip-import");
    let inner = build_zip([
        ("001.jpg".to_string(), make_png(64, 96, [255, 0, 0])),
        ("002.jpg".to_string(), make_png(64, 96, [0, 255, 0])),
    ]);
    let outer = build_zip([
        // 表紙はページにならない（決定 D4）。入れ子だけが本文。
        ("表紙.jpg".to_string(), make_png(64, 96, [10, 10, 10])),
        ("本編.zip".to_string(), inner),
    ]);

    let plan = analyze_zip(&outer).unwrap();
    let content = planned_content(&plan, "本編");
    assert_eq!(content.media_kind, MediaKind::Image);
    assert_eq!(content.renditions.len(), 1);
    assert_eq!(
        content.renditions[0].entries.len(),
        2,
        "入れ子内の画像 2 枚が合流する"
    );
    assert!(plan.warnings.is_empty(), "warnings: {:?}", plan.warnings);

    let imported = thundoku_core::import::import_zip_bytes(
        &env.pool,
        "outer.zip",
        &outer,
        &env.packs(),
        None,
        &mut no_progress,
        None,
    )
    .unwrap();
    assert_eq!(imported.document.source_type, "image-set");
    assert_eq!(imported.document.total_pages, 2);
    assert!(imported.warnings.is_empty(), "{:?}", imported.warnings);

    // DB 上も入れ子内の画像がページとして入る
    let images = db::documents::images_for_book(&env.pool, &imported.book.id).unwrap();
    let pages: Vec<&db::documents::DocumentImage> = images
        .iter()
        .filter(|image| image.image_type == "page")
        .collect();
    assert_eq!(pages.len(), 2, "入れ子内の画像が document_images に入る");
    let stored = db::contents::list_for_book(&env.pool, &imported.book.id).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].display_name, "本編");
    assert_eq!(stored[0].media_kind, "image");

    // pack にもページが入る
    let pack_bytes =
        std::fs::read(env.packs().join(format!("{}.opfspack", imported.book.id))).unwrap();
    let reader = PackReader::open(&pack_bytes).unwrap();
    let paths: Vec<&str> = reader.entries().iter().map(|e| e.path.as_str()).collect();
    assert!(paths.contains(&"pages/page_0001.webp"));
    assert!(paths.contains(&"pages/page_0002.webp"));
}

#[test]
fn nested_zip_depth_two_is_not_expanded() {
    // 深さ 2（ZIP in ZIP in ZIP）は展開しない。1 階層で止まる。
    let env = TestEnv::new("nested-zip-depth");
    let deepest = build_zip([("001.jpg".to_string(), make_png(64, 96, [0, 0, 255]))]);
    let middle = build_zip([
        ("001.jpg".to_string(), make_png(64, 96, [255, 0, 0])),
        ("inner.zip".to_string(), deepest),
    ]);
    let outer = build_zip([("本編.zip".to_string(), middle)]);

    let plan = analyze_zip(&outer).unwrap();
    let content = planned_content(&plan, "本編");
    assert_eq!(
        content.renditions[0].entries.len(),
        1,
        "深さ 2 の画像は合流しない"
    );
    assert!(
        plan.warnings.iter().any(|w| w.contains("depth limit")),
        "深さ上限の警告が入る: {:?}",
        plan.warnings
    );

    let imported = thundoku_core::import::import_zip_bytes(
        &env.pool,
        "outer.zip",
        &outer,
        &env.packs(),
        None,
        &mut no_progress,
        None,
    )
    .unwrap();
    assert_eq!(imported.document.total_pages, 1);
}

#[test]
fn nested_zip_over_entry_limit_is_skipped_with_warning() {
    // エントリ数上限を超える入れ子はスキップし、警告に理由を積む（取り込みは続行）。
    let env = TestEnv::new("nested-zip-limit");
    let nested = build_zip(
        (0..MAX_NESTED_ENTRIES + 1)
            .map(|index| (format!("{index:05}.jpg"), make_png(8, 8, [255, 0, 0]))),
    );
    let outer = build_zip([
        ("本文/001.jpg".to_string(), make_png(64, 96, [0, 0, 255])),
        ("同梱.zip".to_string(), nested),
    ]);

    let imported = thundoku_core::import::import_zip_bytes(
        &env.pool,
        "outer.zip",
        &outer,
        &env.packs(),
        None,
        &mut no_progress,
        None,
    )
    .unwrap();
    assert_eq!(
        imported.document.total_pages, 1,
        "上限超過の入れ子は取り込まれない"
    );
    assert!(
        imported
            .warnings
            .iter()
            .any(|w| w.contains("同梱.zip") && w.contains("limit")),
        "上限超過が warnings に入る: {:?}",
        imported.warnings
    );
    let stored = db::contents::list_for_book(&env.pool, &imported.book.id).unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].display_name, "本文");
}

#[test]
fn zip_without_readable_content_is_not_a_readable_work() {
    // 読めるコンテンツ（画像 / PDF / EPUB）が無い ZIP は型付きで返す（R3）。
    let env = TestEnv::new("not-readable-zip");
    let bytes = build_zip([("readme.txt".to_string(), b"hello".to_vec())]);
    let error = thundoku_core::import::import_zip_bytes(
        &env.pool,
        "text.zip",
        &bytes,
        &env.packs(),
        None,
        &mut no_progress,
        None,
    )
    .unwrap_err();
    assert!(
        matches!(error, ImportError::NotAReadableWork),
        "got {error:?}"
    );
}

#[test]
fn image_pages_keep_their_order_with_parallel_rendering() {
    // 並列変換でもページの対応がずれないこと（ページ N の画像が N 枚目に入る）
    let env = TestEnv::new("parallel-order");
    let zip_path = env.root.join("ordered.zip");
    let mut zip = zip::ZipWriter::new(std::fs::File::create(&zip_path).unwrap());
    let options = zip::write::SimpleFileOptions::default();
    let count = 12u8;
    for page in 0..count {
        zip.start_file(format!("本編/{:02}.png", page + 1), options)
            .unwrap();
        // ページごとに違う色の PNG（赤成分 = 20 * page）
        let image = image::RgbImage::from_pixel(20, 30, image::Rgb([20 * page, 40, 60]));
        let mut png = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        std::io::Write::write_all(&mut zip, &png).unwrap();
    }
    zip.finish().unwrap();

    let mut no_progress = |_p: f32| {};
    let imported = import_file(&env.pool, &zip_path, &env.packs(), None, &mut no_progress).unwrap();
    assert_eq!(imported.document.total_pages, count as i64);

    let pack_bytes =
        std::fs::read(env.packs().join(format!("{}.opfspack", imported.book.id))).unwrap();
    let reader = PackReader::open(&pack_bytes).unwrap();
    for page in 1..=count as i64 {
        let data = reader
            .read_entry(&format!("pages/page_{page:04}.webp"), None)
            .unwrap();
        let decoded = image::load_from_memory(&data).unwrap().to_rgb8();
        let red = decoded.get_pixel(0, 0).0[0] as i32;
        let expected = 20 * (page as i32 - 1);
        // webp は不可逆なので色は多少ずれる。隣のページとは 20 差なので、
        // 許容 8 でも「順序がずれた」ことは検出できる。
        assert!(
            (red - expected).abs() <= 8,
            "ページ {page} の赤が {red}（期待 {expected} 前後）。並列処理で順序がずれた可能性"
        );
    }
}

/// 取り込み元のファイルは**読む前に**大きさを検査する。
///
/// `import_file` は変換のために全体をメモリへ読む（`import_*_bytes`）。読んでから
/// 大きさに気付くと、その時点で RAM を食い潰す（セキュリティ評価 F06）。
/// ここではスパースファイル（実体を持たない巨大ファイル）で、**読まずに**弾くことを見る。
#[test]
fn import_file_rejects_a_source_larger_than_the_limit_without_reading_it() {
    let env = TestEnv::new("too-large-source");
    let path = env.root.join("huge.zip");

    // 実データを書かずに長さだけ大きくする（読み込めばゼロ埋めが返る）。
    let limit = thundoku_core::import::MAX_IMPORT_SOURCE_BYTES;
    {
        let file = std::fs::File::create(&path).unwrap();
        file.set_len(limit + 1024 * 1024).unwrap();
    }

    let started = std::time::Instant::now();
    let error = import_file(&env.pool, &path, &env.packs(), None, &mut no_progress)
        .expect_err("大きすぎるファイルは弾く");
    let elapsed = started.elapsed();

    match error {
        ImportError::SourceTooLarge { size, limit: reported } => {
            assert_eq!(reported, limit);
            assert!(size > limit, "実サイズを報告する（{size}）");
        }
        other => panic!("SourceTooLarge を期待した: {other}"),
    }
    // 読んでいたら数 GiB のゼロ埋めで 1 秒では終わらない。
    assert!(
        elapsed < std::time::Duration::from_secs(1),
        "読む前に弾く（{elapsed:?}）"
    );
}

/// 外側 ZIP の**エントリ数**は展開の前に上限で弾く（セキュリティ評価 F06）。
///
/// 個別サイズの上限だけでは、上限内の小さなエントリが大量にあるアーカイブで
/// 変換（画像の伸長・WebP 再エンコード）を走らせてしまう。
#[test]
fn analyze_zip_rejects_more_entries_than_the_limit() {
    use std::io::Write as _;
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        for index in 0..=opfspack::MAX_ENTRY_COUNT {
            writer
                .start_file(format!("pages/{index:05}.txt"), options)
                .unwrap();
            writer.write_all(b"x").unwrap();
        }
        writer.finish().unwrap();
    }
    let error = analyze_zip(&buffer.into_inner()).expect_err("件数超過は弾く");
    assert!(
        matches!(error, ImportError::ZipTooLarge { .. }),
        "ZipTooLarge を期待した: {error}"
    );
}

/// 宣言サイズの合計が上限を超える ZIP も、展開の前に弾く。
///
/// 中央ディレクトリの「展開後サイズ」を巨大に細工した 1 エントリの ZIP を作る
/// （実際には 1 バイトしか入っていない = 宣言を信用しないことも同時に確かめる）。
#[test]
fn analyze_zip_rejects_a_declared_total_over_the_limit() {
    use std::io::Write as _;
    let mut buffer = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut buffer);
        let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
        // 1 エントリの宣言サイズは u32 までなので、合計で上限（10 GiB）を超えるには
        // 複数エントリが要る（u32::MAX を 3 件で約 12 GiB）。
        for index in 0..3 {
            writer
                .start_file(format!("pages/page_{index:04}.webp"), options)
                .unwrap();
            writer.write_all(b"x").unwrap();
        }
        writer.finish().unwrap();
    }
    let mut bytes = buffer.into_inner();
    // 中央ディレクトリのヘッダー（PK\x01\x02）の 24 バイト目が展開後サイズ（u32 LE）。
    let mut patched = 0;
    for offset in 0..bytes.len().saturating_sub(4) {
        if &bytes[offset..offset + 4] == b"PK\x01\x02" {
            bytes[offset + 24..offset + 28].copy_from_slice(&u32::MAX.to_le_bytes());
            patched += 1;
        }
    }
    assert_eq!(patched, 3, "中央ディレクトリのエントリ数");

    let error = analyze_zip(&bytes).expect_err("宣言合計の超過は弾く");
    match error {
        ImportError::ZipTooLarge { detail } => assert!(detail.contains("展開後"), "{detail}"),
        other => panic!("ZipTooLarge を期待した: {other}"),
    }
}

/// PDF の**総ページ数**は描画の前に上限で弾く（セキュリティ評価 F06）。
///
/// 1 ページ 16MPix の検査だけでは、上限内のページが数万ある PDF で
/// レンダリング（+ WebP エンコード）を走らせ続けてしまう。
#[test]
fn import_pdf_rejects_more_pages_than_the_limit() {
    let env = TestEnv::new("too-many-pages");
    let pages = thundoku_core::import::MAX_PDF_PAGES + 1;
    let kids: Vec<String> = (0..pages).map(|index| format!("{} 0 R", index + 3)).collect();
    let mut objects: Vec<Vec<u8>> = Vec::new();
    objects.push(b"<< /Type /Catalog /Pages 2 0 R >>".to_vec());
    objects.push(
        format!(
            "<< /Type /Pages /Count {pages} /Kids [{}] >>",
            kids.join(" ")
        )
        .into_bytes(),
    );
    for _ in 0..pages {
        objects.push(b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>".to_vec());
    }
    let pdf = build_pdf(&objects);

    let started = std::time::Instant::now();
    let error = import_pdf_bytes(
        &env.pool,
        "many-pages.pdf",
        &pdf,
        &env.packs(),
        None,
        &mut no_progress,
        None,
    )
    .expect_err("ページ数超過は弾く");
    match error {
        ImportError::Pdf(message) => assert!(message.contains("ページ数"), "{message}"),
        other => panic!("Pdf を期待した: {other}"),
    }
    // 描画を始めていたら数万ページ分の時間がかかる。
    assert!(
        started.elapsed() < std::time::Duration::from_secs(30),
        "描画の前に弾く（{:?}）",
        started.elapsed()
    );
}

/// 逐次版レンダラは、ページを**順番どおり 1 件ずつ**渡し、件数を返す。
///
/// 取り込みはこの順序に依存して `pages/page_NNNN.webp` を決めるので、飛ばしたり
/// 前後したりすると pack の中身と DB のページ番号がずれる。
#[test]
fn render_pdf_pages_into_emits_pages_in_order() {
    if !pdfium_ready() {
        return;
    }
    // 3 ページの最小 PDF を組む（順序と件数を見るため、高さを変えて区別する）。
    let objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Count 3 /Kids [3 0 R 4 0 R 5 0 R] >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 200] >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 300] >>".to_vec(),
    ];
    let multi = build_pdf(&objects);

    let mut heights: Vec<u32> = Vec::new();
    let mut seen: Vec<usize> = Vec::new();
    let total = thundoku_core::import::pdf::render_pdf_pages_into(
        &multi,
        &mut no_progress,
        &mut |page| {
            seen.push(seen.len() + 1);
            heights.push(page.height);
            Ok(())
        },
    )
    .expect("レンダリングできる");
    assert_eq!(total, 3, "総ページ数を返す");
    assert_eq!(seen, vec![1, 2, 3], "順番どおりに渡す");
    assert_eq!(heights.len(), 3, "全ページを渡す");
    // MediaBox の高さが違うので、画像の高さも単調に増える（順序が入れ替わっていない）
    assert!(
        heights[0] < heights[1] && heights[1] < heights[2],
        "ページの順序が入れ替わっている: {heights:?}"
    );
}

/// `on_page` がエラーを返したら、そこで止めて同じエラーを返す
/// （取り込み側は「ページ番号と実際のページがずれない」ため、失敗ページを飛ばさない）。
#[test]
fn render_pdf_pages_into_stops_on_error() {
    if !pdfium_ready() {
        return;
    }
    let objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Count 2 /Kids [3 0 R 4 0 R] >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 200] >>".to_vec(),
    ];
    let pdf = build_pdf(&objects);
    let mut calls = 0;
    let error = thundoku_core::import::pdf::render_pdf_pages_into(
        &pdf,
        &mut no_progress,
        &mut |_| {
            calls += 1;
            Err(ImportError::Pdf("テスト用の失敗".into()))
        },
    )
    .expect_err("エラーを返す");
    assert!(matches!(error, ImportError::Pdf(_)), "{error}");
    assert_eq!(calls, 1, "1 ページ目で止める（2 ページ目を渡さない）");
}
