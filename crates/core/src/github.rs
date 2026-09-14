//! GitHub OAuth Device Flow + Issue 投稿（REST）。
//!
//! デスクトップアプリはクライアントシークレットを安全に置けないため、
//! **Device Flow**（RFC 8628）だけを使う。`client_secret` は一切保持しない。
//!
//! - 端末フロー: `POST https://github.com/login/device/code` →
//!   `POST https://github.com/login/oauth/access_token`
//! - Issue 作成: `POST https://api.github.com/repos/{owner}/{repo}/issues`
//! - 画像添付: `POST https://uploads.github.com/user-attachments/assets`
//!   （対象リポジトリへの write 権限が必要。失敗時は本文のみで投稿する）

use crate::tbf::{RequestSpec, ResponseSpec, TbfError, Transport, UreqTransport};

pub const GITHUB_DEVICE_CODE_URL: &str = "https://github.com/login/device/code";
pub const GITHUB_TOKEN_URL: &str = "https://github.com/login/oauth/access_token";
pub const GITHUB_API_URL: &str = "https://api.github.com";
pub const GITHUB_UPLOAD_URL: &str = "https://uploads.github.com/user-attachments/assets";
/// ユーザーコードを入力するページ（アプリは既定ブラウザでここを開く）。
pub const GITHUB_DEVICE_VERIFY_URL: &str = "https://github.com/login/device";
/// Device Flow の grant type。
pub const DEVICE_GRANT_TYPE: &str = "urn:ietf:params:oauth:grant-type:device_code";
/// 既定スコープ。公開リポジトリへの Issue 作成と画像添付に必要な最小の権限を狙う。
pub const DEFAULT_SCOPE: &str = "public_repo";
/// GitHub は `User-Agent` の無いリクエストを 403 で拒否する。
pub const USER_AGENT: &str = concat!("thundoku-shelf/", env!("CARGO_PKG_VERSION"));

/// keyring に保存するアクセストークン。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GithubToken {
    pub access_token: String,
    pub token_type: String,
    /// 空白区切りのスコープ（レスポンスの `scope` をそのまま保持）。
    pub scope: String,
}

impl GithubToken {
    /// `scope` を個別のスコープに分解する。
    pub fn scopes(&self) -> Vec<&str> {
        self.scope.split_whitespace().collect()
    }
}

/// 端末フローの開始で得られるデバイスコード。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceCode {
    pub device_code: String,
    /// ユーザーがブラウザで入力するコード（例 `ABCD-1234`）。
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: u64,
    /// ポーリング間隔（秒）。`slow_down` を受けると 5 秒ずつ延びる。
    pub interval: u64,
}

/// 端末フローのポーリング結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeviceFlowPoll {
    /// まだユーザーが承認していない（`authorization_pending`）。
    Pending,
    /// ポーリングが早すぎる（`slow_down`）。次の間隔で待ち直す。
    SlowDown { interval: u64 },
    /// 承認されてトークンを得た。
    Authorized(GithubToken),
}

/// ログイン中のユーザー。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GithubUser {
    pub login: String,
    pub name: Option<String>,
}

/// 作成した Issue。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueRef {
    pub number: u64,
    pub html_url: String,
}

/// issue form（`.github/ISSUE_TEMPLATE/*.yml`）の入力項目の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TemplateFieldKind {
    Markdown,
    Textarea,
    Input,
    Dropdown,
    Checkboxes,
}

impl TemplateFieldKind {
    fn from_slug(slug: &str) -> Option<Self> {
        match slug {
            "markdown" => Some(Self::Markdown),
            "textarea" => Some(Self::Textarea),
            "input" => Some(Self::Input),
            "dropdown" => Some(Self::Dropdown),
            "checkboxes" => Some(Self::Checkboxes),
            _ => None,
        }
    }
}

/// issue form の 1 項目。レポート画面の入力欄のもとになる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplateField {
    pub kind: TemplateFieldKind,
    /// `attributes.label`（`markdown` は持たない）。
    pub label: Option<String>,
    pub description: Option<String>,
    pub placeholder: Option<String>,
    /// `markdown` の本文、または既定値。
    pub value: Option<String>,
    pub required: bool,
}

/// リポジトリの issue form。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueTemplate {
    pub file_name: String,
    pub name: String,
    pub description: Option<String>,
    /// 既定のタイトル（例 `[Bug] `）。
    pub title: Option<String>,
    pub labels: Vec<String>,
    pub fields: Vec<TemplateField>,
}

#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    #[error("network error: {0}")]
    Network(String),
    #[error("authorization failed: {0}")]
    Auth(String),
    #[error("device code expired: start over")]
    DeviceCodeExpired,
    #[error("authorization denied")]
    AccessDenied,
    #[error("not authorized")]
    NotAuthorized,
    #[error("permission denied: {0}")]
    Forbidden(String),
    #[error("issues are disabled on {0}")]
    IssuesDisabled(String),
    /// 画像アップロードは対象リポジトリへの write 権限が要る（無いと 404）。
    #[error("no write access to the repository: images cannot be attached")]
    AssetUploadDenied,
    #[error("not found: {0}")]
    NotFound(String),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

impl From<TbfError> for GithubError {
    fn from(value: TbfError) -> Self {
        GithubError::Network(value.to_string())
    }
}

/// `application/x-www-form-urlencoded` の percent-encode 集合
/// （`*` `-` `.` `_` はそのまま、空白は `+` にする）。
const FORM_ENCODE_SET: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
    .remove(b'*')
    .remove(b'-')
    .remove(b'.')
    .remove(b'_');

fn form_encode(value: &str) -> String {
    percent_encoding::utf8_percent_encode(value, FORM_ENCODE_SET)
        .to_string()
        .replace(' ', "+")
}

fn form_body(pairs: &[(&str, &str)]) -> Vec<u8> {
    pairs
        .iter()
        .map(|(key, value)| format!("{}={}", form_encode(key), form_encode(value)))
        .collect::<Vec<_>>()
        .join("&")
        .into_bytes()
}

fn json_value(response: &ResponseSpec) -> Result<serde_json::Value, GithubError> {
    serde_json::from_slice(&response.body).map_err(|error| {
        GithubError::InvalidResponse(format!("{error} (status {})", response.status))
    })
}

/// GitHub のエラーレスポンスから `message` を取り出す。
fn error_message(value: &serde_json::Value) -> String {
    value
        .get("message")
        .and_then(|message| message.as_str())
        .unwrap_or("unknown error")
        .to_string()
}

fn yaml_field<'a>(node: &'a yaml_rust2::Yaml, key: &str) -> Option<&'a yaml_rust2::Yaml> {
    match node {
        yaml_rust2::Yaml::Hash(map) => map.get(&yaml_rust2::Yaml::String(key.to_string())),
        _ => None,
    }
}

fn yaml_text(node: &yaml_rust2::Yaml) -> Option<String> {
    match node {
        yaml_rust2::Yaml::String(text) => Some(text.clone()),
        yaml_rust2::Yaml::Integer(number) => Some(number.to_string()),
        yaml_rust2::Yaml::Real(number) => Some(number.clone()),
        yaml_rust2::Yaml::Boolean(flag) => Some(flag.to_string()),
        _ => None,
    }
}

fn yaml_list(node: &yaml_rust2::Yaml) -> Vec<String> {
    match node {
        yaml_rust2::Yaml::Array(items) => items.iter().filter_map(yaml_text).collect(),
        _ => Vec::new(),
    }
}

/// issue form（`.github/ISSUE_TEMPLATE/*.yml`）を解析する。
/// `name` を持たないものはテンプレートではないので `None` を返す。
fn parse_issue_form(file_name: &str, text: &str) -> Result<Option<IssueTemplate>, GithubError> {
    let documents = yaml_rust2::YamlLoader::load_from_str(text)
        .map_err(|error| GithubError::InvalidResponse(format!("{file_name}: {error}")))?;
    let Some(document) = documents.first() else {
        return Ok(None);
    };
    let Some(name) = yaml_field(document, "name").and_then(yaml_text) else {
        return Ok(None);
    };

    let mut fields = Vec::new();
    if let Some(yaml_rust2::Yaml::Array(items)) = yaml_field(document, "body") {
        for item in items {
            let Some(kind) = yaml_field(item, "type")
                .and_then(yaml_text)
                .and_then(|slug| TemplateFieldKind::from_slug(&slug))
            else {
                continue;
            };
            let attributes = yaml_field(item, "attributes");
            let attribute = |key: &str| {
                attributes
                    .and_then(|node| yaml_field(node, key))
                    .and_then(yaml_text)
            };
            fields.push(TemplateField {
                kind,
                label: attribute("label"),
                description: attribute("description"),
                placeholder: attribute("placeholder"),
                value: attribute("value"),
                required: yaml_field(item, "validations")
                    .and_then(|node| yaml_field(node, "required"))
                    .is_some_and(|node| matches!(node, yaml_rust2::Yaml::Boolean(true))),
            });
        }
    }

    Ok(Some(IssueTemplate {
        file_name: file_name.to_string(),
        name,
        description: yaml_field(document, "description").and_then(yaml_text),
        title: yaml_field(document, "title").and_then(yaml_text),
        labels: yaml_field(document, "labels")
            .map(yaml_list)
            .unwrap_or_default(),
        fields,
    }))
}

/// GitHub の REST クライアント。
pub struct GithubClient {
    transport: Box<dyn Transport>,
    token: Option<GithubToken>,
}

impl Default for GithubClient {
    fn default() -> Self {
        Self::new()
    }
}

impl GithubClient {
    pub fn new() -> Self {
        Self::with_transport(Box::new(UreqTransport::new()), None)
    }

    pub fn with_transport(transport: Box<dyn Transport>, token: Option<GithubToken>) -> Self {
        Self { transport, token }
    }

    pub fn token(&self) -> Option<&GithubToken> {
        self.token.as_ref()
    }

    pub fn restore_token(&mut self, token: GithubToken) {
        self.token = Some(token);
    }

    pub fn clear_token(&mut self) {
        self.token = None;
    }

    pub fn is_authenticated(&self) -> bool {
        self.token.is_some()
    }

    fn send(&mut self, request: RequestSpec) -> Result<ResponseSpec, GithubError> {
        self.transport.send(request).map_err(GithubError::from)
    }

    /// 認証ヘッダ付きのリクエストヘッダを作る（未ログインなら `NotAuthorized`）。
    fn authorized_headers(&self, accept: &str) -> Result<Vec<(String, String)>, GithubError> {
        let token = self.token.as_ref().ok_or(GithubError::NotAuthorized)?;
        Ok(vec![
            ("Accept".to_string(), accept.to_string()),
            ("User-Agent".to_string(), USER_AGENT.to_string()),
            (
                "Authorization".to_string(),
                format!("Bearer {}", token.access_token),
            ),
        ])
    }

    /// 端末フローを開始し、ユーザーが入力するコードを得る。
    pub fn begin_device_flow(
        &mut self,
        client_id: &str,
        scope: &str,
    ) -> Result<DeviceCode, GithubError> {
        let response = self.send(RequestSpec {
            method: "POST".to_string(),
            url: GITHUB_DEVICE_CODE_URL.to_string(),
            headers: vec![
                ("Accept".to_string(), "application/json".to_string()),
                (
                    "Content-Type".to_string(),
                    "application/x-www-form-urlencoded".to_string(),
                ),
                ("User-Agent".to_string(), USER_AGENT.to_string()),
            ],
            body: Some(form_body(&[("client_id", client_id), ("scope", scope)])),
            redirects: 0,
        })?;
        let value = json_value(&response)?;
        let device_code = value
            .get("device_code")
            .and_then(|node| node.as_str())
            .ok_or_else(|| GithubError::Auth(error_message(&value)))?;
        Ok(DeviceCode {
            device_code: device_code.to_string(),
            user_code: value
                .get("user_code")
                .and_then(|node| node.as_str())
                .unwrap_or_default()
                .to_string(),
            verification_uri: value
                .get("verification_uri")
                .and_then(|node| node.as_str())
                .unwrap_or(GITHUB_DEVICE_VERIFY_URL)
                .to_string(),
            expires_in: value
                .get("expires_in")
                .and_then(|node| node.as_u64())
                .unwrap_or(900),
            interval: value
                .get("interval")
                .and_then(|node| node.as_u64())
                .unwrap_or(5),
        })
    }

    /// 端末フローを 1 回ポーリングする（呼び出し側が `interval` 秒待って繰り返す）。
    pub fn poll_device_flow(
        &mut self,
        client_id: &str,
        device_code: &str,
    ) -> Result<DeviceFlowPoll, GithubError> {
        let response = self.send(RequestSpec {
            method: "POST".to_string(),
            url: GITHUB_TOKEN_URL.to_string(),
            headers: vec![
                ("Accept".to_string(), "application/json".to_string()),
                (
                    "Content-Type".to_string(),
                    "application/x-www-form-urlencoded".to_string(),
                ),
                ("User-Agent".to_string(), USER_AGENT.to_string()),
            ],
            body: Some(form_body(&[
                ("client_id", client_id),
                ("device_code", device_code),
                ("grant_type", DEVICE_GRANT_TYPE),
            ])),
            redirects: 0,
        })?;
        let value = json_value(&response)?;

        if let Some(access_token) = value.get("access_token").and_then(|node| node.as_str()) {
            return Ok(DeviceFlowPoll::Authorized(GithubToken {
                access_token: access_token.to_string(),
                token_type: value
                    .get("token_type")
                    .and_then(|node| node.as_str())
                    .unwrap_or("bearer")
                    .to_string(),
                scope: value
                    .get("scope")
                    .and_then(|node| node.as_str())
                    .unwrap_or_default()
                    .to_string(),
            }));
        }

        match value.get("error").and_then(|node| node.as_str()) {
            Some("authorization_pending") => Ok(DeviceFlowPoll::Pending),
            // `slow_down` のたびに間隔が 5 秒延びる（GitHub が新しい interval を返す）。
            Some("slow_down") => Ok(DeviceFlowPoll::SlowDown {
                interval: value
                    .get("interval")
                    .and_then(|node| node.as_u64())
                    .unwrap_or(0),
            }),
            Some("expired_token") | Some("token_expired") => Err(GithubError::DeviceCodeExpired),
            Some("access_denied") => Err(GithubError::AccessDenied),
            other => Err(GithubError::Auth(
                other.unwrap_or(&error_message(&value)).to_string(),
            )),
        }
    }

    /// トークンに紐づくユーザーを取得する（本人確認とログイン表示名に使う）。
    pub fn current_user(&mut self) -> Result<GithubUser, GithubError> {
        let headers = self.authorized_headers("application/vnd.github+json")?;
        let response = self.send(RequestSpec {
            method: "GET".to_string(),
            url: format!("{GITHUB_API_URL}/user"),
            headers,
            body: None,
            redirects: 0,
        })?;
        let value = json_value(&response)?;
        let login = value
            .get("login")
            .and_then(|node| node.as_str())
            .ok_or_else(|| GithubError::Auth(error_message(&value)))?;
        Ok(GithubUser {
            login: login.to_string(),
            name: value
                .get("name")
                .and_then(|node| node.as_str())
                .map(String::from),
        })
    }

    /// リポジトリの数値 ID を取得する（画像アップロードの `repository_id` に必要）。
    pub fn repository_id(&mut self, owner: &str, repo: &str) -> Result<u64, GithubError> {
        let headers = self.authorized_headers("application/vnd.github+json")?;
        let response = self.send(RequestSpec {
            method: "GET".to_string(),
            url: format!("{GITHUB_API_URL}/repos/{owner}/{repo}"),
            headers,
            body: None,
            redirects: 0,
        })?;
        let value = json_value(&response)?;
        if response.status != 200 {
            return match response.status {
                404 => Err(GithubError::NotFound(format!("{owner}/{repo}"))),
                403 => Err(GithubError::Forbidden(error_message(&value))),
                status => Err(GithubError::InvalidResponse(format!(
                    "status {status}: {}",
                    error_message(&value)
                ))),
            };
        }
        value
            .get("id")
            .and_then(|node| node.as_u64())
            .ok_or_else(|| GithubError::InvalidResponse("repository id が無い".to_string()))
    }

    /// Issue を作成する。
    pub fn create_issue(
        &mut self,
        owner: &str,
        repo: &str,
        title: &str,
        body: &str,
        labels: &[String],
    ) -> Result<IssueRef, GithubError> {
        let mut headers = self.authorized_headers("application/vnd.github+json")?;
        headers.push(("Content-Type".to_string(), "application/json".to_string()));
        headers.push(("X-GitHub-Api-Version".to_string(), "2022-11-28".to_string()));
        let payload = serde_json::json!({ "title": title, "body": body, "labels": labels });
        let response = self.send(RequestSpec {
            method: "POST".to_string(),
            url: format!("{GITHUB_API_URL}/repos/{owner}/{repo}/issues"),
            headers,
            body: Some(payload.to_string().into_bytes()),
            redirects: 0,
        })?;

        let value = json_value(&response)?;
        match response.status {
            201 => Ok(IssueRef {
                number: value
                    .get("number")
                    .and_then(|node| node.as_u64())
                    .unwrap_or(0),
                html_url: value
                    .get("html_url")
                    .and_then(|node| node.as_str())
                    .unwrap_or_default()
                    .to_string(),
            }),
            403 => Err(GithubError::Forbidden(error_message(&value))),
            404 => Err(GithubError::NotFound(format!("{owner}/{repo}"))),
            410 => Err(GithubError::IssuesDisabled(format!("{owner}/{repo}"))),
            status => Err(GithubError::InvalidResponse(format!(
                "status {status}: {}",
                error_message(&value)
            ))),
        }
    }

    /// 画像を user-attachments へアップロードし、本文に貼る URL を返す。
    ///
    /// 対象リポジトリへの write 権限が無いと 404 が返る（GitHub の仕様）。
    pub fn upload_asset(
        &mut self,
        repo_id: u64,
        file_name: &str,
        content_type: &str,
        bytes: &[u8],
    ) -> Result<String, GithubError> {
        let headers = self.authorized_headers("application/vnd.github+json")?;
        let url = format!(
            "{GITHUB_UPLOAD_URL}?name={}&content_type={}&repository_id={repo_id}",
            form_encode(file_name),
            form_encode(content_type)
        );
        let mut headers = headers;
        headers.push((
            "Content-Type".to_string(),
            "application/octet-stream".to_string(),
        ));
        let response = self.send(RequestSpec {
            method: "POST".to_string(),
            url,
            headers,
            body: Some(bytes.to_vec()),
            redirects: 0,
        })?;

        let value = json_value(&response)?;
        match response.status {
            201 => Ok(value
                .get("url")
                .and_then(|node| node.as_str())
                .unwrap_or_default()
                .to_string()),
            // write 権限が無い場合の 404 は「添付できない」として区別する。
            404 => Err(GithubError::AssetUploadDenied),
            403 => Err(GithubError::Forbidden(error_message(&value))),
            status => Err(GithubError::InvalidResponse(format!(
                "status {status}: {}",
                error_message(&value)
            ))),
        }
    }

    /// `.github/ISSUE_TEMPLATE` の issue form を取得する（`config.yml` は除く）。
    pub fn list_issue_templates(
        &mut self,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<IssueTemplate>, GithubError> {
        let headers = self.authorized_headers("application/vnd.github+json")?;
        let response = self.send(RequestSpec {
            method: "GET".to_string(),
            url: format!("{GITHUB_API_URL}/repos/{owner}/{repo}/contents/.github/ISSUE_TEMPLATE"),
            headers,
            body: None,
            redirects: 0,
        })?;
        let value = json_value(&response)?;
        let entries = value.as_array().ok_or_else(|| {
            GithubError::InvalidResponse(format!(
                "status {}: {}",
                response.status,
                error_message(&value)
            ))
        })?;

        let files: Vec<String> = entries
            .iter()
            .filter(|entry| entry.get("type").and_then(|node| node.as_str()) == Some("file"))
            .filter_map(|entry| entry.get("name").and_then(|node| node.as_str()))
            .filter(|name| name.ends_with(".yml") && *name != "config.yml")
            .map(str::to_string)
            .collect();

        let mut templates = Vec::new();
        for file_name in files {
            let headers = self.authorized_headers("application/vnd.github.raw+json")?;
            let response = self.send(RequestSpec {
                method: "GET".to_string(),
                url: format!(
                    "{GITHUB_API_URL}/repos/{owner}/{repo}/contents/.github/ISSUE_TEMPLATE/{file_name}"
                ),
                headers,
                body: None,
                redirects: 0,
            })?;
            let text = String::from_utf8_lossy(&response.body).to_string();
            if let Some(template) = parse_issue_form(&file_name, &text)? {
                templates.push(template);
            }
        }
        Ok(templates)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tbf::{RequestSpec, ResponseSpec, TbfError, Transport};
    use parking_lot::Mutex;
    use std::sync::Arc;

    type Captured = Arc<Mutex<Vec<RequestSpec>>>;

    /// スクリプト順にレスポンスを返し、リクエストを記録するテスト用 transport。
    struct ScriptedTransport {
        responses: Vec<ResponseSpec>,
        captured: Captured,
    }

    impl Transport for ScriptedTransport {
        fn send(&mut self, req: RequestSpec) -> Result<ResponseSpec, TbfError> {
            self.captured.lock().push(req);
            if self.responses.is_empty() {
                return Err(TbfError::Network("no scripted response".to_string()));
            }
            Ok(self.responses.remove(0))
        }
    }

    fn json(status: u16, body: &str) -> ResponseSpec {
        ResponseSpec {
            status,
            headers: vec![("Content-Type".to_string(), "application/json".to_string())],
            body: body.as_bytes().to_vec(),
        }
    }

    fn raw(status: u16, body: &str) -> ResponseSpec {
        ResponseSpec {
            status,
            headers: Vec::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    fn client_with(responses: Vec<ResponseSpec>) -> (GithubClient, Captured) {
        let captured: Captured = Default::default();
        let client = GithubClient::with_transport(
            Box::new(ScriptedTransport {
                responses,
                captured: captured.clone(),
            }),
            None,
        );
        (client, captured)
    }

    fn logged_in(responses: Vec<ResponseSpec>) -> (GithubClient, Captured) {
        let (mut client, captured) = client_with(responses);
        client.restore_token(GithubToken {
            access_token: "tok".to_string(),
            token_type: "bearer".to_string(),
            scope: "public_repo".to_string(),
        });
        (client, captured)
    }

    fn header(req: &RequestSpec, name: &str) -> Option<String> {
        req.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    }

    fn first_request(captured: &Captured) -> RequestSpec {
        captured
            .lock()
            .first()
            .cloned()
            .expect("リクエストが送られていない")
    }

    fn body_text(req: &RequestSpec) -> String {
        String::from_utf8(req.body.clone().expect("body が無い")).unwrap()
    }

    #[test]
    fn begin_device_flow_asks_for_a_code_without_a_client_secret() {
        let (mut client, captured) = client_with(vec![json(
            200,
            r#"{"device_code":"dc","user_code":"ABCD-1234","verification_uri":"https://github.com/login/device","expires_in":900,"interval":5}"#,
        )]);

        let code = client
            .begin_device_flow("cid", DEFAULT_SCOPE)
            .expect("code");

        assert_eq!(code.device_code, "dc");
        assert_eq!(code.user_code, "ABCD-1234");
        assert_eq!(code.verification_uri, "https://github.com/login/device");
        assert_eq!(code.expires_in, 900);
        assert_eq!(code.interval, 5);

        let req = first_request(&captured);
        assert_eq!(req.method, "POST");
        assert_eq!(req.url, GITHUB_DEVICE_CODE_URL);
        assert_eq!(header(&req, "accept").as_deref(), Some("application/json"));
        let body = body_text(&req);
        assert!(body.contains("client_id=cid"), "{body}");
        assert!(body.contains("scope=public_repo"), "{body}");
        assert!(
            !body.contains("client_secret"),
            "Device Flow で client_secret を送ってはいけない: {body}"
        );
    }

    #[test]
    fn poll_device_flow_maps_pending_slow_down_and_success() {
        let (mut client, captured) = client_with(vec![
            json(200, r#"{"error":"authorization_pending"}"#),
            json(200, r#"{"error":"slow_down","interval":10}"#),
            json(
                200,
                r#"{"access_token":"tok","token_type":"bearer","scope":"public_repo"}"#,
            ),
        ]);

        assert_eq!(
            client.poll_device_flow("cid", "dc").expect("poll"),
            DeviceFlowPoll::Pending
        );
        assert_eq!(
            client.poll_device_flow("cid", "dc").expect("poll"),
            DeviceFlowPoll::SlowDown { interval: 10 }
        );
        match client.poll_device_flow("cid", "dc").expect("poll") {
            DeviceFlowPoll::Authorized(token) => {
                assert_eq!(token.access_token, "tok");
                assert_eq!(token.scopes(), vec!["public_repo"]);
            }
            other => panic!("トークンを期待: {other:?}"),
        }

        let req = first_request(&captured);
        assert_eq!(req.url, GITHUB_TOKEN_URL);
        let body = body_text(&req);
        assert!(body.contains("client_id=cid"), "{body}");
        assert!(body.contains("device_code=dc"), "{body}");
        assert!(body.contains("grant_type="), "{body}");
        assert!(
            !body.contains("client_secret"),
            "Device Flow で client_secret を送ってはいけない: {body}"
        );
    }

    #[test]
    fn poll_device_flow_reports_expired_and_denied() {
        let (mut client, _) = client_with(vec![
            json(200, r#"{"error":"expired_token"}"#),
            json(200, r#"{"error":"access_denied"}"#),
        ]);

        assert!(matches!(
            client.poll_device_flow("cid", "dc"),
            Err(GithubError::DeviceCodeExpired)
        ));
        assert!(matches!(
            client.poll_device_flow("cid", "dc"),
            Err(GithubError::AccessDenied)
        ));
    }

    #[test]
    fn repository_id_reads_the_numeric_id() {
        let (mut client, captured) =
            logged_in(vec![json(200, r#"{"id":1369852509,"full_name":"o/r"}"#)]);

        assert_eq!(client.repository_id("o", "r").expect("id"), 1369852509);

        let req = first_request(&captured);
        assert_eq!(req.method, "GET");
        assert_eq!(req.url, "https://api.github.com/repos/o/r");
    }

    #[test]
    fn create_issue_sends_the_app_user_agent_and_payload() {
        let (mut client, captured) = logged_in(vec![json(
            201,
            r#"{"number":7,"html_url":"https://github.com/o/r/issues/7"}"#,
        )]);

        let issue = client
            .create_issue("o", "r", "タイトル", "本文", &["bug".to_string()])
            .expect("issue");

        assert_eq!(issue.number, 7);
        assert_eq!(issue.html_url, "https://github.com/o/r/issues/7");

        let req = first_request(&captured);
        assert_eq!(req.method, "POST");
        assert_eq!(req.url, "https://api.github.com/repos/o/r/issues");
        let user_agent = header(&req, "user-agent").expect("User-Agent は必須");
        assert!(
            user_agent.starts_with("thundoku-shelf/"),
            "アプリを識別できる User-Agent を送る: {user_agent}"
        );
        assert_eq!(header(&req, "authorization").as_deref(), Some("Bearer tok"));
        assert_eq!(
            header(&req, "x-github-api-version").as_deref(),
            Some("2022-11-28")
        );
        let payload: serde_json::Value =
            serde_json::from_slice(&req.body.clone().unwrap()).unwrap();
        assert_eq!(payload["title"], "タイトル");
        assert_eq!(payload["body"], "本文");
        assert_eq!(payload["labels"][0], "bug");
    }

    #[test]
    fn create_issue_maps_forbidden_and_disabled_issues() {
        let (mut client, _) = logged_in(vec![
            json(403, r#"{"message":"Resource not accessible"}"#),
            json(410, r#"{"message":"Issues are disabled"}"#),
        ]);

        assert!(matches!(
            client.create_issue("o", "r", "t", "b", &[]),
            Err(GithubError::Forbidden(_))
        ));
        assert!(matches!(
            client.create_issue("o", "r", "t", "b", &[]),
            Err(GithubError::IssuesDisabled(_))
        ));
    }

    #[test]
    fn unauthenticated_calls_do_not_touch_the_network() {
        let (mut client, captured) = client_with(Vec::new());

        assert!(matches!(
            client.create_issue("o", "r", "t", "b", &[]),
            Err(GithubError::NotAuthorized)
        ));
        assert!(matches!(
            client.upload_asset(1, "a.png", "image/png", b"x"),
            Err(GithubError::NotAuthorized)
        ));
        assert!(
            captured.lock().is_empty(),
            "未ログインなのにリクエストを送っている"
        );
    }

    #[test]
    fn upload_asset_posts_octet_stream_and_returns_the_asset_url() {
        let (mut client, captured) = logged_in(vec![json(
            201,
            r#"{"url":"https://github.com/user-attachments/assets/abc"}"#,
        )]);

        let url = client
            .upload_asset(123456, "shot.png", "image/png", b"\x89PNG")
            .expect("upload");

        assert_eq!(url, "https://github.com/user-attachments/assets/abc");
        let req = first_request(&captured);
        assert_eq!(req.method, "POST");
        assert!(
            req.url
                .starts_with("https://uploads.github.com/user-attachments/assets?"),
            "{}",
            req.url
        );
        assert!(req.url.contains("repository_id=123456"), "{}", req.url);
        assert!(req.url.contains("name=shot.png"), "{}", req.url);
        assert!(req.url.contains("content_type=image%2Fpng"), "{}", req.url);
        assert_eq!(
            header(&req, "content-type").as_deref(),
            Some("application/octet-stream")
        );
        assert_eq!(req.body.clone().unwrap(), b"\x89PNG");
    }

    #[test]
    fn upload_asset_reports_missing_write_access_separately() {
        // gh CLI と同じく、write 権限が無い場合は 404 が返る
        let (mut client, _) = logged_in(vec![json(404, r#"{"message":"Not Found"}"#)]);

        assert!(matches!(
            client.upload_asset(1, "a.png", "image/png", b"x"),
            Err(GithubError::AssetUploadDenied)
        ));
    }

    #[test]
    fn list_issue_templates_parses_forms_and_skips_config() {
        let dir = r#"[
            {"name":"bug.yml","path":".github/ISSUE_TEMPLATE/bug.yml","type":"file"},
            {"name":"config.yml","path":".github/ISSUE_TEMPLATE/config.yml","type":"file"}
        ]"#;
        let bug = r#"name: 不具合報告
description: 動かないとき
title: "[Bug] "
labels: ["bug"]
body:
  - type: markdown
    attributes:
      value: |
        ありがとうございます。
  - type: textarea
    id: summary
    attributes:
      label: 概要
      placeholder: 何が起きた？
    validations:
      required: true
"#;
        // テンプレート取得は認証必須（private リポジトリでも動くように）
        let (mut client, captured) = logged_in(vec![json(200, dir), raw(200, bug)]);

        let templates = client.list_issue_templates("o", "r").expect("templates");

        assert_eq!(templates.len(), 1, "config.yml はテンプレートではない");
        let template = &templates[0];
        assert_eq!(template.file_name, "bug.yml");
        assert_eq!(template.name, "不具合報告");
        assert_eq!(template.description.as_deref(), Some("動かないとき"));
        assert_eq!(template.title.as_deref(), Some("[Bug] "));
        assert_eq!(template.labels, vec!["bug".to_string()]);
        assert_eq!(template.fields.len(), 2);
        assert_eq!(template.fields[0].kind, TemplateFieldKind::Markdown);
        assert_eq!(template.fields[1].kind, TemplateFieldKind::Textarea);
        assert_eq!(template.fields[1].label.as_deref(), Some("概要"));
        assert_eq!(
            template.fields[1].placeholder.as_deref(),
            Some("何が起きた？")
        );
        assert!(template.fields[1].required);

        let reqs = captured.lock().clone();
        assert_eq!(reqs.len(), 2, "ディレクトリ一覧 + 各フォームを取得する");
        assert_eq!(
            reqs[0].url,
            "https://api.github.com/repos/o/r/contents/.github/ISSUE_TEMPLATE"
        );
        assert_eq!(
            header(&reqs[1], "accept").as_deref(),
            Some("application/vnd.github.raw+json")
        );
    }
}
