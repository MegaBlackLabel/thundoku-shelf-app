//! Import pipeline tests: PDF / EPUB / image ZIP -> .opfspack packs, DB rows
//! and generated tags.

use std::path::PathBuf;

use opfspack::{Identity, PackReader};
use thundoku_core::db;
use thundoku_core::import::{ImportError, import_file, import_pdf_bytes};
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

/// 指定コンテンツ（塗りオペレータ）の 200x200 単一ページ PDF を生成する。
fn solid_pdf_with_content(content: &str) -> Vec<u8> {
    let mut pdf: Vec<u8> = b"%PDF-1.4\n".to_vec();
    let objects: Vec<Vec<u8>> = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Contents 4 0 R >>".to_vec(),
        format!(
            "<< /Length {} >>\nstream\n{}endstream",
            content.len(),
            content
        )
        .into_bytes(),
    ];
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

fn center_rgb(pdf: &[u8]) -> (u8, u8, u8) {
    let pages = thundoku_core::import::pdf::render_pdf_pages(pdf, &mut no_progress).unwrap();
    assert_eq!(pages.len(), 1);
    let img = image::load_from_memory(&pages[0].data).unwrap();
    let rgba = img.to_rgba8();
    let (w, h) = (rgba.width(), rgba.height());
    let p = rgba.get_pixel(w / 2, h / 2);
    (p[0], p[1], p[2])
}

#[test]
fn pdf_rendering_converts_cmyk_red_correctly() {
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
fn unsupported_extension_is_rejected() {
    let env = TestEnv::new("unsupported");
    let path = env.root.join("notes.txt");
    std::fs::write(&path, b"hello").unwrap();
    let err = import_file(&env.pool, &path, &env.packs(), None, &mut no_progress).unwrap_err();
    assert!(matches!(err, ImportError::UnsupportedType(_)));
}

#[test]
fn pdf_import_binds_identity_when_provided() {
    let env = TestEnv::new("pdf-identity");
    let pdf_bytes = std::fs::read(PDF_FIXTURE).unwrap();
    let book_id = uuid::Uuid::new_v4().to_string();
    let identity = Identity {
        sub: "test-sub".into(),
        pack_id: book_id.clone(),
    };
    let imported = import_pdf_bytes(
        &env.pool,
        "bound.pdf",
        &pdf_bytes,
        &env.packs(),
        Some(&identity),
        &mut no_progress,
    )
    .unwrap();
    assert_eq!(imported.book.id, book_id);
    let pack_bytes = std::fs::read(env.packs().join(format!("{book_id}.opfspack"))).unwrap();
    let reader = PackReader::open(&pack_bytes).unwrap();
    assert!(reader.read_entry("pages/page_0001.webp", None).is_err());
    let page = reader
        .read_entry("pages/page_0001.webp", Some(&identity))
        .unwrap();
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
