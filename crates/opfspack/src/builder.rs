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

/// 1 エントリ分の入力（データ本体は [`PackBuilder::build_to_file_streaming`] の
/// `data` が path を受け取って返す）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntrySpec {
    pub path: String,
    pub mime_type: String,
    pub compress: bool,
}

/// 書き出し済みエントリのメタデータ（インデックスを組むために必要な分だけ持つ。
/// データ本体は保持しない = 大きい pack でも RAM が増えない）。
struct ProcessedEntry {
    path: String,
    mime_type: String,
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
        let mut cursor = std::io::Cursor::new(Vec::new());
        self.build_into(&mut cursor, key, compress)?;
        Ok(cursor.into_inner())
    }

    /// pack を**ファイルへ直接**書き出す（組み立て中のバイト列を RAM に持たない）。
    ///
    /// レイアウトは [`Self::build`] と**バイト単位で同一**（テストで固定している）。
    /// 失敗したら書きかけのファイルを削除する（壊れた pack を残さない）。
    pub fn build_to_file(
        self,
        path: &std::path::Path,
        key: Option<&PackKey>,
        compress: bool,
    ) -> Result<u64, PackError> {
        write_file_atomic(path, |sink| self.build_into(sink, key, compress))
    }

    /// エントリを**1 件ずつ供給**して pack をファイルへ書き出す（大きい pack 用）。
    ///
    /// `entries` は path と MIME と per-entry の圧縮指定で、データ本体は `data` が
    /// `entries[i].path` を受け取って返す。組み立て中に RAM へ載るのは**1 エントリ分だけ**
    /// （[`Self::build_to_file`] は全エントリを保持するため、10 GiB 級ではメモリを
    /// 食い潰す）。レイアウトは [`Self::build`] と同一（テストで固定）。
    /// `self.entries` は使わないので [`Self::new`] で作った空のビルダーを渡す。
    /// 失敗したら書きかけのファイルを削除する。
    pub fn build_to_file_streaming<F>(
        self,
        path: &std::path::Path,
        entries: Vec<EntrySpec>,
        key: Option<&PackKey>,
        compress: bool,
        data: F,
    ) -> Result<u64, PackError>
    where
        F: FnMut(&str) -> Result<Vec<u8>, PackError>,
    {
        write_file_atomic(path, |sink| {
            write_pack(sink, self.created_at, entries, key, compress, data)
        })
    }

    /// pack を書き出し先へ組み立てて、書いたバイト数を返す。
    ///
    /// 全エントリを保持してから [`write_pack`] に渡す（[`Self::build`] / [`Self::build_to_file`]
    /// 用。データ本体を全部持てるサイズの pack が対象）。
    fn build_into<W: std::io::Write + std::io::Seek>(
        self,
        writer: &mut W,
        key: Option<&PackKey>,
        compress: bool,
    ) -> Result<u64, PackError> {
        let PackBuilder { created_at, entries } = self;
        let mut specs = Vec::with_capacity(entries.len());
        let mut payloads: std::collections::HashMap<String, std::collections::VecDeque<Vec<u8>>> =
            std::collections::HashMap::new();
        for entry in entries {
            specs.push(EntrySpec {
                path: entry.path.clone(),
                mime_type: entry.mime_type,
                compress: entry.compress,
            });
            payloads
                .entry(entry.path)
                .or_default()
                .push_back(entry.data);
        }
        write_pack(
            writer,
            created_at,
            specs,
            key,
            compress,
            |path| match payloads.get_mut(path).and_then(std::collections::VecDeque::pop_front) {
                Some(data) => Ok(data),
                None => Err(PackError::Io(format!("entry data missing: {path}"))),
            },
        )
    }
}

/// ファイルへ書き出し、**失敗したら書きかけを削除**する（壊れた pack を残さない）。
fn write_file_atomic(
    path: &std::path::Path,
    write: impl FnOnce(
        &mut std::io::BufWriter<std::fs::File>,
    ) -> Result<u64, PackError>,
) -> Result<u64, PackError> {
    let file = std::fs::File::create(path).map_err(|error| PackError::Io(error.to_string()))?;
    let mut sink = std::io::BufWriter::new(file);
    let result = write(&mut sink);
    let flushed = sink.flush().map_err(|error| PackError::Io(error.to_string()));
    match (result, flushed) {
        (Ok(bytes), Ok(())) => Ok(bytes),
        (Err(error), _) | (Ok(_), Err(error)) => {
            let _ = std::fs::remove_file(path);
            Err(error)
        }
    }
}

/// pack のレイアウトを書き出す（[`PackBuilder::build`] / [`PackBuilder::build_to_file`] /
/// [`PackBuilder::build_to_file_streaming`] の共通実装）。
///
/// エントリは **1 件ずつ** `data(path)` で取り出して書く（同時に保持するのは 1 件分だけ）。
/// 位置は自分で数えるので、`data` が順序どおりに返さない場合は壊れた pack を書かずに失敗する。
fn write_pack<W, F>(
    writer: &mut W,
    created_at: u64,
    entries: Vec<EntrySpec>,
    key: Option<&PackKey>,
    compress: bool,
    mut data: F,
) -> Result<u64, PackError>
where
    W: std::io::Write + std::io::Seek,
    F: FnMut(&str) -> Result<Vec<u8>, PackError>,
{
    // Sort by path using UTF-16 code unit comparison — identical to the
    // JS string `<`/`>` used by builder.ts.
    let mut entries = entries;
    entries.sort_by(|a, b| utf16_cmp(&a.path, &b.path));

    // オフセットは先頭からの絶対位置なので、書き出しは先頭から始まっている必要がある
    // （ヘッダーは本体の後で先頭へ戻って書く）。
    let base = writer.stream_position().map_err(io_error)?;
    if base != 0 {
        return Err(PackError::Io(
            "pack は先頭から書き出す必要があります".into(),
        ));
    }

    // 展開後の合計（読み出し側が `MAX_TOTAL_SIZE` として見る値）。
    let mut total_size = 0u64;
    // ヘッダーは本体の後で先頭へ戻って書く。先に**ヘッダー分を空けて**おき、
    // 本体はその続きから書く（オフセットは先頭からの絶対位置）。
    writer
        .seek(std::io::SeekFrom::Start(crate::HEADER_SIZE as u64))
        .map_err(|error| PackError::Io(error.to_string()))?;

    // **読み出し側と同じ上限**を書き出し側でも検査する。ここを欠くと、書けたのに
    // 開けない（読み出しが上限で拒否する）pack を作ってしまう。
    if entries.len() as u32 > crate::MAX_ENTRY_COUNT {
        return Err(PackError::Corrupted(format!(
            "entry count {} exceeds limit {}",
            entries.len(),
            crate::MAX_ENTRY_COUNT
        )));
    }

    let mut written: Vec<ProcessedEntry> = Vec::with_capacity(entries.len());
    let mut offsets = Vec::with_capacity(entries.len());
    // 次に本体を書く物理位置（ヘッダーを除いた本体だけの位置）。
    let mut body_pos = crate::HEADER_SIZE as u64;
    // インデックスに書く絶対オフセット（先頭 + ヘッダー + 本体）。
    let mut current = crate::HEADER_SIZE as u64;
    for spec in &entries {
        let raw = data(&spec.path)?;
        let size = raw.len() as u64;
        let mut bytes = raw;
        let mut flags = entry_flags::NONE;
        if spec.compress {
            bytes = raw_deflate(&bytes, 6)
                .map_err(|e| PackError::Corrupted(format!("compression failed: {e}")))?;
            flags |= entry_flags::COMPRESSED;
        }
        let mut iv = [0u8; 12];
        if let Some(key) = key {
            let (entry_iv, encrypted) = crate::crypto::encrypt(&bytes, key.as_bytes());
            bytes = encrypted;
            iv = entry_iv;
            flags |= entry_flags::ENCRYPTED;
        }
        let compressed_size = bytes.len() as u64;
        check_entry_limits(&spec.path, size, compressed_size, &mut total_size)?;
        if writer.stream_position().map_err(io_error)? != body_pos {
            return Err(PackError::Io("pack の書き出し位置がずれました".into()));
        }
        writer
            .write_all(&bytes)
            .map_err(|error| PackError::Io(error.to_string()))?;
        let padded = align8(bytes.len() as u64) as usize;
        if padded > bytes.len() {
            // 8 バイト境界までの詰め物（最大 7 バイト）
            writer
                .write_all(&vec![0u8; padded - bytes.len()])
                .map_err(|error| PackError::Io(error.to_string()))?;
        }
        // `bytes` はここで drop される（RAM に残るのは 1 エントリ分だけ）。
        body_pos += padded as u64;
        offsets.push(current);
        current += padded as u64;
        written.push(ProcessedEntry {
            path: spec.path.clone(),
            mime_type: spec.mime_type.clone(),
            size,
            compressed_size,
            flags,
            iv,
        });
    }

    let index_offset = current;
    let index_size: u64 = written
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
        written.len() as u32,
        created_at,
    );

    // ヘッダーは本体より前に置く必要があるが、位置は組み立て終わってから確定する。
    // いったん本体まで書いてから先頭へ戻って書く（`Seek` が要る理由）。
    let end = writer.stream_position().map_err(io_error)?;
    debug_assert_eq!(end, current);
    writer
        .seek(std::io::SeekFrom::Start(0))
        .map_err(|error| PackError::Io(error.to_string()))?;
    writer
        .write_all(&header)
        .map_err(|error| PackError::Io(error.to_string()))?;
    writer
        .seek(std::io::SeekFrom::Start(end))
        .map_err(|error| PackError::Io(error.to_string()))?;

    // インデックスはエントリ単位の小さなメタデータなので、メモリ上で組んでから書く。
    let mut index = Vec::with_capacity(index_size as usize);
    for (entry, offset) in written.iter().zip(&offsets) {
        let wire = WireEntry {
            path: entry.path.clone(),
            mime_type: entry.mime_type.clone(),
            offset: *offset,
            size: entry.size,
            compressed_size: entry.compressed_size,
            flags: entry.flags,
            iv: entry.iv,
        };
        index.extend_from_slice(&serialize_index_entry(&wire));
    }
    let index_crc = crate::crypto::crc32(&index);
    index.extend_from_slice(&index_crc.to_le_bytes());
    debug_assert_eq!(index.len() as u64, index_size);
    writer
        .write_all(&index)
        .map_err(|error| PackError::Io(error.to_string()))?;
    let total = index_offset + index_size;
    debug_assert_eq!(writer.stream_position().map_err(io_error)?, total);
    Ok(total)
}

/// 1 エントリ分の上限を検査する（読み出し側の `reader::check_entry_limits` と同じ規則）。
///
/// 書き出し側で見ないと「書けるが開けない pack」ができる。
fn check_entry_limits(
    path: &str,
    size: u64,
    compressed_size: u64,
    total_size: &mut u64,
) -> Result<(), PackError> {
    if size > crate::MAX_ENTRY_SIZE {
        return Err(PackError::Corrupted(format!(
            "entry size {size} exceeds limit {}: {path}",
            crate::MAX_ENTRY_SIZE
        )));
    }
    if compressed_size > crate::MAX_STORED_ENTRY_SIZE {
        return Err(PackError::Corrupted(format!(
            "stored size {compressed_size} exceeds limit {}: {path}",
            crate::MAX_STORED_ENTRY_SIZE
        )));
    }
    *total_size = total_size
        .checked_add(size)
        .ok_or_else(|| PackError::Corrupted(format!("total size overflow: {path}")))?;
    if *total_size > crate::MAX_TOTAL_SIZE {
        return Err(PackError::Corrupted(format!(
            "total size {} exceeds limit {}: {path}",
            *total_size,
            crate::MAX_TOTAL_SIZE
        )));
    }
    Ok(())
}

/// `io::Error` を `PackError` へ（`PackError::Io` は文言だけ持つ）。
fn io_error(error: std::io::Error) -> PackError {
    PackError::Io(error.to_string())
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

#[cfg(test)]
mod tests {
    use super::check_entry_limits;

    /// 合計は `MAX_TOTAL_SIZE` を超えた時点で弾く（読み出し側と同じ値）。
    #[test]
    fn rejects_a_total_size_over_the_limit() {
        let mut total = 0u64;
        // 1 エントリの上限（512 MiB）ちょうどのエントリを、合計の上限に届くまで通す。
        let entries = crate::MAX_TOTAL_SIZE / crate::MAX_ENTRY_SIZE;
        for index in 0..entries {
            check_entry_limits(&format!("{index}"), crate::MAX_ENTRY_SIZE, 1, &mut total).unwrap();
        }
        assert_eq!(total, crate::MAX_TOTAL_SIZE);
        // 1 バイトでも超えたら弾く。
        let error = check_entry_limits("over", 1, 1, &mut total).unwrap_err();
        assert!(error.to_string().contains("total size"), "{error}");
    }

    /// 1 エントリの展開後 / 保存後サイズの上限も読み出し側と揃える。
    #[test]
    fn rejects_entry_sizes_over_the_limits() {
        let mut total = 0u64;
        let too_big = check_entry_limits("a", crate::MAX_ENTRY_SIZE + 1, 1, &mut total).unwrap_err();
        assert!(too_big.to_string().contains("entry size"), "{too_big}");
        let too_much_stored =
            check_entry_limits("a", 1, crate::MAX_STORED_ENTRY_SIZE + 1, &mut total).unwrap_err();
        assert!(
            too_much_stored.to_string().contains("stored size"),
            "{too_much_stored}"
        );
        assert_eq!(total, 0, "弾いた分は合計に足さない");
    }
}
