//! One-off interop emitter: builds a v3 pack with the Rust implementation and
//! writes it to the path given as the first argument, so the TS
//! implementation can read it back (Verification step 2).
//!
//! The root key is the spec vector (`docs/spec/10-pack-keys.md` §8,
//! `PRK = 00 01 … 1f`) and the pack id is `test-pack`, so the pack key is
//! `HKDF(PRK, salt = UTF8("test-pack"))` — the TS side needs no bundle to
//! read this file.

use opfspack::{PackBuilder, PackRootKey};

fn main() {
    let out = std::env::args()
        .nth(1)
        .expect("usage: roundtrip_emit <out.opfspack>");
    let mut root_bytes = [0u8; 32];
    for (index, byte) in root_bytes.iter_mut().enumerate() {
        *byte = index as u8;
    }
    let key = PackRootKey::from_bytes(root_bytes).derive_pack_key("test-pack");
    let mut builder = PackBuilder::new(1_700_000_000_000);
    builder.add_entry(
        "pages/page_0001.webp",
        format!("THUNDOKU_PAGE_0001:{}", "abc".repeat(4000)).into_bytes(),
        "image/webp",
        true,
    );
    builder.add_entry(
        "pages/page_0002.webp",
        format!("THUNDOKU_PAGE_0002:{}", "xyz".repeat(2000)).into_bytes(),
        "image/webp",
        false,
    );
    builder.add_entry(
        "metadata.json",
        br#"{"schemaVersion":1,"title":"Test Book","author":"Test Author"}"#.to_vec(),
        "application/json",
        false,
    );
    let bytes = builder.build(Some(&key), true).unwrap();
    std::fs::write(&out, &bytes).expect("write failed");
    println!("wrote {} bytes to {out}", bytes.len());
}
