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
}

impl From<TbfError> for DriveError {
    fn from(value: TbfError) -> Self {
        DriveError::Network(value.to_string())
    }
}

pub trait DriveApi {
    fn list_files(&mut self, folder_id: &str) -> Result<Vec<DriveFile>, DriveError>;
    fn download(&mut self, file_id: &str) -> Result<Vec<u8>, DriveError>;
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
        let url = format!("{DRIVE_FILES_URL}/{file_id}?alt=media");
        let response = self.request("GET", &url, &[], None, 5)?;
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

    #[test]
    fn parse_size_accepts_number_and_string() {
        // Drive API は size を number で返すことも string で返すこともある
        assert_eq!(parse_size(&serde_json::json!(12345)), Some(12345));
        assert_eq!(parse_size(&serde_json::json!("12345")), Some(12345));
        assert_eq!(parse_size(&serde_json::json!("abc")), None);
        assert_eq!(parse_size(&serde_json::json!(null)), None);
    }
}
