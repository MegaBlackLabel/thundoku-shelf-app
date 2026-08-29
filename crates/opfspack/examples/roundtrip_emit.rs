//! One-off interop emitter: builds a pack with the Rust implementation and
//! writes it to the path given as the first argument, so the TS
//! implementation can read it back (Verification step 2).

use opfspack::{Identity, PackBuilder};

fn main() {
    let out = std::env::args()
        .nth(1)
        .expect("usage: roundtrip_emit <out.opfspack>");
    let id = Identity {
        sub: "test-sub".into(),
        pack_id: "test-pack".into(),
    };
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
    let bytes = builder.build(Some(&id), true).unwrap();
    std::fs::write(&out, &bytes).expect("write failed");
    println!("wrote {} bytes to {out}", bytes.len());
}
