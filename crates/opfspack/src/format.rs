//! Header and index entry (de)serialization — byte-for-byte compatible with
//! the TS `format.ts`.

use crate::{MAGIC, PackError, PackHeader};

pub(crate) struct WireEntry {
    pub(crate) path: String,
    pub(crate) mime_type: String,
    pub(crate) offset: u64,
    pub(crate) size: u64,
    pub(crate) compressed_size: u64,
    pub(crate) flags: u32,
    pub(crate) iv: [u8; 12],
}

impl From<WireEntry> for crate::PackEntry {
    fn from(e: WireEntry) -> Self {
        Self {
            path: e.path,
            mime_type: e.mime_type,
            offset: e.offset,
            size: e.size,
            compressed_size: e.compressed_size,
            flags: e.flags,
            iv: e.iv,
        }
    }
}

pub(crate) fn align8(n: u64) -> u64 {
    n.div_ceil(8) * 8
}

fn read_u16(bytes: &[u8], pos: usize) -> Result<u16, PackError> {
    let slice = bytes
        .get(pos..pos + 2)
        .ok_or_else(|| PackError::Corrupted("truncated index entry".into()))?;
    Ok(u16::from_le_bytes(slice.try_into().unwrap()))
}

fn read_u32(bytes: &[u8], pos: usize) -> Result<u32, PackError> {
    let slice = bytes
        .get(pos..pos + 4)
        .ok_or_else(|| PackError::Corrupted("truncated index entry".into()))?;
    Ok(u32::from_le_bytes(slice.try_into().unwrap()))
}

fn read_u64(bytes: &[u8], pos: usize) -> Result<u64, PackError> {
    let slice = bytes
        .get(pos..pos + 8)
        .ok_or_else(|| PackError::Corrupted("truncated index entry".into()))?;
    Ok(u64::from_le_bytes(slice.try_into().unwrap()))
}

pub(crate) fn serialize_header(
    version: u32,
    flags: u32,
    index_offset: u64,
    index_size: u64,
    entry_count: u32,
    created_at: u64,
) -> [u8; 64] {
    let mut buf = [0u8; 64];
    buf[0..4].copy_from_slice(&MAGIC);
    buf[4..8].copy_from_slice(&version.to_le_bytes());
    buf[8..12].copy_from_slice(&flags.to_le_bytes());
    // bytes 12..16 reserved (0)
    buf[16..24].copy_from_slice(&index_offset.to_le_bytes());
    buf[24..32].copy_from_slice(&index_size.to_le_bytes());
    buf[32..36].copy_from_slice(&entry_count.to_le_bytes());
    // bytes 36..40 reserved (0)
    buf[40..48].copy_from_slice(&created_at.to_le_bytes());
    // bytes 48..60 reserved (0)
    let crc = crate::crypto::crc32(&buf[..60]);
    buf[60..64].copy_from_slice(&crc.to_le_bytes());
    buf
}

pub(crate) fn deserialize_header(bytes: &[u8]) -> Result<PackHeader, PackError> {
    if bytes.len() < crate::HEADER_SIZE {
        return Err(PackError::Corrupted("buffer too small for header".into()));
    }
    if bytes[0..4] != MAGIC {
        return Err(PackError::Corrupted(
            "invalid magic number: expected OPFS".into(),
        ));
    }
    let stored = read_u32(bytes, 60)?;
    let computed = crate::crypto::crc32(&bytes[..60]);
    if stored != computed {
        return Err(PackError::Corrupted(format!(
            "header checksum mismatch: stored={stored}, computed={computed}"
        )));
    }
    Ok(PackHeader {
        version: read_u32(bytes, 4)?,
        flags: read_u32(bytes, 8)?,
        index_offset: read_u64(bytes, 16)?,
        index_size: read_u64(bytes, 24)?,
        entry_count: read_u32(bytes, 32)?,
        created_at: read_u64(bytes, 40)?,
    })
}

/// Aligned on-disk size of an index entry for the given path/mime.
pub(crate) fn index_entry_size(path: &str, mime_type: &str) -> usize {
    let raw = 2 + path.len() + 2 + mime_type.len() + 8 + 8 + 8 + 4 + 12;
    align8(raw as u64) as usize
}

pub(crate) fn serialize_index_entry(entry: &WireEntry) -> Vec<u8> {
    let mut buf = vec![0u8; index_entry_size(&entry.path, &entry.mime_type)];
    let mut pos = 0;
    buf[pos..pos + 2].copy_from_slice(&(entry.path.len() as u16).to_le_bytes());
    pos += 2;
    buf[pos..pos + entry.path.len()].copy_from_slice(entry.path.as_bytes());
    pos += entry.path.len();
    buf[pos..pos + 2].copy_from_slice(&(entry.mime_type.len() as u16).to_le_bytes());
    pos += 2;
    buf[pos..pos + entry.mime_type.len()].copy_from_slice(entry.mime_type.as_bytes());
    pos += entry.mime_type.len();
    buf[pos..pos + 8].copy_from_slice(&entry.offset.to_le_bytes());
    pos += 8;
    buf[pos..pos + 8].copy_from_slice(&entry.size.to_le_bytes());
    pos += 8;
    buf[pos..pos + 8].copy_from_slice(&entry.compressed_size.to_le_bytes());
    pos += 8;
    buf[pos..pos + 4].copy_from_slice(&entry.flags.to_le_bytes());
    pos += 4;
    buf[pos..pos + 12].copy_from_slice(&entry.iv);
    buf
}

pub(crate) fn deserialize_index_entry(bytes: &[u8]) -> Result<WireEntry, PackError> {
    let path_len = read_u16(bytes, 0)? as usize;
    let path = std::str::from_utf8(
        bytes
            .get(2..2 + path_len)
            .ok_or_else(|| PackError::Corrupted("truncated index entry".into()))?,
    )
    .map_err(|_| PackError::Corrupted("invalid path encoding".into()))?
    .to_owned();
    let mut pos = 2 + path_len;
    let mime_len = read_u16(bytes, pos)? as usize;
    pos += 2;
    let mime_type = std::str::from_utf8(
        bytes
            .get(pos..pos + mime_len)
            .ok_or_else(|| PackError::Corrupted("truncated index entry".into()))?,
    )
    .map_err(|_| PackError::Corrupted("invalid mime encoding".into()))?
    .to_owned();
    pos += mime_len;
    let offset = read_u64(bytes, pos)?;
    pos += 8;
    let size = read_u64(bytes, pos)?;
    pos += 8;
    let compressed_size = read_u64(bytes, pos)?;
    pos += 8;
    let flags = read_u32(bytes, pos)?;
    pos += 4;
    let iv: [u8; 12] = bytes
        .get(pos..pos + 12)
        .ok_or_else(|| PackError::Corrupted("truncated index entry".into()))?
        .try_into()
        .unwrap();
    Ok(WireEntry {
        path,
        mime_type,
        offset,
        size,
        compressed_size,
        flags,
        iv,
    })
}
