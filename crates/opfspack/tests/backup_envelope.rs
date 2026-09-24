//! §11 バックアップの暗号化（`thundoku-backup.json` v3 の封筒）の仕様適合テスト。
//!
//! 期待値は `docs/spec/10-pack-keys.md` §11 のテストベクタ（独立実装で検算済み）。
//! **ここが Rust / TS 間のバイト一致の正**なので、値を書き換えるときは仕様書も直すこと。

use opfspack::{BackupEnvelope, PackRootKey};

/// §11 のベクタで使う PRK（`00 01 … 1f`）。
fn vector_root() -> PackRootKey {
    let mut bytes = [0u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = index as u8;
    }
    PackRootKey::from_bytes(bytes)
}

const VECTOR_OWNER_ID: &str = "6366bfc3b6ab37feaf2adb385aeaa515c4aa52cf09e70cac890d888e4409f3b0";

/// 封筒ベクタの nonce（`0b 0a … 00`）。
const VECTOR_NONCE: [u8; 12] = [
    0x0b, 0x0a, 0x09, 0x08, 0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x00,
];

/// 封筒ベクタの平文（`thundoku-backup.json` の中身そのもの）。
const VECTOR_PLAINTEXT: &[u8] = br#"{"format_version":3,"tables":{}}"#;

#[test]
fn backup_keys_match_the_spec_vectors() {
    let root = vector_root();
    assert_eq!(
        hex::encode(root.derive_backup_cipher_key()),
        "514e03cce7c0aa82128b07af6ed7cbd1b2e7a30262f811d39201f21fd4faba8e",
        "バックアップの暗号鍵が仕様書のベクタと一致しない"
    );
    assert_eq!(
        hex::encode(root.derive_backup_hash_key()),
        "840d69fba8e9af9525b86e4c0dfa74403ecfcaa8743a6ed4f7576322c81df043",
        "バックアップの HMAC 鍵が仕様書のベクタと一致しない"
    );
    // 用途分離: pack 鍵とも、互いにも一致しない
    assert_ne!(
        root.derive_backup_cipher_key(),
        root.derive_backup_hash_key(),
        "暗号鍵と HMAC 鍵は別の値でなければならない"
    );
    assert_ne!(
        root.derive_backup_cipher_key(),
        *root.derive_pack_key("test-pack").as_bytes(),
        "pack 鍵とバックアップ鍵を流用してはいけない"
    );
}

#[test]
fn envelope_matches_the_spec_vectors() {
    let root = vector_root();
    assert_eq!(
        opfspack::derive_owner_id("test-sub"),
        VECTOR_OWNER_ID,
        "AAD に埋める owner_id（§2 の式）"
    );
    let envelope =
        BackupEnvelope::seal_with_nonce(VECTOR_PLAINTEXT, &root, VECTOR_OWNER_ID, VECTOR_NONCE);

    assert_eq!(envelope.format_version(), 3);
    assert_eq!(envelope.owner_id(), VECTOR_OWNER_ID);
    assert_eq!(hex::encode(envelope.nonce()), "0b0a09080706050403020100");
    assert_eq!(
        hex::encode(envelope.ciphertext()),
        "a89bac086476371010c84a0f976bee1d8e83650f48c9c2c411b5bbcf54df7cdea7185fdbd89c979adeb6404b869a38b3",
        "封筒の暗号文が仕様書のベクタと一致しない"
    );
    assert_eq!(
        envelope.content_hmac_hex(),
        "1d43363600f7a97ed4357e385917178206398a86b2bb0b0512c67d412007488b",
        "平文の HMAC が仕様書のベクタと一致しない"
    );
    // 逆向きも一致する
    assert_eq!(
        envelope.open(&root, VECTOR_OWNER_ID).expect("開けるはず"),
        VECTOR_PLAINTEXT
    );
}

#[test]
fn envelope_roundtrips_through_json() {
    let root = vector_root();
    let envelope = BackupEnvelope::seal(VECTOR_PLAINTEXT, &root, VECTOR_OWNER_ID);
    let json = envelope.to_json().expect("JSON にできるはず");

    let value: serde_json::Value = serde_json::from_slice(&json).unwrap();
    assert_eq!(value["format_version"], 3);
    assert_eq!(value["owner_id"], VECTOR_OWNER_ID);
    assert_eq!(value["encryption"]["alg"], "aes-256-gcm");
    assert_eq!(value["encryption"]["kdf"], "hkdf-sha256");
    // 平文は JSON のまま暗号化されているので、ファイルから内容は読めない
    assert!(
        !String::from_utf8_lossy(&json).contains("tables"),
        "平文が封筒に残っている"
    );

    let parsed = BackupEnvelope::from_json(&json).expect("読み戻せるはず");
    assert_eq!(parsed, envelope);
    assert_eq!(parsed.open(&root, VECTOR_OWNER_ID).unwrap(), VECTOR_PLAINTEXT);
}

#[test]
fn sealing_the_same_content_keeps_the_hmac_but_changes_the_ciphertext() {
    let root = vector_root();
    let first = BackupEnvelope::seal(VECTOR_PLAINTEXT, &root, VECTOR_OWNER_ID);
    let second = BackupEnvelope::seal(VECTOR_PLAINTEXT, &root, VECTOR_OWNER_ID);

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
    assert_ne!(
        first.to_json().unwrap(),
        second.to_json().unwrap(),
        "ファイルの md5 は毎回変わる（暗号文 md5 を変更検知に使ってはいけない理由）"
    );
}

#[test]
fn opening_with_the_wrong_key_or_owner_fails() {
    let root = vector_root();
    let envelope = BackupEnvelope::seal(VECTOR_PLAINTEXT, &root, VECTOR_OWNER_ID);

    // 別の PRK（別アカウント相当）
    let other = PackRootKey::from_bytes([7u8; 32]);
    assert!(
        envelope.open(&other, VECTOR_OWNER_ID).is_err(),
        "別の鍵で開いてはいけない"
    );
    // AAD の owner_id が違う（封筒の差し替え検知）
    assert!(
        envelope.open(&root, "0000000000000000000000000000000000000000000000000000000000000000")
            .is_err(),
        "owner_id が違えば AAD が合わず開けない"
    );
}

#[test]
fn tampered_ciphertext_or_hmac_is_rejected_without_plaintext() {
    let root = vector_root();
    let envelope = BackupEnvelope::seal(VECTOR_PLAINTEXT, &root, VECTOR_OWNER_ID);

    // 暗号文の 1 バイトを反転（AES-GCM のタグ検証で落ちる）
    let mut ciphertext = envelope.ciphertext().to_vec();
    ciphertext[0] ^= 1;
    let broken = BackupEnvelope::from_json(
        &tamper_json(&envelope.to_json().unwrap(), "ciphertext", |_| {
            base64_encode(&ciphertext)
        })
        .unwrap(),
    )
    .expect("構造としては読める");
    assert!(
        broken.open(&root, VECTOR_OWNER_ID).is_err(),
        "改変された暗号文から平文を返してはいけない"
    );

    // content_hmac だけを差し替えた封筒（平文は同じ）
    let tampered = BackupEnvelope::from_json(
        &tamper_json(&envelope.to_json().unwrap(), "content_hmac", |_| {
            "00".repeat(32)
        })
        .unwrap(),
    )
    .expect("構造としては読める");
    assert!(
        tampered.open(&root, VECTOR_OWNER_ID).is_err(),
        "content_hmac が合わない封筒から平文を返してはいけない"
    );
}

#[test]
fn envelopes_of_other_versions_are_rejected() {
    let root = vector_root();
    let envelope = BackupEnvelope::seal(VECTOR_PLAINTEXT, &root, VECTOR_OWNER_ID);
    let json = String::from_utf8(envelope.to_json().unwrap()).unwrap();
    let bumped = json.replace("\"format_version\":3", "\"format_version\":4");
    assert!(
        BackupEnvelope::from_json(bumped.as_bytes()).is_err(),
        "未知の版の封筒は読まない（平文として扱わない）"
    );
    // 壊れた封筒（base64 でない nonce）
    let broken = json.replace(&base64_encode(envelope.nonce()), "not-base64-but-long");
    assert_ne!(broken, json, "前提: nonce を差し替えられている");
    assert!(BackupEnvelope::from_json(broken.as_bytes()).is_err());
    // 壊れた封筒（content_hmac が 64 hex でない）
    let broken = json.replace(&envelope.content_hmac_hex(), "abcd");
    assert!(BackupEnvelope::from_json(broken.as_bytes()).is_err());
}

// ---- テスト用の小さなヘルパ ------------------------------------------------

fn base64_encode(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

/// 封筒 JSON の一部（`encryption.ciphertext` / `content_hmac`）を差し替える。
fn tamper_json(
    json: &[u8],
    field: &str,
    replace: impl Fn(&str) -> String,
) -> Result<Vec<u8>, serde_json::Error> {
    let mut value: serde_json::Value = serde_json::from_slice(json)?;
    let current = match field {
        "content_hmac" => value[field].take(),
        other => value["encryption"][other].take(),
    };
    let current = current.as_str().unwrap_or_default().to_string();
    let replacement = serde_json::Value::String(replace(&current));
    match field {
        "content_hmac" => value[field] = replacement,
        other => value["encryption"][other] = replacement,
    }
    serde_json::to_vec(&value)
}
