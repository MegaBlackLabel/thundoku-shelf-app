//! Google Drive API client (files list/download, multipart upload, folder
//! create, delete) — the Rust equivalent of the Web `drive-client.ts`.
//! Scope: `drive.file` only (files the app created or the user picked; no appdata).

pub mod sync;

use serde_json::{Value, json};

use crate::tbf::{RequestSpec, ResponseSpec, TbfError, Transport};

const DRIVE_FILES_URL: &str = "https://www.googleapis.com/drive/v3/files";
const DRIVE_UPLOAD_URL: &str = "https://www.googleapis.com/upload/drive/v3/files";
const FIELDS: &str = "nextPageToken,files(id,name,size,md5Checksum,modifiedTime)";
const MULTIPART_BOUNDARY: &str = "thundoku_shelf_boundary";

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

    /// Returns the new file id.
    fn upload_multipart(
        &mut self,
        name: &str,
        folder_id: &str,
        bytes: &[u8],
    ) -> Result<String, DriveError>;
    /// Returns the new folder id.
    fn create_folder(&mut self, name: &str) -> Result<String, DriveError>;
    fn delete(&mut self, file_id: &str) -> Result<(), DriveError>;
    /// ファイルの更新日時（modifiedTime）を現在時刻に更新する（PATCH）。
    fn touch(&mut self, file_id: &str) -> Result<(), DriveError>;
}

pub struct DriveClient {
    transport: Box<dyn Transport>,
    access_token: String,
}

impl DriveClient {
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

    fn upload_multipart(
        &mut self,
        name: &str,
        folder_id: &str,
        bytes: &[u8],
    ) -> Result<String, DriveError> {
        let pack_id = name.strip_suffix(".opfspack").unwrap_or(name);
        let metadata = json!({
            "name": name,
            "parents": [folder_id],
            "mimeType": "application/octet-stream",
            "appProperties": { "app": "thundoku-shelf", "packId": pack_id },
        });
        let mut body = Vec::with_capacity(bytes.len() + 512);
        body.extend_from_slice(
            format!("--{MULTIPART_BOUNDARY}\r\nContent-Type: application/json\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(&serde_json::to_vec(&metadata).expect("json metadata"));
        body.extend_from_slice(
            format!("\r\n--{MULTIPART_BOUNDARY}\r\nContent-Type: application/octet-stream\r\n\r\n")
                .as_bytes(),
        );
        body.extend_from_slice(bytes);
        body.extend_from_slice(format!("\r\n--{MULTIPART_BOUNDARY}--\r\n").as_bytes());

        let url = format!("{DRIVE_UPLOAD_URL}?uploadType=multipart");
        let response = self.request(
            "POST",
            &url,
            &[(
                "Content-Type",
                &format!("multipart/related; boundary={MULTIPART_BOUNDARY}"),
            )],
            Some(body),
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
            .ok_or_else(|| DriveError::InvalidResponse("upload response missing id".into()))
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
