//! 資源上限（SEC-04）の回帰テスト。
//!
//! pack の `entry_count` は index 長と突き合わせずに `Vec::with_capacity` へ
//! 渡されていた。ヘッダ CRC は攻撃者も計算できるため、68 バイトの細工データ
//! （ヘッダ + index CRC のみ）で数百 GB の事前確保を試みさせられる。
//! `open` は index 長から導ける件数上限を先に検査し、確保を試みない。

use opfspack::{PackError, PackReader};

fn crc32(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}

/// `PackReader` は `Debug` を持たないため `expect_err` が使えない。
fn open_error(pack: &[u8]) -> PackError {
    match PackReader::open(pack) {
        Ok(_) => panic!("細工した pack が受理された"),
        Err(error) => error,
    }
}

/// ヘッダ 64B + index 領域 + index CRC の最小構成を組み立てる。
fn crafted_pack(entry_count: u32, index_region: &[u8]) -> Vec<u8> {
    let mut header = vec![0u8; 64];
    header[0..4].copy_from_slice(b"OPFS");
    header[4..8].copy_from_slice(&2u32.to_le_bytes());
    header[16..24].copy_from_slice(&64u64.to_le_bytes());
    let index_size = index_region.len() as u64 + 4;
    header[24..32].copy_from_slice(&index_size.to_le_bytes());
    header[32..36].copy_from_slice(&entry_count.to_le_bytes());
    header[40..48].copy_from_slice(&1_728_000_000_000u64.to_le_bytes());
    let header_crc = crc32(&header[..60]);
    header[60..64].copy_from_slice(&header_crc.to_le_bytes());
    let mut out = header;
    out.extend_from_slice(index_region);
    out.extend_from_slice(&crc32(index_region).to_le_bytes());
    out
}

/// index 領域が空（= CRC 4 バイトのみ）なのに `entry_count = u32::MAX` を
/// 宣言した pack は、確保を試みる前に構造エラーとして拒否される。
#[test]
fn huge_entry_count_is_rejected_before_allocation() {
    let pack = crafted_pack(u32::MAX, &[]);
    let error = open_error(&pack);
    assert!(matches!(error, PackError::Corrupted(_)), "{error}");
}

/// index 長で説明できる件数（1 エントリ = 最低 48 バイト）を超える宣言も拒否する。
/// ここでは index 領域 0 バイトに対して控えめな 1024 件を宣言する。
#[test]
fn entry_count_beyond_index_capacity_is_rejected() {
    let pack = crafted_pack(1024, &[]);
    let error = open_error(&pack);
    assert!(matches!(error, PackError::Corrupted(_)), "{error}");
}

/// 境界: index 長ちょうどの件数は「容量の検査」を通り、以降の検証で落ちる。
/// ここでは 48 バイト（1 エントリ分の最小長）に 1 件を宣言し、
/// 切り詰めエラーではなくエントリ解釈のエラーになることを確認する。
#[test]
fn entry_count_at_index_capacity_is_not_rejected_by_capacity_check() {
    let pack = crafted_pack(1, &[0u8; 48]);
    let error = open_error(&pack);
    let message = error.to_string();
    assert!(matches!(error, PackError::Corrupted(_)), "{message}");
    assert!(
        !message.contains("exceeds index capacity"),
        "容量検査で落ちてはいけない: {message}"
    );
}
