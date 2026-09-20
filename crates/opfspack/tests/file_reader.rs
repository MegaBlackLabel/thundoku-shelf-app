//! ファイル裏打ちリーダー（`PackFileReader`）のテスト。
//!
//! 目的: 大きい pack から 1 エントリ（表紙）だけ取り出すときに、pack 全体を
//! メモリへ読まずに済ませる（この環境には 354 MB の pack がある）。バイト列版
//! （`PackReader`）と同じ内容・同じ失敗の仕方になることを固定する。

use opfspack::{Identity, PackBuilder, PackError, PackFileReader, PackReader, derived_pack_key};

fn entry_data(n: usize) -> Vec<u8> {
    format!("THUNDOKU_PAGE_{n:04}:{}", "abc".repeat(1000)).into_bytes()
}

const META: &[u8] = br#"{"schemaVersion":1,"title":"Test Book"}"#;

fn build_pack(identity: Option<&Identity>, compress: bool) -> Vec<u8> {
    let mut builder = PackBuilder::new(1_728_000_000_000);
    builder.add_entry("metadata.json", META.to_vec(), "application/json", compress);
    builder.add_entry("thumbnail.webp", entry_data(1), "image/webp", compress);
    builder.add_entry("pages/001.webp", entry_data(2), "image/webp", compress);
    builder.build(identity, compress).expect("pack を作れる")
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

/// バイト列版とヘッダ・インデックス・エントリ内容が一致する。
#[test]
fn file_reader_matches_the_in_memory_reader() {
    let bytes = build_pack(None, false);
    let expect = PackReader::open(&bytes).expect("バイト列版で開ける");
    let temp = TempPack::write("plain", &bytes);
    let mut reader = PackFileReader::open(temp.path()).expect("ファイル版で開ける");

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

/// 圧縮 + identity 束縛の pack でも、事前導出した鍵で読める（鍵が無ければ同じ失敗）。
#[test]
fn file_reader_reads_compressed_and_encrypted_entries() {
    let identity = Identity {
        sub: "sub-1".into(),
        pack_id: "pack-1".into(),
    };
    let bytes = build_pack(Some(&identity), true);
    let expect = PackReader::open(&bytes).expect("バイト列版で開ける");
    let temp = TempPack::write("encrypted", &bytes);
    let mut reader = PackFileReader::open(temp.path()).expect("ファイル版で開ける");
    let key = derived_pack_key(&identity);

    assert_eq!(
        reader
            .read_entry_with_key("pages/001.webp", Some(&key))
            .expect("読める"),
        expect
            .read_entry("pages/001.webp", Some(&identity))
            .expect("読める")
    );
    assert!(
        matches!(
            reader.read_entry_with_key("pages/001.webp", None),
            Err(PackError::IdentityRequired(_))
        ),
        "鍵なしで identity 束縛エントリが読めてしまった"
    );
}

/// 無いエントリはバイト列版と同じ `NotFound`。
#[test]
fn file_reader_reports_a_missing_entry() {
    let bytes = build_pack(None, false);
    let temp = TempPack::write("missing", &bytes);
    let mut reader = PackFileReader::open(temp.path()).expect("ファイル版で開ける");

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
