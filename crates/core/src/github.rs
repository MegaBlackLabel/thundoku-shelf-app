//! GitHub の Issue を**ブラウザーで**投稿するための補助。
//!
//! アプリは **GitHub のトークンを一切取得・保存しない**。Issue を 1 つ作るためだけに
//! `public_repo`（＝公開リポジトリへの書き込み）を利用者へ求めるのは権限が過大なので、
//! 「レポート」の送信は投稿先リポジトリの Issue 作成画面を**既定のブラウザーで開く**
//! だけにして、投稿そのものは利用者が GitHub の画面（＝自分の権限）で行う。未ログインでも
//! 投稿できる。
//!
//! ここが持つのはブラウザー投稿に必要な次の 2 つだけ:
//!
//! - Issue 作成画面の URL の組立（[`issue_link`] / [`new_issue_url`] / [`plain_issue_url`]）。
//!   URL に載せられないほど長い本文は、クリップボードへ渡す判断も [`issue_link`] が返す
//! - リポジトリの issue form（`.github/ISSUE_TEMPLATE/*.yml`）を**匿名で**取得する
//!   （[`GithubClient::list_issue_templates`]）。テンプレートは本文の下書き用の補助なので、
//!   取得できなくてもレポートは書ける（画面は理由を出すだけにする）

use crate::tbf::{RequestSpec, ResponseSpec, TbfError, Transport, UreqTransport};

pub const GITHUB_URL: &str = "https://github.com";
pub const GITHUB_API_URL: &str = "https://api.github.com";
/// テンプレート本文の取得先（**匿名**。公開リポジトリなのでトークンは要らない）。
///
/// 一覧は Contents API でしか引けないが、`api.github.com` の匿名枠は 1 時間 60 回なので、
/// 本文はテンプレートの数だけ CDN から取る（`HEAD` は既定ブランチを指す）。
pub const GITHUB_RAW_URL: &str = "https://raw.githubusercontent.com";
/// GitHub は `User-Agent` の無いリクエストを 403 で拒否する。
pub const USER_AGENT: &str = concat!("thundoku-shelf/", env!("CARGO_PKG_VERSION"));

/// ブラウザーで開く URL の長さの上限（目安）。
///
/// これを超える URL は GitHub（前段の nginx）に弾かれる（414）。本文を載せない素の
/// Issue 作成画面を開き、本文はクリップボードで渡す（[`issue_link`]）。
pub const MAX_ISSUE_URL_CHARS: usize = 8_000;

/// 取り込むテンプレート 1 ファイルの上限。issue form は数 KB で足りる。
/// 外部から来るデータなので、大きすぎるものは YAML へ渡さない（メモリ枯渇を防ぐ）。
const MAX_TEMPLATE_BYTES: usize = 256 * 1024;

/// 取り込むテンプレートの最大件数（転送量の上限）。
const MAX_TEMPLATE_FILES: usize = 20;

/// `?title=` / `?body=` / パスの 1 セグメントに載せる percent-encode。
///
/// 空白は `+` ではなく `%20` にする（`+` を空白として解釈するかは受け手の実装次第で、
/// 本文に含まれる `+` と区別できない）。
fn percent_encode(value: &str) -> String {
    const ENCODE_SET: &percent_encoding::AsciiSet = &percent_encoding::NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~');
    percent_encoding::utf8_percent_encode(value, ENCODE_SET).to_string()
}

/// 素の Issue 作成画面の URL（クエリ無し）。
///
/// `owner` / `repo` はアプリの定数（`[A-Za-z0-9-_.]` だけ）なので encode しない。
pub fn plain_issue_url(owner: &str, repo: &str) -> String {
    format!("{GITHUB_URL}/{owner}/{repo}/issues/new")
}

/// タイトルと本文をクエリに載せた Issue 作成画面の URL。
///
/// GitHub の画面はこの値で下書きを埋める（利用者は開いたあとで書き直せる）。
pub fn new_issue_url(owner: &str, repo: &str, title: &str, body: &str) -> String {
    format!(
        "{}?title={}&body={}",
        plain_issue_url(owner, repo),
        percent_encode(title),
        percent_encode(body)
    )
}

/// 「Issue を作成」で開く URL と、URL に載せられない本文。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IssueLink {
    /// ブラウザーで開く URL。
    pub url: String,
    /// **クリップボードへ渡す本文**。URL が長すぎて載せられないときだけ `Some`。
    pub copy_body: Option<String>,
}

/// レポートのタイトルと本文から、開く URL を決める。
///
/// 本文はクエリに載せる。長すぎる URL は GitHub に弾かれるので、[`MAX_ISSUE_URL_CHARS`]
/// を超えたら本文を載せず、素の Issue 作成画面を開いて本文をクリップボードへ渡す
/// （利用者が GitHub の画面へ貼り付ける）。
pub fn issue_link(owner: &str, repo: &str, title: &str, body: &str) -> IssueLink {
    let url = new_issue_url(owner, repo, title, body);
    if url.chars().count() <= MAX_ISSUE_URL_CHARS {
        return IssueLink {
            url,
            copy_body: None,
        };
    }
    IssueLink {
        url: plain_issue_url(owner, repo),
        copy_body: Some(body.to_string()),
    }
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

/// issue form の 1 項目。レポート画面の下書きのもとになる。
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
    pub fields: Vec<TemplateField>,
}

#[derive(Debug, thiserror::Error)]
pub enum GithubError {
    #[error("network error: {0}")]
    Network(String),
    /// 403（匿名アクセスの拒否 / 回数制限）。`Retry-After` の無い 403 はここへ来る。
    #[error("forbidden: {0}")]
    Forbidden(String),
    /// 回数制限に当たった（`Retry-After` 秒待って再試行する）。
    #[error("rate limited (retry after {retry_after:?}s)")]
    RateLimited { retry_after: Option<u64> },
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

impl From<TbfError> for GithubError {
    fn from(value: TbfError) -> Self {
        GithubError::Network(value.to_string())
    }
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

/// issue form（`.github/ISSUE_TEMPLATE/*.yml`）を解析する。
/// `name` を持たないものはテンプレートではないので `None` を返す。
///
/// 呼び出し側が**大きさを確認してから**渡す（[`MAX_TEMPLATE_BYTES`]）。
fn parse_issue_form(file_name: &str, text: &str) -> Result<Option<IssueTemplate>, GithubError> {
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
        fields,
    }))
}

/// 匿名リクエストのヘッダ（**`Authorization` は付けない**）。
fn anonymous_headers(accept: &str) -> Vec<(String, String)> {
    vec![
        ("Accept".to_string(), accept.to_string()),
        ("User-Agent".to_string(), USER_AGENT.to_string()),
    ]
}

/// GitHub の issue form を**匿名で**取得するクライアント。
pub struct GithubClient {
    transport: Box<dyn Transport>,
}

impl Default for GithubClient {
    fn default() -> Self {
        Self::new()
    }
}

impl GithubClient {
    pub fn new() -> Self {
        Self::with_transport(Box::new(UreqTransport::new()))
    }

    pub fn with_transport(transport: Box<dyn Transport>) -> Self {
        Self { transport }
    }

    fn send(&mut self, request: RequestSpec) -> Result<ResponseSpec, GithubError> {
        self.transport.send(request).map_err(GithubError::from)
    }

    /// `.github/ISSUE_TEMPLATE` の issue form を取得する（`config.yml` は除く）。
    ///
    /// **認証は使わない**（公開リポジトリなのでトークンは要らない）。テンプレートを
    /// 置いていないリポジトリは空を返す。1 ファイル読めなくても他は活かす（1 件の失敗で
    /// 全部を失う理由が無い）。
    pub fn list_issue_templates(
        &mut self,
        owner: &str,
        repo: &str,
    ) -> Result<Vec<IssueTemplate>, GithubError> {
        // 一覧だけは Contents API（どのファイル名が置かれているかは他に知る手段が無い）。
        let response = self.send(RequestSpec {
            method: "GET".to_string(),
            url: format!("{GITHUB_API_URL}/repos/{owner}/{repo}/contents/.github/ISSUE_TEMPLATE"),
            headers: anonymous_headers("application/vnd.github+json"),
            body: None,
            redirects: 0,
        })?;
        // テンプレートを置いていないリポジトリは 404 になる。異常ではないので空で返す。
        if response.status == 404 {
            return Ok(Vec::new());
        }
        if let Some(error) = rate_limited(&response) {
            return Err(error);
        }
        if response.status == 403 {
            return Err(GithubError::Forbidden(error_message(&json_or_null(
                &response,
            ))));
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
            let response = self.send(RequestSpec {
                method: "GET".to_string(),
                url: format!(
                    "{GITHUB_RAW_URL}/{owner}/{repo}/HEAD/.github/ISSUE_TEMPLATE/{}",
                    percent_encode(&file_name)
                ),
                headers: anonymous_headers("text/plain"),
                body: None,
                redirects: 0,
            })?;
            if response.status != 200 {
                log::debug!(
                    "github: テンプレート {file_name} を取得できない（status {}）",
                    response.status
                );
                continue;
            }
            if response.body.len() > MAX_TEMPLATE_BYTES {
                log::warn!(
                    "github: テンプレート {file_name} が大きすぎるため読みません（{} バイト）",
                    response.body.len()
                );
                continue;
            }
            let text = String::from_utf8_lossy(&response.body);
            match parse_issue_form(&file_name, &text) {
                Ok(Some(template)) => templates.push(template),
                Ok(None) => {}
                // 壊れた 1 ファイルのせいで他のテンプレートを失わない。
                Err(error) => log::warn!("github: テンプレート {file_name} を解釈できない: {error}"),
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

    const OWNER: &str = "MegaBlackLabel";
    const REPO: &str = "thundoku-shelf-app";

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
        let client = GithubClient::with_transport(Box::new(ScriptedTransport {
            responses,
            captured: captured.clone(),
        }));
        (client, captured)
    }

    fn header(req: &RequestSpec, name: &str) -> Option<String> {
        req.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.clone())
    }

    /// bug.yml 相当の最小の issue form。
    const BUG_FORM: &str = r#"name: 不具合報告
description: 動かないとき
title: "[Bug] "
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

    /// タイトルと本文がクエリに載り、URL を壊す文字が escape されること。
    #[test]
    fn issue_url_encodes_the_title_and_body() {
        let url = new_issue_url(OWNER, REPO, "一覧 & 表示 #1", "## 概要\n100% + 20");

        assert_eq!(
            url,
            "https://github.com/MegaBlackLabel/thundoku-shelf-app/issues/new\
             ?title=%E4%B8%80%E8%A6%A7%20%26%20%E8%A1%A8%E7%A4%BA%20%231\
             &body=%23%23%20%E6%A6%82%E8%A6%81%0A100%25%20%2B%2020"
        );
        // `&` がそのまま出ると本文がクエリの区切りとして切られる（本文が途中で消える）。
        assert!(!url.contains(" & "), "{url}");
        assert_eq!(plain_issue_url(OWNER, REPO).matches('?').count(), 0);
    }

    /// 短い下書きは URL に載せ、クリップボードは使わないこと。
    #[test]
    fn issue_link_puts_the_draft_in_the_url_when_it_fits() {
        let link = issue_link(OWNER, REPO, "[Bug] 起動しない", "## 概要\n落ちる");

        assert_eq!(
            link.url,
            new_issue_url(OWNER, REPO, "[Bug] 起動しない", "## 概要\n落ちる")
        );
        assert!(link.copy_body.is_none());
    }

    /// 上限ちょうどは URL に載せ、1 文字超えたら本文をクリップボードへ回すこと。
    #[test]
    fn issue_link_copies_the_body_when_the_url_is_too_long() {
        // 本文が英数字だけなら encode 後も 1 文字 = 1 文字なので、長さを直接作れる。
        let head = new_issue_url(OWNER, REPO, "t", "");
        let fits = "a".repeat(MAX_ISSUE_URL_CHARS - head.chars().count());

        let link = issue_link(OWNER, REPO, "t", &fits);
        assert_eq!(link.url.chars().count(), MAX_ISSUE_URL_CHARS);
        assert!(link.copy_body.is_none(), "上限ちょうどで本文を落としている");

        let too_long = format!("{fits}a");
        let link = issue_link(OWNER, REPO, "t", &too_long);
        assert_eq!(link.url, plain_issue_url(OWNER, REPO));
        assert_eq!(link.copy_body.as_deref(), Some(too_long.as_str()));
    }

    /// テンプレート取得が**匿名**で、一覧は API・本文は CDN から取ること。
    #[test]
    fn template_listing_is_anonymous_and_parses_forms() {
        let dir = r#"[
            {"name":"bug.yml","path":".github/ISSUE_TEMPLATE/bug.yml","type":"file"},
            {"name":"config.yml","path":".github/ISSUE_TEMPLATE/config.yml","type":"file"}
        ]"#;
        let (mut client, captured) = client_with(vec![json(200, dir), raw(200, BUG_FORM)]);

        let templates = client
            .list_issue_templates(OWNER, REPO)
            .expect("templates");

        assert_eq!(templates.len(), 1, "config.yml はテンプレートではない");
        let template = &templates[0];
        assert_eq!(template.file_name, "bug.yml");
        assert_eq!(template.name, "不具合報告");
        assert_eq!(template.description.as_deref(), Some("動かないとき"));
        assert_eq!(template.title.as_deref(), Some("[Bug] "));
        assert_eq!(template.fields.len(), 2);
        assert_eq!(template.fields[0].kind, TemplateFieldKind::Markdown);
        assert_eq!(template.fields[1].kind, TemplateFieldKind::Textarea);
        assert_eq!(template.fields[1].label.as_deref(), Some("概要"));
        assert_eq!(template.fields[1].placeholder.as_deref(), Some("何が起きた？"));
        assert!(template.fields[1].required);

        let reqs = captured.lock().clone();
        assert_eq!(reqs.len(), 2, "ディレクトリ一覧 + 各フォームを取得する");
        assert_eq!(
            reqs[0].url,
            "https://api.github.com/repos/MegaBlackLabel/thundoku-shelf-app/contents/.github/ISSUE_TEMPLATE"
        );
        assert_eq!(
            reqs[1].url,
            "https://raw.githubusercontent.com/MegaBlackLabel/thundoku-shelf-app/HEAD/.github/ISSUE_TEMPLATE/bug.yml"
        );
        for req in &reqs {
            // トークンを持たないので、認証ヘッダを送ってはいけない。
            assert!(
                header(req, "authorization").is_none(),
                "匿名のはずが認証ヘッダを送っている: {:?}",
                req.headers
            );
            let user_agent = header(req, "user-agent").expect("User-Agent は必須");
            assert!(
                user_agent.starts_with("thundoku-shelf/"),
                "アプリを識別できる User-Agent を送る: {user_agent}"
            );
        }
    }

    #[test]
    fn template_listing_returns_empty_when_the_repository_has_none() {
        // テンプレートを置いていないリポジトリは Contents API が 404 を返す。異常ではない。
        let (mut client, captured) = client_with(vec![json(404, r#"{"message":"Not Found"}"#)]);

        let templates = client
            .list_issue_templates(OWNER, REPO)
            .expect("templates");

        assert!(templates.is_empty());
        assert_eq!(
            captured.lock().len(),
            1,
            "テンプレートが無いならディレクトリ一覧で終わる"
        );
    }

    #[test]
    fn template_listing_reports_rate_limit_and_forbidden_separately() {
        // 匿名アクセスの回数制限は 403（Retry-After 付き）または 429 で返る。
        let mut throttled = json(403, r#"{"message":"API rate limit exceeded"}"#);
        throttled
            .headers
            .push(("Retry-After".to_string(), "60".to_string()));
        let (mut client, _) = client_with(vec![throttled]);
        assert!(matches!(
            client.list_issue_templates(OWNER, REPO),
            Err(GithubError::RateLimited {
                retry_after: Some(60)
            })
        ));

        // Retry-After の無い 403 は「拒否」として区別する（待っても直らない）。
        let (mut client, _) = client_with(vec![json(403, r#"{"message":"Forbidden"}"#)]);
        assert!(matches!(
            client.list_issue_templates(OWNER, REPO),
            Err(GithubError::Forbidden(_))
        ));
    }

    /// 1 ファイル読めなくても、読めたテンプレートは返すこと（下書き用の補助なので
    /// 1 件の失敗で全部を失わない）。
    #[test]
    fn template_listing_keeps_other_files_when_one_cannot_be_read() {
        let dir = r#"[
            {"name":"broken.yml","type":"file"},
            {"name":"bug.yml","type":"file"}
        ]"#;
        let (mut client, _) = client_with(vec![
            json(200, dir),
            raw(500, "boom"),
            raw(200, BUG_FORM),
        ]);

        let templates = client
            .list_issue_templates(OWNER, REPO)
            .expect("templates");

        assert_eq!(templates.len(), 1);
        assert_eq!(templates[0].file_name, "bug.yml");
    }

    /// 大きすぎるファイルと解析できないファイルは飛ばし、残りを返すこと。
    #[test]
    fn template_listing_skips_oversized_and_unparsable_files() {
        let dir = r#"[
            {"name":"huge.yml","type":"file"},
            {"name":"broken.yml","type":"file"},
            {"name":"bug.yml","type":"file"}
        ]"#;
        let huge = "a".repeat(MAX_TEMPLATE_BYTES + 1);
        let (mut client, _) = client_with(vec![
            json(200, dir),
            raw(200, &huge),
            raw(200, "name: [unclosed"),
            raw(200, BUG_FORM),
        ]);

        let templates = client
            .list_issue_templates(OWNER, REPO)
            .expect("templates");

        assert_eq!(templates.len(), 1, "{templates:?}");
        assert_eq!(templates[0].file_name, "bug.yml");
    }

    /// `name` を持たない YAML はテンプレートではない（config.yml 以外の置き方）。
    #[test]
    fn parse_issue_form_requires_a_name() {
        assert_eq!(
            parse_issue_form("other.yml", "description: これはテンプレートではない\n").expect("parse"),
            None
        );
    }

    /// 通信できないときはテンプレート無しで終わること（画面が壊れない）。
    #[test]
    fn template_listing_maps_network_failures() {
        // 応答を 1 つも積まない transport は `Network` を返す。
        let (mut client, _) = client_with(Vec::new());

        assert!(matches!(
            client.list_issue_templates(OWNER, REPO),
            Err(GithubError::Network(_))
        ));
    }
}
