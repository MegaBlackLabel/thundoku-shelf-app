//! 展開上限とフラグ整合（SEC-04 の続き）の回帰テスト。
//!
//! 背景: `raw_inflate` は上限なしの `read_to_end` で、index が宣言する `size` を
//! 実データと照合していなかった。数十 KiB の DEFLATE 入力から数 GB を展開させ得る
//! うえ、`read_entry_range` は宣言値でスライスしていたため panic し得た。
//! ここでは「細工した pack は確保の前に落ちる」「宣言サイズと実長の不一致は
//! 全経路でエラー」「正常な多ページ pack は今までどおり読める」を固定する。

use opfspack::{
    MAX_ENTRY_COUNT, MAX_ENTRY_SIZE, MAX_STORED_ENTRY_SIZE, MAX_TOTAL_SIZE, PackBuilder, PackError,
    PackFileReader, PackKey, PackReader, PackRootKey, entry_flags, pack_flags,
};

/// テスト用の pack 鍵（ルート鍵は固定値にしておく。暗号化の検証だけが目的）。
fn test_pack_key() -> PackKey {
    PackRootKey::from_bytes([7u8; 32]).derive_pack_key("test-pack")
}

const SIZE_FIELD: usize = 8;
const COMPRESSED_SIZE_FIELD: usize = 16;
const FLAGS_FIELD: usize = 24;

fn crc32(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}

fn align8(n: u64) -> u64 {
    n.div_ceil(8) * 8
}

fn u32_at(bytes: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap())
}

fn u64_at(bytes: &[u8], pos: usize) -> u64 {
    u64::from_le_bytes(bytes[pos..pos + 8].try_into().unwrap())
}

fn index_offset(bytes: &[u8]) -> usize {
    u64_at(bytes, 16) as usize
}

fn index_size(bytes: &[u8]) -> usize {
    u64_at(bytes, 24) as usize
}

/// `index` 番目の index エントリの、フィールド先頭からの相対位置。
fn entry_field(bytes: &[u8], index: usize, field: usize) -> usize {
    let mut pos = index_offset(bytes);
    for i in 0..=index {
        let path_len = u16::from_le_bytes(bytes[pos..pos + 2].try_into().unwrap()) as usize;
        let mime_len = u16::from_le_bytes(
            bytes[pos + 2 + path_len..pos + 4 + path_len]
                .try_into()
                .unwrap(),
        ) as usize;
        if i == index {
            return pos + 2 + path_len + 2 + mime_len + field;
        }
        pos += align8((44 + path_len + mime_len) as u64) as usize;
    }
    panic!("index {index} は範囲外");
}

fn set_size(bytes: &mut [u8], index: usize, size: u64) {
    let pos = entry_field(bytes, index, SIZE_FIELD);
    bytes[pos..pos + 8].copy_from_slice(&size.to_le_bytes());
    fix_index_crc(bytes);
}

fn set_compressed_size(bytes: &mut [u8], index: usize, size: u64) {
    let pos = entry_field(bytes, index, COMPRESSED_SIZE_FIELD);
    bytes[pos..pos + 8].copy_from_slice(&size.to_le_bytes());
    fix_index_crc(bytes);
}

fn set_entry_flags(bytes: &mut [u8], index: usize, flags: u32) {
    let pos = entry_field(bytes, index, FLAGS_FIELD);
    bytes[pos..pos + 4].copy_from_slice(&flags.to_le_bytes());
    fix_index_crc(bytes);
}

fn entry_flags_at(bytes: &[u8], index: usize) -> u32 {
    let pos = entry_field(bytes, index, FLAGS_FIELD);
    u32_at(bytes, pos)
}

fn set_pack_flags(bytes: &mut [u8], flags: u32) {
    bytes[8..12].copy_from_slice(&flags.to_le_bytes());
    fix_header_crc(bytes);
}

fn headers(bytes: &[u8]) -> (u32, u32) {
    (u32_at(bytes, 8), u32_at(bytes, 32))
}

fn fix_index_crc(bytes: &mut [u8]) {
    let offset = index_offset(bytes);
    let size = index_size(bytes);
    let crc = crc32(&bytes[offset..offset + size - 4]);
    bytes[offset + size - 4..offset + size].copy_from_slice(&crc.to_le_bytes());
}

fn fix_header_crc(bytes: &mut [u8]) {
    let crc = crc32(&bytes[..60]);
    bytes[60..64].copy_from_slice(&crc.to_le_bytes());
}

/// `PackReader` は `Debug` を持たないため `expect_err` が使えない。
fn open_error(pack: &[u8]) -> PackError {
    match PackReader::open(pack) {
        Ok(_) => panic!("細工した pack が受理された"),
        Err(error) => error,
    }
}

fn file_open_error(path: &std::path::Path) -> PackError {
    match PackFileReader::open(path) {
        Ok(_) => panic!("細工した pack が受理された"),
        Err(error) => error,
    }
}

/// 単一エントリの pack（`compress` はエントリの圧縮指定）。
fn single_entry_pack(data: Vec<u8>, compress: bool) -> Vec<u8> {
    let mut builder = PackBuilder::new(1_728_000_000_000);
    builder.add_entry("a.bin", data, "application/octet-stream", compress);
    builder.build(None, true).expect("pack を作れる")
}

/// テスト中だけ実ファイルを持つ pack。
struct TempPack(std::path::PathBuf);

impl TempPack {
    fn write(tag: &str, bytes: &[u8]) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!(
            "opfspack-limits-{tag}-{}.opfspack",
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

/// 小さな DEFLATE 入力から宣言サイズを超えて展開しようとしても、宣言 +1 バイトで
/// 打ち切られエラーになる（32 MiB を「1 KiB」と偽った pack）。
#[test]
fn deflate_bomb_is_capped_by_the_declared_size() {
    let bomb = vec![0u8; 32 * 1024 * 1024];
    let mut bytes = single_entry_pack(bomb, true);
    set_size(&mut bytes, 0, 1024);

    let reader = PackReader::open(&bytes).expect("構造としては開ける");
    let error = reader.read_entry("a.bin", None).unwrap_err();
    assert!(matches!(error, PackError::Corrupted(_)), "{error}");
}

/// 上限内でも「宣言 +1 バイト」で打ち切られる（64 MiB を「8 MiB」と偽った pack）。
/// 宣言値を信じて確保するので、展開後の長さは宣言値で決まる。
#[test]
fn expansion_stops_at_the_declared_size() {
    const DECLARED: u64 = 8 * 1024 * 1024;
    let bomb = vec![0u8; 64 * 1024 * 1024];
    let mut bytes = single_entry_pack(bomb, true);
    set_size(&mut bytes, 0, DECLARED);

    let reader = PackReader::open(&bytes).expect("構造としては開ける");
    let error = reader.read_entry("a.bin", None).unwrap_err();
    assert!(matches!(error, PackError::Corrupted(_)), "{error}");
    assert!(
        error.to_string().contains(&(DECLARED + 1).to_string()),
        "宣言 +1 バイトで打ち切られていない: {error}"
    );
}

/// ファイル裏打ちリーダーも同じ上限で止まる。
#[test]
fn file_reader_enforces_the_decompression_cap() {
    let bomb = vec![0u8; 16 * 1024 * 1024];
    let mut bytes = single_entry_pack(bomb, true);
    set_size(&mut bytes, 0, 512);
    let temp = TempPack::write("bomb", &bytes);

    let mut reader = PackFileReader::open(temp.path()).expect("構造としては開ける");
    let error = reader.read_entry_with_key("a.bin", None).unwrap_err();
    assert!(matches!(error, PackError::Corrupted(_)), "{error}");
}

/// 宣言 `size` が実長より大きい pack はエラー（展開後の照合）。
#[test]
fn declared_size_larger_than_payload_is_rejected() {
    let mut bytes = single_entry_pack(pattern(64 * 1024), true);
    let actual = u64_at(&bytes, entry_field(&bytes, 0, SIZE_FIELD));
    set_size(&mut bytes, 0, actual + 1000);

    let reader = PackReader::open(&bytes).expect("構造としては開ける");
    let error = reader.read_entry("a.bin", None).unwrap_err();
    assert!(matches!(error, PackError::Corrupted(_)), "{error}");
}

/// 宣言 `size` が実長より小さい pack もエラー（切り詰めて返さない）。
#[test]
fn declared_size_smaller_than_payload_is_rejected() {
    let mut bytes = single_entry_pack(pattern(64 * 1024), true);
    let actual = u64_at(&bytes, entry_field(&bytes, 0, SIZE_FIELD));
    set_size(&mut bytes, 0, actual - 100);

    let reader = PackReader::open(&bytes).expect("構造としては開ける");
    let error = reader.read_entry("a.bin", None).unwrap_err();
    assert!(matches!(error, PackError::Corrupted(_)), "{error}");
}

/// 非圧縮エントリでも宣言 `size` の不一致はエラー。
#[test]
fn declared_size_mismatch_is_rejected_for_stored_entries() {
    let data = pattern(4096);
    let mut bytes = single_entry_pack(data, false);
    set_size(&mut bytes, 0, 4097);

    let reader = PackReader::open(&bytes).expect("構造としては開ける");
    let error = reader.read_entry("a.bin", None).unwrap_err();
    assert!(matches!(error, PackError::Corrupted(_)), "{error}");
}

/// 切断された DEFLATE データ（保存サイズを削った pack）はエラー。
#[test]
fn truncated_deflate_stream_is_rejected() {
    let data = pattern(64 * 1024);
    let mut bytes = single_entry_pack(data, true);
    let stored = u64_at(&bytes, entry_field(&bytes, 0, COMPRESSED_SIZE_FIELD));
    set_compressed_size(&mut bytes, 0, stored - 16);

    let reader = PackReader::open(&bytes).expect("構造としては開ける");
    let error = reader.read_entry("a.bin", None).unwrap_err();
    assert!(matches!(error, PackError::Corrupted(_)), "{error}");
}

/// 1 エントリの展開上限を超える宣言は `Vec` の確保前に落ちる。
#[test]
fn entry_size_over_the_limit_is_rejected() {
    let mut bytes = single_entry_pack(b"tiny".to_vec(), false);
    set_size(&mut bytes, 0, MAX_ENTRY_SIZE + 1);

    let error = open_error(&bytes);
    assert!(matches!(error, PackError::Corrupted(_)), "{error}");
    assert!(error.to_string().contains("size"), "{error}");
}

/// 1 冊合計の展開上限を超える宣言は落ちる（各エントリは上限内でも合計で見る）。
#[test]
fn total_size_over_the_limit_is_rejected() {
    let mut builder = PackBuilder::new(0);
    for i in 0..5 {
        builder.add_entry(
            &format!("{i}.bin"),
            vec![0u8; 8],
            "application/octet-stream",
            false,
        );
    }
    let mut bytes = builder.build(None, true).unwrap();
    // 前提: 各エントリは単体上限（512 MiB）内でも、5 件で合計上限（2 GiB）を超える。
    const {
        assert!(MAX_ENTRY_SIZE * 5 > MAX_TOTAL_SIZE);
    }
    for i in 0..5 {
        set_size(&mut bytes, i, MAX_ENTRY_SIZE);
    }

    let error = open_error(&bytes);
    assert!(matches!(error, PackError::Corrupted(_)), "{error}");
    assert!(error.to_string().contains("total size"), "{error}");
}

/// 保存サイズ（格納バイト数）の上限を超える宣言も落ちる。
#[test]
fn stored_size_over_the_limit_is_rejected() {
    let mut bytes = single_entry_pack(b"tiny".to_vec(), false);
    set_compressed_size(&mut bytes, 0, MAX_STORED_ENTRY_SIZE + 1);

    let error = open_error(&bytes);
    assert!(matches!(error, PackError::Corrupted(_)), "{error}");
    assert!(error.to_string().contains("stored size"), "{error}");
}

/// 未定義ビットが立ったエントリは破損として拒否する。
#[test]
fn unknown_entry_flag_bits_are_rejected() {
    let mut bytes = single_entry_pack(b"tiny".to_vec(), false);
    let flags = entry_flags_at(&bytes, 0) | 1 << 5;
    set_entry_flags(&mut bytes, 0, flags);

    let error = open_error(&bytes);
    assert!(matches!(error, PackError::Corrupted(_)), "{error}");
    assert!(error.to_string().contains("unknown entry flags"), "{error}");
}

/// `ENCRYPTED` だけ立った（平文 pack に暗号化エントリを混ぜた）エントリは拒否する。
#[test]
fn encrypted_entry_flag_in_a_plaintext_pack_is_rejected() {
    let mut bytes = single_entry_pack(b"tiny".to_vec(), false);
    set_entry_flags(&mut bytes, 0, entry_flags::ENCRYPTED);

    let error = open_error(&bytes);
    assert!(matches!(error, PackError::Corrupted(_)), "{error}");
}

/// `IDENTITY_BOUND` だけ立ったエントリは v3 では不正なビット（v2 の identity
/// 束縛は version 3 に存在しない）として拒否する。
#[test]
fn identity_bound_flag_is_rejected_in_v3() {
    let mut bytes = single_entry_pack(b"tiny".to_vec(), false);
    set_entry_flags(&mut bytes, 0, entry_flags::IDENTITY_BOUND);

    let error = open_error(&bytes);
    assert!(matches!(error, PackError::Corrupted(_)), "{error}");
}

/// 平文エントリしか無い pack に header の `ENCRYPTED` だけ立てるのは矛盾。
#[test]
fn encrypted_pack_flag_with_plaintext_entries_is_rejected() {
    let mut bytes = single_entry_pack(b"tiny".to_vec(), false);
    let (flags, _) = headers(&bytes);
    set_pack_flags(&mut bytes, flags | pack_flags::ENCRYPTED);

    let error = open_error(&bytes);
    assert!(matches!(error, PackError::Corrupted(_)), "{error}");
}

/// 暗号化エントリを持つ pack で header が `ENCRYPTED` を宣言しないのは矛盾。
#[test]
fn encrypted_entries_without_the_pack_flag_are_rejected() {
    let key = test_pack_key();
    let mut builder = PackBuilder::new(0);
    builder.add_entry("a.bin", b"tiny".to_vec(), "application/octet-stream", false);
    let mut bytes = builder.build(Some(&key), true).unwrap();
    assert_ne!(headers(&bytes).0 & pack_flags::ENCRYPTED, 0);
    let flags = headers(&bytes).0 & !pack_flags::ENCRYPTED;
    set_pack_flags(&mut bytes, flags);

    let error = open_error(&bytes);
    assert!(matches!(error, PackError::Corrupted(_)), "{error}");
}

/// 暗号化 pack に平文エントリを混ぜるのは矛盾（builder は全エントリを暗号化する）。
#[test]
fn plaintext_entry_in_an_encrypted_pack_is_rejected() {
    let key = test_pack_key();
    let mut builder = PackBuilder::new(0);
    builder.add_entry("a.bin", b"tiny".to_vec(), "application/octet-stream", false);
    builder.add_entry("b.bin", b"tiny".to_vec(), "application/octet-stream", false);
    let mut bytes = builder.build(Some(&key), true).unwrap();
    set_entry_flags(&mut bytes, 1, entry_flags::NONE);

    let error = open_error(&bytes);
    assert!(matches!(error, PackError::Corrupted(_)), "{error}");
}

/// 件数上限ちょうどは読める（境界）。上限は `entry_count` の検査で先に落ちる。
#[test]
fn entry_count_at_the_limit_is_accepted() {
    let bytes = many_entries(MAX_ENTRY_COUNT);
    let reader = PackReader::open(&bytes).expect("上限ちょうどは開ける");
    assert_eq!(reader.entries().len(), MAX_ENTRY_COUNT as usize);
}

/// 件数上限を超える pack は拒否する（index 長では説明できてしまう件数でも）。
#[test]
fn entry_count_over_the_limit_is_rejected() {
    let bytes = many_entries(MAX_ENTRY_COUNT + 1);
    let error = open_error(&bytes);
    assert!(matches!(error, PackError::Corrupted(_)), "{error}");
    assert!(error.to_string().contains("exceeds limit"), "{error}");
}

/// 正常系: 数 MB の多ページ pack（圧縮・非圧縮混在）は今までどおり読める。
#[test]
fn valid_multipage_pack_still_reads() {
    const PAGES: usize = 300;
    let mut builder = PackBuilder::new(1_728_000_000_000);
    builder.add_entry(
        "metadata.json",
        br#"{"schemaVersion":1,"title":"Test Book"}"#.to_vec(),
        "application/json",
        false,
    );
    builder.add_entry("thumbnail.webp", page(0), "image/webp", false);
    for i in 1..=PAGES {
        builder.add_entry(
            &format!("pages/{i:04}.webp"),
            page(i),
            "image/webp",
            i % 2 == 0,
        );
    }
    let bytes = builder.build(None, true).expect("pack を作れる");
    let reader = PackReader::open(&bytes).expect("正常な pack は開ける");
    assert_eq!(reader.entries().len(), PAGES + 2);
    for i in [1, 2, 150, 299, PAGES] {
        assert_eq!(
            reader
                .read_entry(&format!("pages/{i:04}.webp"), None)
                .expect("読める"),
            page(i),
            "pages/{i:04}.webp の内容が一致しない"
        );
    }
    // 範囲読みも従来どおり動く
    assert_eq!(
        reader
            .read_entry_range("pages/0001.webp", 10, 60, None)
            .expect("範囲読み"),
        page(1)[10..60].to_vec()
    );
}

/// 正常系: 空エントリ（size 0）と非圧縮エントリは今までどおり読める。
#[test]
fn empty_and_uncompressed_entries_still_read() {
    let mut builder = PackBuilder::new(0);
    builder.add_entry(
        "empty-compressed.bin",
        Vec::new(),
        "application/octet-stream",
        true,
    );
    builder.add_entry(
        "empty-stored.bin",
        Vec::new(),
        "application/octet-stream",
        false,
    );
    builder.add_entry(
        "stored.bin",
        pattern(1024),
        "application/octet-stream",
        false,
    );
    let bytes = builder.build(None, false).expect("pack を作れる");
    let reader = PackReader::open(&bytes).expect("開ける");

    assert!(
        reader
            .read_entry("empty-compressed.bin", None)
            .expect("空エントリは読める")
            .is_empty()
    );
    assert!(
        reader
            .read_entry("empty-stored.bin", None)
            .expect("空エントリは読める")
            .is_empty()
    );
    assert_eq!(
        reader
            .read_entry("stored.bin", None)
            .expect("非圧縮エントリは読める"),
        pattern(1024)
    );
}

/// 宣言 `size` を偽った pack の範囲読みは panic せずエラーになる。
#[test]
fn range_read_on_a_dishonest_index_is_an_error() {
    let mut bytes = single_entry_pack(pattern(4096), true);
    set_size(&mut bytes, 0, 4096 + 5000);

    let reader = PackReader::open(&bytes).expect("構造としては開ける");
    let error = reader
        .read_entry_range("a.bin", 0, 4096 + 5000, None)
        .unwrap_err();
    assert!(
        matches!(error, PackError::Corrupted(_) | PackError::InvalidRange(_)),
        "{error}"
    );
}

/// 上限超過を宣言した pack はファイル裏打ちでも開けない。
#[test]
fn file_reader_rejects_over_limit_entries() {
    let mut bytes = single_entry_pack(b"tiny".to_vec(), false);
    set_size(&mut bytes, 0, MAX_ENTRY_SIZE + 1);
    let temp = TempPack::write("over-limit", &bytes);

    let error = file_open_error(temp.path());
    assert!(matches!(error, PackError::Corrupted(_)), "{error}");
}

/// 中身が偏りの少ないバイト列（DEFLATE がほぼ縮まない）。
fn pattern(len: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(len);
    let mut state = 0x1234_5678u32;
    while out.len() < len {
        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(len);
    out
}

/// 1 ページ分のデータ（数 MB 級の pack を作るため、ほどよく圧縮が効く内容）。
fn page(n: usize) -> Vec<u8> {
    format!("THUNDOKU_PAGE_{n:04}:{}", "abc".repeat(5000)).into_bytes()
}

/// `count` 件のエントリを持つ pack（上限検査の境界を作るためだけに使う）。
fn many_entries(count: u32) -> Vec<u8> {
    let mut builder = PackBuilder::new(0);
    for i in 0..count {
        builder.add_entry(&format!("p{i:05}.txt"), b"x".to_vec(), "text/plain", false);
    }
    builder.build(None, true).expect("pack を作れる")
}
