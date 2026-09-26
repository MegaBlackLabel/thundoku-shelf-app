//! `PackBuilder::build_to_file_streaming`（エントリを 1 件ずつ供給して組み立てる）のテスト。
//!
//! 目的: 10 GiB 級の pack でも **RAM に載るのが 1 エントリ分だけ**になること。
//! そのために (1) メモリ組み立てと**バイト単位で同一**の pack を作ること、
//! (2) データを**まとめて要求しない**こと（前のエントリを書いてから次を要求する）、
//! (3) 途中で失敗したら壊れた pack を残さないこと を固定する。

use opfspack::{EntrySpec, PackBuilder, PackError, PackFileReader, PackKey, PackRootKey};

const META: &[u8] = br#"{"schemaVersion":1,"title":"Test Book"}"#;
const CREATED_AT: u64 = 1_728_000_000_000;

fn test_key() -> PackKey {
    PackRootKey::from_bytes([9u8; 32]).derive_pack_key("pack-1")
}

fn spec(path: &str, mime: &str, compress: bool) -> EntrySpec {
    EntrySpec {
        path: path.to_owned(),
        mime_type: mime.to_owned(),
        compress,
    }
}

/// エントリと生データ（順序はわざと path 昇順でない）。
fn entries() -> Vec<(EntrySpec, Vec<u8>)> {
    vec![
        (spec("thumbnail.webp", "image/webp", true), vec![7u8; 1000]),
        (
            spec("metadata.json", "application/json", true),
            META.to_vec(),
        ),
        (spec("pages/002.webp", "image/webp", true), vec![2u8; 4096]),
        // 圧縮しないエントリ + 8 バイト境界の詰め物が入る長さ
        (spec("pages/001.webp", "image/webp", false), vec![1u8; 1001]),
    ]
}

fn entries_specs() -> Vec<EntrySpec> {
    entries().into_iter().map(|(spec, _)| spec).collect()
}

fn temp_dir(tag: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("opfspack-build-streaming-{tag}"));
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// メモリ組み立てと同じ pack を、1 件ずつ供給して作れる（平文 / 圧縮 / 暗号化）。
#[test]
fn streaming_build_is_byte_identical_to_the_memory_build() {
    // 暗号化は IV が毎回乱数なのでバイト同一にはならない（長さと復号内容だけ固定する）。
    for (label, key, compress, byte_identical) in [
        ("plain", None, false, true),
        ("compressed", None, true, true),
        ("encrypted", Some(test_key()), true, false),
    ] {
        // メモリ組み立て（同じエントリを add_entry する）。
        let mut memory_builder = PackBuilder::new(CREATED_AT);
        for (entry, data) in entries() {
            memory_builder.add_entry(&entry.path, data, &entry.mime_type, entry.compress);
        }
        let memory = memory_builder
            .build(key.as_ref(), compress)
            .expect("組める");

        // 1 件ずつ供給してファイルへ書く。
        let path = temp_dir(label).join(format!("stream-{label}.opfspack"));
        let mut payloads: std::collections::HashMap<String, Vec<u8>> = entries()
            .into_iter()
            .map(|(entry, data)| (entry.path, data))
            .collect();
        let written = PackBuilder::new(CREATED_AT)
            .build_to_file_streaming(&path, entries_specs(), key.as_ref(), compress, |entry| {
                payloads
                    .remove(entry)
                    .ok_or_else(|| PackError::Io(format!("missing data: {entry}")))
            })
            .expect("書ける");

        assert_eq!(written, memory.len() as u64, "{label}: 書いた長さ");
        let on_disk = std::fs::read(&path).unwrap();
        assert_eq!(on_disk.len(), memory.len(), "{label}: 全体の長さ");
        if byte_identical {
            assert_eq!(on_disk, memory, "{label}: バイト一致");
        }

        // 読める pack になっている（同じ内容が取り出せる）。
        let reader = PackFileReader::open(&path).expect("開ける");
        for (entry, data) in entries() {
            let got = reader
                .read_entry(&entry.path, key.as_ref())
                .unwrap_or_else(|e| panic!("{label}: {} を読めない: {e}", entry.path));
            assert_eq!(got, data, "{label}: {}", entry.path);
        }
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}

/// データは**まとめて要求しない**: 2 件目を要求した時点で、1 件目はもうファイルへ
/// 書かれている（= 同時にメモリへ載るのは 1 エントリ分だけ）。
#[test]
fn streaming_build_requests_entries_one_at_a_time() {
    let dir = temp_dir("lazy");
    let path = dir.join("lazy.opfspack");
    let big = 16 * 1024;
    let specs = vec![
        spec("pages/001.webp", "image/webp", false),
        spec("pages/002.webp", "image/webp", false),
    ];
    let observed: std::cell::RefCell<Vec<u64>> = std::cell::RefCell::new(Vec::new());
    let mut seen = 0usize;

    PackBuilder::new(CREATED_AT)
        .build_to_file_streaming(&path, specs, None, false, |_entry| {
            let on_disk = std::fs::metadata(&path).map(|m| m.len()).unwrap_or(0);
            observed.borrow_mut().push(on_disk);
            seen += 1;
            Ok(vec![seen as u8; big])
        })
        .expect("書ける");

    let observed = observed.into_inner();
    assert_eq!(observed.len(), 2, "2 件だけ要求する");
    // 1 件目の時点では本体がまだ書かれていない（ヘッダーも本体の後で書く）。
    assert_eq!(observed[0], 0, "1 件目は 0 バイトのうちに要求される");
    // 2 件目を要求する前に、1 件目の 16KiB が（BufWriter の分を除いても）書かれている。
    assert!(
        observed[1] >= 8 * 1024,
        "1 件目を書き終えてから 2 件目を要求する（観測: {} バイト）",
        observed[1]
    );
    assert!(
        observed[1] < 2 * big as u64,
        "2 件目を要求する時点で全部は書かれていない（観測: {} バイト）",
        observed[1]
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// 供給に失敗したらエラーになり、**書きかけの pack を残さない**。
#[test]
fn streaming_build_removes_the_partial_file_on_error() {
    let dir = temp_dir("error");
    let path = dir.join("error.opfspack");
    let result = PackBuilder::new(CREATED_AT).build_to_file_streaming(
        &path,
        entries_specs(),
        None,
        true,
        |entry| {
            if entry == "pages/001.webp" {
                Err(PackError::Io("ページの読み出しに失敗".into()))
            } else {
                Ok(vec![3u8; 100])
            }
        },
    );
    assert!(result.is_err(), "エラーになる");
    assert!(!path.exists(), "書きかけの pack を残さない");
    let _ = std::fs::remove_dir_all(&dir);
}
