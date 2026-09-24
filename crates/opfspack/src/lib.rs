//! .opfspack binary format: reader/writer ported from the TypeScript
//! reference implementation (packages/opfspack in thundoku-shelf).
//! Byte layout must stay interoperable with the Web implementation.
//!
//! Wire-format facts (verified against `src/format.ts`, `src/builder.ts`,
//! `src/reader.ts` and the v2 fixtures the TS implementation generated; the
//! version 3 fixtures are emitted by this crate's builder for now):
//!
//! - everything little-endian
//! - header: 64 bytes, magic `OPFS`, version 3, CRC-32 (IEEE) over bytes 0..60
//! - entries stored back-to-back after the header, each 8-byte aligned
//! - index: sorted by path (UTF-16 code unit order), each entry 8-byte
//!   aligned, index CRC-32 appended — **`index_size` includes the CRC**
//! - compression is RAW DEFLATE (fflate `deflateSync`; no zlib wrapper)
//! - encryption: pack_key = HKDF-SHA256(ikm = PRK, salt = packId,
//!   info = "opfspack-entry-key") -> AES-256-GCM per entry (no AAD).
//!   The PRK is a random per-account root key, stored wrapped (see
//!   `docs/spec/10-pack-keys.md` and [`PackKeyBundle`]); v3 packs carry no
//!   identity material at all.
//! - v2 packs (keys derived from `sub`) are **rejected** as
//!   [`PackError::Version`] and must be re-imported.

#![forbid(unsafe_code)]

mod builder;
mod crypto;
mod format;
mod keys;
mod reader;

pub use builder::PackBuilder;
pub use keys::{
    KEY_BUNDLE_FORMAT_VERSION, PackKey, PackKeyBundle, PackRootKey, PackRootKeyWrap, WrapKind,
    sub_wrap_kek,
};
pub use reader::{PackFileReader, PackReader};

/// Magic bytes at offset 0 of every pack.
pub const MAGIC: [u8; 4] = [0x4f, 0x50, 0x46, 0x53];
/// Fixed header size in bytes.
pub const HEADER_SIZE: usize = 64;
/// The only supported pack version (v2 packs are rejected: re-import).
pub const FORMAT_VERSION: u32 = 3;

/// `sub` ラップの KEK に使う salt（v2 と同一。仕様 §2）。
pub const APP_SALT: &[u8] = b"opfspack-v1-identity-salt-2024";
/// `sub` ラップの PBKDF2 反復回数（v2 の master key と同一）。
pub const SUB_WRAP_ITERATIONS: u32 = 100_000;
/// パスフレーズラップの既定の PBKDF2 反復回数（暫定値。ラップに記録するので
/// 後から上げられる — 仕様 §3.2 / §10）。
pub const PASSPHRASE_WRAP_ITERATIONS: u32 = 600_000;

/// 展開後（復号・伸長後）サイズの上限 — 1 エントリあたり。
///
/// index の `size` は攻撃者が自由に書けるため、解析時（`Vec` の確保・本体の
/// 読み出しより前）にこの値と突き合わせる。既知の最大 pack は展開後 1.33 GB /
/// 3,321 ページで、1 エントリは数 MB 級なので正常な本は通る。
pub const MAX_ENTRY_SIZE: u64 = 512 * 1024 * 1024;

/// 展開後サイズの合計上限 — 1 pack あたり（既知の最大は 1.33 GB）。
/// 合計は `checked_add` で積み上げ、overflow と上限超過の両方を拒否する。
pub const MAX_TOTAL_SIZE: u64 = 2 * 1024 * 1024 * 1024;

/// 保存サイズ（圧縮・暗号化後）の上限 — 1 エントリあたり。
/// 展開上限 512 MiB に暗号タグ 16 バイトと DEFLATE の膨張余地を加えても
/// 収まる範囲として合計上限と同じ値を用いる。
pub const MAX_STORED_ENTRY_SIZE: u64 = 2 * 1024 * 1024 * 1024;

/// index が持てるエントリ数の上限（既知の最大は 3,321 件）。
/// `entry_count` は index 長から導ける件数（1 エントリ ≧ 48 バイト）でも
/// 制限されるが、こちらは索引領域そのものが大きい pack にも効く。
pub const MAX_ENTRY_COUNT: u32 = 10_000;

/// Pack-level flags stored in the header.
pub mod pack_flags {
    /// No flags set.
    pub const NONE: u32 = 0;
    /// The pack was built with global compression enabled.
    pub const COMPRESSED: u32 = 1 << 0;
    /// Every entry in the pack is AES-256-GCM encrypted (builder sets this
    /// exactly when it was given a [`crate::PackKey`]).
    pub const ENCRYPTED: u32 = 1 << 1;
    /// 定義済みビットのマスク。version 3 に存在しないビットが立っていれば
    /// 破損として拒否する（v2 も同じ 2 ビットしか書かない）。
    pub const KNOWN_MASK: u32 = COMPRESSED | ENCRYPTED;
}

/// Entry-level flags stored in each index entry.
pub mod entry_flags {
    /// No flags set.
    pub const NONE: u32 = 0;
    /// Entry payload is raw-DEFLATE compressed.
    pub const COMPRESSED: u32 = 1 << 0;
    /// Entry payload is AES-256-GCM encrypted with the pack key.
    pub const ENCRYPTED: u32 = 1 << 1;
    /// v2 only: entry required the Google identity (`sub`) to decrypt.
    /// **version 3 never sets this bit** — v3 keys come from a wrapped root
    /// key, so a set bit means the pack is damaged/rejected (`Corrupted`).
    pub const IDENTITY_BOUND: u32 = 1 << 2;
    /// LZ4 compression — never produced by the TS implementation; reading
    /// such an entry fails with [`PackError::UnsupportedLz4`].
    pub const LZ4: u32 = 1 << 3;
    /// 定義済みビットのマスク。version 3 に存在しないビット（`IDENTITY_BOUND`
    /// を含む）が立っていれば破損として拒否する（`reader.rs` の index 解析時）。
    pub const KNOWN_MASK: u32 = COMPRESSED | ENCRYPTED | LZ4;
}

/// Parsed pack header (CRC already validated by [`PackReader::open`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackHeader {
    pub version: u32,
    pub flags: u32,
    pub index_offset: u64,
    pub index_size: u64,
    pub entry_count: u32,
    pub created_at: u64,
}

/// One index entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackEntry {
    pub path: String,
    pub mime_type: String,
    /// Offset of the stored payload inside the pack body.
    pub offset: u64,
    /// Original (uncompressed, decrypted) size in bytes.
    pub size: u64,
    /// Stored size in bytes (compressed / encrypted).
    pub compressed_size: u64,
    pub flags: u32,
    /// AES-GCM nonce; zero for non-encrypted entries.
    pub iv: [u8; 12],
}

/// Errors produced by pack reading and writing.
#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("unsupported pack version: {0}")]
    Version(u32),
    #[error("corrupted pack: {0}")]
    Corrupted(String),
    #[error("lz4 compression not supported")]
    UnsupportedLz4,
    #[error("pack key required for encrypted entry: {0}")]
    KeyRequired(String),
    #[error("unsupported key bundle format version: {0}")]
    UnsupportedKeyBundle(u32),
    #[error("entry not found: {0}")]
    NotFound(String),
    #[error("invalid range: {0}")]
    InvalidRange(String),
    #[error("pack I/O error: {0}")]
    Io(String),
}

/// SHA-256(`"opfspack:v1:{sub}"`) as lowercase hex — matches
/// `deriveOwnerId` in the TS `auth/identity-key.ts`.
pub fn derive_owner_id(sub: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(format!("opfspack:v1:{sub}").as_bytes());
    digest.iter().map(|b| format!("{b:02x}")).collect()
}
