//! Pack reading — validation order matches the TS `reader.ts`:
//! size >= 64 -> magic -> version -> header CRC -> index CRC -> entry bounds.

use crate::{
    FORMAT_VERSION, Identity, PackEntry, PackError, PackHeader, entry_flags,
    format::{deserialize_header, deserialize_index_entry, index_entry_size},
};

pub struct PackReader<'a> {
    bytes: &'a [u8],
    header: PackHeader,
    entries: Vec<PackEntry>,
}

impl<'a> PackReader<'a> {
    /// Parse and fully validate the pack (header CRC, version, index CRC,
    /// entry boundaries). Returns [`PackError::Version`] for unsupported
    /// versions and [`PackError::Corrupted`] for structural damage.
    pub fn open(bytes: &'a [u8]) -> Result<Self, PackError> {
        let header = deserialize_header(bytes)?;
        if header.version != FORMAT_VERSION {
            return Err(PackError::Version(header.version));
        }
        let index_offset = header.index_offset as usize;
        let index_size = header.index_size as usize;
        let end = index_offset
            .checked_add(index_size)
            .ok_or_else(|| PackError::Corrupted("index offset overflow".into()))?;
        if end > bytes.len() {
            return Err(PackError::Corrupted("index extends beyond buffer".into()));
        }
        if index_size < 4 {
            return Err(PackError::Corrupted("index too small for CRC".into()));
        }
        let stored_crc = u32::from_le_bytes(bytes[end - 4..end].try_into().expect("4-byte slice"));
        let computed_crc = crate::crypto::crc32(&bytes[index_offset..end - 4]);
        if stored_crc != computed_crc {
            return Err(PackError::Corrupted(format!(
                "index CRC mismatch: stored={stored_crc}, computed={computed_crc}"
            )));
        }

        let mut entries = Vec::with_capacity(header.entry_count as usize);
        let mut pos = index_offset;
        let entries_end = end - 4;
        for _ in 0..header.entry_count {
            if pos >= entries_end {
                return Err(PackError::Corrupted(format!(
                    "index truncated: expected {} entries",
                    header.entry_count
                )));
            }
            let entry = deserialize_index_entry(&bytes[pos..entries_end])?;
            pos += index_entry_size(&entry.path, &entry.mime_type);
            // The entry payload must live inside the body region
            // (between the header and the index).
            let data_end = entry
                .offset
                .checked_add(entry.compressed_size)
                .ok_or_else(|| {
                    PackError::Corrupted(format!("entry offset overflow: {}", entry.path))
                })?;
            if entry.offset < crate::HEADER_SIZE as u64 || data_end > header.index_offset {
                return Err(PackError::Corrupted(format!(
                    "entry extends beyond body: {}",
                    entry.path
                )));
            }
            entries.push(entry.into());
        }
        Ok(Self {
            bytes,
            header,
            entries,
        })
    }

    pub fn header(&self) -> &PackHeader {
        &self.header
    }

    pub fn entries(&self) -> &[PackEntry] {
        &self.entries
    }

    pub fn entry(&self, path: &str) -> Option<&PackEntry> {
        self.entries.iter().find(|e| e.path == path)
    }

    /// Read and decrypt/decompress the full entry payload.
    pub fn read_entry(
        &self,
        path: &str,
        identity: Option<&Identity>,
    ) -> Result<Vec<u8>, PackError> {
        let key = identity.map(|id| {
            let master = crate::crypto::master_key(&id.sub);
            crate::crypto::pack_key(&master, &id.pack_id)
        });
        self.read_entry_with_key(path, key.as_ref())
    }

    /// Read an entry using a pre-derived pack key (avoids re-running PBKDF2
    /// 100k iterations per page). `key` is only used for identity-bound entries.
    pub fn read_entry_with_key(
        &self,
        path: &str,
        key: Option<&[u8; 32]>,
    ) -> Result<Vec<u8>, PackError> {
        let entry = self
            .entry(path)
            .ok_or_else(|| PackError::NotFound(path.to_owned()))?;
        if entry.flags & entry_flags::LZ4 != 0 {
            return Err(PackError::UnsupportedLz4);
        }
        let start = entry.offset as usize;
        let end = start + entry.compressed_size as usize;
        let mut data = self.bytes[start..end].to_vec();
        if entry.flags & entry_flags::IDENTITY_BOUND != 0 {
            let key = key.ok_or_else(|| PackError::IdentityRequired(path.to_owned()))?;
            data = crate::crypto::decrypt(&data, &entry.iv, key)
                .map_err(|_| PackError::Corrupted(format!("decryption failed: {path}")))?;
        }
        if entry.flags & entry_flags::COMPRESSED != 0 {
            data = raw_inflate(&data)
                .map_err(|e| PackError::Corrupted(format!("decompression failed: {e}")))?;
        }
        Ok(data)
    }

    /// Read a byte range of the uncompressed, decrypted payload
    /// (validated against the original entry size).
    pub fn read_entry_range(
        &self,
        path: &str,
        start: u64,
        end: u64,
        identity: Option<&Identity>,
    ) -> Result<Vec<u8>, PackError> {
        let entry = self
            .entry(path)
            .ok_or_else(|| PackError::NotFound(path.to_owned()))?;
        if start >= end || end > entry.size {
            return Err(PackError::InvalidRange(format!(
                "range {start}..{end} exceeds {} bytes",
                entry.size
            )));
        }
        let full = self.read_entry(path, identity)?;
        Ok(full[start as usize..end as usize].to_vec())
    }
}

fn raw_inflate(data: &[u8]) -> Result<Vec<u8>, std::io::Error> {
    use std::io::Read;
    let mut out = Vec::new();
    flate2::read::DeflateDecoder::new(data).read_to_end(&mut out)?;
    Ok(out)
}
