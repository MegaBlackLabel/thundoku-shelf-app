//! `thundoku-keys.json`（[`PackKeyBundle`]）と keyring 形式の契約テスト。
//!
//! JSON のフィールド名・並び・値の形（base64 の長さなど）は**TS 版との一致点**
//! （仕様 §3.2）なのでここで固定する。KDF とラップの期待値そのものは
//! `tests/keys_v3.rs`（仕様 §8 のベクタ）が見る。

use opfspack::{APP_SALT, KEY_BUNDLE_FORMAT_VERSION, MAX_PASSPHRASE_ITERATIONS, PackError, PackKeyBundle, PackRootKey, PackRootKeyWrap, WrapKind, derive_owner_id};

/// 仕様 §8 のベクタ PRK（`00 01 … 1f`）。
fn vector_root() -> PackRootKey {
    PackRootKey::from_bytes(std::array::from_fn(|index| index as u8))
}

fn owner_id() -> String {
    derive_owner_id("test-sub")
}

/// JSON 文字列から `"name":"value"` の値を取り出す（形の検査用）。
fn string_field<'a>(json: &'a str, name: &str) -> &'a str {
    let key = format!("\"{name}\":\"");
    let start = json.find(&key).expect("field missing") + key.len();
    let rest = &json[start..];
    &rest[..rest.find('"').expect("unterminated string")]
}

/// `format_version` / `owner_id` / 時刻 / `wraps[]` の形が仕様 §3.2 のとおり。
/// sub ラップの salt は `APP_SALT` の base64 固定。
#[test]
fn bundle_json_has_the_spec_shape() {
    let owner = owner_id();
    let mut bundle = PackKeyBundle::new(owner.clone(), 1_790_000_000_000);
    bundle.upsert_wrap(PackRootKeyWrap::wrap_with_sub(
        &vector_root(),
        "test-sub",
        &owner,
        1_790_000_000_000,
    ));
    let json = String::from_utf8(bundle.to_json().expect("JSON にできる")).expect("UTF-8");

    // フィールド名と並び（serde は構造体の順に書く = 仕様 §3.2 の例と同じ順）
    assert!(
        json.starts_with(&format!(
            "{{\"format_version\":{KEY_BUNDLE_FORMAT_VERSION},\"owner_id\":\"{owner}\",\
             \"created_at\":1790000000000,\"updated_at\":1790000000000,\"wraps\":[{{"
        )),
        "bundle の JSON 形が仕様と違う: {json}"
    );
    assert_eq!(KEY_BUNDLE_FORMAT_VERSION, 1);
    assert!(json.contains("\"kind\":\"sub\""));
    assert!(json.contains("\"kdf\":\"pbkdf2-sha256\""));
    assert!(json.contains("\"iterations\":100000"));
    // salt = APP_SALT の base64（仕様 §3.2 の例と同じ文字列）
    assert_eq!(
        string_field(&json, "salt"),
        "b3Bmc3BhY2stdjEtaWRlbnRpdHktc2FsdC0yMDI0"
    );
    assert_eq!(APP_SALT, b"opfspack-v1-identity-salt-2024");
    // nonce は base64 12B（16 文字）、ciphertext は 48B（64 文字）
    assert_eq!(string_field(&json, "nonce").len(), 16);
    assert_eq!(string_field(&json, "ciphertext").len(), 64);
    // 読み戻して同じ鍵が得られる（片道でない）
    assert_eq!(
        PackKeyBundle::from_json(json.as_bytes())
            .expect("読み戻せる")
            .unwrap_with_sub("test-sub"),
        Some(vector_root())
    );
}

/// 同じ `kind` は置換され（重複しない）、`created_at` は保持される。
#[test]
fn upsert_replaces_the_same_kind() {
    let owner = owner_id();
    let first = vector_root();
    let second = PackRootKey::from_bytes([0x5a; 32]);
    let mut bundle = PackKeyBundle::new(owner.clone(), 1_790_000_000_000);
    bundle.upsert_wrap(PackRootKeyWrap::wrap_with_passphrase(
        &first,
        "first",
        &owner,
        1_000,
        1_790_000_000_000,
    ));
    bundle.upsert_wrap(PackRootKeyWrap::wrap_with_passphrase(
        &second,
        "second",
        &owner,
        1_000,
        1_790_000_001_000,
    ));

    assert_eq!(bundle.wraps().len(), 1, "同じ kind が重複している");
    assert_eq!(bundle.created_at(), 1_790_000_000_000);
    assert_eq!(bundle.unwrap_with_passphrase("first"), None);
    assert_eq!(bundle.unwrap_with_passphrase("second"), Some(second));
    let json = String::from_utf8(bundle.to_json().expect("JSON にできる")).expect("UTF-8");
    assert_eq!(json.matches("\"kind\":\"passphrase\"").count(), 1);
}

/// `remove_wrap` は削除の有無を返し、削除後はもう解けない（`sub` 依存をやめる操作）。
#[test]
fn remove_wrap_drops_only_the_requested_kind() {
    let owner = owner_id();
    let root = vector_root();
    let mut bundle = PackKeyBundle::new(owner.clone(), 1_790_000_000_000);
    bundle.upsert_wrap(PackRootKeyWrap::wrap_with_sub(
        &root,
        "test-sub",
        &owner,
        1_790_000_000_000,
    ));
    bundle.upsert_wrap(PackRootKeyWrap::wrap_with_passphrase(
        &root,
        "ぱすわーど",
        &owner,
        1_000,
        1_790_000_000_000,
    ));

    assert!(bundle.remove_wrap(WrapKind::Sub));
    assert!(!bundle.remove_wrap(WrapKind::Sub), "2 回目は削除できない");
    assert!(!bundle.has_wrap(WrapKind::Sub));
    assert_eq!(bundle.unwrap_with_sub("test-sub"), None);
    assert!(bundle.has_wrap(WrapKind::Passphrase));
    assert_eq!(bundle.unwrap_with_passphrase("ぱすわーど"), Some(root));
}

/// keyring に置く PRK の base64（標準アルファベット + padding）の往復。
#[test]
fn root_key_base64_roundtrips() {
    let root = vector_root();
    assert_eq!(
        root.to_base64(),
        "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8="
    );
    assert_eq!(PackRootKey::from_base64(&root.to_base64()), Some(root));
    // 長さが違う / base64 でない / 空 は鍵として受け付けない
    assert_eq!(PackRootKey::from_base64("AAAA"), None);
    assert_eq!(PackRootKey::from_base64(&"A".repeat(44)), None);
    assert_eq!(PackRootKey::from_base64("!!!not base64!!!"), None);
    assert_eq!(PackRootKey::from_base64(""), None);
}

/// 壊れた bundle は黙って受け付けない（未知 kind / base64 でない / 長さ不足）。
#[test]
fn bundle_json_rejects_malformed_wraps() {
    let wrap = |kind: &str, iterations: u32, salt: &str, ciphertext: &str| {
        format!(
            "{{\"format_version\":1,\"owner_id\":\"{owner}\",\"created_at\":1,\"updated_at\":1,\
             \"wraps\":[{{\"kind\":\"{kind}\",\"kdf\":\"pbkdf2-sha256\",\
             \"iterations\":{iterations},\"salt\":\"{salt}\",\
             \"nonce\":\"AAECAwQFBgcICQoL\",\"ciphertext\":\"{ciphertext}\"}}]}}",
            owner = owner_id(),
        )
    };
    let valid_ciphertext = "A".repeat(64);
    let cases = [
        (
            "未知 kind",
            wrap("webauthn", 100_000, "AA==", &valid_ciphertext),
        ),
        (
            "base64 でない salt",
            wrap("sub", 100_000, "!!!", &valid_ciphertext),
        ),
        (
            "48B でない ciphertext",
            wrap("sub", 100_000, "AA==", "AAAA"),
        ),
        ("iterations 0", wrap("sub", 0, "AA==", &valid_ciphertext)),
    ];
    for (label, json) in cases {
        assert!(
            matches!(
                PackKeyBundle::from_json(json.as_bytes()),
                Err(PackError::Corrupted(_))
            ),
            "{label} が受理された"
        );
    }
}

/// 指定の `iterations` を持つ最小の bundle JSON（wrap は 1 つ、形式としては妥当）。
fn bundle_with_iterations(iterations: u32) -> String {
    format!(
        "{{\"format_version\":1,\"owner_id\":\"{owner}\",\"created_at\":1,\"updated_at\":1,\
         \"wraps\":[{{\"kind\":\"sub\",\"kdf\":\"pbkdf2-sha256\",\
         \"iterations\":{iterations},\"salt\":\"AA==\",\
         \"nonce\":\"AAECAwQFBgcICQoL\",\"ciphertext\":\"{ciphertext}\"}}]}}",
        owner = owner_id(),
        ciphertext = "A".repeat(64),
    )
}

/// 過大な `iterations` は**復号（PBKDF2）の前**に弾く。
///
/// 鍵ファイルは同期先からも来る（相手に書き換えられ得る）ので、`u32::MAX` のような値を
/// そのまま PBKDF2 へ渡すと、復元しようとした利用者の CPU を何時間も焼く。
#[test]
fn bundle_json_rejects_absurd_iterations_before_running_the_kdf() {
    assert!(matches!(
        PackKeyBundle::from_json(bundle_with_iterations(0).as_bytes()),
        Err(PackError::Corrupted(_))
    ));
    // 上限ちょうどは受け付け、上限 + 1 は弾く。
    assert!(
        PackKeyBundle::from_json(bundle_with_iterations(MAX_PASSPHRASE_ITERATIONS).as_bytes())
            .is_ok(),
        "上限ちょうどは受理する"
    );
    assert!(
        matches!(
            PackKeyBundle::from_json(bundle_with_iterations(MAX_PASSPHRASE_ITERATIONS + 1).as_bytes()),
            Err(PackError::Corrupted(_))
        ),
        "上限 + 1 は弾く"
    );

    // 桁違いの値でも**待たされない**（PBKDF2 を走らせていない）。
    let started = std::time::Instant::now();
    let result = PackKeyBundle::from_json(bundle_with_iterations(u32::MAX).as_bytes());
    assert!(matches!(result, Err(PackError::Corrupted(_))));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "復号を始めていない（{:?}）",
        started.elapsed()
    );
}
