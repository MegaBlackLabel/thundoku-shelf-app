//! 表紙バンドル（`thundoku-thumbs.json`）の封筒と鍵の仕様適合テスト。
//!
//! 期待値は `docs/spec/10-pack-keys.md` §11.8 のテストベクタ（Python の
//! hashlib / pycryptodome で独立に検算済み。§11.6 のバックアップベクタも
//! 同じ手順で再現できることを確認している）。**ここが Rust / TS 間の
//! バイト一致の正**なので、値を書き換えるときは仕様書も直すこと。

use opfspack::{BACKUP_LABEL, EnvelopeLabel, PackRootKey, SealedEnvelope, THUMBS_LABEL};

/// §11 のベクタで使う PRK（`00 01 … 1f`）。
fn vector_root() -> PackRootKey {
    let mut bytes = [0u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = index as u8;
    }
    PackRootKey::from_bytes(bytes)
}

const VECTOR_OWNER_ID: &str = "6366bfc3b6ab37feaf2adb385aeaa515c4aa52cf09e70cac890d888e4409f3b0";

/// 封筒ベクタの nonce（`0b 0a … 00`。§11.6 と同じ値を使う）。
const VECTOR_NONCE: [u8; 12] = [
    0x0b, 0x0a, 0x09, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x00,
];

/// 封筒ベクタの平文（表紙バンドルの最小形）。
const VECTOR_PLAINTEXT: &[u8] = br#"{"format_version":1,"entries":[]}"#;

#[test]
fn thumbs_keys_match_the_spec_vectors() {
    let root = vector_root();
    assert_eq!(
        hex::encode(root.derive_thumbs_cipher_key()),
        "c6bcc317a110145e9c2ab08f079195fd43790f37a54a5217a8ac58e050cea8e1",
        "表紙バンドルの暗号鍵が仕様書のベクタと一致しない"
    );
    assert_eq!(
        hex::encode(root.derive_thumbs_hash_key()),
        "43ea28b23b2dab950943cf6a97ee9266560b2ca1781eaee08fa28e9f8b168d25",
        "表紙バンドルの HMAC 鍵が仕様書のベクタと一致しない"
    );
    // 用途分離: バックアップの鍵とも pack 鍵とも一致しない
    assert_ne!(
        root.derive_thumbs_cipher_key(),
        root.derive_backup_cipher_key(),
        "表紙と DB バックアップで同じ暗号鍵を使ってはいけない"
    );
    assert_ne!(
        root.derive_thumbs_hash_key(),
        root.derive_backup_hash_key(),
        "表紙と DB バックアップで同じ HMAC 鍵を使ってはいけない"
    );
    assert_ne!(
        root.derive_thumbs_cipher_key(),
        *root.derive_pack_key("test-pack").as_bytes(),
        "pack 鍵と表紙バンドルの鍵を流用してはいけない"
    );
}

#[test]
fn thumbs_envelope_matches_the_spec_vectors() {
    let root = vector_root();
    let envelope = SealedEnvelope::seal_with_nonce(
        &THUMBS_LABEL,
        VECTOR_PLAINTEXT,
        &root,
        VECTOR_OWNER_ID,
        VECTOR_NONCE,
    );

    assert_eq!(
        envelope.format_version(),
        1,
        "表紙バンドルの封筒の版は 1（バックアップの 3 と別）"
    );
    assert_eq!(envelope.owner_id(), VECTOR_OWNER_ID);
    assert_eq!(hex::encode(envelope.nonce()), "0b0a09080706050403020100");
    assert_eq!(
        hex::encode(envelope.ciphertext()),
        "61697d8103eb4a2fc6440c6230179c9ca86081b45f26bdb39a34cfd01d7bef36609fc48cd14c8cf74c6c5e9b65b1c4c091",
        "表紙バンドルの暗号文が仕様書のベクタと一致しない"
    );
    assert_eq!(
        envelope.content_hmac_hex(),
        "46fba895ee29f2dcdddd05c9e7ee8e0f690d8722f6b337744ecc8d87f4e6e7e6",
        "表紙バンドルの平文 HMAC が仕様書のベクタと一致しない"
    );
    assert_eq!(
        envelope.open(&root, VECTOR_OWNER_ID).expect("開けるはず"),
        VECTOR_PLAINTEXT
    );
}

#[test]
fn thumbs_envelope_roundtrips_through_json() {
    let root = vector_root();
    let envelope = SealedEnvelope::seal(&THUMBS_LABEL, VECTOR_PLAINTEXT, &root, VECTOR_OWNER_ID);
    let json = envelope.to_json().expect("JSON にできるはず");

    let value: serde_json::Value = serde_json::from_slice(&json).unwrap();
    assert_eq!(value["format_version"], 1);
    assert_eq!(value["owner_id"], VECTOR_OWNER_ID);
    assert!(value["content_hmac"].is_string());
    // 中身（entries）はファイルから読めない
    assert!(
        !String::from_utf8_lossy(&json).contains("entries"),
        "平文が封筒に残っている"
    );

    let parsed = SealedEnvelope::from_json(&THUMBS_LABEL, &json).expect("読み戻せるはず");
    assert_eq!(parsed, envelope);
    assert_eq!(
        parsed.open(&root, VECTOR_OWNER_ID).unwrap(),
        VECTOR_PLAINTEXT
    );
}

#[test]
fn a_thumbs_envelope_is_not_accepted_as_a_backup_envelope() {
    // 別ファイルへの差し替え検知（バックアップとして表紙を読ませない）。
    let root = vector_root();
    let thumbs = SealedEnvelope::seal(&THUMBS_LABEL, VECTOR_PLAINTEXT, &root, VECTOR_OWNER_ID);
    let json = thumbs.to_json().unwrap();
    assert!(
        SealedEnvelope::from_json(&BACKUP_LABEL, &json).is_err(),
        "版が違う封筒を別のラベルで読んではいけない"
    );
}

#[test]
fn the_aad_is_bound_to_the_label() {
    // 同じ `format_version` でも AAD 接頭辞が違えば復号できない
    // （＝同じ内容のファイルを別ラベルとして開かせない）。
    static FAKE_LABEL: EnvelopeLabel = EnvelopeLabel {
        aad_prefix: "thundoku-thumbs-fake",
        format_version: 1,
        kdf_salt: b"thundoku-thumbs:v1",
        cipher_info: b"thundoku-thumbs-key",
        hash_info: b"thundoku-thumbs-hash",
    };
    let root = vector_root();
    let envelope = SealedEnvelope::seal(&THUMBS_LABEL, VECTOR_PLAINTEXT, &root, VECTOR_OWNER_ID);
    let json = envelope.to_json().unwrap();

    let relabeled = SealedEnvelope::from_json(&FAKE_LABEL, &json).expect("構造としては読める");
    assert!(
        relabeled.open(&root, VECTOR_OWNER_ID).is_err(),
        "AAD のラベルが違えば開けない"
    );
}

#[test]
fn the_same_plaintext_keeps_the_hmac_across_labels_and_nonces() {
    let root = vector_root();
    let first = SealedEnvelope::seal(&THUMBS_LABEL, VECTOR_PLAINTEXT, &root, VECTOR_OWNER_ID);
    let second = SealedEnvelope::seal(&THUMBS_LABEL, VECTOR_PLAINTEXT, &root, VECTOR_OWNER_ID);
    assert_eq!(
        first.content_hmac_hex(),
        second.content_hmac_hex(),
        "同じ平文なら content_hmac は同じ（変更検知に使える）"
    );
    assert_ne!(
        first.ciphertext(),
        second.ciphertext(),
        "nonce は乱数なので暗号文は毎回変わる"
    );
}
