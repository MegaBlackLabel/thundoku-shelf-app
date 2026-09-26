//! 技術書典 (techbookfest.org) direct client: login, bookshelf, checklist,
//! favorites, sample pages, download URL resolution.
//!
//! Mirrors the Web API implementation in `packages/thundoku-api`
//! (`routes/books.ts`, `routes/samples.ts`, `routes/events.ts`,
//! `lib/checklist-graphql.ts`, `lib/favorites-import.ts`) but talks to
//! techbookfest.org directly instead of going through the Workers.

pub mod queries;
pub mod redirect;
pub mod sync;
pub mod transport;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

pub use transport::{RequestSpec, ResponseSpec, Transport, UreqTransport};

pub const TBF_HOME: &str = "https://techbookfest.org/";
pub const TBF_GRAPHQL: &str = "https://techbookfest.org/api/graphql";
pub const TBF_GRAPHQL_V2: &str = "https://techbookfest.org/api/2/graphql";
pub const TBF_DOWNLOAD_BASE: &str = "https://techbookfest.org/api/product-dlc";
pub const SITE_ID_TECHBOOKFEST: &str = "techbookfest";

/// Same UA string the Workers `books.ts` uses.
const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TbfSession {
    pub cookies: Vec<(String, String)>,
    /// Raw (percent-encoded) XSRF-TOKEN cookie value, as sent in `Cookie:`.
    pub xsrf_raw: String,
    /// Decoded value sent in the `X-XSRF-TOKEN` header.
    pub xsrf_token: String,
}

impl TbfSession {
    /// アプリ内 WebView から取得した Cookie 群からセッションを構築する。
    /// XSRF-TOKEN があれば xsrf フィールドに反映する（`percent_decode` は
    /// bootstrap と同じ方式）。
    pub fn from_cookies(cookies: Vec<(String, String)>) -> TbfSession {
        let xsrf = cookies
            .iter()
            .find(|(name, _)| name == "XSRF-TOKEN")
            .cloned();
        TbfSession {
            cookies,
            xsrf_raw: xsrf
                .as_ref()
                .map(|(_, value)| value.clone())
                .unwrap_or_default(),
            xsrf_token: xsrf
                .as_ref()
                .map(|(_, value)| percent_decode(value))
                .unwrap_or_default(),
        }
    }

    /// セッション Cookie（XSRF-TOKEN 以外）があるか。
    pub fn is_logged_in(&self) -> bool {
        self.cookies.iter().any(|(name, _)| name != "XSRF-TOKEN")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum TbfError {
    #[error("network error: {0}")]
    Network(String),
    #[error("invalid credentials")]
    InvalidCredentials,
    #[error("session expired")]
    SessionExpired,
    #[error("not found")]
    NotFound,
    #[error("upstream error: {0}")]
    Upstream(String),
    #[error("invalid response: {0}")]
    InvalidResponse(String),
    /// 送信先が許可リストに無い（セッション Cookie / XSRF を送らない）。
    #[error("blocked download url: {0}")]
    BlockedUrl(String),
    #[error("cancelled")]
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TbfShelfItem {
    pub id: String,
    pub title: String,
    pub circle_name: String,
    pub thumbnail_url: Option<String>,
    pub format: String,
    pub caused_at: Option<String>,
    pub event_name: Option<String>,
    pub event_slug: Option<String>,
    pub file_name: Option<String>,
    pub download_url: Option<String>,
    pub is_downloadable: bool,
    pub tags: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TbfChecklistEntry {
    /// `{eventSlug}:{productIdentity}` — stable per event+product.
    pub id: String,
    pub circle_name: String,
    pub space_number: String,
    pub tbf_circle_id: Option<String>,
    pub product_id: Option<String>,
    pub product_title: String,
    pub thumbnail_url: Option<String>,
    pub price: Option<i64>,
    pub is_purchased: bool,
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TbfEventInfo {
    pub slug: String,
    pub tbf_event_id: String,
    pub event_name: String,
    pub event_date: Option<String>,
    pub event_start_date: Option<String>,
    pub event_end_date: Option<String>,
    pub event_format: String,
    pub is_cancelled: bool,
    pub display_order: i64,
    pub is_featured: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SamplePage {
    pub page_number: i64,
    pub url: String,
    pub width: Option<i64>,
    pub height: Option<i64>,
}

/// セッション Cookie の収集元。**このホスト（とそのサブドメイン）宛にだけ**資格情報を付ける。
pub const SITE_HOST: &str = "techbookfest.org";

/// **宛先ごとの資格情報ヘッダー**（ダウンロード本体とリダイレクトの各ホップで使う）。
///
/// 収集元ホスト（[`SITE_HOST`] とそのサブドメイン）宛のときだけ Cookie と
/// `X-XSRF-TOKEN` を返す。GCS など別システムへは空を返す（fail-closed）。
fn download_credential_headers(
    cookie_header: &str,
    xsrf_token: Option<&str>,
    url: &str,
) -> Vec<(String, String)> {
    let mut headers: Vec<(String, String)> = Vec::new();
    let Ok(parsed) = crate::download_url::parse(url) else {
        return headers;
    };
    if !crate::download_url::host_within(&parsed.host, SITE_HOST) {
        return headers;
    }
    if !cookie_header.is_empty() {
        headers.push(("Cookie".into(), cookie_header.to_string()));
    }
    if let Some(token) = xsrf_token {
        headers.push(("X-XSRF-TOKEN".into(), token.to_string()));
    }
    headers
}

/// ダウンロード URL 解決（`resolve_download_url`）で cookie を付けて GET してよいホスト。
///
/// 解決対象は `bookshelf_items.download_url`（= GraphQL の `downloadURL`、改変
/// バックアップ由来もあり得る）と、そこから作る自サイト URL。自サイト以外へは
/// セッション Cookie を送らない。
const TBF_RESOLVE_RULES: &[crate::download_url::HostRule] =
    &[crate::download_url::HostRule::exact(SITE_HOST, None)];

/// 本体ダウンロードで追うリダイレクトの上限（各ホップは [`TBF_DOWNLOAD_RULES`] で検証する）。
const TBF_REDIRECT_LIMIT: usize = 5;

/// 解決後のファイル本体を取得してよいホスト（`validate_download_url` と同じ範囲）。
///
/// **許可 = Cookie を付ける、ではない**: 本体が GCS のときは
/// [`download_credential_headers`] が Cookie を外す（`storage.googleapis.com` は配布先で
/// あって、こちらのセッションの宛先ではない）。
const TBF_DOWNLOAD_RULES: &[crate::download_url::HostRule] = &[
    crate::download_url::HostRule::exact(SITE_HOST, Some("/api/product-dlc/")),
    crate::download_url::HostRule::exact("storage.googleapis.com", Some("/tbf-tokyo-product-dlc/")),
];

/// 自サイトの画像（`/api/image/`）を取得してよいホスト。**試し読みページと表紙**が該当する。
///
/// 本体用の [`TBF_DOWNLOAD_RULES`] とは**別のリスト**にする。本体は `/api/product-dlc/`
/// だけを許可しているため、そのまま使うと画像が全部ブロックされる（実測で再現）。
/// 逆に画像パスを本体用に混ぜると「画像取得のつもりで本体の URL を渡す」経路ができるので、
/// 入口ごとに分ける。同じサイトの画像なので [`download_headers`] が Cookie を付ける。
const TBF_SITE_IMAGE_RULES: &[crate::download_url::HostRule] =
    &[crate::download_url::HostRule::exact(SITE_HOST, Some("/api/image/"))];

pub struct TbfClient {
    transport: Box<dyn Transport>,
    cookies: Vec<(String, String)>,
    xsrf_raw: Option<String>,
    xsrf_token: Option<String>,
}

impl Default for TbfClient {
    fn default() -> Self {
        Self::new()
    }
}

impl TbfClient {
    pub fn new() -> Self {
        Self::with_transport(Box::new(UreqTransport::new()))
    }
    pub fn with_transport(transport: Box<dyn Transport>) -> Self {
        Self {
            transport,
            cookies: Vec::new(),
            xsrf_raw: None,
            xsrf_token: None,
        }
    }

    /// Current session snapshot (for keyring persistence). `None` when no
    /// XSRF token has been captured yet.
    pub fn session(&self) -> Option<TbfSession> {
        let xsrf_raw = self.xsrf_raw.clone()?;
        let xsrf_token = self.xsrf_token.clone()?;
        Some(TbfSession {
            cookies: self.cookies.clone(),
            xsrf_raw,
            xsrf_token,
        })
    }

    pub fn restore_session(&mut self, session: TbfSession) {
        self.cookies = session.cookies;
        self.xsrf_raw = Some(session.xsrf_raw);
        self.xsrf_token = Some(session.xsrf_token);
    }

    /// True when a session cookie (anything beyond XSRF-TOKEN) is present.
    pub fn is_authenticated(&self) -> bool {
        self.cookies.iter().any(|(name, _)| name != "XSRF-TOKEN")
    }

    /// GET the home page and capture the XSRF-TOKEN cookie
    /// (books.ts `first-event` route, same bootstrap style).
    pub fn bootstrap(&mut self) -> Result<(), TbfError> {
        let resp = self.request(
            "GET",
            TBF_HOME,
            &[
                ("User-Agent", USER_AGENT),
                ("Accept-Language", "ja,en-US;q=0.9,en;q=0.8"),
            ],
            None,
            5,
        )?;
        self.absorb_cookies(&resp);
        if self.xsrf_raw.is_none() {
            return Err(TbfError::InvalidResponse(
                "no XSRF-TOKEN cookie in response".into(),
            ));
        }
        Ok(())
    }

    /// Login with the captured `UserLoginMutation`. Success requires a
    /// non-null `data.loginUser.user.id` AND a session cookie in the
    /// response (the session cookie name is confirmed against the real
    /// site during E2E; any new cookie beyond XSRF-TOKEN counts).
    pub fn login(&mut self, email: &str, password: &str) -> Result<TbfSession, TbfError> {
        let body = json!({
            "operationName": "UserLoginMutation",
            "variables": { "loginInput": { "email": email, "password": password } },
            "extensions": { "clientLibrary": { "name": "@apollo/client", "version": "4.1.9" } },
            "query": queries::LOGIN_MUTATION,
        });
        let url = format!("{TBF_GRAPHQL}?operationName=UserLoginMutation");
        let resp = self.request(
            "POST",
            &url,
            &[("Content-Type", "application/json")],
            Some(serde_json::to_vec(&body).map_err(|e| TbfError::InvalidResponse(e.to_string()))?),
            5,
        )?;
        self.absorb_cookies(&resp);
        if resp.status == 401 || resp.status == 403 {
            return Err(TbfError::InvalidCredentials);
        }
        let payload: Value = serde_json::from_slice(&resp.body)
            .map_err(|_| TbfError::InvalidResponse("login response was not JSON".into()))?;
        if payload.get("errors").is_some() {
            return Err(TbfError::InvalidCredentials);
        }
        let user_id = payload
            .pointer("/data/loginUser/user/id")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        match user_id {
            Some(_) if self.is_authenticated() => {
                self.session().ok_or(TbfError::InvalidCredentials)
            }
            _ => Err(TbfError::InvalidCredentials),
        }
    }

    /// 技術書典サイト側のセッションを無効化する（サーバー側ログアウト）。
    ///
    /// Web 版フロントエンドと同じ `logoutUser` ミューテーション
    /// （`LogoutUserMutation`）を送る。`request` が Cookie と
    /// X-XSRF-TOKEN ヘッダーを自動付与する。
    /// 成功した場合はこのクライアントのセッションも破棄する。
    pub fn logout(&mut self) -> Result<(), TbfError> {
        let body = json!({
            "operationName": "LogoutUserMutation",
            "variables": { "input": {} },
            "extensions": { "clientLibrary": { "name": "@apollo/client", "version": "4.1.9" } },
            "query": "mutation LogoutUserMutation($input: LogoutUserInput!) { logoutUser(input: $input) { clientMutationId user { id email __typename } } }",
        });
        let url = format!("{TBF_GRAPHQL}?operationName=LogoutUserMutation");
        let resp = self.request(
            "POST",
            &url,
            &[("Content-Type", "application/json")],
            Some(serde_json::to_vec(&body).map_err(|e| TbfError::InvalidResponse(e.to_string()))?),
            5,
        )?;
        if resp.status == 401 || resp.status == 403 {
            return Err(TbfError::InvalidCredentials);
        }
        let payload: Value = serde_json::from_slice(&resp.body)
            .map_err(|_| TbfError::InvalidResponse("logout response was not JSON".into()))?;
        if payload.get("errors").is_some() {
            return Err(TbfError::InvalidResponse(
                payload
                    .pointer("/errors/0/message")
                    .and_then(Value::as_str)
                    .unwrap_or("logout errors")
                    .to_string(),
            ));
        }
        // サイト側のセッション破棄が完了したらローカルも破棄する
        self.clear_session();
        Ok(())
    }

    /// メモリ上のセッションを破棄する（ログアウト）。
    ///
    /// Cookie も XSRF トークンも残さない（空の `TbfSession` を `restore_session` すると
    /// `session()` が空のセッションを返し続けるため、必ずこれを使う）。
    pub fn clear_session(&mut self) {
        self.cookies.clear();
        self.xsrf_raw = None;
        self.xsrf_token = None;
    }

    /// Fetch the bookshelf with cursor pagination (BookShelfQuery).
    pub fn bookshelf(&mut self) -> Result<Vec<TbfShelfItem>, TbfError> {
        let mut all = Vec::new();
        let mut after: Option<String> = None;
        loop {
            let mut variables = serde_json::Map::new();
            variables.insert("first".into(), json!(100));
            variables.insert("after".into(), json!(after));
            let payload = self.graphql(
                "BookShelfQuery",
                &format!("{TBF_GRAPHQL}?operationName=BookShelfQuery&appVersion=20260417a-web"),
                Value::Object(variables),
                queries::BOOKSHELF_QUERY,
            )?;
            let shelf = payload
                .pointer("/data/viewer/bookShelfItems")
                .ok_or_else(|| TbfError::InvalidResponse("missing bookShelfItems".into()))?;
            for edge in shelf
                .pointer("/edges")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                all.push(map_shelf_node(&edge["node"]));
            }
            let has_next = shelf
                .pointer("/pageInfo/hasNextPage")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !has_next {
                break;
            }
            after = shelf
                .pointer("/pageInfo/endCursor")
                .and_then(Value::as_str)
                .map(String::from);
            if after.is_none() {
                break;
            }
        }
        Ok(all)
    }

    /// Event masters: canonical list (Web `CANONICAL_EVENT_MASTERS`) plus
    /// best-effort live discovery of future events (tbf21..tbf30).
    pub fn events(&mut self) -> Result<Vec<TbfEventInfo>, TbfError> {
        let mut discovered: Vec<TbfEventInfo> = Vec::new();
        for n in (21..=30).rev() {
            let slug = format!("tbf{n}");
            let Ok(name) = self.discover_event_name(&slug) else {
                continue;
            };
            discovered.push(TbfEventInfo {
                slug: slug.clone(),
                tbf_event_id: format!("Event:{slug}"),
                event_name: name.unwrap_or_else(|| slug.clone()),
                event_date: None,
                event_start_date: None,
                event_end_date: None,
                event_format: "offline".into(),
                is_cancelled: false,
                display_order: 0,
                is_featured: false,
            });
        }
        let mut merged: Vec<TbfEventInfo> = Vec::new();
        for event in discovered {
            if !merged.iter().any(|e| e.slug == event.slug) {
                merged.push(event);
            }
        }
        for event in canonical_events() {
            if !merged.iter().any(|e| e.slug == event.slug) {
                merged.push(event);
            }
        }
        for (index, event) in merged.iter_mut().enumerate() {
            event.display_order = index as i64;
            event.is_featured = index == 0;
        }
        Ok(merged)
    }

    fn discover_event_name(&mut self, slug: &str) -> Result<Option<String>, TbfError> {
        let body = json!({
            "operationName": "TbfEventQuery",
            "query": queries::EVENT_QUERY,
            "variables": { "eventID": format!("Event:{slug}") },
        });
        let resp = self.request(
            "POST",
            &format!("{TBF_GRAPHQL_V2}?operationName=TbfEventQuery"),
            &[("Content-Type", "application/json")],
            Some(serde_json::to_vec(&body).map_err(|e| TbfError::InvalidResponse(e.to_string()))?),
            5,
        )?;
        if !(200..300).contains(&resp.status) {
            return Err(TbfError::Upstream(format!(
                "event query status {}",
                resp.status
            )));
        }
        let payload: Value = serde_json::from_slice(&resp.body)
            .map_err(|_| TbfError::InvalidResponse("event query response was not JSON".into()))?;
        if payload.get("errors").is_some() {
            return Err(TbfError::Upstream("event query errors".into()));
        }
        Ok(payload
            .pointer("/data/event/name")
            .and_then(Value::as_str)
            .map(String::from))
    }

    /// Checklist for one event (EventOfflineCircleChecklistQuery,
    /// checkedProductInfos pagination, followingOrganizations: 0 — same as
    /// the Web favorites import).
    pub fn checklist(&mut self, event_slug: &str) -> Result<Vec<TbfChecklistEntry>, TbfError> {
        let event_id = format!("Event:{event_slug}");
        let mut all = Vec::new();
        // 購入日時フィルタ用の開催期間（ループ前に 1 回だけ計算）
        let window = event_window(event_slug);
        let mut cursor: Option<String> = None;
        loop {
            let mut variables = serde_json::Map::new();
            variables.insert("checkedProductInfosFirst".into(), json!(100));
            variables.insert("checkedProductInfosAfter".into(), json!(cursor));
            variables.insert("followingOrganizationsFirst".into(), json!(0));
            variables.insert("eventID".into(), json!(event_id));
            let payload = self.graphql(
                "EventOfflineCircleChecklistQuery",
                &format!(
                    "{TBF_GRAPHQL}?operationName=EventOfflineCircleChecklistQuery&appVersion=20260424a-web"
                ),
                Value::Object(variables),
                queries::CHECKLIST_QUERY,
            )?;
            let checked = payload
                .pointer("/data/viewer/checkedProductInfos")
                .ok_or_else(|| TbfError::InvalidResponse("missing checkedProductInfos".into()))?;
            for edge in checked
                .pointer("/edges")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
            {
                if let Some(entry) = map_checked_product(&edge["node"]) {
                    // checkedProductInfos はセッション全体（全イベント）の
                    // 購入商品を返すため、サークルのイベントで絞り込む
                    // （id は `{eventSlug}:{product}` 形式）。
                    // さらに購入日時が対象イベントの開催期間内かも確認する
                    // （対象イベントに出展しているだけの商品を除外）。
                    if entry.id.starts_with(&format!("{event_slug}:"))
                        && within_event_window(&window, entry.created_at.as_deref())
                    {
                        all.push(entry);
                    }
                }
            }
            let has_next = checked
                .pointer("/pageInfo/hasNextPage")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            if !has_next {
                break;
            }
            cursor = checked
                .pointer("/pageInfo/endCursor")
                .and_then(Value::as_str)
                .map(String::from);
            if cursor.is_none() {
                break;
            }
        }
        Ok(all)
    }

    /// Favorites import — identical query/mapping to [`Self::checklist`],
    /// matching `collectFavoritesFromTechBookFestGraphQL`.
    pub fn favorites(&mut self, event_slug: &str) -> Result<Vec<TbfChecklistEntry>, TbfError> {
        self.checklist(event_slug)
    }

    /// Sample pages for a product (ProductImagesQuery).
    pub fn product_sample_pages(
        &mut self,
        product_database_id: &str,
    ) -> Result<Vec<SamplePage>, TbfError> {
        let body = json!({
            "operationName": "ProductImagesQuery",
            "query": queries::PRODUCT_IMAGES_QUERY,
            "variables": { "productInfoID": format!("ProductInfo:{product_database_id}") },
        });
        let url = format!("{TBF_GRAPHQL}?operationName=ProductImagesQuery");
        let resp = self.request(
            "POST",
            &url,
            &[("Content-Type", "application/json")],
            Some(serde_json::to_vec(&body).map_err(|e| TbfError::InvalidResponse(e.to_string()))?),
            5,
        )?;
        if resp.status == 401 || resp.status == 403 {
            return Err(TbfError::SessionExpired);
        }
        if !(200..300).contains(&resp.status) {
            return Ok(Vec::new());
        }
        let payload: Value = match serde_json::from_slice(&resp.body) {
            Ok(value) => value,
            Err(_) => return Ok(Vec::new()),
        };
        if payload.get("errors").is_some() {
            return Ok(Vec::new());
        }
        let edges = payload
            .pointer("/data/product/images/edges")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(edges
            .iter()
            .enumerate()
            .filter_map(|(index, edge)| {
                let node = &edge["node"];
                let url = node.get("url")?.as_str()?;
                Some(SamplePage {
                    page_number: index as i64 + 1,
                    url: absolutize(url),
                    width: node.get("width").and_then(Value::as_i64),
                    height: node.get("height").and_then(Value::as_i64),
                })
            })
            .collect())
    }

    /// Resolve a downloadable URL: GET the endpoint without following
    /// redirects and return the absolute `Location`. The input is the
    /// bookshelf item's `downloadURL` (GraphQL `downloadContent.downloadURL`,
    /// which may carry a DLC id different from `database_id`) or a fallback
    /// `/api/product-dlc/{id}/download` URL — mirrors the Web
    /// `resolveDownloadUrlFromTechBookFest` (including `validateDownloadUrl`).
    pub fn resolve_download_url(&mut self, url: &str) -> Result<String, TbfError> {
        // Cookie / XSRF を付ける前に解決対象のホストを検証する（保存 URL は
        // バックアップ由来もあり得る）。
        crate::download_url::check(url, TBF_RESOLVE_RULES)
            .map_err(|error| TbfError::BlockedUrl(format!("{url}: {error}")))?;
        let resp = self.request(
            "GET",
            url,
            &[
                ("User-Agent", USER_AGENT),
                (
                    "Accept",
                    "text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,*/*;q=0.8",
                ),
                ("Accept-Language", "ja,en-US;q=0.9,en;q=0.8"),
            ],
            None,
            0,
        )?;
        if resp.status == 401 || resp.status == 403 {
            return Err(TbfError::SessionExpired);
        }
        if (300..400).contains(&resp.status)
            && let Some(location) = resp.header("location")
        {
            let absolute = absolutize(location);
            if validate_download_url(&absolute) {
                return Ok(absolute);
            }
        }
        Err(TbfError::NotFound)
    }

    /// Download a file from a resolved URL with the session cookies attached.
    pub fn download(&mut self, url: &str) -> Result<Vec<u8>, TbfError> {
        self.download_with_progress(url, &mut |_, _| true)
    }

    /// Download a file while reporting `(downloaded, total)` bytes to
    /// `on_progress` (called from the background executor). The callback
    /// returns whether to continue; `false` aborts with [`TbfError::Cancelled`].
    ///
    /// 許可先は [`TBF_DOWNLOAD_RULES`]（本体 = `/api/product-dlc/` と署名付き GCS）。
    /// 自サイトの画像（試し読みページ・表紙）は [`Self::download_site_image_with_progress`] を使う。
    pub fn download_with_progress(
        &mut self,
        url: &str,
        on_progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<Vec<u8>, TbfError> {
        self.download_with_rules(url, TBF_DOWNLOAD_RULES, on_progress)
    }

    /// 自サイトの画像を取得する（試し読みページ = `product_sample_pages` の URL、表紙）。
    ///
    /// 許可先は [`TBF_SITE_IMAGE_RULES`]（自サイトの `/api/image/` だけ）。本体用の
    /// 許可リストとは別にしてある（本体の URL を画像取得の入口へ渡せない）。
    pub fn download_site_image(&mut self, url: &str) -> Result<Vec<u8>, TbfError> {
        self.download_site_image_with_progress(url, &mut |_, _| true)
    }

    /// 進捗つきの自サイト画像取得（画像ごとの `(downloaded, total)`）。
    pub fn download_site_image_with_progress(
        &mut self,
        url: &str,
        on_progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<Vec<u8>, TbfError> {
        self.download_with_rules(url, TBF_SITE_IMAGE_RULES, on_progress)
    }

    /// 許可リストを差し替えて取得する共通の実装。
    fn download_with_rules(
        &mut self,
        url: &str,
        rules: &[crate::download_url::HostRule],
        on_progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<Vec<u8>, TbfError> {
        // 本体取得も認証付き。起点を検証し、リダイレクトは**各ホップ検証**して追う
        // （許可外ホストへ Cookie / XSRF トークンを残さない）。
        let parsed = crate::download_url::check(url, rules)
            .map_err(|error| TbfError::BlockedUrl(format!("{url}: {error}")))?;
        let cookie_header = self.cookie_header();
        let xsrf_token = self.xsrf_token.clone();
        let mut credentials = |destination: &url::Url| {
            download_credential_headers(
                &cookie_header,
                xsrf_token.as_deref(),
                destination.as_str(),
            )
        };
        let response = crate::tbf::redirect::send_with_validated_redirects(
            self.transport.as_mut(),
            RequestSpec {
                method: "GET".to_string(),
                url: parsed.url.as_str().to_string(),
                // 資格情報はホップごとに `credentials` が足す
                headers: vec![("User-Agent".to_string(), USER_AGENT.to_string())],
                body: None,
                redirects: 0,
            },
            TBF_REDIRECT_LIMIT,
            rules,
            &mut credentials,
            on_progress,
        )?;
        if !(200..300).contains(&response.status) {
            return Err(TbfError::Upstream(format!(
                "download status {}",
                response.status
            )));
        }
        Ok(response.body)
    }

    // -- internals ---------------------------------------------------------

    fn graphql(
        &mut self,
        operation_name: &str,
        url: &str,
        variables: Value,
        query: &str,
    ) -> Result<Value, TbfError> {
        let body =
            json!({ "operationName": operation_name, "variables": variables, "query": query });
        let resp = self.request(
            "POST",
            url,
            &[("Content-Type", "application/json")],
            Some(serde_json::to_vec(&body).map_err(|e| TbfError::InvalidResponse(e.to_string()))?),
            5,
        )?;
        self.absorb_cookies(&resp);
        if resp.status == 401 || resp.status == 403 {
            return Err(TbfError::SessionExpired);
        }
        if !(200..300).contains(&resp.status) {
            return Err(TbfError::Upstream(format!(
                "GraphQL request failed with status {}",
                resp.status
            )));
        }
        let payload: Value = serde_json::from_slice(&resp.body)
            .map_err(|_| TbfError::InvalidResponse("GraphQL response was not JSON".into()))?;
        if payload.get("errors").is_some() {
            if is_auth_related(&payload) {
                return Err(TbfError::SessionExpired);
            }
            return Err(TbfError::Upstream(
                "GraphQL response contained errors".into(),
            ));
        }
        Ok(payload)
    }

    fn request(
        &mut self,
        method: &str,
        url: &str,
        extra_headers: &[(&str, &str)],
        body: Option<Vec<u8>>,
        redirects: u32,
    ) -> Result<ResponseSpec, TbfError> {
        let mut headers: Vec<(String, String)> = extra_headers
            .iter()
            .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
            .collect();
        let cookie_header = self.cookie_header();
        if !cookie_header.is_empty() {
            headers.push(("Cookie".into(), cookie_header));
        }
        if let Some(token) = &self.xsrf_token {
            headers.push(("X-XSRF-TOKEN".into(), token.clone()));
        }
        self.transport.send(RequestSpec {
            method: method.to_string(),
            url: url.to_string(),
            headers,
            body,
            redirects,
        })
    }

    fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ")
    }

    fn absorb_cookies(&mut self, resp: &ResponseSpec) {
        for (name, value) in resp.set_cookies() {
            if name == "XSRF-TOKEN" {
                self.xsrf_raw = Some(value.clone());
                self.xsrf_token = Some(percent_decode(&value));
            }
            self.cookies.retain(|(existing, _)| existing != &name);
            self.cookies.push((name, value));
        }
    }
}

// -- response mapping helpers (ported from the Web implementation) ---------

fn str_of(value: &Value) -> Option<&str> {
    value.as_str()
}

/// `/foo` -> `https://techbookfest.org/foo`; absolute URLs unchanged.
fn absolutize(url: &str) -> String {
    if url.starts_with('/') {
        format!("https://techbookfest.org{url}")
    } else {
        url.to_string()
    }
}

/// Web の `validateDownloadUrl` と同一: 相対パスは `/api/product-dlc/`、
/// 絶対 URL は techbookfest.org の `/api/product-dlc/` または TBF 署名付き
/// GCS URL（storage.googleapis.com の `/tbf-tokyo-product-dlc/`）のみ許可。
fn validate_download_url(url: &str) -> bool {
    if let Some(rest) = url.strip_prefix('/') {
        return rest.starts_with("api/product-dlc/");
    }
    let Some(rest) = url.strip_prefix("https://") else {
        return false;
    };
    let (host, path) = match rest.split_once('/') {
        Some((host, path)) => (host, format!("/{path}")),
        None => (rest, String::new()),
    };
    if host == "techbookfest.org" && path.starts_with("/api/product-dlc/") {
        return true;
    }
    if host == "storage.googleapis.com" && path.starts_with("/tbf-tokyo-product-dlc/") {
        return true;
    }
    false
}

fn map_shelf_node(node: &Value) -> TbfShelfItem {
    let product = &node["product"];
    let download_content = &product["downloadContent"];
    let file_name = str_of(&download_content["fileName"]);
    let format = file_name
        .and_then(|f| f.rsplit('.').next())
        .filter(|ext| !ext.is_empty())
        .map(|ext| ext.to_uppercase())
        .unwrap_or_else(|| "BOOK".to_string());
    let event = &node["marketHandshake"]["event"];
    let event_id = str_of(&event["id"]);
    let download_url = str_of(&download_content["downloadURL"]).map(absolutize);
    TbfShelfItem {
        id: str_of(&product["databaseID"]).unwrap_or("").to_string(),
        title: str_of(&product["name"]).unwrap_or("").to_string(),
        circle_name: str_of(&product["organization"]["name"])
            .unwrap_or("")
            .to_string(),
        thumbnail_url: str_of(&product["coverImage"]["url"]).map(absolutize),
        format,
        caused_at: str_of(&node["causedAt"]).map(String::from),
        event_name: str_of(&event["name"]).map(String::from),
        event_slug: event_id.and_then(|id| normalize_event_slug(Some(id))),
        file_name: file_name.map(String::from),
        download_url,
        is_downloadable: !download_content.is_null(),
        tags: product.get("tags").and_then(Value::as_array).map(|tags| {
            tags.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        }),
    }
}

fn map_checked_product(node: &Value) -> Option<TbfChecklistEntry> {
    let product_info = &node["productInfo"];
    let circles = product_info
        .pointer("/organization/circles/edges")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let exhibit = choose_best_circle(&circles)?;
    if exhibit["hasOfflineCourse"].as_bool() == Some(false) {
        return None;
    }
    let event_slug = resolve_event_slug(str_of(&exhibit["event"]["databaseID"]))?;
    let product_identity = str_of(&product_info["databaseID"])
        .or_else(|| str_of(&product_info["id"]))
        .unwrap_or("")
        .to_string();
    let id = format!("{event_slug}:{product_identity}");
    Some(TbfChecklistEntry {
        id,
        circle_name: str_of(&product_info["organization"]["name"])
            .unwrap_or("")
            .to_string(),
        space_number: exhibit
            .pointer("/spaces/0")
            .and_then(Value::as_str)
            .unwrap_or("TBD")
            .to_string(),
        tbf_circle_id: str_of(&exhibit["databaseID"])
            .or_else(|| str_of(&exhibit["id"]))
            .map(String::from),
        product_id: Some(product_identity),
        product_title: str_of(&product_info["name"]).unwrap_or("").to_string(),
        thumbnail_url: str_of(&product_info["coverImage"]["url"]).map(absolutize),
        price: extract_price(product_info.get("productVariants")),
        is_purchased: !product_info["loginUserBookShelfItem"].is_null(),
        created_at: str_of(&node["createdAt"]).map(String::from),
    })
}

/// Lowest tbf number wins (`chooseBestCircle`).
fn choose_best_circle(edges: &[Value]) -> Option<&Value> {
    let mut best: Option<(&Value, u32)> = None;
    for edge in edges {
        let node = &edge["node"];
        let Some(slug) = resolve_event_slug(str_of(&node["event"]["databaseID"])) else {
            continue;
        };
        let Some(number) = parse_tbf_number(&slug) else {
            continue;
        };
        if best.is_none_or(|(_, score)| number < score) {
            best = Some((node, number));
        }
    }
    best.map(|(node, _)| node)
}

/// `tbf20` -> 20 (`parseTbfNumber`).
fn parse_tbf_number(slug: &str) -> Option<u32> {
    let digits = slug.strip_prefix("tbf")?;
    digits.parse::<u32>().ok()
}

/// `tbf\d+` match in a database id, lowercased (`resolveEventSlug`).
fn resolve_event_slug(database_id: Option<&str>) -> Option<String> {
    let database_id = database_id?;
    let lower = database_id.to_lowercase();
    let start = lower.find("tbf")?;
    let tail = &lower[start..];
    let end = tail
        .char_indices()
        .take_while(|(i, c)| *i < 3 || c.is_ascii_digit())
        .map(|(i, _)| i)
        .last()
        .unwrap_or(2);
    Some(tail[..=end].to_string())
}

/// First non-DRAFT price, else the first price (`extractPriceFromVariants`).
fn extract_price(variants: Option<&Value>) -> Option<i64> {
    let edges = variants?.pointer("/edges")?.as_array()?;
    for edge in edges {
        let variant = &edge["node"];
        if let Some(price) = variant.get("price").and_then(Value::as_i64)
            && variant.get("status").and_then(Value::as_str) != Some("DRAFT")
        {
            return Some(price);
        }
    }
    edges.first()?.get("node")?.get("price")?.as_i64()
}

/// books.ts `normalizeTechBookFestEventSlug`.
fn normalize_event_slug(value: Option<&str>) -> Option<String> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }
    if let Some(rest) = value.strip_prefix("Event:") {
        return Some(rest.to_string());
    }
    if let Some(slug) = resolve_event_slug(Some(value)) {
        return Some(slug);
    }
    let digits: String = value.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        Some(format!("tbf{digits}"))
    }
}

/// books.ts `isAuthRelatedGraphQLError`.
fn is_auth_related(payload: &Value) -> bool {
    let Some(errors) = payload.get("errors").and_then(Value::as_array) else {
        return false;
    };
    errors.iter().any(|error| {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let code = error
            .pointer("/extensions/code")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        [
            "unauthorized",
            "unauthenticated",
            "forbidden",
            "login",
            "session",
        ]
        .iter()
        .any(|needle| message.contains(needle) || code.contains(needle))
    })
}

/// Minimal `decodeURIComponent` for XSRF token values (%XX sequences).
fn percent_decode(input: &str) -> String {
    let bytes = input.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%'
            && i + 2 < bytes.len()
            && let (Some(hi), Some(lo)) = (hex_val(bytes[i + 1]), hex_val(bytes[i + 2]))
        {
            out.push(hi * 16 + lo);
            i += 3;
            continue;
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn hex_val(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Web `CANONICAL_EVENT_MASTERS` (events.ts) — verbatim data.
/// 対象イベントの開催期間（canonical_events のハードコード値）。
/// 期間が登録されていないイベント（tbf06 等）は None。
pub(crate) fn event_window(event_slug: &str) -> Option<(String, String)> {
    canonical_events()
        .iter()
        .find(|e| e.slug == event_slug)
        .and_then(|e| match (&e.event_start_date, &e.event_end_date) {
            (Some(start), Some(end)) => Some((start.clone(), end.clone())),
            _ => None,
        })
}

/// 購入日時（ISO8601）が開催期間（event_window の結果）内かを判定する。
/// checkedProductInfos の circles は criteria(eventID) で対象イベントに
/// 絞られるため、対象イベントに出展しているサークルの商品は**購入イベント
/// と無関係に** id が `{対象イベント}:` になってしまう。そこで購入日時で
/// 対象イベントの開催期間内かを確認し、他イベントで購入した商品を除外する。
/// 期間・日時が不明な場合は id 一致のみの従来判定にフォールバックする。
pub(crate) fn within_event_window(
    window: &Option<(String, String)>,
    created_at: Option<&str>,
) -> bool {
    let Some((start, end)) = window else {
        return true;
    };
    let Some(created) = created_at else {
        return true;
    };
    let day = created.get(..10).unwrap_or(created);
    day >= start.as_str() && day <= end.as_str()
}

pub(crate) fn canonical_events() -> Vec<TbfEventInfo> {
    type CanonicalEvent = (
        &'static str,
        &'static str,
        Option<&'static str>,
        Option<&'static str>,
        &'static str,
        bool,
    );
    const ENTRIES: &[CanonicalEvent] = &[
        (
            "tbf20",
            "技術書典20",
            Some("2026-04-11"),
            Some("2026-04-26"),
            "hybrid",
            false,
        ),
        (
            "tbf19",
            "技術書典19",
            Some("2025-11-15"),
            Some("2025-11-30"),
            "hybrid",
            false,
        ),
        (
            "tbf18",
            "技術書典18",
            Some("2025-05-31"),
            Some("2025-06-15"),
            "hybrid",
            false,
        ),
        (
            "tbf17",
            "技術書典17",
            Some("2024-11-02"),
            Some("2024-11-17"),
            "hybrid",
            false,
        ),
        (
            "tbf16",
            "技術書典16",
            Some("2024-05-25"),
            Some("2024-06-09"),
            "hybrid",
            false,
        ),
        (
            "tbf15",
            "技術書典15",
            Some("2023-11-11"),
            Some("2023-11-26"),
            "hybrid",
            false,
        ),
        (
            "tbf14",
            "技術書典14",
            Some("2023-05-20"),
            Some("2023-06-04"),
            "hybrid",
            false,
        ),
        (
            "tbf13",
            "技術書典13",
            Some("2022-09-10"),
            Some("2022-09-25"),
            "hybrid",
            false,
        ),
        (
            "tbf12",
            "技術書典12",
            Some("2022-01-22"),
            Some("2022-01-30"),
            "online",
            false,
        ),
        (
            "tbf11",
            "技術書典11",
            Some("2021-07-10"),
            Some("2021-07-25"),
            "hybrid",
            false,
        ),
        (
            "tbf10",
            "技術書典10",
            Some("2020-12-26"),
            Some("2021-01-06"),
            "online",
            false,
        ),
        (
            "tbf9",
            "技術書典9",
            Some("2020-09-12"),
            Some("2020-09-22"),
            "online",
            false,
        ),
        (
            "tbf8",
            "技術書典8",
            Some("2020-02-29"),
            Some("2020-03-01"),
            "offline",
            true,
        ),
        (
            "tbf-ouen-matsuri",
            "技術書典 応援祭",
            Some("2020-03-07"),
            Some("2020-04-05"),
            "online",
            false,
        ),
        (
            "tbf7",
            "技術書典7",
            Some("2019-09-22"),
            Some("2019-09-22"),
            "offline",
            false,
        ),
        (
            "tbf6",
            "技術書典6",
            Some("2019-04-14"),
            Some("2019-04-14"),
            "offline",
            false,
        ),
        (
            "tbf5",
            "技術書典5",
            Some("2018-10-08"),
            Some("2018-10-08"),
            "offline",
            false,
        ),
        (
            "tbf4",
            "技術書典4",
            Some("2018-04-22"),
            Some("2018-04-22"),
            "offline",
            false,
        ),
        (
            "tbf3",
            "技術書典3",
            Some("2017-10-22"),
            Some("2017-10-22"),
            "offline",
            false,
        ),
        (
            "tbf2",
            "技術書典2",
            Some("2017-04-09"),
            Some("2017-04-09"),
            "offline",
            false,
        ),
        (
            "tbf1",
            "技術書典1",
            Some("2016-06-25"),
            Some("2016-06-25"),
            "offline",
            false,
        ),
    ];
    ENTRIES
        .iter()
        .enumerate()
        .map(
            |(index, (slug, name, start, end, format, cancelled))| TbfEventInfo {
                slug: (*slug).to_string(),
                tbf_event_id: format!("Event:{slug}"),
                event_name: (*name).to_string(),
                event_date: start.map(String::from),
                event_start_date: start.map(String::from),
                event_end_date: end.map(String::from),
                event_format: (*format).to_string(),
                is_cancelled: *cancelled,
                display_order: index as i64,
                is_featured: index == 0,
            },
        )
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ダウンロードの Cookie / XSRF は**収集元ホスト宛のときだけ**付ける。
    ///
    /// 技術書典の本体は `storage.googleapis.com`（`/tbf-tokyo-product-dlc/`）から落ちる。
    /// ここへセッション Cookie を付けると、別システム（Google）へ資格情報が飛ぶ。
    #[test]
    fn download_headers_send_cookies_only_to_the_site_host() {
        let cookies = "session=sess-123";
        // 本番と同じ組み立て（`User-Agent` は常に付け、資格情報は宛先ごとに足す）
        let build = |cookie: &str, token: Option<&str>, url: &str| {
            let mut headers = vec![("User-Agent".to_string(), USER_AGENT.to_string())];
            headers.extend(download_credential_headers(cookie, token, url));
            headers
        };
        // 本体（GCS）へは付けない
        let gcs = build(
            cookies,
            Some("tok"),
            "https://storage.googleapis.com/tbf-tokyo-product-dlc/x.zip",
        );
        assert!(
            !gcs.iter().any(|(name, _)| name == "Cookie"),
            "別ホストへ Cookie を送っている: {gcs:?}"
        );
        assert!(
            !gcs.iter().any(|(name, _)| name == "X-XSRF-TOKEN"),
            "別ホストへ XSRF を送っている: {gcs:?}"
        );
        assert!(
            gcs.iter()
                .any(|(name, value)| name == "User-Agent" && !value.is_empty()),
            "User-Agent は付ける: {gcs:?}"
        );

        // サイト自身（API 経由の本体）へは従来どおり付ける
        let site = build(
            cookies,
            Some("tok"),
            "https://techbookfest.org/api/product-dlc/xyz",
        );
        assert!(
            site.iter()
                .any(|(name, value)| name == "Cookie" && value == cookies),
            "サイト宛に Cookie を付けていない: {site:?}"
        );
        assert!(
            site.iter()
                .any(|(name, value)| name == "X-XSRF-TOKEN" && value == "tok"),
            "サイト宛に XSRF を付けていない: {site:?}"
        );

        // URL を解釈できないときは付けない（fail-closed）
        let broken = build(cookies, Some("tok"), "not-a-url");
        assert!(
            !broken.iter().any(|(name, _)| name == "Cookie"),
            "解釈できない URL へ Cookie を送っている: {broken:?}"
        );
    }

    #[test]
    fn session_from_cookies_extracts_xsrf() {
        let session = TbfSession::from_cookies(vec![
            ("XSRF-TOKEN".to_string(), "abc%2Bdef".to_string()),
            ("session".to_string(), "sess-123".to_string()),
            ("locale".to_string(), "ja".to_string()),
        ]);
        assert_eq!(session.xsrf_raw, "abc%2Bdef");
        assert_eq!(session.xsrf_token, "abc+def");
        assert!(session.is_logged_in());
        assert!(
            session
                .cookies
                .iter()
                .any(|(n, v)| n == "session" && v == "sess-123")
        );
    }

    #[test]
    fn session_without_xsrf_is_still_usable() {
        let session = TbfSession::from_cookies(vec![("session".to_string(), "s".to_string())]);
        assert_eq!(session.xsrf_raw, "");
        assert_eq!(session.xsrf_token, "");
        assert!(session.is_logged_in());
    }

    #[test]
    fn xsrf_only_session_is_not_logged_in() {
        let session = TbfSession::from_cookies(vec![("XSRF-TOKEN".to_string(), "x".to_string())]);
        assert!(!session.is_logged_in());
    }

    #[test]
    fn purchase_date_filters_out_other_events() {
        // tbf20 の開催期間は 2026-04-11〜04-26
        let window = event_window("tbf20");
        assert!(window.is_some(), "tbf20 must have a known window");
        let within = |created: &str| within_event_window(&window, Some(created));
        // tbf20 開催期間内の購入 → 保持
        assert!(within("2026-04-12T11:53:48.345105+09:00"));
        // tbf19 の開催期間内（2025-11-13）の購入 → 除外
        assert!(!within("2025-11-13T22:48:08.541519+09:00"));
        // 期間境界（開始日・終了日）も含む
        assert!(within("2026-04-11T00:00:00+09:00"));
        assert!(within("2026-04-26T23:59:59+09:00"));
    }

    #[test]
    fn unknown_event_window_falls_back_to_include() {
        // canonical に期間の無いイベント（tbf06 等）は id 一致のみで判定
        assert!(event_window("tbf06").is_none());
        assert!(within_event_window(
            &None,
            Some("2020-03-07T10:00:00+09:00")
        ));
        // created_at 不明も保持（フォールバック）
        assert!(within_event_window(
            &Some(("2026-04-11".into(), "2026-04-26".into())),
            None
        ));
    }
}
