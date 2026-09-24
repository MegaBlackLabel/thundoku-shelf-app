//! Pack building — layout identical to the TS `builder.ts`:
//! sorted entries -> compress (raw deflate) -> encrypt (AES-GCM) -> offsets
//! -> header -> body (8-byte aligned) -> index -> index CRC.

use std::cmp::Ordering;
use std::io::Write;

use crate::{
    FORMAT_VERSION, PackError, PackKey, entry_flags,
    format::{WireEntry, align8, index_entry_size, serialize_header, serialize_index_entry},
    pack_flags,
};

struct BuilderEntry {
    path: String,
    mime_type: String,
    data: Vec<u8>,
    compress: bool,
}

struct ProcessedEntry {
    path: String,
    mime_type: String,
    data: Vec<u8>,
    size: u64,
    compressed_size: u64,
    flags: u32,
    iv: [u8; 12],
}

pub struct PackBuilder {
    created_at: u64,
    entries: Vec<BuilderEntry>,
}

impl PackBuilder {
    pub fn new(created_at: u64) -> Self {
        Self {
            created_at,
            entries: Vec::new(),
        }
    }

    pub fn add_entry(&mut self, path: &str, data: Vec<u8>, mime_type: &str, compress: bool) {
        self.entries.push(BuilderEntry {
            path: path.to_owned(),
            mime_type: mime_type.to_owned(),
            data,
            compress,
        });
    }

    /// Build the pack. `compress` sets the pack-level flag (and is the TS
    /// global default); per-entry compression comes from `add_entry`.
    /// `key` is the pack key derived from the account root key
    /// ([`crate::PackRootKey::derive_pack_key`]): when given, every entry is
    /// AES-256-GCM encrypted with it. v3 packs carry no identity material.
    pub fn build(self, key: Option<&PackKey>, compress: bool) -> Result<Vec<u8>, PackError> {
        // Sort by path using UTF-16 code unit comparison — identical to the
        // JS string `<`/`>` used by builder.ts.
        let mut entries = self.entries;
        entries.sort_by(|a, b| utf16_cmp(&a.path, &b.path));

        let mut processed = Vec::with_capacity(entries.len());
        for entry in entries {
            let size = entry.data.len() as u64;
            let mut data = entry.data;
            let mut flags = entry_flags::NONE;
            if entry.compress {
                data = raw_deflate(&data, 6)
                    .map_err(|e| PackError::Corrupted(format!("compression failed: {e}")))?;
                flags |= entry_flags::COMPRESSED;
            }
            let mut iv = [0u8; 12];
            if let Some(key) = key {
                let (entry_iv, encrypted) = crate::crypto::encrypt(&data, key.as_bytes());
                data = encrypted;
                iv = entry_iv;
                flags |= entry_flags::ENCRYPTED;
            }
            let compressed_size = data.len() as u64;
            processed.push(ProcessedEntry {
                path: entry.path,
                mime_type: entry.mime_type,
                data,
                size,
                compressed_size,
                flags,
                iv,
            });
        }

        // Body offsets: header, then 8-byte aligned payloads.
        let mut offsets = Vec::with_capacity(processed.len());
        let mut current = crate::HEADER_SIZE as u64;
        for entry in &processed {
            offsets.push(current);
            current += align8(entry.data.len() as u64);
        }
        let index_offset = current;
        let index_size: u64 = processed
            .iter()
            .map(|e| index_entry_size(&e.path, &e.mime_type) as u64)
            .sum::<u64>()
            + 4;

        let pack_flags = if compress {
            pack_flags::COMPRESSED
        } else {
            pack_flags::NONE
        } | if key.is_some() {
            pack_flags::ENCRYPTED
        } else {
            pack_flags::NONE
        };

        let header = serialize_header(
            FORMAT_VERSION,
            pack_flags,
            index_offset,
            index_size,
            processed.len() as u32,
            self.created_at,
        );

        let mut pack = Vec::with_capacity((index_offset + index_size) as usize);
        pack.extend_from_slice(&header);
        for (entry, offset) in processed.iter().zip(&offsets) {
            debug_assert_eq!(pack.len() as u64, *offset);
            pack.extend_from_slice(&entry.data);
            let padded = align8(entry.data.len() as u64) as usize;
            pack.resize(pack.len() + (padded - entry.data.len()), 0);
        }

        let index_start = pack.len();
        for (entry, offset) in processed.iter().zip(&offsets) {
            let wire = WireEntry {
                path: entry.path.clone(),
                mime_type: entry.mime_type.clone(),
                offset: *offset,
                size: entry.size,
                compressed_size: entry.compressed_size,
                flags: entry.flags,
                iv: entry.iv,
            };
            pack.extend_from_slice(&serialize_index_entry(&wire));
        }
        let index_crc = crate::crypto::crc32(&pack[index_start..]);
        pack.extend_from_slice(&index_crc.to_le_bytes());

        debug_assert_eq!(pack.len(), (index_offset + index_size) as usize);
        Ok(pack)
    }
}

/// UTF-16 code unit comparison — mirrors JavaScript string comparison.
fn utf16_cmp(a: &str, b: &str) -> Ordering {
    let mut ai = a.encode_utf16();
    let mut bi = b.encode_utf16();
    loop {
        match (ai.next(), bi.next()) {
            (Some(x), Some(y)) => {
                if x != y {
                    return x.cmp(&y);
                }
            }
            (Some(_), None) => return Ordering::Greater,
            (None, Some(_)) => return Ordering::Less,
            (None, None) => return Ordering::Equal,
        }
    }
}

/// RAW DEFLATE (RFC 1951, no zlib wrapper) — what fflate `deflateSync`
/// produces.
fn raw_deflate(data: &[u8], level: u32) -> Result<Vec<u8>, std::io::Error> {
    let mut encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::new(level));
    encoder.write_all(data)?;
    encoder.finish()
}
