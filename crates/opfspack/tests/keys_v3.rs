//! v3（ルート鍵 + ラップ）の仕様適合テスト。
//!
//! 期待値は `docs/spec/10-pack-keys.md` §8 のテストベクタ（独立実装で検算済み）。
//! **ここが Rust / TS 間のバイト一致の正**なので、値を書き換えるときは仕様書も直すこと。

use opfspack::{
    FORMAT_VERSION, PackBuilder, PackError, PackKeyBundle, PackReader, PackRootKey, WrapKind,
};

/// §8 のベクタで使う PRK（`00 01 … 1f`）。
fn vector_root() -> PackRootKey {
    let mut bytes = [0u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = index as u8;
    }
    PackRootKey::from_bytes(bytes)
}

const VECTOR_OWNER_ID: &str = "6366bfc3b6ab37feaf2adb385aeaa515c4aa52cf09e70cac890d888e4409f3b0";

#[test]
fn pack_key_matches_the_spec_vector() {
    let key = vector_root().derive_pack_key("test-pack");
    assert_eq!(
        hex::encode(key.as_bytes()),
        "9c65c8705e14eacc536cf438b3ff2fa58d399ffa50b10c451d543b2c058f7cd6",
        "v3 の pack 鍵が仕様書のベクタと一致しない"
    );
}

#[test]
fn sub_wrap_matches_the_spec_vector() {
    let root = vector_root();
    let nonce = [0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
    let wrap = opfspack::PackRootKeyWrap::wrap_with_kek(
        &root,
        &opfspack::sub_wrap_kek("test-sub"),
        WrapKind::Sub,
        opfspack::APP_SALT.to_vec(),
        opfspack::SUB_WRAP_ITERATIONS,
        nonce,
        VECTOR_OWNER_ID,
    );
    assert_eq!(
        hex::encode(wrap.nonce()),
        "000102030405060708090a0b",
        "nonce が仕様書のベクタと一致しない"
    );
    assert_eq!(
        hex::encode(wrap.ciphertext()),
        "9c7b7700f1ffdfd814eec081c21a0187bc4382a816f5a48918a789a5f485bd1b428c7b8cffa54ccdacf74a0ae0e094fa",
        "ラップの暗号文が仕様書のベクタと一致しない"
    );
    // 逆向きも一致する
    assert_eq!(
        wrap.unwrap(&opfspack::sub_wrap_kek("test-sub"), VECTOR_OWNER_ID),
        Some(vector_root())
    );
}

#[test]
fn sub_wrap_kek_matches_the_v2_master_key() {
    // v2 の master key と同式（Web 版の既存 deriveMasterKey を使い回すための要件）
    assert_eq!(
        hex::encode(opfspack::sub_wrap_kek("test-sub")),
        "e0aaeaedbc447f52d089dc7f7422eb4575edb21c67fe954c006e9a1c9131bd73"
    );
}

#[test]
fn bundle_roundtrips_through_json() {
    let root = vector_root();
    let mut bundle = PackKeyBundle::new(VECTOR_OWNER_ID.to_string(), 1_790_000_000_000);
    bundle.upsert_wrap(opfspack::PackRootKeyWrap::wrap_with_sub(
        &root,
        "test-sub",
        VECTOR_OWNER_ID,
        1_790_000_000_000,
    ));
    let json = bundle.to_json().expect("JSON にできるはず");
    let parsed = PackKeyBundle::from_json(&json).expect("読み戻せるはず");
    assert_eq!(parsed.owner_id(), VECTOR_OWNER_ID);
    assert!(parsed.has_wrap(WrapKind::Sub));
    assert_eq!(
        parsed.unwrap_with_sub("test-sub"),
        Some(vector_root()),
        "JSON を経由すると解けなくなっている"
    );
    assert_eq!(parsed.unwrap_with_sub("other-sub"), None);
}

#[test]
fn bundle_rejects_unknown_format_version() {
    let json = br#"{"format_version":99,"owner_id":"x","wraps":[]}"#;
    assert!(matches!(
        PackKeyBundle::from_json(json),
        Err(PackError::UnsupportedKeyBundle(99))
    ));
}

#[test]
fn passphrase_wrap_roundtrips_and_is_normalized() {
    let root = vector_root();
    let wrap = opfspack::PackRootKeyWrap::wrap_with_passphrase(
        &root,
        "ぱすわーど",
        VECTOR_OWNER_ID,
        opfspack::PASSPHRASE_WRAP_ITERATIONS,
        1_790_000_000_000,
    );
    assert_eq!(wrap.unwrap_with_passphrase("ぱすわーど", VECTOR_OWNER_ID), Some(vector_root()));
    assert_eq!(wrap.unwrap_with_passphrase("ちがう", VECTOR_OWNER_ID), None);
    // NFKC 正規化（合成済み ↔ 分解）で同じ鍵になること
    let decomposed = "は\u{3099}すわーど"; // "ば" を分解した形
    let composed = "ばすわーど";
    let wrap2 = opfspack::PackRootKeyWrap::wrap_with_passphrase(
        &root,
        composed,
        VECTOR_OWNER_ID,
        1000,
        1,
    );
    assert_eq!(
        wrap2.unwrap_with_passphrase(decomposed, VECTOR_OWNER_ID),
        Some(vector_root()),
        "NFKC 正規化していない（合成文字で解けなくなる）"
    );
}

#[test]
fn wrapped_root_key_is_not_unwrapped_with_the_wrong_owner_id() {
    // AAD に owner_id が入っているので、別アカウントの値では解けない（差し替え検知）
    let root = vector_root();
    let wrap = opfspack::PackRootKeyWrap::wrap_with_sub(
        &root,
        "test-sub",
        VECTOR_OWNER_ID,
        1_790_000_000_000,
    );
    assert_eq!(wrap.unwrap(&opfspack::sub_wrap_kek("test-sub"), "other-owner"), None);
}

#[test]
fn v3_packs_are_written_and_read_with_the_root_key() {
    let root = vector_root();
    let key = root.derive_pack_key("test-pack");
    let mut builder = PackBuilder::new(1_790_000_000_000);
    builder.add_entry("pages/page_0001.txt", b"hello v3".to_vec(), "text/plain", true);
    let bytes = builder.build(Some(&key), true).expect("書けるはず");
    assert_eq!(
        u32::from_le_bytes(bytes[4..8].try_into().unwrap()),
        FORMAT_VERSION,
        "header の version が 3 でない"
    );

    let reader = PackReader::open(&bytes).expect("開けるはず");
    let data = reader
        .read_entry_with_key("pages/page_0001.txt", Some(&key))
        .expect("読めるはず");
    assert_eq!(data, b"hello v3");
}

#[test]
fn reading_with_the_wrong_or_missing_key_fails() {
    let root = vector_root();
    let key = root.derive_pack_key("test-pack");
    let mut builder = PackBuilder::new(1_790_000_000_000);
    builder.add_entry("pages/page_0001.txt", b"secret".to_vec(), "text/plain", true);
    let bytes = builder.build(Some(&key), true).expect("書けるはず");
    let reader = PackReader::open(&bytes).expect("開けるはず");

    // 鍵なし
    assert!(matches!(
        reader.read_entry_with_key("pages/page_0001.txt", None),
        Err(PackError::KeyRequired(_))
    ));
    // 別のルート鍵（別アカウント相当）
    let other = vector_root().derive_pack_key("other-pack");
    assert!(matches!(
        reader.read_entry_with_key("pages/page_0001.txt", Some(&other)),
        Err(PackError::Corrupted(_))
    ));
}

#[test]
fn v2_packs_are_rejected() {
    let root = vector_root();
    let key = root.derive_pack_key("test-pack");
    let mut builder = PackBuilder::new(1_790_000_000_000);
    builder.add_entry("a.txt", b"x".to_vec(), "text/plain", false);
    let mut bytes = builder.build(Some(&key), false).expect("書けるはず");
    // header の version は bytes[4..8]、ヘッダ CRC は bytes[60..64]（= CRC32(bytes[..60])）。
    // どちらも format どおり **リトルエンディアン**で書く。
    bytes[4..8].copy_from_slice(&2u32.to_le_bytes());
    let crc = crc32fast::hash(&bytes[..60]);
    bytes[60..64].copy_from_slice(&crc.to_le_bytes());
    assert!(
        matches!(PackReader::open(&bytes), Err(PackError::Version(2))),
        "v2 の pack を読もうとしている"
    );
}

#[test]
fn plaintext_packs_still_read_without_a_key() {
    let mut builder = PackBuilder::new(1_790_000_000_000);
    builder.add_entry("a.txt", b"plain".to_vec(), "text/plain", true);
    let bytes = builder.build(None, true).expect("書けるはず");
    let reader = PackReader::open(&bytes).expect("開けるはず");
    assert_eq!(reader.read_entry_with_key("a.txt", None).unwrap(), b"plain");
}
