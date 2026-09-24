//! GitHub OAuth Device Flow + Issue 投稿（REST）。
//!
//! デスクトップアプリはクライアントシークレットを安全に置けないため、
//! **Device Flow**（RFC 8628）だけを使う。`client_secret` は一切保持しない。
//!
//! - 端末フロー: `POST https://github.com/login/device/code` →
//!   `POST https://github.com/login/oauth/access_token`
//! - Issue 作成: `POST https://api.github.com/repos/{owner}/{repo}/issues`
//! - 画像添付: `POST https://uploads.github.com/user-attachments/assets`
//!   （対象リポジトリへの write 権限が必要）
//!
//! レポートは [`GithubClient::submit_report`] が「リポジトリ ID → 添付 →
//! Issue」の順で送る。画像は選択しただけでは上げず（[`read_attachment`]）、
//! 利用者が送信を押したときに初めて外部へ出る。アップロードに失敗したら Issue は
//! 作らない（fail-closed）。

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

/// 取り込むテンプレート 1 ファイルの上限。issue form は数 KB で足りる。
/// 外部から来るデータなので、大きすぎるものは読まない（メモリ枯渇を防ぐ）。
const MAX_TEMPLATE_BYTES: usize = 256 * 1024;

/// 取り込むテンプレートの最大件数（転送量の上限）。
const MAX_TEMPLATE_FILES: usize = 20;

/// 添付できる画像の拡張子（`rfd` のフィルタと MIME 判定で同じ並びを使う）。
pub const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

/// 添付できる画像 1 枚の上限サイズ。GitHub 側の上限（10MB）に合わせて手前で弾く
/// （送信までローカルに持つので、読む前に大きさを見る）。
pub const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;

/// keyring に保存するアクセストークン。
#[derive(Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GithubToken {
    pub access_token: String,
    pub token_type: String,
    /// 空白区切りのスコープ（レスポンスの `scope` をそのまま保持）。
    pub scope: String,
}

impl std::fmt::Debug for GithubToken {
    /// `access_token` は出さない。`{:?}` を 1 回書いただけでトークンがログに載る
    /// 事故を防ぐ（フィールドを増やしたときも同様に隠すこと）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GithubToken")
            .field("access_token", &"***")
            .field("token_type", &self.token_type)
            .field("scope", &self.scope)
            .finish()
    }
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

/// Issue に添付する画像 1 件。
///
/// 実体は**送信するときまで GitHub へ上げない**（選択しただけで
/// user-attachments へ送らない）。`url` は一度アップロードできたら保持し、
/// 再試行で上げ直さない（失敗のたびに同じ画像が増えるのを防ぐ）。
pub struct IssueAttachment {
    /// 選択したファイルの名前（表示と alt に使う）。
    pub file_name: String,
    /// アップロード時に送る MIME（拡張子から決める）。
    pub content_type: String,
    /// 画像の実体。
    pub bytes: Vec<u8>,
    /// アップロードできた URL（`None` は未アップロード）。
    pub url: Option<String>,
}

impl std::fmt::Debug for IssueAttachment {
    /// `bytes` は出さない。画像 1 枚を丸ごとログに載せない（トークンと同じ理由）。
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IssueAttachment")
            .field("file_name", &self.file_name)
            .field("content_type", &self.content_type)
            .field("bytes", &format_args!("{} バイト", self.bytes.len()))
            .field("url", &self.url)
            .finish()
    }
}

impl IssueAttachment {
    /// ファイル名から MIME を決めて添付を作る（未アップロードの状態）。
    pub fn new(file_name: String, bytes: Vec<u8>) -> Self {
        Self {
            content_type: image_content_type(&file_name).to_string(),
            file_name,
            bytes,
            url: None,
        }
    }
}

/// 選択された画像をローカルに読み、送信まで保持する添付を作る。
///
/// **HTTP はしない**（読むだけ）。大きすぎるファイルは読む前に弾く（実体を
/// そのままメモリに載せるため）。`Err` はそのまま画面に出せる日本語。
pub fn read_attachment(path: &std::path::Path, max_bytes: u64) -> Result<IssueAttachment, String> {
    let file_name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .ok_or_else(|| format!("ファイル名が取得できません: {}", path.display()))?;
    // metadata が取れないときは read のエラーにする（存在しないなどを 1 本にまとめる）。
    if let Ok(metadata) = std::fs::metadata(path)
        && metadata.len() > max_bytes
    {
        return Err(format!(
            "{file_name} は大きすぎます（上限 {}MB）",
            max_bytes / (1024 * 1024)
        ));
    }
    let bytes =
        std::fs::read(path).map_err(|error| format!("{file_name} を読めませんでした: {error}"))?;
    Ok(IssueAttachment::new(file_name, bytes))
}

/// 拡張子から画像の MIME を決める（`IMAGE_EXTENSIONS` 以外は PNG として送る）。
fn image_content_type(file_name: &str) -> &'static str {
    match std::path::Path::new(file_name)
        .extension()
        .and_then(|extension| extension.to_str())
    {
        Some(extension)
            if extension.eq_ignore_ascii_case("jpg") || extension.eq_ignore_ascii_case("jpeg") =>
        {
            "image/jpeg"
        }
        Some(extension) if extension.eq_ignore_ascii_case("gif") => "image/gif",
        Some(extension) if extension.eq_ignore_ascii_case("webp") => "image/webp",
        _ => "image/png",
    }
}

/// 本文に貼る alt テキストを作る。
///
/// ファイル名はローカル FS 由来の任意文字列で、`]` `)` などが入ると Markdown の
/// リンク構造を壊し、外部 URL を本文に紛れ込ませられる（テンプレート文面を
/// そのまま流し込むのと同じ理由で、そのままは使わない）。
fn markdown_alt(file_name: &str) -> String {
    let cleaned: String = file_name
        .chars()
        .filter(|c| !matches!(c, '[' | ']' | '(' | ')' | '\\' | '\n' | '\r'))
        .take(80)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "画像".to_string()
    } else {
        cleaned.to_string()
    }
}

/// 本文の末尾へ、アップロードできた添付を選択順に `![alt](url)` として足す。
fn body_with_attachments(body: &str, attachments: &[IssueAttachment]) -> String {
    let mut text = body.trim_end().to_string();
    for attachment in attachments {
        // まだ上がっていない添付は貼らない（壊れたリンクを本文に残さない）。
        let Some(url) = attachment.url.as_deref() else {
            continue;
        };
        if !text.is_empty() {
            text.push_str("\n\n");
        }
        text.push_str(&format!(
            "![{}]({url})",
            markdown_alt(&attachment.file_name)
        ));
    }
    text
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
    /// 回数制限に当たった（`Retry-After` 秒待って再試行する）。
    #[error("rate limited (retry after {retry_after:?}s)")]
    RateLimited { retry_after: Option<u64> },
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

/// エラー時の本文は JSON とは限らない（プロキシや攻撃的な中間装置が HTML を返す）。
/// status で分岐する前に parse で落ちないよう、読めなければ `Null` として扱う。
fn json_or_null(response: &ResponseSpec) -> serde_json::Value {
    serde_json::from_slice(&response.body).unwrap_or(serde_json::Value::Null)
}

/// GitHub のエラーレスポンスから理由を取り出す。
///
/// 通常の API は `message`、OAuth（device flow）は `error` / `error_description` を返す。
fn error_message(value: &serde_json::Value) -> String {
    ["message", "error_description", "error"]
        .iter()
        .find_map(|key| value.get(key).and_then(|node| node.as_str()))
        .unwrap_or("unknown error")
        .to_string()
}

/// `Retry-After` ヘッダ（秒）。回数制限の待ち時間を利用者に見せるために使う。
fn retry_after(response: &ResponseSpec) -> Option<u64> {
    response
        .headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("retry-after"))
        .and_then(|(_, value)| value.trim().parse::<u64>().ok())
}

/// 回数制限（429、または `Retry-After` 付きの 403）なら対応するエラーを返す。
///
/// GitHub は二次レート制限を 403 で返すことがあり、`Retry-After` が付く。
fn rate_limited(response: &ResponseSpec) -> Option<GithubError> {
    let retry = retry_after(response);
    if response.status == 429 || (response.status == 403 && retry.is_some()) {
        Some(GithubError::RateLimited { retry_after: retry })
    } else {
        None
    }
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
    if text.len() > MAX_TEMPLATE_BYTES {
        return Err(GithubError::InvalidResponse(format!(
            "{file_name}: テンプレートが大きすぎます（{} バイト）",
            text.len()
        )));
    }
    // 注: yaml-rust2 はアンカー/エイリアスを上限なく展開するため、alias 爆弾で
    // メモリを食い潰せる。いまの取得元はこのアプリのリポジトリ固定（＝自分たちが
    // 書いた YAML）なので信用しているが、取得元を可変にするならノード数に予算を
    // 設けるパーサへ替えること。
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
        if response.status != 200 {
            return Err(GithubError::Auth(format!(
                "status {}: {}",
                response.status,
                error_message(&json_or_null(&response))
            )));
        }
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
        if response.status != 200 {
            return Err(GithubError::Auth(format!(
                "status {}: {}",
                response.status,
                error_message(&json_or_null(&response))
            )));
        }
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
        if response.status != 200 {
            return Err(GithubError::Auth(format!(
                "status {}: {}",
                response.status,
                error_message(&json_or_null(&response))
            )));
        }
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
        if let Some(error) = rate_limited(&response) {
            return Err(error);
        }
        if response.status != 200 {
            return match response.status {
                404 => Err(GithubError::NotFound(format!("{owner}/{repo}"))),
                403 => Err(GithubError::Forbidden(error_message(&json_or_null(
                    &response,
                )))),
                // 失効したトークンは呼び出し側でログインをやり直させる。
                401 => Err(GithubError::Auth(error_message(&json_or_null(&response)))),
                status => Err(GithubError::InvalidResponse(format!(
                    "status {status}: {}",
                    error_message(&json_or_null(&response))
                ))),
            };
        }
        let value = json_value(&response)?;
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

        if let Some(error) = rate_limited(&response) {
            return Err(error);
        }

        match response.status {
            // 成功したときだけ JSON として読む（エラー本文は JSON とは限らない）。
            201 => {
                let value = json_value(&response)?;
                Ok(IssueRef {
                    number: value
                        .get("number")
                        .and_then(|node| node.as_u64())
                        .unwrap_or(0),
                    html_url: value
                        .get("html_url")
                        .and_then(|node| node.as_str())
                        .unwrap_or_default()
                        .to_string(),
                })
            }
            403 => Err(GithubError::Forbidden(error_message(&json_or_null(
                &response,
            )))),
            404 => Err(GithubError::NotFound(format!("{owner}/{repo}"))),
            410 => Err(GithubError::IssuesDisabled(format!("{owner}/{repo}"))),
            // 401 はトークンが失効/取り消しされている。呼び出し側がログインを
            // やり直せるように `Auth` として返す（「予期しない応答」にしない）。
            401 => Err(GithubError::Auth(error_message(&json_or_null(&response)))),
            status => Err(GithubError::InvalidResponse(format!(
                "status {status}: {}",
                error_message(&json_or_null(&response))
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

        if let Some(error) = rate_limited(&response) {
            return Err(error);
        }

        match response.status {
            201 => {
                let value = json_value(&response)?;
                let url = value
                    .get("url")
                    .and_then(|node| node.as_str())
                    .unwrap_or_default();
                // この URL はそのまま Issue 本文へ貼る。期待する形でなければ貼らない
                // （空文字を「添付できた」として扱わない）。
                if !url.starts_with("https://github.com/user-attachments/assets/") {
                    return Err(GithubError::InvalidResponse(format!(
                        "画像の URL が不正です: {url}"
                    )));
                }
                Ok(url.to_string())
            }
            // write 権限が無い場合の 404 は「添付できない」として区別する。
            404 => Err(GithubError::AssetUploadDenied),
            403 => Err(GithubError::Forbidden(error_message(&json_or_null(
                &response,
            )))),
            // 失効したトークンは呼び出し側でログインをやり直させる。
            401 => Err(GithubError::Auth(error_message(&json_or_null(&response)))),
            status => Err(GithubError::InvalidResponse(format!(
                "status {status}: {}",
                error_message(&json_or_null(&response))
            ))),
        }
    }

    /// レポートを送る: リポジトリ ID → 添付のアップロード → Issue 作成。
    ///
    /// 添付は**この時点で初めて** GitHub へ上げる（選択しただけでは送らない）。
    /// 途中で失敗したら Issue は作らない（利用者が押していない本文だけの Issue を
    /// 勝手に立てない）。既にアップロードできた添付は `url` を残すので、再試行で
    /// 上げ直さない（同じ画像が user-attachments に増えない）。
    pub fn submit_report(
        &mut self,
        owner: &str,
        repo: &str,
        title: &str,
        body: &str,
        labels: &[String],
        attachments: &mut [IssueAttachment],
    ) -> Result<IssueRef, GithubError> {
        // 上げるものがあるときだけ repository_id を取る（添付ごとに取り直さない。
        // 添付が無い送信に余計な要求を足さない）。
        if attachments
            .iter()
            .any(|attachment| attachment.url.is_none())
        {
            let repo_id = self.repository_id(owner, repo)?;
            for attachment in attachments.iter_mut() {
                if attachment.url.is_some() {
                    continue;
                }
                let url = self.upload_asset(
                    repo_id,
                    &attachment.file_name,
                    &attachment.content_type,
                    &attachment.bytes,
                )?;
                attachment.url = Some(url);
            }
        }
        let body = body_with_attachments(body, attachments);
        self.create_issue(owner, repo, title, &body, labels)
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
        // テンプレートを置いていないリポジトリは 404 になる。異常ではないので空で返す。
        if response.status == 404 {
            return Ok(Vec::new());
        }
        // 失効したトークンは呼び出し側でログインをやり直させる。
        if response.status == 401 {
            return Err(GithubError::Auth(error_message(&json_or_null(&response))));
        }
        if let Some(error) = rate_limited(&response) {
            return Err(error);
        }
        if response.status != 200 {
            return Err(GithubError::InvalidResponse(format!(
                "status {}: {}",
                response.status,
                error_message(&json_or_null(&response))
            )));
        }
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
            .take(MAX_TEMPLATE_FILES)
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
            // 1 件読めなくても他は活かす（テンプレートは本文の下書き用の補助で、
            // 1 ファイルの失敗で全部を失う理由が無い）。大きすぎる本文は読まない。
            if response.status != 200 || response.body.len() > MAX_TEMPLATE_BYTES {
                continue;
            }
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

    /// テスト用の一時ディレクトリ（画像の読み込みを確かめるためだけに使う）。
    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir =
            std::env::temp_dir().join(format!("thundoku-attach-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// まだアップロードしていない添付（`url` は未確定）。
    fn attachment(file_name: &str) -> IssueAttachment {
        IssueAttachment::new(file_name.to_string(), b"\x89PNG".to_vec())
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

    #[test]
    fn list_issue_templates_returns_empty_when_the_repository_has_none() {
        // テンプレートを置いていないリポジトリは Contents API が 404 を返す。異常ではない。
        let (mut client, captured) = logged_in(vec![json(404, r#"{"message":"Not Found"}"#)]);

        let templates = client.list_issue_templates("o", "r").expect("templates");

        assert!(templates.is_empty());
        assert_eq!(
            captured.lock().len(),
            1,
            "テンプレートが無いならディレクトリ一覧で終わる"
        );
    }

    #[test]
    fn upload_asset_rejects_an_unexpected_url() {
        // 応答の url はそのまま本文へ貼るので、期待する形でなければ成功にしない。
        let (mut client, _) = logged_in(vec![json(
            201,
            r#"{"url":"https://evil.example/asset.png"}"#,
        )]);

        assert!(matches!(
            client.upload_asset(1, "a.png", "image/png", b"x"),
            Err(GithubError::InvalidResponse(_))
        ));
    }

    #[test]
    fn token_debug_does_not_print_the_access_token() {
        let token = GithubToken {
            access_token: "gho_supersecret".to_string(),
            token_type: "bearer".to_string(),
            scope: "public_repo".to_string(),
        };

        let printed = format!("{token:?}");

        assert!(!printed.contains("gho_supersecret"), "{printed}");
        assert!(printed.contains("public_repo"), "{printed}");
    }

    #[test]
    fn rate_limit_reports_the_waiting_time() {
        // 429 と、Retry-After 付きの 403（二次レート制限）を回数制限として扱う。
        let mut throttled = json(429, r#"{"message":"API rate limit exceeded"}"#);
        throttled
            .headers
            .push(("Retry-After".to_string(), "60".to_string()));
        let mut secondary = json(
            403,
            r#"{"message":"You have exceeded a secondary rate limit"}"#,
        );
        secondary
            .headers
            .push(("Retry-After".to_string(), "30".to_string()));
        let (mut client, _) = logged_in(vec![throttled, secondary]);

        assert!(matches!(
            client.create_issue("o", "r", "t", "b", &[]),
            Err(GithubError::RateLimited {
                retry_after: Some(60)
            })
        ));
        assert!(matches!(
            client.create_issue("o", "r", "t", "b", &[]),
            Err(GithubError::RateLimited {
                retry_after: Some(30)
            })
        ));
    }

    #[test]
    fn expired_token_is_reported_as_an_auth_error() {
        // 401（トークンの失効・取り消し）は「予期しない応答」ではなく認証エラーとして
        // 返す。呼び出し側が再ログインへ誘導できるようにするため。
        let (mut client, _) = logged_in(vec![
            json(401, r#"{"message":"Bad credentials"}"#),
            json(401, r#"{"message":"Bad credentials"}"#),
            json(401, r#"{"message":"Bad credentials"}"#),
        ]);

        assert!(matches!(
            client.create_issue("o", "r", "t", "b", &[]),
            Err(GithubError::Auth(_))
        ));
        assert!(matches!(
            client.repository_id("o", "r"),
            Err(GithubError::Auth(_))
        ));
        assert!(matches!(
            client.list_issue_templates("o", "r"),
            Err(GithubError::Auth(_))
        ));
    }

    #[test]
    fn create_issue_keeps_the_status_when_the_error_body_is_not_json() {
        // プロキシなどが HTML を返しても、status から種別を判定できること。
        let (mut client, _) = logged_in(vec![raw(403, "<html>Forbidden</html>")]);

        assert!(matches!(
            client.create_issue("o", "r", "t", "b", &[]),
            Err(GithubError::Forbidden(_))
        ));
    }

    /// 拡張子から MIME を決めること（未知の拡張子は PNG として送る）。
    #[test]
    fn attachment_content_type_follows_the_extension() {
        let cases = [
            ("shot.png", "image/png"),
            ("shot.PNG", "image/png"),
            ("shot.jpg", "image/jpeg"),
            ("shot.jpeg", "image/jpeg"),
            ("anime.gif", "image/gif"),
            ("shot.webp", "image/webp"),
            // 受け付けない拡張子は既定（PNG）として送る
            ("shot.bmp", "image/png"),
            ("shot", "image/png"),
        ];

        for (file_name, expected) in cases {
            assert_eq!(attachment(file_name).content_type, expected, "{file_name}");
        }
    }

    /// ファイル名をそのまま alt に使わないこと（Markdown のリンク構造を壊せると、
    /// 本文へ外部 URL を紛れ込ませられる）。
    #[test]
    fn markdown_alt_strips_link_breaking_characters() {
        assert_eq!(markdown_alt("shot [1](x).png"), "shot 1x.png");
        // 改行も落とす（本文の構造を崩す）。
        assert_eq!(markdown_alt("a\nb]c.png"), "abc.png");
        // 何も残らないときは既定の文言を使う（空の `![]()` を作らない）。
        assert_eq!(markdown_alt("[]()"), "画像");
        // 長すぎる名前は alt を膨らませるだけなので切る。
        assert_eq!(markdown_alt(&"あ".repeat(100)).chars().count(), 80);
    }

    /// 本文の末尾へ、アップロードできた添付だけを選択順に足すこと。
    #[test]
    fn body_with_attachments_appends_only_uploaded_images() {
        let mut uploaded = attachment("a.png");
        uploaded.url = Some("https://github.com/user-attachments/assets/1".to_string());
        let pending = attachment("b.png");

        assert_eq!(
            body_with_attachments("## 概要\n\n", &[uploaded, pending]),
            "## 概要\n\n![a.png](https://github.com/user-attachments/assets/1)"
        );

        // 本文が空なら画像だけになる。
        let mut only = attachment("c.png");
        only.url = Some("https://github.com/user-attachments/assets/3".to_string());
        assert_eq!(
            body_with_attachments("", &[only]),
            "![c.png](https://github.com/user-attachments/assets/3)"
        );
    }

    /// 選択はローカルに読むだけであること。上限を超えるファイルも読めないファイルも
    /// 同じで、この段階では 1 件も送らない（送信は `submit_report`）。
    #[test]
    fn read_attachment_stays_local_and_rejects_oversized_or_unreadable_files() {
        // モックの transport を用意して、選択〜読み込みで 1 件も送らないことを見る
        // （応答を 1 つも積まないので、送れば必ず失敗する）。
        let (_client, captured) = logged_in(Vec::new());
        let dir = temp_dir("read");
        // 中身は書かずサイズだけ作る（読み込む前に弾くことを確かめるテストなので）。
        let huge = dir.join("huge.png");
        std::fs::File::create(&huge)
            .unwrap()
            .set_len(MAX_IMAGE_BYTES + 1)
            .unwrap();

        let error = read_attachment(&huge, MAX_IMAGE_BYTES).expect_err("上限を超える");
        assert!(error.contains("huge.png"), "{error}");
        assert!(error.contains("大きすぎます"), "{error}");

        let missing = dir.join("missing.png");
        let error = read_attachment(&missing, MAX_IMAGE_BYTES).expect_err("読めない");
        assert!(error.contains("missing.png"), "{error}");

        let path = dir.join("shot.jpg");
        std::fs::write(&path, b"\xff\xd8\xff").unwrap();
        let attachment = read_attachment(&path, MAX_IMAGE_BYTES).expect("読める");
        assert_eq!(attachment.file_name, "shot.jpg");
        assert_eq!(attachment.content_type, "image/jpeg");
        assert_eq!(attachment.bytes, b"\xff\xd8\xff");
        assert!(
            attachment.url.is_none(),
            "選択しただけでアップロードしている"
        );
        // キャンセル（`rfd` が `None` を返す）もアプリ側で終わり、ここへ来ない。
        // サイズ超過・読込失敗・読めた場合のどれでも要求は 1 件も出ない。
        let requests = captured.lock().clone();
        assert!(
            requests.is_empty(),
            "画像の選択で HTTP を出している: {requests:?}"
        );
    }

    /// 選択しただけでは送らず、送信時に選択順で 1 件ずつアップロードすること。
    #[test]
    fn submit_report_uploads_selected_images_in_order_at_submit_time() {
        let dir = temp_dir("submit");
        let first = dir.join("shot [1].png");
        // Windows で使える文字だけで、Markdown のリンク構造を壊せる名前を作る
        // （`:` や `/` はファイル名に使えない）。
        let second = dir.join("evil](x).png");
        std::fs::write(&first, b"\x89PNG-first").unwrap();
        std::fs::write(&second, b"\x89PNG-second").unwrap();

        let (mut client, captured) = logged_in(vec![
            json(200, r#"{"id":42,"full_name":"o/r"}"#),
            json(
                201,
                r#"{"url":"https://github.com/user-attachments/assets/1"}"#,
            ),
            json(
                201,
                r#"{"url":"https://github.com/user-attachments/assets/2"}"#,
            ),
            json(
                201,
                r#"{"number":7,"html_url":"https://github.com/o/r/issues/7"}"#,
            ),
        ]);

        let mut attachments = vec![
            read_attachment(&first, MAX_IMAGE_BYTES).expect("1 件目"),
            read_attachment(&second, MAX_IMAGE_BYTES).expect("2 件目"),
        ];
        // 選択しただけの時点では 1 件も送っていない（送信を押すまで外部へ出さない）。
        assert!(captured.lock().is_empty(), "選択だけで HTTP を出している");
        // 本文に貼るのは送信時なので、この時点の本文は素のまま。
        assert_eq!(body_with_attachments("## 概要", &attachments), "## 概要");

        let issue = client
            .submit_report("o", "r", "タイトル", "## 概要", &[], &mut attachments)
            .expect("submit");
        assert_eq!(issue.number, 7);

        let reqs = captured.lock().clone();
        assert_eq!(reqs.len(), 4, "要求の数が違う");
        // repository_id は 1 回だけ取る（添付ごとに取り直さない）。
        assert_eq!(reqs[0].url, "https://api.github.com/repos/o/r");
        // 添付は選択順（中身で見分ける）。
        assert!(
            reqs[1].url.starts_with(GITHUB_UPLOAD_URL),
            "{}",
            reqs[1].url
        );
        assert!(reqs[1].url.contains("repository_id=42"), "{}", reqs[1].url);
        assert_eq!(reqs[1].body.clone().unwrap(), b"\x89PNG-first");
        assert_eq!(reqs[2].body.clone().unwrap(), b"\x89PNG-second");
        assert_eq!(reqs[3].url, "https://api.github.com/repos/o/r/issues");

        let payload: serde_json::Value =
            serde_json::from_slice(&reqs[3].body.clone().unwrap()).unwrap();
        let body = payload["body"].as_str().unwrap();
        assert!(body.starts_with("## 概要\n\n"), "{body}");
        let first_at = body.find("assets/1").expect("1 件目の URL が本文に無い");
        let second_at = body.find("assets/2").expect("2 件目の URL が本文に無い");
        assert!(first_at < second_at, "選択順に入っていない: {body}");
        assert!(
            body.contains("![shot 1.png](https://github.com/user-attachments/assets/1)"),
            "{body}"
        );
        assert!(
            body.contains("![evilx.png](https://github.com/user-attachments/assets/2)"),
            "{body}"
        );
        assert!(
            !body.contains("](x)"),
            "ファイル名で Markdown を壊せている: {body}"
        );
    }

    /// 2 件目のアップロードに失敗したら Issue は作らず、1 件目の URL は保持して
    /// 再試行で上げ直さないこと。
    #[test]
    fn submit_report_is_fail_closed_and_does_not_reupload_after_a_failure() {
        let mut attachments = vec![attachment("a.png"), attachment("b.png")];
        let (mut client, captured) = logged_in(vec![
            json(200, r#"{"id":42}"#),
            json(
                201,
                r#"{"url":"https://github.com/user-attachments/assets/1"}"#,
            ),
            json(500, r#"{"message":"boom"}"#),
        ]);

        let error = client
            .submit_report("o", "r", "タイトル", "本文", &[], &mut attachments)
            .expect_err("2 件目は失敗する");
        assert!(
            matches!(error, GithubError::InvalidResponse(_)),
            "{error:?}"
        );

        let reqs = captured.lock().clone();
        assert_eq!(
            reqs.len(),
            3,
            "アップロードの途中で止まっていない: {reqs:?}"
        );
        assert!(
            !reqs.iter().any(|req| req.url.ends_with("/issues")),
            "アップロードに失敗したのに Issue を作っている"
        );
        assert_eq!(
            attachments[0].url.as_deref(),
            Some("https://github.com/user-attachments/assets/1"),
            "上げられた分の URL を捨てている"
        );
        assert!(attachments[1].url.is_none());

        // 再試行: 未アップロードの 2 件目だけを上げて Issue を作る。
        let (mut client, retry) = logged_in(vec![
            json(200, r#"{"id":42}"#),
            json(
                201,
                r#"{"url":"https://github.com/user-attachments/assets/2"}"#,
            ),
            json(
                201,
                r#"{"number":9,"html_url":"https://github.com/o/r/issues/9"}"#,
            ),
        ]);
        let issue = client
            .submit_report("o", "r", "タイトル", "本文", &[], &mut attachments)
            .expect("再試行");
        assert_eq!(issue.number, 9);

        let reqs = retry.lock().clone();
        let uploads: Vec<&RequestSpec> = reqs
            .iter()
            .filter(|req| req.url.starts_with(GITHUB_UPLOAD_URL))
            .collect();
        assert_eq!(uploads.len(), 1, "確定済みの添付を上げ直している: {reqs:?}");
        assert!(uploads[0].url.contains("name=b.png"), "{}", uploads[0].url);
    }

    /// Issue の作成に失敗したときも、確定済みの添付は再アップロードしないこと。
    #[test]
    fn submit_report_does_not_reupload_when_the_issue_call_fails() {
        let mut attachments = vec![attachment("a.png")];
        let (mut client, _) = logged_in(vec![
            json(200, r#"{"id":42}"#),
            json(
                201,
                r#"{"url":"https://github.com/user-attachments/assets/1"}"#,
            ),
            json(422, r#"{"message":"Validation Failed"}"#),
        ]);

        assert!(
            client
                .submit_report("o", "r", "タイトル", "本文", &[], &mut attachments)
                .is_err()
        );
        assert!(attachments[0].url.is_some());

        // 再試行は Issue の作成だけ（添付は上げ直さない）。
        let (mut client, captured) = logged_in(vec![json(
            201,
            r#"{"number":9,"html_url":"https://github.com/o/r/issues/9"}"#,
        )]);
        let issue = client
            .submit_report("o", "r", "タイトル", "本文", &[], &mut attachments)
            .expect("再試行");

        assert_eq!(issue.number, 9);
        let reqs = captured.lock().clone();
        assert_eq!(reqs.len(), 1, "添付を上げ直している: {reqs:?}");
        assert_eq!(reqs[0].url, "https://api.github.com/repos/o/r/issues");
        let payload: serde_json::Value =
            serde_json::from_slice(&reqs[0].body.clone().unwrap()).unwrap();
        assert!(
            payload["body"]
                .as_str()
                .unwrap()
                .contains("https://github.com/user-attachments/assets/1"),
            "{}",
            payload["body"]
        );
    }

    /// 添付が無いときは余計な要求（repository_id の取得）をしないこと。
    #[test]
    fn submit_report_without_attachments_creates_the_issue_directly() {
        let (mut client, captured) = logged_in(vec![json(
            201,
            r#"{"number":7,"html_url":"https://github.com/o/r/issues/7"}"#,
        )]);

        let mut attachments: Vec<IssueAttachment> = Vec::new();
        let issue = client
            .submit_report(
                "o",
                "r",
                "タイトル",
                "本文",
                &["bug".to_string()],
                &mut attachments,
            )
            .expect("submit");

        assert_eq!(issue.number, 7);
        let reqs = captured.lock().clone();
        assert_eq!(
            reqs.len(),
            1,
            "添付が無いのに余計な要求を出している: {reqs:?}"
        );
        assert_eq!(reqs[0].url, "https://api.github.com/repos/o/r/issues");
    }
}
