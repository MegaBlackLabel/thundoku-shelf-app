//! FANZA同人 の購入済み作品クライアント。
//!
//! 一覧・詳細は `www.dmm.co.jp` 同 origin の JSON API（セッション Cookie のみで取得可能。
//! GET に CSRF 不要）を叩く。ダウンロードは詳細で得た proxy URL を 302 追跡して
//! CDN の実 ZIP を取得する。`Transport` は `tbf::transport` を再利用（モック可能）。

use std::collections::HashMap;

use crate::fanza::classify;
use crate::tbf::TbfError;
use crate::tbf::transport::{RequestSpec, ResponseSpec, Transport};
use serde::Deserialize;
use serde_json::Value;

pub const LIBRARY_BASE: &str = "https://www.dmm.co.jp/dc/doujin/api/mylibraries/";
/// jar 等の参照メタ列は一覧 API では返さない（`details` / 商品ページで取得）。
pub const PAGE_LIMIT: usize = 20;

/// DMM/FANZA は非ブラウザの User-Agent を 403 で弾くため、ブラウザ UA + Referer を送る
///（BoothClient と同じ流儀）。
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

#[derive(Debug, thiserror::Error)]
pub enum FanzaError {
    #[error("セッション切れ・未ログイン")]
    SessionExpired,
    #[error("不正アクセス（{0}）")]
    Unauthorized(u16),
    #[error("DRM 付きで取り込めない")]
    DrmProtected,
    #[error("HTTP {0}")]
    Http(u16),
    #[error("parsing failed: {0}")]
    Parse(String),
    #[error("db: {0}")]
    Database(#[from] sqlx::Error),
    #[error("transport: {0}")]
    Transport(#[from] TbfError),
}

impl From<serde_json::Error> for FanzaError {
    fn from(e: serde_json::Error) -> Self {
        FanzaError::Parse(e.to_string())
    }
}

/// FANZA のセッション（`www.dmm.co.jp` / `accounts.dmm.co.jp` の Cookie 群）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct FanzaSession {
    cookies: HashMap<String, String>,
}

impl FanzaSession {
    pub fn new(cookies: HashMap<String, String>) -> Self {
        Self { cookies }
    }

    pub fn logged_in(&self) -> bool {
        !self.cookies.is_empty()
    }

    pub fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(k, v)| format!("{k}={v}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    pub fn cookies_count(&self) -> usize {
        self.cookies.len()
    }
}

/// 一覧 API の 1 購入作品（`data.items` の各要素 + 購入日はグループ key から）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanzaPurchase {
    pub content_id: String,
    pub title: String,
    pub genre: String,
    pub image_src: String,
    pub maker_name: String,
    pub purchase_date: Option<String>,
    pub is_streaming: bool,
    pub is_unavailable: bool,
}

/// 詳細 API（`mylibraries/details/{id}/`）の主要フィールド。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanzaDetail {
    pub content_id: String,
    pub title: String,
    pub genre: String,
    pub maker_name: String,
    pub maker_id: Option<String>,
    pub delivery_date: Option<String>,
    pub download_link: Option<String>,
    pub is_drm: bool,
    pub file_size: Option<String>,
}

impl FanzaDetail {
    /// 詳細メタを 2 軸分類して `is_viewable_included` 判定（DRM 込み）。
    pub fn meta(&self) -> crate::fanza::FanzaMeta {
        classify("", &self.genre)
    }
}

/// 作品ページ（SSR HTML、一般公開）から抽出する情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FanzaProductPage {
    /// ジャンルタグ（`拘束`、`触手` などの複数）。
    pub genre_tags: Vec<String>,
    /// 作者（作品情報の「作者」行。行が無い / 空の作品は `None`）。
    pub author: Option<String>,
}

/// 作品情報の「作者」行（`<dt class="informationList__ttl">作者</dt>` の次の `<a>` テキスト）。
/// 実機 HTML（2026-09-11 / d_818290）で検証済み。
const AUTHOR_RE: &str = r#"(?s)<dt class="informationList__ttl">\s*作者\s*</dt>\s*<dd class="informationList__txt">\s*<a[^>]*>([^<]*)</a>"#;

/// 作品ページ HTML からジャンルタグ（`genreTag__txt`）と作者（`作者` 行）を抽出する。
pub fn parse_product_page(html: &str) -> FanzaProductPage {
    let mut genre_tags = Vec::new();
    let re = regex::Regex::new(r#"class="genreTag__txt"[^>]*>([^<]+)</a>"#).unwrap();
    for cap in re.captures_iter(html) {
        let tag = cap[1].trim().to_string();
        if !tag.is_empty() {
            genre_tags.push(tag);
        }
    }
    let author = regex::Regex::new(AUTHOR_RE)
        .ok()
        .and_then(|re| re.captures(html))
        .map(|cap| cap[1].trim().to_string())
        .filter(|author| !author.is_empty());
    FanzaProductPage { genre_tags, author }
}

/// FANZA 同人クライアント（`tbf::transport` を再利用、`Transport` でモック可能）。
pub struct FanzaClient {
    transport: Box<dyn Transport>,
    session: FanzaSession,
}

impl FanzaClient {
    pub fn with_transport(transport: Box<dyn Transport>, session: FanzaSession) -> Self {
        Self { transport, session }
    }

    fn cookie_headers(&self) -> Vec<(String, String)> {
        vec![
            ("Cookie".to_string(), self.session.cookie_header()),
            ("Accept".to_string(), "application/json".to_string()),
            ("User-Agent".to_string(), USER_AGENT.to_string()),
        ]
    }

    fn check_status(&self, resp: &ResponseSpec) -> Result<(), FanzaError> {
        if resp.status == 401 || resp.status == 403 {
            return Err(FanzaError::Unauthorized(resp.status));
        }
        if resp.status == 200 {
            return Ok(());
        }
        Err(FanzaError::Http(resp.status))
    }

    fn get_json(&mut self, url: &str) -> Result<Value, FanzaError> {
        let spec = RequestSpec {
            method: "GET".into(),
            url: url.into(),
            headers: self.cookie_headers(),
            body: None,
            redirects: 3,
        };
        let resp = self.transport.send(spec).map_err(FanzaError::Transport)?;
        self.check_status(&resp)?;
        let json: Value = serde_json::from_slice(&resp.body)?;
        if json.get("error_code").and_then(Value::as_i64) != Some(0) {
            return Err(FanzaError::SessionExpired);
        }
        Ok(json)
    }

    /// 購入済み作品一覧を全ページ取得する（`hasNext` でページを回し、`total` で打ち切り）。
    pub fn purchased(&mut self) -> Result<Vec<FanzaPurchase>, FanzaError> {
        let mut out = Vec::new();
        let mut page = 1usize;
        loop {
            let url = format!(
                "{LIBRARY_BASE}?page={page}&sort=purchasedate_desc&genre=all&limit={PAGE_LIMIT}"
            );
            let json = self.get_json(&url)?;
            let data = json
                .get("data")
                .ok_or_else(|| FanzaError::Parse("no data".into()))?;
            let items = data.get("items").cloned().unwrap_or(Value::Null);
            if let Value::Object(map) = &items {
                for (date, list) in map {
                    if let Value::Array(arr) = list {
                        for v in arr {
                            out.push(parse_purchase(v, Some(date.clone())));
                        }
                    }
                }
            }
            let has_next = data
                .get("hasNext")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let total = data.get("total").and_then(Value::as_i64).unwrap_or(0) as usize;
            if !has_next || out.len() >= total || out.len() >= PAGE_LIMIT * 100 {
                break;
            }
            page += 1;
        }
        Ok(out)
    }

    /// 作品詳細（download link / DRM / メタ）を取得する。
    pub fn detail(&mut self, content_id: &str) -> Result<FanzaDetail, FanzaError> {
        let url = format!("{LIBRARY_BASE}details/{content_id}/");
        let json = self.get_json(&url)?;
        let d = json
            .get("data")
            .ok_or_else(|| FanzaError::Parse("no data".into()))?;
        let download_link = d
            .get("downloadLinks")
            .and_then(Value::as_object)
            .and_then(|m| m.get("1"))
            .and_then(Value::as_str)
            .map(|s| {
                if s.starts_with('/') {
                    format!("https://www.dmm.co.jp{s}")
                } else {
                    s.to_string()
                }
            });
        let is_drm = d
            .get("drm")
            .map(|v| {
                v.get("dmmBooks").and_then(Value::as_bool).unwrap_or(false)
                    || v.get("softDenchi")
                        .and_then(Value::as_bool)
                        .unwrap_or(false)
            })
            .unwrap_or(false)
            || d.get("isSdrm").and_then(Value::as_str) != Some("0");
        Ok(FanzaDetail {
            content_id: s(d, "contentId"),
            title: s(d, "title"),
            genre: s(d, "genre"),
            maker_name: s(d, "makerName"),
            maker_id: d
                .get("makerId")
                .and_then(Value::as_i64)
                .map(|v| v.to_string()),
            delivery_date: s_opt(d, "deliveryDate"),
            download_link,
            is_drm,
            file_size: s_opt(d, "fileSize"),
        })
    }

    /// 作品ページ（SSR HTML、一般公開）からジャンルタグ等を取得する。
    pub fn product_page(&mut self, cid: &str) -> Result<FanzaProductPage, FanzaError> {
        let url = format!("https://www.dmm.co.jp/dc/doujin/-/detail/=/cid={cid}/");
        let spec = RequestSpec {
            method: "GET".into(),
            url,
            headers: self.cookie_headers(),
            body: None,
            redirects: 3,
        };
        let resp = self.transport.send(spec).map_err(FanzaError::Transport)?;
        self.check_status(&resp)?;
        let html = String::from_utf8_lossy(&resp.body).to_string();
        Ok(parse_product_page(&html))
    }

    /// ダウンロード proxy URL を 302 追跡して ZIP 本体を取得する。HTML レスポンスは拒否。
    ///
    /// CDN（`contents.doujin.dmm.co.jp`）は CloudFront プライベート配布で、署名 Cookie
    /// （`CloudFront-Policy`/`CloudFront-Signature`/`CloudFront-Key-Pair-Id`）が要る。
    /// ureq はクロスホスト（proxy → CDN）リダイレクトで `Cookie` ヘッダを落とすため、
    /// proxy の 302 `Location` を手動で取得し、CDN へ直接（署名 Cookie 付き）取得する。
    pub fn download_with_progress(
        &mut self,
        download_url: &str,
        on_progress: &mut dyn FnMut(u64, u64),
    ) -> Result<Vec<u8>, FanzaError> {
        // 1) proxy を manual（redirects=0）で叩いて 302 Location を取得
        let proxy_headers = vec![
            ("Cookie".to_string(), self.session.cookie_header()),
            ("User-Agent".to_string(), USER_AGENT.to_string()),
            ("Referer".to_string(), "https://www.dmm.co.jp/".to_string()),
        ];
        let proxy_spec = RequestSpec {
            method: "GET".into(),
            url: download_url.into(),
            headers: proxy_headers,
            body: None,
            redirects: 0,
        };
        let proxy_resp = self
            .transport
            .send(proxy_spec)
            .map_err(FanzaError::Transport)?;
        let cd_url = if proxy_resp.status == 302 {
            let loc = proxy_resp
                .header("location")
                .ok_or_else(|| FanzaError::Parse("プロキシ応答に location がありません".into()))?;
            if loc.starts_with('/') {
                format!("https://www.dmm.co.jp{loc}")
            } else {
                loc.to_string()
            }
        } else if proxy_resp.status == 401 || proxy_resp.status == 403 {
            return Err(FanzaError::Unauthorized(proxy_resp.status));
        } else {
            return Err(FanzaError::Http(proxy_resp.status));
        };
        // 2) CDN へ直接（署名 Cookie 込みのフルブラウザヘッダ付き）
        //    署名 Cookie（CloudFront-*）はログイン時ではなく、このダウンロード proxy の
        //    応答（Set-Cookie）で lazy に発行されるため、ここで取って CDN へ送る。
        //    （ureq をリダイレクトに任せるとクロスホストで Cookie が落ちるので手動追跡）
        let mut cookie = self.session.cookie_header();
        for (k, v) in proxy_resp.set_cookies() {
            if k.starts_with("CloudFront-") && !cookie.contains(&format!("{k}=")) {
                if !cookie.is_empty() {
                    cookie.push_str("; ");
                }
                cookie.push_str(&format!("{k}={v}"));
            }
        }
        let headers = vec![
            ("Cookie".to_string(), cookie),
            ("User-Agent".to_string(), USER_AGENT.to_string()),
            ("Referer".to_string(), "https://www.dmm.co.jp/".to_string()),
            (
                "Accept".to_string(),
                "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8".to_string(),
            ),
            ("Accept-Language".to_string(), "ja,en;q=0.9".to_string()),
            ("Sec-Fetch-Dest".to_string(), "document".to_string()),
            ("Sec-Fetch-Mode".to_string(), "navigate".to_string()),
            ("Sec-Fetch-Site".to_string(), "cross-site".to_string()),
            ("Upgrade-Insecure-Requests".to_string(), "1".to_string()),
        ];
        let spec = RequestSpec {
            method: "GET".into(),
            url: cd_url,
            headers,
            body: None,
            redirects: 3,
        };
        let resp = self
            .transport
            .send_download(spec, on_progress)
            .map_err(FanzaError::Transport)?;
        self.check_status(&resp)?;
        if resp.body.starts_with(b"<!doctype") || resp.body.starts_with(b"<html") {
            return Err(FanzaError::Parse("HTML response (not a file)".into()));
        }
        Ok(resp.body)
    }
}

fn s(v: &Value, key: &str) -> String {
    v.get(key).and_then(Value::as_str).unwrap_or("").to_string()
}

fn s_opt(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(|s| s.to_string())
}

#[derive(Deserialize)]
struct RawPurchase {
    #[serde(rename = "contentId", default)]
    content_id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    genre: String,
    #[serde(rename = "imageSrc", default)]
    image_src: String,
    #[serde(rename = "makerName", default)]
    maker_name: String,
    #[serde(rename = "isStreaming", default)]
    is_streaming: bool,
    #[serde(rename = "isUnavailable", default)]
    is_unavailable: bool,
}

fn parse_purchase(v: &Value, purchase_date: Option<String>) -> FanzaPurchase {
    let r: RawPurchase = serde_json::from_value(v.clone()).unwrap_or(RawPurchase {
        content_id: String::new(),
        title: String::new(),
        genre: String::new(),
        image_src: String::new(),
        maker_name: String::new(),
        is_streaming: false,
        is_unavailable: false,
    });
    FanzaPurchase {
        content_id: r.content_id,
        title: r.title,
        genre: r.genre,
        image_src: r.image_src,
        maker_name: r.maker_name,
        purchase_date,
        is_streaming: r.is_streaming,
        is_unavailable: r.is_unavailable,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fanza::{AiType, FanzaMeta, MediaCategory};

    fn json_body(v: Value) -> Vec<u8> {
        serde_json::to_vec(&v).unwrap()
    }

    fn page_json(items: Vec<Value>, has_next: bool, total: i64) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("2026年09月03日".into(), Value::Array(items));
        serde_json::json!({
            "error_code": 0,
            "data": { "items": m, "total": total, "hasNext": has_next }
        })
    }

    fn purchase(content: &str, genre: &str, image_path: &str) -> Value {
        serde_json::json!({
            "contentId": content,
            "title": "タイトル",
            "imageSrc": format!("https://dmm/digital/{image_path}/d/x.jpg"),
            "genre": genre,
            "makerName": "サークル",
            "isStreaming": true,
            "isUnavailable": false,
        })
    }

    fn session() -> FanzaSession {
        FanzaSession::new(HashMap::from([("login_id".into(), "abc".into())]))
    }

    /// スクリプト化されたトランスポート。URL で応答を分岐する（tbf::sync の MockTransport と同流儀）。
    struct MockTransport {
        handler: Box<dyn FnMut(RequestSpec) -> Result<ResponseSpec, TbfError> + Send>,
    }

    impl Transport for MockTransport {
        fn send(&mut self, spec: RequestSpec) -> Result<ResponseSpec, TbfError> {
            (self.handler)(spec)
        }
    }

    /// GET リクエストに Cookie ヘッダが付くこと、`hasNext` でページングし `total` で打ち切ることを検証。
    #[test]
    fn purchased_pages_until_hasnext_false_and_sends_cookie() {
        use parking_lot::Mutex;
        use std::sync::Arc;
        let calls = Arc::new(Mutex::new(Vec::<String>::new()));
        let calls2 = calls.clone();
        let transport = MockTransport {
            handler: Box::new(move |spec: RequestSpec| {
                calls2.lock().push(format!(
                    "{} {}",
                    spec.url,
                    spec.headers
                        .iter()
                        .map(|(k, v)| format!("{k}={v}"))
                        .collect::<Vec<_>>()
                        .join("&")
                ));
                let page1 = if spec.url.contains("page=1") {
                    page_json(vec![purchase("d_1", "コミック", "comic")], true, 3)
                } else {
                    page_json(
                        vec![
                            purchase("d_2", "CG", "cg"),
                            purchase("d_3", "ボイス", "voice"),
                        ],
                        false,
                        3,
                    )
                };
                Ok(ResponseSpec {
                    status: 200,
                    headers: vec![],
                    body: json_body(page1),
                })
            }),
        };
        let mut client = FanzaClient::with_transport(Box::new(transport), session());
        let list = client.purchased().unwrap();
        assert_eq!(list.len(), 3);
        assert_eq!(list[0].content_id, "d_1");
        assert_eq!(list[0].purchase_date.as_deref(), Some("2026年09月03日"));
        let calls = calls.lock();
        assert!(calls.len() >= 2);
        // Cookie ヘッダが全リクエストに付く
        for c in calls.iter() {
            assert!(c.contains("Cookie="), "no cookie header: {c}");
        }
    }

    /// 詳細レスポンスが download_link / DRM / maker_id / file_size にマップされる。
    #[test]
    fn detail_maps_download_link_and_drm() {
        let body = serde_json::json!({
            "error_code": 0,
            "data": {
                "contentId": "d_1", "title": "T", "genre": "コミック・一部AI",
                "makerName": "C", "makerId": 42, "deliveryDate": "2026年09月03日",
                "fileSize": "57.15MB",
                "downloadLinks": { "1": "/dc/-/proxy/=/transfer_type=download/shop=doujin/product_id=d_1/" },
                "drm": { "dmmBooks": false, "softDenchi": false }, "isSdrm": "0"
            }
        });
        let body_bytes = json_body(body);
        let transport = MockTransport {
            handler: Box::new(move |_| {
                Ok(ResponseSpec {
                    status: 200,
                    headers: vec![],
                    body: body_bytes.clone(),
                })
            }),
        };
        let mut client = FanzaClient::with_transport(Box::new(transport), session());
        let d = client.detail("d_1").unwrap();
        assert_eq!(
            d.download_link.as_deref(),
            Some(
                "https://www.dmm.co.jp/dc/-/proxy/=/transfer_type=download/shop=doujin/product_id=d_1/"
            )
        );
        assert!(!d.is_drm);
        assert_eq!(d.maker_id.as_deref(), Some("42"));
        assert_eq!(d.file_size.as_deref(), Some("57.15MB"));
        // 詳細メタの 2 軸分類（コミック・一部AI）
        assert_eq!(
            d.meta(),
            FanzaMeta {
                media: MediaCategory::Comic,
                ai: AiType::PartialAi
            }
        );
    }

    /// セッション切れ（error_code != 0）および DRM 付き詳細の判定。
    #[test]
    fn detail_rejects_error_code_and_flags_drm() {
        let transport = MockTransport {
            handler: Box::new(move |_| {
                Ok(ResponseSpec {
                    status: 200,
                    headers: vec![],
                    body: json_body(serde_json::json!({ "error_code": 1, "data": {} })),
                })
            }),
        };
        let mut client = FanzaClient::with_transport(Box::new(transport), session());
        assert!(matches!(
            client.detail("d_1").unwrap_err(),
            FanzaError::SessionExpired
        ));

        let body = serde_json::json!({
            "error_code": 0, "data": {
                "contentId": "d_1", "title": "T", "genre": "コミック",
                "makerName": "C", "downloadLinks": { "1": "/x" },
                "drm": { "dmmBooks": true, "softDenchi": false }, "isSdrm": "1"
            }
        });
        let body_bytes2 = json_body(body);
        let transport2 = MockTransport {
            handler: Box::new(move |_| {
                Ok(ResponseSpec {
                    status: 200,
                    headers: vec![],
                    body: body_bytes2.clone(),
                })
            }),
        };
        let mut c2 = FanzaClient::with_transport(Box::new(transport2), session());
        assert!(c2.detail("d_1").unwrap().is_drm);
    }

    /// ダウンロード: proxy の 302 Location を手動で取得し、CDN へ直接（署名 Cookie 付き）
    /// 取得すること。ureq がクロスホストリダイレクトで Cookie を落とす問題の回帰テスト。
    #[test]
    fn download_manually_follows_proxy_302_to_cdn_with_cookie() {
        use parking_lot::Mutex;
        use std::sync::Arc;
        let cdn_spec = Arc::new(Mutex::new(None::<RequestSpec>));
        let cdn_spec2 = cdn_spec.clone();
        let transport = MockTransport {
            handler: Box::new(move |spec: RequestSpec| {
                if spec.url.contains("/dc/-/proxy/") {
                    Ok(ResponseSpec {
                        status: 302,
                        headers: vec![
                            (
                                "location".into(),
                                "https://doujin.contents.doujin.dmm.co.jp/bb/dm_comic/x.zip".into(),
                            ),
                            (
                                "set-cookie".into(),
                                "CloudFront-Signature=abc; path=/; domain=dmm.co.jp; secure; httponly".into(),
                            ),
                            (
                                "set-cookie".into(),
                                "CloudFront-Key-Pair-Id=K123; path=/; domain=dmm.co.jp; secure; httponly".into(),
                            ),
                        ],
                        body: vec![],
                    })
                } else {
                    *cdn_spec2.lock() = Some(spec);
                    Ok(ResponseSpec {
                        status: 200,
                        headers: vec![("content-type".into(), "application/zip".into())],
                        body: b"PK\x03\x04zipdata".to_vec(),
                    })
                }
            }),
        };
        let mut client = FanzaClient::with_transport(
            Box::new(transport),
            FanzaSession::new(HashMap::from([("login_id".into(), "abc".into())])),
        );
        let mut on = |_: u64, _: u64| {};
        let bytes = client
            .download_with_progress(
                "https://www.dmm.co.jp/dc/-/proxy/=/transfer_type=download/shop=doujin/product_id=x/",
                &mut on,
            )
            .unwrap();
        assert_eq!(bytes, b"PK\x03\x04zipdata");
        // CDN リクエストには Cookie（CloudFront 署名含む）が送られ、proxy URL ではなく CDN URL 宛。
        let spec = cdn_spec.lock().clone().expect("CDN request made");
        assert!(
            spec.url
                .starts_with("https://doujin.contents.doujin.dmm.co.jp/")
        );
        assert!(
            spec.headers
                .iter()
                .any(|(k, v)| k == "Cookie" && v.contains("login_id=abc"))
        );
        // proxy 応答で発行された署名 Cookie（CloudFront-*）が CDN へ届く
        let cf = spec
            .headers
            .iter()
            .find(|(k, _)| k == "Cookie")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        assert!(
            cf.contains("CloudFront-Signature=abc"),
            "CloudFront signature cookie missing: {cf}"
        );
        assert!(cf.contains("CloudFront-Key-Pair-Id=K123"));
        assert!(spec.redirects == 3);
    }

    /// 作品ページ HTML からジャンルタグ（genreTag__txt）を抽出する。
    #[test]
    fn parse_product_page_extracts_genre_tags() {
        let html = r#"<ul class="genreTagList"><li><a href="/x" class="genreTag__txt">拘束</a></li><li><a href="/y" class="genreTag__txt">触手</a></li><li><a href="/z" class="genreTag__txt">ファンタジー</a></li></ul>"#;
        let page = parse_product_page(html);
        assert_eq!(
            page.genre_tags,
            vec![
                "拘束".to_string(),
                "触手".to_string(),
                "ファンタジー".to_string()
            ]
        );
    }

    /// 作品ページ HTML の「作者」行（`informationList__ttl`）を抽出する。
    /// 実機の HTML（2026-09-11 取得 / d_818290）と同じ整形で検証する。
    #[test]
    fn parse_product_page_extracts_author() {
        let html = r#"<div class="productInformation__item">
        <dl class="informationList">
            <dt class="informationList__ttl">作者</dt>
            <dd class="informationList__txt"><a href="https://www.dmm.co.jp/dc/doujin/-/list/=/article=creator/id=e90603bd-64d3-11f0-ba33-0242ac160002/section=mens/">Ash横島</a></dd>
        </dl>
    </div>"#;
        let page = parse_product_page(html);
        assert_eq!(page.author.as_deref(), Some("Ash横島"));
        // 作者行が無い作品は None
        assert_eq!(parse_product_page("<div></div>").author, None);
        // 作者行はあるが空のときも None
        let empty = r#"<dt class="informationList__ttl">作者</dt><dd class="informationList__txt"><a href="/x"></a></dd>"#;
        assert_eq!(parse_product_page(empty).author, None);
    }

    /// 実機プローブ: `UreqTransport` 経由で CDN ダウンロードが 200 になるか確認する。
    /// `FANZA_TEST_COOKIE` に Cookie（`name=value; ...`）を設定して実行する。
    #[test]
    #[ignore]
    fn live_download_probe() {
        let cookie = std::env::var("FANZA_TEST_COOKIE").expect("FANZA_TEST_COOKIE not set");
        let mut t = crate::tbf::UreqTransport::new();
        let base = vec![
            ("Cookie".into(), cookie),
            ("User-Agent".into(), USER_AGENT.into()),
            ("Referer".into(), "https://www.dmm.co.jp/".into()),
        ];
        // 1) proxy を manual (redirects=0) で叩いて 302 Location を取得
        let p_spec = RequestSpec {
            method: "GET".into(),
            url: "https://www.dmm.co.jp/dc/-/proxy/=/transfer_type=download/shop=doujin/product_id=d_815503/".into(),
            headers: base.clone(),
            body: None,
            redirects: 0,
        };
        let p = t.send(p_spec).unwrap();
        eprintln!("PROXY status={}", p.status);
        let loc = p.header("location").unwrap_or("").to_string();
        eprintln!("LOC={loc}");
        // 2) CDN へ直接（Cookie 含む Sec-Fetch ヘッダ付き）
        let mut c_headers = base;
        c_headers.push(("Sec-Fetch-Dest".into(), "document".into()));
        c_headers.push(("Sec-Fetch-Mode".into(), "navigate".into()));
        c_headers.push(("Sec-Fetch-Site".into(), "cross-site".into()));
        c_headers.push((
            "Accept".into(),
            "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8".into(),
        ));
        let c_spec = RequestSpec {
            method: "GET".into(),
            url: loc,
            headers: c_headers,
            body: None,
            redirects: 3,
        };
        let mut on = |_: u64, _: u64| {};
        let r = t.send_download(c_spec, &mut on).unwrap();
        eprintln!(
            "CDN status={} len={} ct={:?}",
            r.status,
            r.body.len(),
            r.header("content-type")
        );
    }
}
