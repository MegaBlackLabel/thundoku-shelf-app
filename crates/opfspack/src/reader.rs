//! Pack reading — validation order matches the TS `reader.ts`:
//! size >= 64 -> magic -> version -> header CRC -> index CRC -> entry bounds.

use crate::{
    FORMAT_VERSION, PackEntry, PackError, PackHeader, PackKey, entry_flags,
    format::{deserialize_header, deserialize_index_entry, index_entry_size},
};
use sha2::{Digest as _, Sha256};
use std::io::Read as _;

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
            header.flags,
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
    /// Same as [`Self::read_entry_with_key`] (kept because callers on both
    /// sides of the crate use either name).
    pub fn read_entry(&self, path: &str, key: Option<&PackKey>) -> Result<Vec<u8>, PackError> {
        self.read_entry_with_key(path, key)
    }

    /// Read an entry using the pack key derived from the account root key
    /// (callers cache it instead of re-running HKDF per page). `key` is only
    /// used for encrypted entries; a missing key is [`PackError::KeyRequired`].
    pub fn read_entry_with_key(
        &self,
        path: &str,
        key: Option<&PackKey>,
    ) -> Result<Vec<u8>, PackError> {
        let entry = self
            .entry(path)
            .ok_or_else(|| PackError::NotFound(path.to_owned()))?;
        if entry.flags & entry_flags::LZ4 != 0 {
            return Err(PackError::UnsupportedLz4);
        }
        let start = entry.offset as usize;
        let end = start
            .checked_add(entry.compressed_size as usize)
            .ok_or_else(|| {
                PackError::Corrupted(format!("entry offset overflow: {}", entry.path))
            })?;
        // 境界は `open` で検証済みだが、スライスで panic しないよう `get` で取る。
        let data = self
            .bytes
            .get(start..end)
            .ok_or_else(|| PackError::Corrupted(format!("entry out of bounds: {}", entry.path)))?
            .to_vec();
        decode_entry_payload(entry, data, key)
    }

    /// Read a byte range of the uncompressed, decrypted payload
    /// (validated against the original entry size).
    pub fn read_entry_range(
        &self,
        path: &str,
        start: u64,
        end: u64,
        key: Option<&PackKey>,
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
        let full = self.read_entry(path, key)?;
        // 宣言 `size` ではなく**実際に展開できた長さ**で最終確認する。
        // 宣言値を信じてスライスすると、細工した index で panic し得る。
        if end > full.len() as u64 {
            return Err(PackError::Corrupted(format!(
                "range {start}..{end} exceeds decoded {} bytes: {path}",
                full.len()
            )));
        }
        Ok(full[start as usize..end as usize].to_vec())
    }
}

fn raw_inflate(data: &[u8], expected_size: u64) -> Result<Vec<u8>, std::io::Error> {
    // 展開後の上限は index の `size`。宣言 +1 バイトまでしか読まないため、
    // 小さな DEFLATE 入力から数 GB を展開させる細工でもメモリを食い潰せない
    // （1 バイトでも超えたら即エラー）。
    let limit = expected_size
        .checked_add(1)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "size overflow"))?;
    let mut out = Vec::new();
    flate2::read::DeflateDecoder::new(data)
        .take(limit)
        .read_to_end(&mut out)?;
    if out.len() as u64 > expected_size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!(
                "inflated {} bytes exceeds declared {expected_size}",
                out.len()
            ),
        ));
    }
    Ok(out)
}

/// index エントリの最小整列サイズ（空パス・空 MIME の 44 バイトを 8 バイト境界へ
/// 切り上げ）。`entry_count` から確保する前に「index 長で説明できる件数か」を
/// 判定するために使う。
const MIN_INDEX_ENTRY_SIZE: usize = 48;

/// インデックス領域（CRC を含まない）をエントリ列へ解釈する。
/// 各エントリの格納領域が本体（ヘッダ〜インデックス）に収まっていることも検証する。
/// サイズ・件数の上限は `Vec` の確保や本体の読み出しより前に適用する。
fn parse_index_entries(
    index: &[u8],
    index_offset: u64,
    entry_count: u32,
    pack_flags: u32,
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
    // 索引領域そのものが大きい pack では上の検査をすり抜けるため、件数上限を別に持つ。
    if entry_count > crate::MAX_ENTRY_COUNT {
        return Err(PackError::Corrupted(format!(
            "entry_count {entry_count} exceeds limit {}",
            crate::MAX_ENTRY_COUNT
        )));
    }
    let mut entries = Vec::with_capacity(entry_count as usize);
    let mut pos = 0;
    let mut total_size: u64 = 0;
    for _ in 0..entry_count {
        if pos >= index.len() {
            return Err(PackError::Corrupted(format!(
                "index truncated: expected {entry_count} entries"
            )));
        }
        let entry: PackEntry = deserialize_index_entry(&index[pos..])?.into();
        pos += index_entry_size(&entry.path, &entry.mime_type);
        validate_entry(&entry, &mut total_size)?;
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
        entries.push(entry);
    }
    validate_pack_encryption(pack_flags, &entries)?;
    Ok(entries)
}

/// index が宣言するフラグとサイズを検査する。`size` / `compressed_size` は
/// 攻撃者が自由に書けるため、確保や読み出しの前に上限と突き合わせる。
fn validate_entry(entry: &PackEntry, total_size: &mut u64) -> Result<(), PackError> {
    // version 3 で `IDENTITY_BOUND`（v2 の identity 束縛）が立っていれば、
    // その pack は v3 として壊れている（鍵スケジュールは header version で
    // 選ぶので、フラグでは選ばない）。
    if entry.flags & !entry_flags::KNOWN_MASK != 0 {
        return Err(PackError::Corrupted(format!(
            "unknown entry flags {:#x}: {}",
            entry.flags, entry.path
        )));
    }
    if entry.size > crate::MAX_ENTRY_SIZE {
        return Err(PackError::Corrupted(format!(
            "entry size {} exceeds limit {}: {}",
            entry.size,
            crate::MAX_ENTRY_SIZE,
            entry.path
        )));
    }
    if entry.compressed_size > crate::MAX_STORED_ENTRY_SIZE {
        return Err(PackError::Corrupted(format!(
            "stored size {} exceeds limit {}: {}",
            entry.compressed_size,
            crate::MAX_STORED_ENTRY_SIZE,
            entry.path
        )));
    }
    *total_size = total_size
        .checked_add(entry.size)
        .ok_or_else(|| PackError::Corrupted(format!("total size overflow: {}", entry.path)))?;
    if *total_size > crate::MAX_TOTAL_SIZE {
        return Err(PackError::Corrupted(format!(
            "total size {} exceeds limit {}: {}",
            *total_size,
            crate::MAX_TOTAL_SIZE,
            entry.path
        )));
    }
    Ok(())
}

/// ヘッダの `ENCRYPTED` とエントリの暗号化フラグの整合を検査する。
/// builder は鍵を渡されたときに**全**エントリを暗号化するため、混在は破損。
fn validate_pack_encryption(pack_flags: u32, entries: &[PackEntry]) -> Result<(), PackError> {
    let header_encrypted = pack_flags & crate::pack_flags::ENCRYPTED != 0;
    let encrypted = |entry: &PackEntry| entry.flags & entry_flags::ENCRYPTED != 0;
    if header_encrypted && !entries.iter().all(encrypted) {
        return Err(PackError::Corrupted(
            "encrypted pack contains a plaintext entry".into(),
        ));
    }
    if !header_encrypted && entries.iter().any(encrypted) {
        return Err(PackError::Corrupted(
            "pack contains encrypted entries without the ENCRYPTED flag".into(),
        ));
    }
    Ok(())
}

/// 格納バイト列（AES-GCM 暗号文 / raw DEFLATE）を平文へ戻す。
fn decode_entry_payload(
    entry: &PackEntry,
    mut data: Vec<u8>,
    key: Option<&PackKey>,
) -> Result<Vec<u8>, PackError> {
    if entry.flags & entry_flags::ENCRYPTED != 0 {
        let key = key.ok_or_else(|| PackError::KeyRequired(entry.path.clone()))?;
        data = crate::crypto::decrypt(&data, &entry.iv, key.as_bytes())
            .map_err(|_| PackError::Corrupted(format!("decryption failed: {}", entry.path)))?;
    }
    if entry.flags & entry_flags::COMPRESSED != 0 {
        data = raw_inflate(&data, entry.size)
            .map_err(|e| PackError::Corrupted(format!("decompression failed: {e}")))?;
    }
    // 宣言 `size` と実長の一致は圧縮・非圧縮・暗号化の全経路で必須。
    // 欠くと、細工した index で `read_entry_range` のスライスが panic し得る。
    if data.len() as u64 != entry.size {
        return Err(PackError::Corrupted(format!(
            "entry size mismatch: {} declares {} bytes, decoded {}",
            entry.path,
            entry.size,
            data.len()
        )));
    }
    Ok(data)
}

fn io_error(error: std::io::Error) -> PackError {
    PackError::Io(error.to_string())
}

/// `offset` から `buf` を読む（**ファイル位置を動かさない**ので `&File` で読める）。
///
/// 位置読みにすることで `PackFileReader` の読み出しが `&self` になり、
/// `&dyn PackRead` でメモリ読みと同じように扱える（Mutex での直列化も不要）。
fn read_at(file: &std::fs::File, offset: u64, buf: &mut [u8]) -> Result<(), PackError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileExt as _;
        return file.read_exact_at(buf, offset).map_err(io_error);
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt as _;
        let mut filled = 0usize;
        while filled < buf.len() {
            let read = file
                .seek_read(&mut buf[filled..], offset + filled as u64)
                .map_err(io_error)?;
            if read == 0 {
                // 途中で EOF（他プロセスが切り詰めた等）
                return Err(io_error(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "unexpected end of pack",
                )));
            }
            filled += read;
        }
        Ok(())
    }
}

/// pack を読む側の共通 API。
///
/// メモリ上の [`PackReader`]（`&[u8]` を借りる）と、ファイル裏打ちの
/// [`PackFileReader`]（必要な範囲だけ読む）を**同じ経路で扱う**ための trait。
/// 取り込み・再構築・表紙・ビューアーはこれを使うことで、pack 全体を RAM に
/// 載せるかどうかを呼び出し側で選べる（大きい pack ではファイル裏打ちを選ぶ）。
pub trait PackRead {
    fn header(&self) -> &PackHeader;
    fn entries(&self) -> &[PackEntry];
    fn entry(&self, path: &str) -> Option<&PackEntry>;
    fn read_entry(&self, path: &str, key: Option<&PackKey>) -> Result<Vec<u8>, PackError>;
    fn read_entry_with_key(
        &self,
        path: &str,
        key: Option<&PackKey>,
    ) -> Result<Vec<u8>, PackError>;
    fn read_entry_range(
        &self,
        path: &str,
        start: u64,
        end: u64,
        key: Option<&PackKey>,
    ) -> Result<Vec<u8>, PackError>;

    /// **pack ファイル全体**の SHA-256（小文字 hex）。
    ///
    /// `document_images` の親（`imported_documents.file_hash`）に記録する値で、
    /// メモリ読みなら借りているバイト列、ファイル裏打ちなら**ファイルを順に読んで**
    /// 計算する（大きい pack でも全体を RAM に載せない）。
    fn source_sha256(&self) -> Result<String, PackError>;
}

impl PackRead for PackReader<'_> {
    fn header(&self) -> &PackHeader {
        PackReader::header(self)
    }
    fn entries(&self) -> &[PackEntry] {
        PackReader::entries(self)
    }
    fn entry(&self, path: &str) -> Option<&PackEntry> {
        PackReader::entry(self, path)
    }
    fn read_entry(&self, path: &str, key: Option<&PackKey>) -> Result<Vec<u8>, PackError> {
        PackReader::read_entry(self, path, key)
    }
    fn read_entry_with_key(
        &self,
        path: &str,
        key: Option<&PackKey>,
    ) -> Result<Vec<u8>, PackError> {
        PackReader::read_entry_with_key(self, path, key)
    }
    fn read_entry_range(
        &self,
        path: &str,
        start: u64,
        end: u64,
        key: Option<&PackKey>,
    ) -> Result<Vec<u8>, PackError> {
        PackReader::read_entry_range(self, path, start, end, key)
    }

    fn source_sha256(&self) -> Result<String, PackError> {
        Ok(hex_sha256(&Sha256::digest(self.bytes)))
    }
}

impl PackRead for PackFileReader {
    fn header(&self) -> &PackHeader {
        PackFileReader::header(self)
    }
    fn entries(&self) -> &[PackEntry] {
        PackFileReader::entries(self)
    }
    fn entry(&self, path: &str) -> Option<&PackEntry> {
        PackFileReader::entry(self, path)
    }
    fn read_entry(&self, path: &str, key: Option<&PackKey>) -> Result<Vec<u8>, PackError> {
        PackFileReader::read_entry(self, path, key)
    }
    fn read_entry_with_key(
        &self,
        path: &str,
        key: Option<&PackKey>,
    ) -> Result<Vec<u8>, PackError> {
        PackFileReader::read_entry_with_key(self, path, key)
    }
    fn read_entry_range(
        &self,
        path: &str,
        start: u64,
        end: u64,
        key: Option<&PackKey>,
    ) -> Result<Vec<u8>, PackError> {
        PackFileReader::read_entry_range(self, path, start, end, key)
    }

    /// ファイルを 1 MiB ずつ順に読んでハッシュする（全体を RAM に載せない）。
    fn source_sha256(&self) -> Result<String, PackError> {
        let len = self.file.metadata().map_err(io_error)?.len();
        let mut hasher = Sha256::new();
        let mut buf = vec![0u8; 1024 * 1024];
        let mut offset = 0u64;
        while offset < len {
            let chunk = ((len - offset) as usize).min(buf.len());
            read_at(&self.file, offset, &mut buf[..chunk])?;
            hasher.update(&buf[..chunk]);
            offset += chunk as u64;
        }
        Ok(hex_sha256(&hasher.finalize()))
    }
}

/// SHA-256 の結果を小文字 hex にする（`core` の `sha256_hex` と同じ表現）。
fn hex_sha256(digest: &[u8]) -> String {
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
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
        let file = std::fs::File::open(path).map_err(io_error)?;
        let len = file.metadata().map_err(io_error)?.len();
        if len < crate::HEADER_SIZE as u64 {
            return Err(PackError::Corrupted(format!("pack too small: {len} bytes")));
        }

        let mut header_bytes = [0u8; crate::HEADER_SIZE];
        read_at(&file, 0, &mut header_bytes)?;
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
        read_at(&file, index_offset, &mut index)?;
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
            header.flags,
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
    pub fn read_entry(&self, path: &str, key: Option<&PackKey>) -> Result<Vec<u8>, PackError> {
        self.read_entry_with_key(path, key)
    }

    /// エントリの格納バイト列だけを読んで復号する（本体全体は読まない）。
    pub fn read_entry_with_key(
        &self,
        path: &str,
        key: Option<&PackKey>,
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
        read_at(&self.file, entry.offset, &mut data)?;
        decode_entry_payload(&entry, data, key)
    }

    /// エントリの一部だけを復号して返す（`PackReader` と同じ意味）。
    pub fn read_entry_range(
        &self,
        path: &str,
        start: u64,
        end: u64,
        key: Option<&PackKey>,
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
        let full = self.read_entry_with_key(path, key)?;
        // 宣言 `size` ではなく**実際に展開できた長さ**で最終確認する（細工した index 対策）。
        if end > full.len() as u64 {
            return Err(PackError::Corrupted(format!(
                "range {start}..{end} exceeds decoded {} bytes: {path}",
                full.len()
            )));
        }
        Ok(full[start as usize..end as usize].to_vec())
    }
}
