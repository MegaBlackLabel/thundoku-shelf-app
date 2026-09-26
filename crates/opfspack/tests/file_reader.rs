//! ファイル裏打ちリーダー（`PackFileReader`）のテスト。
//!
//! 目的: 大きい pack から 1 エントリ（表紙）だけ取り出すときに、pack 全体を
//! メモリへ読まずに済ませる（この環境には 354 MB の pack がある）。バイト列版
//! （`PackReader`）と同じ内容・同じ失敗の仕方になることを固定する。

use opfspack::{PackBuilder, PackError, PackFileReader, PackKey, PackRead, PackReader, PackRootKey};

fn entry_data(n: usize) -> Vec<u8> {
    format!("THUNDOKU_PAGE_{n:04}:{}", "abc".repeat(1000)).into_bytes()
}

const META: &[u8] = br#"{"schemaVersion":1,"title":"Test Book"}"#;

/// テスト用のルート鍵（固定値）と、その pack 鍵。
fn test_key() -> PackKey {
    PackRootKey::from_bytes([9u8; 32]).derive_pack_key("pack-1")
}

fn build_pack(key: Option<&PackKey>, compress: bool) -> Vec<u8> {
    let mut builder = PackBuilder::new(1_728_000_000_000);
    builder.add_entry("metadata.json", META.to_vec(), "application/json", compress);
    builder.add_entry("thumbnail.webp", entry_data(1), "image/webp", compress);
    builder.add_entry("pages/001.webp", entry_data(2), "image/webp", compress);
    builder.build(key, compress).expect("pack を作れる")
}

/// テスト中だけ実ファイルを持つ pack。
struct TempPack(std::path::PathBuf);

impl TempPack {
    fn write(tag: &str, bytes: &[u8]) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "opfspack-file-reader-{tag}-{}.opfspack",
            std::process::id()
        ));
        std::fs::write(&path, bytes).expect("pack を書ける");
        Self(path)
    }

    fn path(&self) -> &std::path::Path {
        &self.0
    }
}

impl Drop for TempPack {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// どちらのリーダーでも同じ `PackRead` で読める（呼び出し側が「メモリに載せるか」
/// 「ファイルから読むか」を選べるようにする＝大きい pack を扱う経路を共有する）。
#[test]
fn both_readers_share_the_pack_read_api() {
    let key = test_key();
    let bytes = build_pack(Some(&key), true);
    let temp = TempPack::write("shared-api", &bytes);
    let memory = PackReader::open(&bytes).expect("バイト列版で開ける");
    let file = PackFileReader::open(temp.path()).expect("ファイル版で開ける");

    let readers: [(&str, &dyn PackRead); 2] = [("memory", &memory), ("file", &file)];
    for (label, reader) in readers {
        assert_eq!(
            reader.header().entry_count,
            memory.header().entry_count,
            "{label}: ヘッダが一致しない"
        );
        assert_eq!(reader.entries().len(), memory.entries().len(), "{label}");
        assert_eq!(
            reader
                .read_entry("metadata.json", Some(&key))
                .expect("読める"),
            META,
            "{label}: メタデータが一致しない"
        );
        assert_eq!(
            reader
                .read_entry_range("metadata.json", 0, 5, Some(&key))
                .expect("範囲読みできる"),
            &META[..5],
            "{label}: 範囲読みが一致しない"
        );
        assert!(
            matches!(reader.read_entry("missing", None), Err(PackError::NotFound(_))),
            "{label}: 無いエントリの扱いが違う"
        );
        // pack 全体のハッシュも同じ（ファイル裏打ちは順に読んで計算する）
        assert_eq!(
            reader.source_sha256().expect("ハッシュを計算できる"),
            memory.source_sha256().expect("ハッシュを計算できる"),
            "{label}: pack 全体の SHA-256 が一致しない"
        );
    }
}

/// バイト列版とヘッダ・インデックス・エントリ内容が一致する。
#[test]
fn file_reader_matches_the_in_memory_reader() {
    let bytes = build_pack(None, false);
    let expect = PackReader::open(&bytes).expect("バイト列版で開ける");
    let temp = TempPack::write("plain", &bytes);
    let reader = PackFileReader::open(temp.path()).expect("ファイル版で開ける");

    assert_eq!(reader.header().version, expect.header().version);
    assert_eq!(reader.header().entry_count, expect.header().entry_count);
    assert_eq!(reader.header().index_offset, expect.header().index_offset);
    assert_eq!(reader.entries(), expect.entries());

    for path in ["metadata.json", "thumbnail.webp", "pages/001.webp"] {
        assert_eq!(
            reader.read_entry_with_key(path, None).expect("読める"),
            expect.read_entry_with_key(path, None).expect("読める"),
            "{path} の内容が一致しない"
        );
    }
}

/// 圧縮 + 暗号化の pack でも、事前導出した鍵で読める（鍵が無ければ同じ失敗）。
#[test]
fn file_reader_reads_compressed_and_encrypted_entries() {
    let key = test_key();
    let bytes = build_pack(Some(&key), true);
    let expect = PackReader::open(&bytes).expect("バイト列版で開ける");
    let temp = TempPack::write("encrypted", &bytes);
    let reader = PackFileReader::open(temp.path()).expect("ファイル版で開ける");

    assert_eq!(
        reader
            .read_entry_with_key("pages/001.webp", Some(&key))
            .expect("読める"),
        expect
            .read_entry("pages/001.webp", Some(&key))
            .expect("読める")
    );
    assert!(
        matches!(
            reader.read_entry_with_key("pages/001.webp", None),
            Err(PackError::KeyRequired(_))
        ),
        "鍵なしで暗号化エントリが読めてしまった"
    );
}

/// 無いエントリはバイト列版と同じ `NotFound`。
#[test]
fn file_reader_reports_a_missing_entry() {
    let bytes = build_pack(None, false);
    let temp = TempPack::write("missing", &bytes);
    let reader = PackFileReader::open(temp.path()).expect("ファイル版で開ける");

    assert!(matches!(
        reader.read_entry_with_key("pages/999.webp", None),
        Err(PackError::NotFound(_))
    ));
}

/// インデックスを壊した pack は開けない（CRC 検証が効いている）。
#[test]
fn file_reader_rejects_a_corrupted_index() {
    let bytes = build_pack(None, false);
    let index_offset = {
        let reader = PackReader::open(&bytes).expect("バイト列版で開ける");
        reader.header().index_offset as usize
    };
    let mut corrupted = bytes.clone();
    corrupted[index_offset + 8] ^= 0xff;
    let temp = TempPack::write("corrupt-index", &corrupted);

    assert!(matches!(
        PackFileReader::open(temp.path()),
        Err(PackError::Corrupted(_))
    ));
}

/// 開けないパスは I/O エラーとして返る（panic しない）。
#[test]
fn file_reader_reports_a_missing_file() {
    let mut path = std::env::temp_dir();
    path.push("opfspack-file-reader-does-not-exist.opfspack");
    let _ = std::fs::remove_file(&path);

    assert!(matches!(PackFileReader::open(&path), Err(PackError::Io(_))));
}
