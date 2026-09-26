//! Google Drive API client (files list/download, multipart upload, folder
//! create, delete) — the Rust equivalent of the Web `drive-client.ts`.
//! Scope: `drive.file` only (files the app created or the user picked; no appdata).

pub mod sync;

use serde_json::{Value, json};

use crate::tbf::{RequestSpec, ResponseSpec, TbfError, Transport};
use std::io::Read as _;
use std::io::Write as _;

const DRIVE_FILES_URL: &str = "https://www.googleapis.com/drive/v3/files";
const DRIVE_UPLOAD_URL: &str = "https://www.googleapis.com/upload/drive/v3/files";
const FIELDS: &str = "nextPageToken,files(id,name,size,md5Checksum,modifiedTime)";
const MULTIPART_BOUNDARY: &str = "thundoku_shelf_boundary";

/// resumable upload の 1 チャンクの大きさ（8 MiB）。
///
/// Google は 256 KiB の倍数を要求する（最後のチャンクだけ端数でよい）。
/// 8 MiB にすると、10 GiB の pack で 1280 回の `PUT` になる（1 回の失敗で
/// 巻き戻る範囲が小さく、進捗も細かく見える）。
pub const RESUMABLE_CHUNK_BYTES: u64 = 8 * 1024 * 1024;

/// Drive API の size フィールドをパースする（number と string の両対応）。
fn parse_size(value: &serde_json::Value) -> Option<i64> {
    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriveFile {
    pub id: String,
    pub name: String,
    pub size: Option<i64>,
    pub md5_checksum: Option<String>,
    pub modified_time: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum DriveError {
    #[error("network error: {0}")]
    Network(String),
    #[error("drive http {0}: {1}")]
    Http(u16, String),
    #[error("invalid drive response: {0}")]
    InvalidResponse(String),
    /// 進捗コールバックが中止を要求した（部分的な本文は取り込まない）。
    #[error("cancelled")]
    Cancelled,
    /// ファイルへの書き込み・読み出しに失敗した（大きい本文をディスクへ落とす経路）。
    #[error("io error: {0}")]
    Io(String),
}

impl From<TbfError> for DriveError {
    fn from(value: TbfError) -> Self {
        match value {
            TbfError::Cancelled => DriveError::Cancelled,
            other => DriveError::Network(other.to_string()),
        }
    }
}

pub trait DriveApi {
    fn list_files(&mut self, folder_id: &str) -> Result<Vec<DriveFile>, DriveError>;
    fn download(&mut self, file_id: &str) -> Result<Vec<u8>, DriveError>;

    /// 進捗つきの取得（pack / DB JSON のような**大きくなり得る本文**用）。
    ///
    /// 通常の API 応答は `Transport::send` の上限（16MiB）で打ち切られるが、この経路は
    /// ダウンロード用の上限（2GiB）と進捗・キャンセルを使う。既定実装は
    /// [`Self::download`] に委譲する（テストのモック用。本番は `DriveClient` が上書きする）。
    fn download_with_progress(
        &mut self,
        file_id: &str,
        on_progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<Vec<u8>, DriveError> {
        let bytes = self.download(file_id)?;
        let total = bytes.len() as u64;
        if !on_progress(total, total) {
            return Err(DriveError::Cancelled);
        }
        Ok(bytes)
    }

    /// 本文を**ファイルへ直接**書きながら取得する（大きい pack 用）。
    ///
    /// 既定実装はメモリに読んでから書く（テストのモック向け。本番は `DriveClient` が
    /// 上書きし、`send_download_to` でストリームする）。失敗したら**部分ファイルを残さない**。
    fn download_to_file(
        &mut self,
        file_id: &str,
        path: &std::path::Path,
        on_progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<u64, DriveError> {
        let bytes = self.download_with_progress(file_id, on_progress)?;
        if let Err(error) = std::fs::write(path, &bytes) {
            let _ = std::fs::remove_file(path);
            return Err(DriveError::Io(error.to_string()));
        }
        Ok(bytes.len() as u64)
    }

    /// Returns the new file id.
    fn upload_multipart(
        &mut self,
        name: &str,
        folder_id: &str,
        bytes: &[u8],
    ) -> Result<String, DriveError>;
    /// メモリ上のデータを multipart で上げ、**送れたバイト数**を通知する。
    ///
    /// 既定実装は進捗なしの [`Self::upload_multipart`] に委譲する（モック向け）。
    /// 本番は 1 チャンクずつ通知する。
    fn upload_multipart_with_progress(
        &mut self,
        name: &str,
        folder_id: &str,
        bytes: &[u8],
        _on_progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<String, DriveError> {
        self.upload_multipart(name, folder_id, bytes)
    }

    /// ファイルを**ストリームして**アップロードする（大きい pack 用。戻り値は新しいファイル id）。
    ///
    /// 既定実装はファイルを読んでから [`Self::upload_multipart`] に渡す（テストのモック向け。
    /// 本番は `DriveClient` が上書きし、本文を組み立てずに流す）。
    fn upload_multipart_from_file(
        &mut self,
        name: &str,
        folder_id: &str,
        path: &std::path::Path,
    ) -> Result<String, DriveError> {
        let bytes = std::fs::read(path).map_err(|error| DriveError::Io(error.to_string()))?;
        self.upload_multipart(name, folder_id, &bytes)
    }

    /// 大きいファイルを **resumable upload** で上げ、**送れたバイト数**を通知する。
    ///
    /// `on_progress(sent, total)` が `false` を返したら中止する（`DriveError::Cancelled`）。
    /// 既定実装は進捗なしの [`Self::upload_resumable_from_file`] に委譲する（モック向け）。
    fn upload_resumable_from_file_with_progress(
        &mut self,
        name: &str,
        folder_id: &str,
        path: &std::path::Path,
        _on_progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<String, DriveError> {
        self.upload_resumable_from_file(name, folder_id, path)
    }

    /// 大きいファイルを **resumable upload** で上げる（戻り値は新しいファイル id）。
    ///
    /// 既定実装は [`Self::upload_multipart_from_file`] に委譲する（テストのモック向け。
    /// 本番は `DriveClient` が上書きする）。
    fn upload_resumable_from_file(
        &mut self,
        name: &str,
        folder_id: &str,
        path: &std::path::Path,
    ) -> Result<String, DriveError> {
        self.upload_multipart_from_file(name, folder_id, path)
    }

    /// Returns the new folder id.
    fn create_folder(&mut self, name: &str) -> Result<String, DriveError>;
    fn delete(&mut self, file_id: &str) -> Result<(), DriveError>;
    /// ファイルの更新日時（modifiedTime）を現在時刻に更新する（PATCH）。
    fn touch(&mut self, file_id: &str) -> Result<(), DriveError>;
}

/// multipart アップロードの前後の境界（メタデータ部分と本体の間に挟む）。
fn multipart_wrapper(name: &str, folder_id: &str) -> (Vec<u8>, Vec<u8>) {
    let pack_id = name.strip_suffix(".opfspack").unwrap_or(name);
    let metadata = json!({
        "name": name,
        "parents": [folder_id],
        "mimeType": "application/octet-stream",
        "appProperties": { "app": "thundoku-shelf", "packId": pack_id },
    });
    let prefix = format!(
        "--{MULTIPART_BOUNDARY}\r\nContent-Type: application/json\r\n\r\n{}\r\n--{MULTIPART_BOUNDARY}\r\nContent-Type: application/octet-stream\r\n\r\n",
        serde_json::to_string(&metadata).expect("json metadata")
    );
    let suffix = format!("\r\n--{MULTIPART_BOUNDARY}--\r\n");
    (prefix.into_bytes(), suffix.into_bytes())
}

/// `308 Resume Incomplete` の `Range` から「受理済みのバイト数」を取り出す。
///
/// `Range` が無い 308 は進捗が分からないのでエラーにする（無限ループにしない）。
fn resumable_progress(response: &ResponseSpec, sent_offset: u64) -> Result<u64, DriveError> {
    let range = response
        .headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("range"))
        .map(|(_, value)| value.clone());
    let Some(range) = range else {
        return Err(DriveError::InvalidResponse(format!(
            "resumable 308 without Range (sent up to {sent_offset})"
        )));
    };
    let last = range
        .trim()
        .strip_prefix("bytes=0-")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .ok_or_else(|| DriveError::InvalidResponse(format!("unexpected Range: {range}")))?;
    Ok(last + 1)
}

/// アップロード応答（作成されたファイルの JSON）から id を取り出す。
fn parse_upload_response(response: &ResponseSpec) -> Result<String, DriveError> {
    if !(200..300).contains(&response.status) {
        return Err(DriveError::Http(
            response.status,
            String::from_utf8_lossy(&response.body).into_owned(),
        ));
    }
    let payload: Value = serde_json::from_slice(&response.body)
        .map_err(|e| DriveError::InvalidResponse(format!("invalid JSON: {e}")))?;
    payload
        .get("id")
        .and_then(Value::as_str)
        .map(String::from)
        .ok_or_else(|| DriveError::InvalidResponse("upload response missing id".into()))
}

pub struct DriveClient {
    transport: Box<dyn Transport>,
    access_token: String,
}

impl DriveClient {
    /// multipart の本文を**組み立てずに**送る（前後の境界 + `body`）。
    ///
    /// `Content-Length` は呼び出し側が正確な値を渡す（`send_stream_with_progress` が要る）。
    fn send_multipart(
        &mut self,
        _name: &str,
        _folder_id: &str,
        body: &mut dyn std::io::Read,
        content_length: u64,
        on_progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<String, DriveError> {
        let url = format!("{DRIVE_UPLOAD_URL}?uploadType=multipart");
        let response = self
            .transport
            .send_stream_with_progress(
                RequestSpec {
                    method: "POST".to_string(),
                    url,
                    headers: vec![
                        (
                            "Authorization".to_string(),
                            format!("Bearer {}", self.access_token),
                        ),
                        (
                            "Content-Type".to_string(),
                            format!("multipart/related; boundary={MULTIPART_BOUNDARY}"),
                        ),
                    ],
                    body: None,
                    redirects: 5,
                },
                body,
                content_length,
                on_progress,
            )
            .map_err(DriveError::from)?;
        parse_upload_response(&response)
    }

    /// ファイルを**ストリームして**アップロードする（pack 全体を RAM に載せない）。
    ///
    /// multipart の前後の境界だけメモリに持ち、本体は `File` から読んで
    /// `Transport::send_stream` で送る（`Content-Length` は前後 + ファイル長で正確に分かる）。
    pub fn new(transport: Box<dyn Transport>, access_token: impl Into<String>) -> Self {
        Self {
            transport,
            access_token: access_token.into(),
        }
    }

    fn request(
        &mut self,
        method: &str,
        url: &str,
        extra_headers: &[(&str, &str)],
        body: Option<Vec<u8>>,
        redirects: u32,
    ) -> Result<ResponseSpec, DriveError> {
        let mut headers: Vec<(String, String)> = vec![(
            "Authorization".into(),
            format!("Bearer {}", self.access_token),
        )];
        headers.extend(
            extra_headers
                .iter()
                .map(|(k, v)| ((*k).to_string(), (*v).to_string())),
        );
        self.transport
            .send(RequestSpec {
                method: method.to_string(),
                url: url.to_string(),
                headers,
                body,
                redirects,
            })
            .map_err(DriveError::from)
    }
}

impl DriveApi for DriveClient {
    fn list_files(&mut self, folder_id: &str) -> Result<Vec<DriveFile>, DriveError> {
        let mut files = Vec::new();
        let mut page_token: Option<String> = None;
        loop {
            let q = percent_encode(&format!("'{folder_id}' in parents and trashed=false"));
            let mut url =
                format!("{DRIVE_FILES_URL}?q={q}&fields={FIELDS}&pageSize=100&spaces=drive");
            if let Some(token) = &page_token {
                url.push_str(&format!("&pageToken={}", percent_encode(token)));
            }
            let response = self.request("GET", &url, &[], None, 5)?;
            if response.status == 401 || response.status == 403 {
                return Err(DriveError::Http(
                    response.status,
                    "authorization failed".into(),
                ));
            }
            if !(200..300).contains(&response.status) {
                return Err(DriveError::Http(
                    response.status,
                    String::from_utf8_lossy(&response.body).into_owned(),
                ));
            }
            let payload: Value = serde_json::from_slice(&response.body)
                .map_err(|e| DriveError::InvalidResponse(format!("invalid JSON: {e}")))?;
            for file in payload
                .pointer("/files")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                files.push(DriveFile {
                    id: file
                        .get("id")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    name: file
                        .get("name")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                    size: file.get("size").and_then(parse_size),
                    md5_checksum: file
                        .get("md5Checksum")
                        .and_then(Value::as_str)
                        .map(String::from),
                    modified_time: file
                        .get("modifiedTime")
                        .and_then(Value::as_str)
                        .map(String::from),
                });
            }
            page_token = payload
                .get("nextPageToken")
                .and_then(Value::as_str)
                .map(String::from);
            if page_token.is_none() {
                break;
            }
        }
        Ok(files)
    }

    fn download(&mut self, file_id: &str) -> Result<Vec<u8>, DriveError> {
        self.download_with_progress(file_id, &mut |_, _| true)
    }

    fn download_with_progress(
        &mut self,
        file_id: &str,
        on_progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<Vec<u8>, DriveError> {
        let url = format!("{DRIVE_FILES_URL}/{file_id}?alt=media");
        let headers = vec![(
            "Authorization".to_string(),
            format!("Bearer {}", self.access_token),
        )];
        // 通常 API（`send`）は応答本文を `MAX_API_BODY_BYTES`（16MiB）で打ち切るため、
        // pack や DB JSON は「保存できたのに戻せない」状態になっていた。取得は
        // ダウンロード用の経路（2GiB + 進捗 + キャンセル）を通す。
        let response = self
            .transport
            .send_download(
                RequestSpec {
                    method: "GET".to_string(),
                    url,
                    headers,
                    body: None,
                    redirects: 5,
                },
                on_progress,
            )
            .map_err(DriveError::from)?;
        if !(200..300).contains(&response.status) {
            return Err(DriveError::Http(
                response.status,
                String::from_utf8_lossy(&response.body).into_owned(),
            ));
        }
        Ok(response.body)
    }

    /// 本文を**ファイルへ直接**書きながら取得する（pack のような大きい本文用）。
    ///
    /// メモリには載せない（`BufWriter` で 8 KiB ずつ書く）ので、2GiB を超える pack でも
    /// 取得できる。失敗・中止・2xx 以外では**部分ファイルを消す**。
    fn download_to_file(
        &mut self,
        file_id: &str,
        path: &std::path::Path,
        on_progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<u64, DriveError> {
        let url = format!("{DRIVE_FILES_URL}/{file_id}?alt=media");
        let headers = vec![(
            "Authorization".to_string(),
            format!("Bearer {}", self.access_token),
        )];
        let file = match std::fs::File::create(path) {
            Ok(file) => file,
            Err(error) => {
                let _ = std::fs::remove_file(path);
                return Err(DriveError::Io(error.to_string()));
            }
        };
        let mut sink = std::io::BufWriter::new(file);
        let result = self.transport.send_download_to(
            RequestSpec {
                method: "GET".to_string(),
                url,
                headers,
                body: None,
                redirects: 5,
            },
            &mut sink,
            on_progress,
        );
        // ステータスを見る前に書き切る（エラーならこの後にファイルごと消す）。
        if let Err(error) = sink.flush() {
            let _ = std::fs::remove_file(path);
            return Err(DriveError::Io(error.to_string()));
        }
        drop(sink);
        match result {
            Ok(response) if (200..300).contains(&response.status) => Ok(response.bytes),
            Ok(response) => {
                let _ = std::fs::remove_file(path);
                Err(DriveError::Http(
                    response.status,
                    String::from_utf8_lossy(&response.error_body).into_owned(),
                ))
            }
            Err(error) => {
                let _ = std::fs::remove_file(path);
                Err(DriveError::from(error))
            }
        }
    }

    fn upload_multipart(
        &mut self,
        name: &str,
        folder_id: &str,
        bytes: &[u8],
    ) -> Result<String, DriveError> {
        self.upload_multipart_with_progress(name, folder_id, bytes, &mut |_, _| true)
    }

    fn upload_multipart_with_progress(
        &mut self,
        name: &str,
        folder_id: &str,
        bytes: &[u8],
        on_progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<String, DriveError> {
        let (prefix, suffix) = multipart_wrapper(name, folder_id);
        let mut body = Vec::with_capacity(bytes.len() + prefix.len() + suffix.len());
        body.extend_from_slice(&prefix);
        body.extend_from_slice(bytes);
        body.extend_from_slice(&suffix);
        let content_length = body.len() as u64;
        let mut reader = std::io::Cursor::new(body);
        self.send_multipart(name, folder_id, &mut reader, content_length, on_progress)
    }

    fn upload_multipart_from_file(
        &mut self,
        name: &str,
        folder_id: &str,
        path: &std::path::Path,
    ) -> Result<String, DriveError> {
        let size = std::fs::metadata(path)
            .map_err(|error| DriveError::Io(error.to_string()))?
            .len();
        let (prefix, suffix) = multipart_wrapper(name, folder_id);
        let file = std::fs::File::open(path).map_err(|error| DriveError::Io(error.to_string()))?;
        let content_length = prefix.len() as u64 + size + suffix.len() as u64;
        let mut body = std::io::Cursor::new(prefix)
            .chain(std::io::BufReader::new(file))
            .chain(std::io::Cursor::new(suffix));
        self.send_multipart(name, folder_id, &mut body, content_length, &mut |_, _| true)
    }

    /// 大きいファイルを **resumable upload** で上げる（pack 全体を RAM に載せない）。
    ///
    /// Google の手順: `uploadType=resumable` の POST でセッション URI（`Location`）を
    /// 得て、`Content-Range: bytes <start>-<end>/<total>` つきの `PUT` を
    /// [`RESUMABLE_CHUNK_BYTES`] ずつ繰り返す。途中は `308 Resume Incomplete` と
    /// `Range`（受理済みの範囲）が返るので**その位置から続ける**（送り直さない）。
    /// 再開は同じ呼び出しの中だけで行う（中断したら次の同期で最初から）。
    fn upload_resumable_from_file(
        &mut self,
        name: &str,
        folder_id: &str,
        path: &std::path::Path,
    ) -> Result<String, DriveError> {
        self.upload_resumable_from_file_with_progress(name, folder_id, path, &mut |_, _| true)
    }

    fn upload_resumable_from_file_with_progress(
        &mut self,
        name: &str,
        folder_id: &str,
        path: &std::path::Path,
        on_progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<String, DriveError> {
        let size = std::fs::metadata(path)
            .map_err(|error| DriveError::Io(error.to_string()))?
            .len();
        if size == 0 {
            // 0 バイトは `Content-Range` を作れないので通常の multipart で上げる。
            return self.upload_multipart_from_file(name, folder_id, path);
        }
        let size_header = size.to_string();
        // 1) セッション開始（メタデータだけ送る）。
        let session = {
            let pack_id = name.strip_suffix(".opfspack").unwrap_or(name);
            let metadata = json!({
                "name": name,
                "parents": [folder_id],
                "mimeType": "application/octet-stream",
                "appProperties": { "app": "thundoku-shelf", "packId": pack_id },
            });
            let response = self.request(
                "POST",
                &format!("{DRIVE_UPLOAD_URL}?uploadType=resumable"),
                &[
                    ("Content-Type", "application/json; charset=UTF-8"),
                    ("X-Upload-Content-Type", "application/octet-stream"),
                    ("X-Upload-Content-Length", size_header.as_str()),
                ],
                Some(serde_json::to_vec(&metadata).expect("json metadata")),
                0,
            )?;
            if !(200..300).contains(&response.status) {
                return Err(DriveError::Http(
                    response.status,
                    String::from_utf8_lossy(&response.body).into_owned(),
                ));
            }
            response
                .headers
                .iter()
                .find(|(header, _)| header.eq_ignore_ascii_case("location"))
                .map(|(_, value)| value.clone())
                .ok_or_else(|| {
                    DriveError::InvalidResponse("resumable session uri missing".into())
                })?
        };

        // 2) チャンク送信。308 が返ったら受理済みの位置から続ける。
        let mut offset = 0u64;
        // 進捗は**単調増加**で報告する（サーバーが一部しか受理しなかったときに
        // 巻き戻って見えないように、報告済みの最大値を覚えておく）。
        let mut reported = 0u64;
        while offset < size {
            let length = RESUMABLE_CHUNK_BYTES.min(size - offset);
            let mut file =
                std::fs::File::open(path).map_err(|error| DriveError::Io(error.to_string()))?;
            {
                use std::io::Seek as _;
                file.seek(std::io::SeekFrom::Start(offset))
                    .map_err(|error| DriveError::Io(error.to_string()))?;
            }
            let mut reader = std::io::BufReader::new(file).take(length);
            let response = self
                .transport
                .send_stream_with_progress(
                    RequestSpec {
                        method: "PUT".to_string(),
                        url: session.clone(),
                        headers: vec![
                            (
                                "Content-Range".to_string(),
                                format!("bytes {}-{}/{}", offset, offset + length - 1, size),
                            ),
                            (
                                "Content-Type".to_string(),
                                "application/octet-stream".to_string(),
                            ),
                        ],
                        body: None,
                        redirects: 0,
                    },
                    &mut reader,
                    length,
                    &mut |sent, _total| {
                        reported = reported.max(offset + sent);
                        on_progress(reported, size)
                    },
                )
                .map_err(DriveError::from)?;
            match response.status {
                200 | 201 => {
                    // 完了時は必ず 100% を報告する（最後のチャンクが短い場合の端数）。
                    on_progress(size, size);
                    return parse_upload_response(&response);
                }
                308 => {
                    let accepted = resumable_progress(&response, offset)?;
                    if accepted <= offset {
                        // 進まない 308 で回り続けない（無限ループにしない）。
                        return Err(DriveError::InvalidResponse(format!(
                            "resumable upload made no progress at {offset}"
                        )));
                    }
                    offset = accepted;
                }
                status => {
                    return Err(DriveError::Http(
                        status,
                        String::from_utf8_lossy(&response.body).into_owned(),
                    ));
                }
            }
        }
        Err(DriveError::InvalidResponse(
            "resumable upload finished without a response".into(),
        ))
    }

    fn create_folder(&mut self, name: &str) -> Result<String, DriveError> {
        let body = json!({ "name": name, "mimeType": "application/vnd.google-apps.folder" });
        let response = self.request(
            "POST",
            DRIVE_FILES_URL,
            &[("Content-Type", "application/json")],
            Some(serde_json::to_vec(&body).expect("json body")),
            5,
        )?;
        if !(200..300).contains(&response.status) {
            return Err(DriveError::Http(
                response.status,
                String::from_utf8_lossy(&response.body).into_owned(),
            ));
        }
        let payload: Value = serde_json::from_slice(&response.body)
            .map_err(|e| DriveError::InvalidResponse(format!("invalid JSON: {e}")))?;
        payload
            .get("id")
            .and_then(Value::as_str)
            .map(String::from)
            .ok_or_else(|| DriveError::InvalidResponse("folder response missing id".into()))
    }

    fn touch(&mut self, file_id: &str) -> Result<(), DriveError> {
        let url = format!("{DRIVE_FILES_URL}/{file_id}");
        let body = json!({ "modifiedTime": chrono::Utc::now().to_rfc3339() });
        let response = self.request(
            "PATCH",
            &url,
            &[("Content-Type", "application/json")],
            Some(serde_json::to_vec(&body).expect("json body")),
            5,
        )?;
        if !(200..300).contains(&response.status) {
            return Err(DriveError::Http(
                response.status,
                String::from_utf8_lossy(&response.body).into_owned(),
            ));
        }
        Ok(())
    }

    fn delete(&mut self, file_id: &str) -> Result<(), DriveError> {
        let url = format!("{DRIVE_FILES_URL}/{file_id}");
        let response = self.request("DELETE", &url, &[], None, 5)?;
        if !(200..300).contains(&response.status) {
            return Err(DriveError::Http(
                response.status,
                String::from_utf8_lossy(&response.body).into_owned(),
            ));
        }
        Ok(())
    }
}

fn percent_encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, percent_encoding::NON_ALPHANUMERIC).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::Arc;

    #[test]
    fn parse_size_accepts_number_and_string() {
        // Drive API は size を number で返すことも string で返すこともある
        assert_eq!(parse_size(&serde_json::json!(12345)), Some(12345));
        assert_eq!(parse_size(&serde_json::json!("12345")), Some(12345));
        assert_eq!(parse_size(&serde_json::json!("abc")), None);
        assert_eq!(parse_size(&serde_json::json!(null)), None);
    }

    /// 手元で **時間とピークメモリ**を測るための計測（CI では走らせない）。
    /// 実行: `cargo test -p thundoku-core --lib -- --ignored --nocapture measure_stream_upload`
    #[test]
    #[ignore]
    fn measure_stream_upload_memory_and_time() {
        struct Discard {
            bytes: Arc<Mutex<u64>>,
        }
        impl Transport for Discard {
            fn send(&mut self, spec: RequestSpec) -> Result<ResponseSpec, TbfError> {
                // メモリ版: 本文（Vec）はすでに組み立て済み。長さだけ数える。
                *self.bytes.lock() += spec.body.map(|b| b.len() as u64).unwrap_or(0);
                Ok(ResponseSpec {
                    status: 200,
                    headers: Vec::new(),
                    body: br#"{"id":"f"}"#.to_vec(),
                })
            }
            fn send_stream(
                &mut self,
                _spec: RequestSpec,
                body: &mut dyn std::io::Read,
                _length: u64,
            ) -> Result<ResponseSpec, TbfError> {
                let mut buf = vec![0u8; 256 * 1024];
                loop {
                    let read = body.read(&mut buf).expect("read");
                    if read == 0 {
                        break;
                    }
                    *self.bytes.lock() += read as u64;
                }
                Ok(ResponseSpec {
                    status: 200,
                    headers: Vec::new(),
                    body: br#"{"id":"f"}"#.to_vec(),
                })
            }
        }

        let size = 256 * 1024 * 1024u64; // 256MiB
        let dir = std::env::temp_dir().join("thundoku-upload-measure");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.opfspack");
        {
            let mut file = std::fs::File::create(&path).unwrap();
            let chunk = vec![9u8; 1024 * 1024];
            for _ in 0..(size / 1024 / 1024) {
                std::io::Write::write_all(&mut file, &chunk).unwrap();
            }
        }

        let bytes = Arc::new(Mutex::new(0u64));
        let mut client = DriveClient::new(
            Box::new(Discard {
                bytes: Arc::clone(&bytes),
            }),
            "token",
        );
        let started = std::time::Instant::now();
        client
            .upload_multipart_from_file("big.opfspack", "folder", &path)
            .expect("stream");
        let stream_time = started.elapsed();

        let mut client2 = DriveClient::new(
            Box::new(Discard {
                bytes: Arc::clone(&bytes),
            }),
            "token",
        );
        let payload = std::fs::read(&path).unwrap();
        let started = std::time::Instant::now();
        client2
            .upload_multipart("big.opfspack", "folder", &payload)
            .expect("memory");
        let memory_time = started.elapsed();

        // 実測は **時間**だけ（ピーク RSS は移植性が無いので測らない）。
        // メモリの常駐量はコードから決まる: メモリ版は payload 256MiB + 組み立てた
        // body 256MiB ≒ 512MiB、ストリーム版は 256KiB の読み取り + `BufReader` 64KiB
        // + 前後の境界だけで、本体サイズに依存しない。
        println!(
            "256MiB: stream={stream_time:?} (常駐 約 320KiB), memory={memory_time:?} (常駐 約 512MiB)"
        );
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 2 GiB を超える pack 用の **resumable upload** が Google の手順どおりに進むこと。
    ///
    /// ① `uploadType=resumable` の POST でセッション URI（`Location`）を得る
    /// ② `Content-Range: bytes <start>-<end>/<total>` つきの `PUT` を繰り返す
    /// ③ 途中は `308 Resume Incomplete` + `Range` が返る（その位置から続ける）
    /// ④ 最後は 200 + ファイル JSON（`id`）
    /// 送る本文は**ファイルの内容と一致**し、1 チャンクずつ読む（全体を載せない）。
    #[test]
    fn upload_resumable_from_file_follows_the_protocol() {
        /// (Content-Range, 本文) の記録。
        type ChunkLog = Arc<Mutex<Vec<(String, Vec<u8>)>>>;

        struct Resumable {
            /// 開始要求の本文（メタデータ JSON）。
            session_body: Arc<Mutex<Vec<u8>>>,
            /// 開始要求のヘッダー（`X-Upload-Content-Length` を見る）。
            session_headers: Arc<Mutex<Vec<(String, String)>>>,
            /// PUT ごとの (Content-Range, 本文)。
            chunks: ChunkLog,
        }
        impl Transport for Resumable {
            fn send(&mut self, spec: RequestSpec) -> Result<ResponseSpec, TbfError> {
                if spec.url.contains("uploadType=resumable") {
                    *self.session_body.lock() = spec.body.unwrap_or_default();
                    *self.session_headers.lock() = spec.headers.clone();
                    return Ok(ResponseSpec {
                        status: 200,
                        headers: vec![(
                            "Location".to_string(),
                            "https://upload.example/session-1".to_string(),
                        )],
                        body: Vec::new(),
                    });
                }
                panic!("セッション開始以外の send は使わない: {}", spec.url);
            }

            fn send_stream(
                &mut self,
                spec: RequestSpec,
                body: &mut dyn std::io::Read,
                _content_length: u64,
            ) -> Result<ResponseSpec, TbfError> {
                assert_eq!(spec.method, "PUT");
                assert_eq!(spec.url, "https://upload.example/session-1");
                let content_range = spec
                    .headers
                    .iter()
                    .find(|(name, _)| name == "Content-Range")
                    .map(|(_, value)| value.clone())
                    .expect("Content-Range が要る");
                let mut chunk = Vec::new();
                body.read_to_end(&mut chunk).expect("read chunk");
                let mut chunks = self.chunks.lock();
                chunks.push((content_range.clone(), chunk));
                let sent = chunks.len() as u64;
                // 最後のチャンクを除いて 308（受理済みの範囲を返す）。
                if sent < 3 {
                    let (_, end) = parse_content_range(&content_range);
                    return Ok(ResponseSpec {
                        status: 308,
                        headers: vec![("Range".to_string(), format!("bytes=0-{end}"))],
                        body: Vec::new(),
                    });
                }
                Ok(ResponseSpec {
                    status: 200,
                    headers: Vec::new(),
                    body: br#"{"id":"file-1"}"#.to_vec(),
                })
            }
        }

        /// `bytes <start>-<end>/<total>` から (start, end) を取り出す（テスト用）。
        fn parse_content_range(value: &str) -> (u64, u64) {
            let rest = value.trim_start_matches("bytes ");
            let (range, _total) = rest.split_once('/').expect("total");
            let (_start, end) = range.split_once('-').expect("start-end");
            (0, end.parse().unwrap())
        }

        let dir = std::env::temp_dir().join("thundoku-resumable-upload-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("big.opfspack");
        // 2.5 チャンク = 3 回の PUT（最後だけ端数）。
        let size = RESUMABLE_CHUNK_BYTES * 5 / 2;
        let payload: Vec<u8> = (0..size).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &payload).unwrap();

        let chunks: ChunkLog = Arc::new(Mutex::new(Vec::new()));
        let mut client = DriveClient::new(
            Box::new(Resumable {
                session_body: Arc::new(Mutex::new(Vec::new())),
                session_headers: Arc::new(Mutex::new(Vec::new())),
                chunks: Arc::clone(&chunks),
            }),
            "token",
        );
        // 進捗も同時に見る（送れたバイト数を単調に、最後は 100%）。
        let progress: Arc<Mutex<Vec<(u64, u64)>>> = Arc::new(Mutex::new(Vec::new()));
        let progress_for_callback = Arc::clone(&progress);
        let id = client
            .upload_resumable_from_file_with_progress(
                "big.opfspack",
                "folder-1",
                &path,
                &mut |sent, total| {
                    progress_for_callback.lock().push((sent, total));
                    true
                },
            )
            .expect("resumable で上げられる");
        assert_eq!(id, "file-1");
        let seen = progress.lock().clone();
        assert!(!seen.is_empty(), "進捗が報告される");
        assert_eq!(seen.last().copied(), Some((size, size)), "最後は 100%");
        assert!(
            seen.windows(2).all(|w| w[0].0 <= w[1].0),
            "単調に増える: {seen:?}"
        );

        let chunks = chunks.lock();
        assert_eq!(chunks.len(), 3, "1 チャンクずつ送る");
        // 送った本文の合計がファイルと一致する（順序も含めて）
        let sent: Vec<u8> = chunks.iter().flat_map(|(_, body)| body.clone()).collect();
        assert_eq!(sent, payload, "送った本文が一致しない");
        // Content-Range は連続していて、最後だけ端数
        assert_eq!(chunks[0].0, format!("bytes 0-{}/{}", RESUMABLE_CHUNK_BYTES - 1, size));
        assert_eq!(
            chunks[1].0,
            format!(
                "bytes {}-{}/{}",
                RESUMABLE_CHUNK_BYTES,
                RESUMABLE_CHUNK_BYTES * 2 - 1,
                size
            )
        );
        assert_eq!(
            chunks[2].0,
            format!("bytes {}-{}/{}", RESUMABLE_CHUNK_BYTES * 2, size - 1, size)
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 進まない `308` で回り続けない（無限ループにしない）。
    ///
    /// `Range: bytes=0-0`（1 バイトだけ受理）を返し続けるサーバーでも、
    /// 2 回目の応答で「進んでいない」と判断して止まること。
    #[test]
    fn upload_resumable_from_file_stops_when_the_server_makes_no_progress() {
        struct Stalled {
            puts: Arc<Mutex<usize>>,
        }
        impl Transport for Stalled {
            fn send(&mut self, _spec: RequestSpec) -> Result<ResponseSpec, TbfError> {
                Ok(ResponseSpec {
                    status: 200,
                    headers: vec![(
                        "Location".to_string(),
                        "https://upload.example/session-stalled".to_string(),
                    )],
                    body: Vec::new(),
                })
            }
            fn send_stream(
                &mut self,
                _spec: RequestSpec,
                body: &mut dyn std::io::Read,
                _content_length: u64,
            ) -> Result<ResponseSpec, TbfError> {
                *self.puts.lock() += 1;
                let mut chunk = Vec::new();
                body.read_to_end(&mut chunk).expect("read chunk");
                Ok(ResponseSpec {
                    status: 308,
                    headers: vec![("Range".to_string(), "bytes=0-0".to_string())],
                    body: Vec::new(),
                })
            }
        }

        let dir = std::env::temp_dir().join("thundoku-resumable-stalled-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("stalled.opfspack");
        std::fs::write(&path, vec![5u8; 1024]).unwrap();

        let puts = Arc::new(Mutex::new(0usize));
        let mut client = DriveClient::new(
            Box::new(Stalled {
                puts: Arc::clone(&puts),
            }),
            "token",
        );
        let error = client
            .upload_resumable_from_file("stalled.opfspack", "folder-1", &path)
            .expect_err("進まない応答はエラーにする");
        assert!(error.to_string().contains("no progress"), "{error}");
        assert!(*puts.lock() <= 2, "2 回で止まる（{} 回）", puts.lock());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// `upload_multipart_from_file` は **`send`（メモリ）を使わず** `send_stream` で
    /// 本文を流し、前後の境界 + ファイル + `Content-Length` がメモリ版と一致すること。
    #[test]
    fn upload_multipart_from_file_streams_the_same_body() {
        struct StreamingUpload {
            api_calls: Arc<Mutex<usize>>,
            body: Arc<Mutex<Vec<u8>>>,
            content_length: Arc<Mutex<u64>>,
        }
        impl Transport for StreamingUpload {
            fn send(&mut self, _spec: RequestSpec) -> Result<ResponseSpec, TbfError> {
                *self.api_calls.lock() += 1;
                Err(TbfError::Upstream(
                    "multipart は send_stream で送ること".into(),
                ))
            }

            fn send_stream(
                &mut self,
                _spec: RequestSpec,
                body: &mut dyn std::io::Read,
                content_length: u64,
            ) -> Result<ResponseSpec, TbfError> {
                let mut captured = Vec::new();
                body.read_to_end(&mut captured).expect("read body");
                *self.body.lock() = captured;
                *self.content_length.lock() = content_length;
                Ok(ResponseSpec {
                    status: 200,
                    headers: Vec::new(),
                    body: br#"{"id":"file-1"}"#.to_vec(),
                })
            }
        }

        // **2GiB 級の本体を載せない**ことが目的なので、ここでは小さなファイルで
        // 「流した本文がメモリ版と同一」を確かめる。
        let dir = std::env::temp_dir().join("thundoku-drive-stream-upload-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("sample.opfspack");
        let payload: Vec<u8> = (0..64 * 1024u32).map(|i| (i % 251) as u8).collect();
        std::fs::write(&path, &payload).unwrap();

        let api_calls = Arc::new(Mutex::new(0usize));
        let body_cell = Arc::new(Mutex::new(Vec::new()));
        let length_cell = Arc::new(Mutex::new(0u64));
        let transport = StreamingUpload {
            api_calls: Arc::clone(&api_calls),
            body: Arc::clone(&body_cell),
            content_length: Arc::clone(&length_cell),
        };
        let mut client = DriveClient::new(Box::new(transport), "token");
        let id = client
            .upload_multipart_from_file("book.opfspack", "folder-1", &path)
            .expect("upload");
        assert_eq!(id, "file-1");
        assert_eq!(*api_calls.lock(), 0, "send は使わない");

        let (prefix, suffix) = multipart_wrapper("book.opfspack", "folder-1");
        // multipart のメタデータ（名前・親・appProperties の packId）が入っている
        let prefix_text = String::from_utf8_lossy(&prefix);
        assert!(prefix_text.contains("\"name\":\"book.opfspack\""), "{prefix_text}");
        assert!(prefix_text.contains("\"packId\":\"book\""), "{prefix_text}");

        // **流した本文がメモリ版の組み立てとバイト一致**し、長さも申告どおり
        let mut expected = prefix.clone();
        expected.extend_from_slice(&payload);
        expected.extend_from_slice(&suffix);
        assert_eq!(*body_cell.lock(), expected, "ストリーム本文が一致しない");
        assert_eq!(
            *length_cell.lock(),
            expected.len() as u64,
            "Content-Length が本文長と一致しない"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// pack / DB JSON は 16MiB を超え得る。通常 API（`send`）は応答本文を
    /// `MAX_API_BODY_BYTES`（16MiB）で打ち切るため、取得は**ダウンロード用の経路**
    /// （2GiB + 進捗/キャンセル）を通すこと。
    #[test]
    fn download_uses_the_streaming_path_instead_of_the_api_cap() {
        struct StreamingOnly {
            api_calls: Arc<Mutex<usize>>,
        }
        impl Transport for StreamingOnly {
            fn send(&mut self, _spec: RequestSpec) -> Result<ResponseSpec, TbfError> {
                *self.api_calls.lock() += 1;
                Err(TbfError::Upstream(
                    "応答本文が上限（16777216 バイト）を超えました".into(),
                ))
            }

            fn send_download(
                &mut self,
                _spec: RequestSpec,
                on_progress: &mut dyn FnMut(u64, u64) -> bool,
            ) -> Result<ResponseSpec, TbfError> {
                let body = vec![7u8; 17 * 1024 * 1024];
                if !on_progress(body.len() as u64, body.len() as u64) {
                    return Err(TbfError::Cancelled);
                }
                Ok(ResponseSpec {
                    status: 200,
                    headers: Vec::new(),
                    body,
                })
            }
        }

        let api_calls = Arc::new(Mutex::new(0usize));
        let mut client = DriveClient::new(
            Box::new(StreamingOnly {
                api_calls: api_calls.clone(),
            }),
            "token",
        );
        let bytes = client
            .download("file-1")
            .expect("16MiB を超える pack が取得できるはず");
        assert_eq!(bytes.len(), 17 * 1024 * 1024);
        assert_eq!(
            *api_calls.lock(),
            0,
            "通常 API（16MiB 上限）の経路を使っている"
        );
    }

    /// 大きい本文は**ファイルへ直接**書きながら取得し、進捗も報告する。
    /// 中止・2xx 以外では**部分ファイルを残さない**（次回の取り込みを壊さない）。
    #[test]
    fn download_to_file_streams_chunks_and_cleans_up() {
        struct Chunked {
            chunks: Vec<Vec<u8>>,
            status: u16,
            seen: Arc<Mutex<Vec<(u64, u64)>>>,
        }
        impl Transport for Chunked {
            fn send(&mut self, _spec: RequestSpec) -> Result<ResponseSpec, TbfError> {
                Err(TbfError::Upstream("通常 API（16MiB 上限）は使わない".into()))
            }

            fn send_download_to(
                &mut self,
                _spec: RequestSpec,
                sink: &mut dyn std::io::Write,
                on_progress: &mut dyn FnMut(u64, u64) -> bool,
            ) -> Result<crate::tbf::transport::DownloadResult, TbfError> {
                if self.status != 200 {
                    return Ok(crate::tbf::transport::DownloadResult {
                        status: self.status,
                        headers: Vec::new(),
                        bytes: 0,
                        error_body: b"denied".to_vec(),
                    });
                }
                let total: u64 = self.chunks.iter().map(|chunk| chunk.len() as u64).sum();
                let mut written = 0u64;
                for chunk in &self.chunks {
                    if let Err(error) = sink.write_all(chunk) {
                        return Err(TbfError::Network(error.to_string()));
                    }
                    written += chunk.len() as u64;
                    self.seen.lock().push((written, total));
                    if !on_progress(written, total) {
                        return Err(TbfError::Cancelled);
                    }
                }
                Ok(crate::tbf::transport::DownloadResult {
                    status: 200,
                    headers: Vec::new(),
                    bytes: written,
                    error_body: Vec::new(),
                })
            }
        }

        let dir = std::env::temp_dir().join("thundoku-drive-file-test");
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("テンポラリを作れる");
        let path = dir.join("pack.opfspack");

        // 1) 正常: チャンクが順に書かれ、進捗が出る
        let seen = Arc::new(Mutex::new(Vec::new()));
        let mut client = DriveClient::new(
            Box::new(Chunked {
                chunks: vec![vec![1u8; 1024], vec![2u8; 2048]],
                status: 200,
                seen: seen.clone(),
            }),
            "token",
        );
        let bytes = client
            .download_to_file("file-1", &path, &mut |_, _| true)
            .expect("ファイルへ書ける");
        assert_eq!(bytes, 3072);
        assert_eq!(std::fs::read(&path).expect("読める").len(), 3072);
        assert!(seen.lock().len() >= 2, "チャンクごとに進捗を報告していない");

        // 2) 中止: 部分ファイルを残さない
        let mut client = DriveClient::new(
            Box::new(Chunked {
                chunks: vec![vec![3u8; 1024], vec![4u8; 1024]],
                status: 200,
                seen: Arc::new(Mutex::new(Vec::new())),
            }),
            "token",
        );
        let error = client
            .download_to_file("file-1", &path, &mut |_, _| false)
            .expect_err("中止が伝わっていない");
        assert!(matches!(error, DriveError::Cancelled), "{error:?}");
        assert!(!path.exists(), "中止したのに部分ファイルが残っている");

        // 3) 2xx 以外: 部分ファイルを残さず、エラー本文を返す
        let mut client = DriveClient::new(
            Box::new(Chunked {
                chunks: Vec::new(),
                status: 404,
                seen: Arc::new(Mutex::new(Vec::new())),
            }),
            "token",
        );
        match client
            .download_to_file("file-1", &path, &mut |_, _| true)
            .expect_err("エラーになるはず")
        {
            DriveError::Http(status, body) => {
                assert_eq!(status, 404);
                assert!(body.contains("denied"), "{body}");
            }
            other => panic!("{other:?}"),
        }
        assert!(!path.exists(), "失敗したのに部分ファイルが残っている");

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// 進捗コールバックが `false` を返したら中止する（部分的な本文を返さない）。
    #[test]
    fn download_with_progress_reports_cancellation() {
        struct FixedBody(Vec<u8>);
        impl Transport for FixedBody {
            fn send(&mut self, _spec: RequestSpec) -> Result<ResponseSpec, TbfError> {
                Ok(ResponseSpec {
                    status: 200,
                    headers: Vec::new(),
                    body: self.0.clone(),
                })
            }
        }

        let mut client = DriveClient::new(Box::new(FixedBody(vec![1u8; 1024])), "token");
        let error = client
            .download_with_progress("file-1", &mut |_, _| false)
            .expect_err("中止が伝わっていない");
        assert!(matches!(error, DriveError::Cancelled), "{error:?}");
    }
}
