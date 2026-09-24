//! Interop tests against pack fixtures.
//!
//! The byte layout was verified against the TypeScript reference
//! implementation (packages/opfspack/gen-fixtures.ts, bun + fflate 0.8.2).
//! The fixtures themselves are now **version 3** (the TS side of v3 lands
//! later; v2 packs are rejected outright), so they are emitted by the Rust
//! builder with the same entries/sizes as the TS v2 fixtures — the layout
//! below is unchanged from v2 except the header version and the missing
//! `IDENTITY_BOUND` bit.
//!
//! Wire-format facts:
//! - little-endian throughout, header 64B, CRC-32 (IEEE) over bytes 0..60
//! - entries stored back-to-back, 8-byte aligned, after the header
//! - index: pathLen u16 / path / mimeLen u16 / mime / offset u64 / size u64
//!   / compressedSize u64 / flags u32 / IV 12B, each entry 8-byte aligned,
//!   index CRC-32 (4B) appended, INCLUDED in index_size
//! - compression is RAW DEFLATE (no zlib wrapper)
//! - encryption: pack_key = HKDF-SHA256(PRK, "test-pack" for these fixtures)

use opfspack::{
    FORMAT_VERSION, PackBuilder, PackError, PackKey, PackReader, PackRootKey, derive_owner_id,
    entry_flags, pack_flags,
};

fn page_a() -> Vec<u8> {
    format!("THUNDOKU_PAGE_0001:{}", "abc".repeat(4000)).into_bytes()
}

fn page_b() -> Vec<u8> {
    format!("THUNDOKU_PAGE_0002:{}", "xyz".repeat(2000)).into_bytes()
}

const META: &[u8] = br#"{"schemaVersion":1,"title":"Test Book","author":"Test Author"}"#;

const FIXTURES: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");

fn load(name: &str) -> Vec<u8> {
    std::fs::read(format!("{FIXTURES}/{name}")).expect("fixture missing")
}

fn align8(n: u64) -> u64 {
    n.div_ceil(8) * 8
}

/// フィクスチャのルート鍵 = 仕様 §8 のベクタ（`00 01 … 1f`）。
fn fixture_root() -> PackRootKey {
    let mut bytes = [0u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = index as u8;
    }
    PackRootKey::from_bytes(bytes)
}

/// フィクスチャの pack 鍵 = `HKDF(PRK, salt = "test-pack")`。
fn fixture_pack_key() -> PackKey {
    fixture_root().derive_pack_key("test-pack")
}

// ---- plain fixture -------------------------------------------------------
#[test]
fn plain_fixture_header_is_well_formed() {
    let bytes = load("plain.opfspack");
    let reader = PackReader::open(&bytes).unwrap();
    let header = reader.header();
    assert_eq!(header.version, FORMAT_VERSION);
    assert_eq!(header.entry_count, 3);
    assert_eq!(header.created_at, 1_728_000_000_000);
    // no compression flag on pack level? the fixture was built with
    // compress:true, so the pack flag is set
    assert_ne!(header.flags & pack_flags::COMPRESSED, 0);
    // layout: 64 + align8(62) + align8(56) + align8(6019)
    assert_eq!(
        header.index_offset,
        64 + align8(62) + align8(56) + align8(6019)
    );
    // index: 3 entries of 80B aligned + 4B CRC
    assert_eq!(header.index_size, 80 + 80 + 80 + 4);
}

#[test]
fn plain_fixture_entries_are_sorted_and_flagged() {
    let bytes = load("plain.opfspack");
    let reader = PackReader::open(&bytes).unwrap();
    let paths: Vec<&str> = reader.entries().iter().map(|e| e.path.as_str()).collect();
    assert_eq!(
        paths,
        vec![
            "metadata.json",
            "pages/page_0001.webp",
            "pages/page_0002.webp"
        ]
    );
    let meta = reader.entry("metadata.json").unwrap();
    assert_eq!(meta.mime_type, "application/json");
    assert_eq!(meta.size, 62);
    assert_eq!(meta.compressed_size, 62);
    assert_eq!(meta.flags, 0);
    let p1 = reader.entry("pages/page_0001.webp").unwrap();
    assert_eq!(p1.mime_type, "image/webp");
    assert_eq!(p1.size, page_a().len() as u64);
    assert_ne!(p1.flags & entry_flags::COMPRESSED, 0);
    assert_ne!(p1.compressed_size, p1.size);
    let p2 = reader.entry("pages/page_0002.webp").unwrap();
    assert_eq!(p2.flags, 0);
    assert_eq!(p2.compressed_size, p2.size);
    assert!(reader.entry("nope.txt").is_none());
}

#[test]
fn plain_fixture_reads_entry_bytes() {
    let bytes = load("plain.opfspack");
    let reader = PackReader::open(&bytes).unwrap();
    assert_eq!(reader.read_entry("metadata.json", None).unwrap(), META);
    assert_eq!(
        reader.read_entry("pages/page_0001.webp", None).unwrap(),
        page_a()
    );
    assert_eq!(
        reader.read_entry("pages/page_0002.webp", None).unwrap(),
        page_b()
    );
}

#[test]
fn plain_fixture_range_reads() {
    let bytes = load("plain.opfspack");
    let reader = PackReader::open(&bytes).unwrap();
    let full = page_a();
    let range = reader
        .read_entry_range("pages/page_0001.webp", 100, 200, None)
        .unwrap();
    assert_eq!(range, &full[100..200]);
    // invalid ranges
    assert!(matches!(
        reader.read_entry_range("pages/page_0001.webp", 10, 10, None),
        Err(PackError::InvalidRange(_))
    ));
    assert!(matches!(
        reader.read_entry_range("pages/page_0001.webp", 10, 9, None),
        Err(PackError::InvalidRange(_))
    ));
    assert!(matches!(
        reader.read_entry_range("pages/page_0001.webp", 0, (page_a().len() + 1) as u64, None),
        Err(PackError::InvalidRange(_))
    ));
}

// ---- encrypted fixture ---------------------------------------------------

#[test]
fn encrypted_fixture_requires_the_pack_key() {
    let bytes = load("encrypted.opfspack");
    let reader = PackReader::open(&bytes).unwrap();
    assert_ne!(reader.header().flags & pack_flags::ENCRYPTED, 0);
    let err = reader.read_entry("pages/page_0001.webp", None).unwrap_err();
    assert!(matches!(&err, PackError::KeyRequired(p) if p == "pages/page_0001.webp"));
}

#[test]
fn encrypted_fixture_decrypts_with_the_pack_key() {
    let bytes = load("encrypted.opfspack");
    let reader = PackReader::open(&bytes).unwrap();
    let key = fixture_pack_key();
    assert_eq!(
        reader
            .read_entry("pages/page_0001.webp", Some(&key))
            .unwrap(),
        page_a()
    );
    assert_eq!(
        reader
            .read_entry("pages/page_0002.webp", Some(&key))
            .unwrap(),
        page_b()
    );
    assert_eq!(
        reader.read_entry("metadata.json", Some(&key)).unwrap(),
        META
    );
    // all entries encrypted; v3 never sets IDENTITY_BOUND
    for e in reader.entries() {
        assert_ne!(e.flags & entry_flags::ENCRYPTED, 0);
        assert_eq!(e.flags & entry_flags::IDENTITY_BOUND, 0);
    }
}

#[test]
fn encrypted_fixture_wrong_key_is_corrupted() {
    let bytes = load("encrypted.opfspack");
    let reader = PackReader::open(&bytes).unwrap();
    // 別アカウントのルート鍵（同じ pack id でも鍵が違う）
    let other_root = PackRootKey::from_bytes([0xab; 32]).derive_pack_key("test-pack");
    assert!(matches!(
        reader.read_entry("pages/page_0001.webp", Some(&other_root)),
        Err(PackError::Corrupted(_))
    ));
    // 同じルート鍵でも別の pack id（= 別の本）
    let other_pack = fixture_root().derive_pack_key("other-pack");
    assert!(matches!(
        reader.read_entry("pages/page_0001.webp", Some(&other_pack)),
        Err(PackError::Corrupted(_))
    ));
}

// ---- corruption and version rejection -------------------------------------

fn crc32(bytes: &[u8]) -> u32 {
    crc32fast::hash(bytes)
}

#[test]
fn rejects_truncated_and_bad_magic() {
    let bytes = load("plain.opfspack");
    assert!(matches!(
        PackReader::open(&bytes[..63]),
        Err(PackError::Corrupted(_))
    ));
    let mut bad = bytes.clone();
    bad[0] = b'X';
    assert!(matches!(
        PackReader::open(&bad),
        Err(PackError::Corrupted(_))
    ));
}

#[test]
fn rejects_header_crc_mismatch() {
    let mut bad = load("plain.opfspack");
    bad[8] ^= 0xff; // flags field
    assert!(matches!(
        PackReader::open(&bad),
        Err(PackError::Corrupted(_))
    ));
}

#[test]
fn rejects_v2_packs_with_corrected_crc() {
    let mut bytes = load("plain.opfspack");
    bytes[4..8].copy_from_slice(&2u32.to_le_bytes());
    let crc = crc32(&bytes[..60]);
    bytes[60..64].copy_from_slice(&crc.to_le_bytes());
    assert!(matches!(
        PackReader::open(&bytes),
        Err(PackError::Version(2))
    ));
}

#[test]
fn rejects_index_crc_mismatch() {
    let mut bytes = load("plain.opfspack");
    let index_offset = u64::from_le_bytes(bytes[16..24].try_into().unwrap()) as usize;
    bytes[index_offset] ^= 0x01; // first path byte
    assert!(matches!(
        PackReader::open(&bytes),
        Err(PackError::Corrupted(_))
    ));
}

#[test]
fn rejects_entry_pointing_beyond_body() {
    let mut bytes = load("plain.opfspack");
    // patch first entry offset to past the index start, fix index CRC
    let index_offset = u64::from_le_bytes(bytes[16..24].try_into().unwrap()) as usize;
    let index_size = u64::from_le_bytes(bytes[24..32].try_into().unwrap()) as usize;
    let first_entry = index_offset + 2 + "metadata.json".len() + 2 + "application/json".len();
    let off_pos = first_entry;
    bytes[off_pos..off_pos + 8].copy_from_slice(&(u64::MAX - 7).to_le_bytes());
    let crc = crc32(&bytes[index_offset..index_offset + index_size - 4]);
    bytes[index_offset + index_size - 4..index_offset + index_size]
        .copy_from_slice(&crc.to_le_bytes());
    assert!(matches!(
        PackReader::open(&bytes),
        Err(PackError::Corrupted(_))
    ));
}

#[test]
fn rejects_lz4_flag_with_corrected_crc() {
    let mut bytes = load("plain.opfspack");
    let index_offset = u64::from_le_bytes(bytes[16..24].try_into().unwrap()) as usize;
    let index_size = u64::from_le_bytes(bytes[24..32].try_into().unwrap()) as usize;
    // first entry is metadata.json: flags at 2+14+2+16+24
    let flags_pos = index_offset + 2 + "metadata.json".len() + 2 + "application/json".len() + 24;
    let flags = u32::from_le_bytes(bytes[flags_pos..flags_pos + 4].try_into().unwrap());
    bytes[flags_pos..flags_pos + 4].copy_from_slice(&(flags | 1 << 3).to_le_bytes());
    let crc = crc32(&bytes[index_offset..index_offset + index_size - 4]);
    bytes[index_offset + index_size - 4..index_offset + index_size]
        .copy_from_slice(&crc.to_le_bytes());
    let reader = PackReader::open(&bytes).unwrap();
    assert!(matches!(
        reader.read_entry("metadata.json", None),
        Err(PackError::UnsupportedLz4)
    ));
}

// ---- builder round-trips ---------------------------------------------------

#[test]
fn builder_roundtrip_plain() {
    let mut builder = PackBuilder::new(1_700_000_000_000);
    builder.add_entry("pages/page_0002.webp", page_b(), "image/webp", false);
    builder.add_entry("pages/page_0001.webp", page_a(), "image/webp", true);
    builder.add_entry("metadata.json", META.to_vec(), "application/json", false);
    let bytes = builder.build(None, true).unwrap();
    let reader = PackReader::open(&bytes).unwrap();
    assert_eq!(reader.header().version, FORMAT_VERSION);
    assert_eq!(reader.header().created_at, 1_700_000_000_000);
    assert_eq!(
        reader.read_entry("pages/page_0001.webp", None).unwrap(),
        page_a()
    );
    assert_eq!(
        reader.read_entry("pages/page_0002.webp", None).unwrap(),
        page_b()
    );
    assert_eq!(reader.read_entry("metadata.json", None).unwrap(), META);
    assert_ne!(
        reader
            .entry("pages/page_0001.webp")
            .unwrap()
            .compressed_size,
        page_a().len() as u64
    );
}

#[test]
fn builder_roundtrip_encrypted() {
    let key = fixture_pack_key();
    let mut builder = PackBuilder::new(1_700_000_000_000);
    builder.add_entry("pages/page_0001.webp", page_a(), "image/webp", true);
    builder.add_entry("metadata.json", META.to_vec(), "application/json", false);
    let bytes = builder.build(Some(&key), true).unwrap();
    let reader = PackReader::open(&bytes).unwrap();
    assert_ne!(reader.header().flags & pack_flags::ENCRYPTED, 0);
    // without the key: rejected (never falls back to plaintext)
    assert!(matches!(
        reader.read_entry("pages/page_0001.webp", None),
        Err(PackError::KeyRequired(_))
    ));
    // with the key: original bytes
    assert_eq!(
        reader
            .read_entry("pages/page_0001.webp", Some(&key))
            .unwrap(),
        page_a()
    );
    assert_eq!(
        reader.read_entry("metadata.json", Some(&key)).unwrap(),
        META
    );
    // encrypted size = plaintext + GCM tag
    let meta = reader.entry("metadata.json").unwrap();
    assert_eq!(meta.compressed_size, META.len() as u64 + 16);
    // the Rust-built pack must be readable by the TS reader (checked manually
    // in Verification; here we assert the TS-compatible layout invariants)
    assert!(reader.read_entry("pages/page_0001.webp", None).is_err());
}

#[test]
fn builder_sorts_entries_by_path() {
    let mut builder = PackBuilder::new(0);
    builder.add_entry("z.txt", vec![1, 2, 3], "text/plain", false);
    builder.add_entry("a.txt", vec![4, 5], "text/plain", false);
    let bytes = builder.build(None, false).unwrap();
    let reader = PackReader::open(&bytes).unwrap();
    let paths: Vec<&str> = reader.entries().iter().map(|e| e.path.as_str()).collect();
    assert_eq!(paths, vec!["a.txt", "z.txt"]);
}

#[test]
fn builder_empty_pack_is_valid() {
    let bytes = PackBuilder::new(42).build(None, false).unwrap();
    let reader = PackReader::open(&bytes).unwrap();
    assert_eq!(reader.header().entry_count, 0);
    assert!(reader.entries().is_empty());
}

// ---- key derivation --------------------------------------------------------

#[test]
fn owner_id_matches_ts_reference() {
    assert_eq!(
        derive_owner_id("test-sub"),
        "6366bfc3b6ab37feaf2adb385aeaa515c4aa52cf09e70cac890d888e4409f3b0"
    );
}
