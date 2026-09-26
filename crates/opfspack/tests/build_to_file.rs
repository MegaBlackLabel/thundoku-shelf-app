//! `PackBuilder::build_to_file` のテスト。
//!
//! 目的: pack を**ファイルへ直接**書き出す経路でも、メモリ組み立て（`build`）と
//! **バイト単位で同一**の pack を作ること（形式の互換性はここが正）。大きい pack で
//! 組み立て中のバイト列を RAM に持たないための経路なので、差が出ると
//! Web 版や既存の pack と互換が壊れる。

use opfspack::{PackBuilder, PackKey, PackRootKey};

fn test_key() -> PackKey {
    PackRootKey::from_bytes([9u8; 32]).derive_pack_key("pack-1")
}

/// 同じ内容の pack を毎回組み立て直す（`build` は消費するため）。
fn builder() -> PackBuilder {
    let mut builder = PackBuilder::new(1_728_000_000_000);
    builder.add_entry(
        "metadata.json",
        br#"{"schemaVersion":1,"title":"Test Book"}"#.to_vec(),
        "application/json",
        true,
    );
    builder.add_entry("thumbnail.webp", vec![7u8; 1000], "image/webp", true);
    // 8 バイト境界の詰め物が入る長さ（1001, 1002 …）も混ぜる
    builder.add_entry("pages/001.webp", vec![1u8; 1001], "image/webp", true);
    builder.add_entry("pages/002.webp", vec![2u8; 4096], "image/webp", true);
    builder
}

fn temp_dir() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join("opfspack-build-to-file");
    let _ = std::fs::create_dir_all(&dir);
    dir
}

/// 平文 / 圧縮の pack は、ファイルへ書いてもメモリ組み立てと**バイト単位で同一**
/// （DEFLATE は決定的なので比較できる）。
#[test]
fn build_to_file_is_byte_identical_for_plain_and_compressed_packs() {
    for (label, compress) in [("plain", false), ("compressed", true)] {
        let memory = builder().build(None, compress).expect("メモリで組める");
        let path = temp_dir().join(format!("build-to-file-{label}.opfspack"));
        let written = builder()
            .build_to_file(&path, None, compress)
            .expect("ファイルへ書ける");
        assert_eq!(
            written,
            memory.len() as u64,
            "{label}: 書いたバイト数が一致しない"
        );
        assert_eq!(
            std::fs::read(&path).expect("読める"),
            memory,
            "{label}: ファイルとメモリで中身が違う"
        );
        let _ = std::fs::remove_file(&path);
    }
}

/// 暗号化 pack は IV が毎回乱数なのでバイト同一にはならないが、**構造と復号内容**は
/// 一致する（ファイル経路でも同じ pack として読める）。
#[test]
fn build_to_file_writes_an_equivalent_encrypted_pack() {
    let key = test_key();
    let memory = builder().build(Some(&key), true).expect("メモリで組める");
    let path = temp_dir().join("build-to-file-encrypted.opfspack");
    let written = builder()
        .build_to_file(&path, Some(&key), true)
        .expect("ファイルへ書ける");
    assert_eq!(written, memory.len() as u64, "長さが一致しない");

    let from_memory = opfspack::PackReader::open(&memory).expect("メモリ版を開ける");
    let from_file = opfspack::PackFileReader::open(&path).expect("ファイル版を開ける");
    assert_eq!(
        from_file.header().entry_count,
        from_memory.header().entry_count
    );
    assert_eq!(from_file.header().flags, from_memory.header().flags);
    // `PackEntry` は IV を含む（暗号化は毎回乱数）ので、**安定したフィールドだけ**比べる。
    let file_entries = from_file.entries();
    let memory_entries = from_memory.entries();
    assert_eq!(file_entries.len(), memory_entries.len());
    for (from_file, from_memory) in file_entries.iter().zip(memory_entries) {
        assert_eq!(from_file.path, from_memory.path);
        assert_eq!(from_file.mime_type, from_memory.mime_type);
        assert_eq!(from_file.offset, from_memory.offset, "{}", from_file.path);
        assert_eq!(from_file.size, from_memory.size, "{}", from_file.path);
        assert_eq!(
            from_file.compressed_size, from_memory.compressed_size,
            "{}",
            from_file.path
        );
        assert_eq!(from_file.flags, from_memory.flags, "{}", from_file.path);
    }
    for entry in from_memory.entries() {
        assert_eq!(
            from_file
                .read_entry(&entry.path, Some(&key))
                .expect("復号できる"),
            from_memory
                .read_entry(&entry.path, Some(&key))
                .expect("復号できる"),
            "{}: 復号した中身が違う",
            entry.path
        );
    }
    let _ = std::fs::remove_file(&path);
}

/// 書き出せないパス（存在しないディレクトリ）はエラーになり、ファイルを残さない。
#[test]
fn build_to_file_reports_an_unwritable_path() {
    let path = temp_dir().join("does-not-exist").join("pack.opfspack");
    let error = builder()
        .build_to_file(&path, None, false)
        .expect_err("書けないはず");
    assert!(matches!(error, opfspack::PackError::Io(_)), "{error:?}");
    assert!(!path.exists(), "失敗したのにファイルが残っている");
}
