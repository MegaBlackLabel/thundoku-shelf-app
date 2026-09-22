//! Pack reading — validation order matches the TS `reader.ts`:
//! size >= 64 -> magic -> version -> header CRC -> index CRC -> entry bounds.

use crate::{
    FORMAT_VERSION, Identity, PackEntry, PackError, PackHeader, entry_flags,
    format::{deserialize_header, deserialize_index_entry, index_entry_size},
};
use std::io::{Read, Seek, SeekFrom};

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

        let entries = parse_index_entries(
            &bytes[index_offset..end - 4],
            header.index_offset,
            header.entry_count,
        )?;
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
        let data = self.bytes[start..end].to_vec();
        decode_entry_payload(entry, data, key)
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

/// index エントリの最小整列サイズ（空パス・空 MIME の 44 バイトを 8 バイト境界へ
/// 切り上げ）。`entry_count` から確保する前に「index 長で説明できる件数か」を
/// 判定するために使う。
const MIN_INDEX_ENTRY_SIZE: usize = 48;

/// インデックス領域（CRC を含まない）をエントリ列へ解釈する。
/// 各エントリの格納領域が本体（ヘッダ〜インデックス）に収まっていることも検証する。
fn parse_index_entries(
    index: &[u8],
    index_offset: u64,
    entry_count: u32,
) -> Result<Vec<PackEntry>, PackError> {
    // 件数は index 長から導ける上限で先に検査する。ここを飛ばすと、細工した
    // `entry_count`（u32::MAX 等）+ 68 バイトのデータで数百 GB の
    // `Vec::with_capacity` を試みさせられる（ヘッダ CRC は攻撃者も計算できるため
    // 改竄の検知にはならない）。
    let max_entries = index.len() / MIN_INDEX_ENTRY_SIZE;
    if entry_count as usize > max_entries {
        return Err(PackError::Corrupted(format!(
            "entry_count {entry_count} exceeds index capacity {max_entries}"
        )));
    }
    let mut entries = Vec::with_capacity(entry_count as usize);
    let mut pos = 0;
    for _ in 0..entry_count {
        if pos >= index.len() {
            return Err(PackError::Corrupted(format!(
                "index truncated: expected {entry_count} entries"
            )));
        }
        let entry = deserialize_index_entry(&index[pos..])?;
        pos += index_entry_size(&entry.path, &entry.mime_type);
        let data_end = entry
            .offset
            .checked_add(entry.compressed_size)
            .ok_or_else(|| {
                PackError::Corrupted(format!("entry offset overflow: {}", entry.path))
            })?;
        if entry.offset < crate::HEADER_SIZE as u64 || data_end > index_offset {
            return Err(PackError::Corrupted(format!(
                "entry extends beyond body: {}",
                entry.path
            )));
        }
        entries.push(entry.into());
    }
    Ok(entries)
}

/// 格納バイト列（AES-GCM 暗号文 / raw DEFLATE）を平文へ戻す。
fn decode_entry_payload(
    entry: &PackEntry,
    mut data: Vec<u8>,
    key: Option<&[u8; 32]>,
) -> Result<Vec<u8>, PackError> {
    if entry.flags & entry_flags::IDENTITY_BOUND != 0 {
        let key = key.ok_or_else(|| PackError::IdentityRequired(entry.path.clone()))?;
        data = crate::crypto::decrypt(&data, &entry.iv, key)
            .map_err(|_| PackError::Corrupted(format!("decryption failed: {}", entry.path)))?;
    }
    if entry.flags & entry_flags::COMPRESSED != 0 {
        data = raw_inflate(&data)
            .map_err(|e| PackError::Corrupted(format!("decompression failed: {e}")))?;
    }
    Ok(data)
}

fn io_error(error: std::io::Error) -> PackError {
    PackError::Io(error.to_string())
}

fn read_at(file: &mut std::fs::File, offset: u64, buf: &mut [u8]) -> Result<(), PackError> {
    file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
    file.read_exact(buf).map_err(io_error)
}

/// ファイル裏打ちの pack リーダー。
///
/// `PackReader` は pack 全体を `&[u8]` で受け取るため、大きい pack（数百 MB）から
/// 表紙 1 枚を取り出すのにも全体をメモリへ読む必要があった。こちらは**ヘッダと
/// インデックスだけ**を読んで開き、エントリの読み出しは必要な範囲だけシークして読む。
/// 検証（ヘッダ CRC / バージョン / インデックス CRC / エントリ境界）は `PackReader`
/// と同一で、`parse_index_entries` を共有している。
pub struct PackFileReader {
    file: std::fs::File,
    header: PackHeader,
    entries: Vec<PackEntry>,
}

impl PackFileReader {
    pub fn open(path: &std::path::Path) -> Result<Self, PackError> {
        let mut file = std::fs::File::open(path).map_err(io_error)?;
        let len = file.metadata().map_err(io_error)?.len();
        if len < crate::HEADER_SIZE as u64 {
            return Err(PackError::Corrupted(format!("pack too small: {len} bytes")));
        }

        let mut header_bytes = [0u8; crate::HEADER_SIZE];
        read_at(&mut file, 0, &mut header_bytes)?;
        let header = deserialize_header(&header_bytes)?;
        if header.version != FORMAT_VERSION {
            return Err(PackError::Version(header.version));
        }

        let index_offset = header.index_offset;
        let end = index_offset
            .checked_add(header.index_size)
            .ok_or_else(|| PackError::Corrupted("index offset overflow".into()))?;
        if end > len {
            return Err(PackError::Corrupted("index extends beyond file".into()));
        }
        if header.index_size < 4 {
            return Err(PackError::Corrupted("index too small for CRC".into()));
        }

        let mut index = vec![0u8; header.index_size as usize];
        read_at(&mut file, index_offset, &mut index)?;
        let stored_crc =
            u32::from_le_bytes(index[index.len() - 4..].try_into().expect("4-byte slice"));
        let computed_crc = crate::crypto::crc32(&index[..index.len() - 4]);
        if stored_crc != computed_crc {
            return Err(PackError::Corrupted(format!(
                "index CRC mismatch: stored={stored_crc}, computed={computed_crc}"
            )));
        }

        let entries = parse_index_entries(
            &index[..index.len() - 4],
            header.index_offset,
            header.entry_count,
        )?;
        Ok(Self {
            file,
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

    /// エントリの格納バイト列だけを読んで復号する（本体全体は読まない）。
    pub fn read_entry_with_key(
        &mut self,
        path: &str,
        key: Option<&[u8; 32]>,
    ) -> Result<Vec<u8>, PackError> {
        let entry = self
            .entry(path)
            .ok_or_else(|| PackError::NotFound(path.to_owned()))?
            .clone();
        if entry.flags & entry_flags::LZ4 != 0 {
            return Err(PackError::UnsupportedLz4);
        }
        // 境界は `open` の時点で検証済み（本体領域内に収まっている）。
        let mut data = vec![0u8; entry.compressed_size as usize];
        read_at(&mut self.file, entry.offset, &mut data)?;
        decode_entry_payload(&entry, data, key)
    }
}
