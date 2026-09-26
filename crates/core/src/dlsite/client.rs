//! DLsite の購入済み作品クライアント。
//!
//! 購入一覧はストア別の `mypage/userbuy/.../page/{n}` HTML（公開 JSON API は無い）を
//! スクレイプする。作品メタは `product/info/ajax`（Cookie + XHR ヘッダ）で取得する。
//! ダウンロードは `down_url` を 302 追跡して `download.dlsite.com` の実 ZIP を取得する
//! （`jwt` 署名 Cookie を 302 の Set-Cookie で捕捉して CDN へ送る）。`Transport` は
//! `tbf::transport` を再利用（モック可能）。

use std::collections::{BTreeMap, HashMap};

use crate::session_cookies::{CookieEntry, HostScopedCookies};
use crate::tbf::TbfError;
use crate::tbf::transport::{RequestSpec, Transport};

/// 列挙するストアフロア。`soft`（PC ソフト）/ `app`（スマホゲーム）はゲーム系・対象外。
pub const STORES: [&str; 4] = ["maniax", "home", "books", "ai"];
/// 購入履歴の 1 ページあたりの行数境界（実測は未確認だが、過剰アクセス防止の上限）。
pub const MAX_PAGES_PER_STORE: usize = 200;

/// DLsite は以前「非ブラウザ UA を弾く」観測があったためブラウザ UA + Referer を送っていた。
/// いまは **UA だけ自認に変えても購入履歴が 200 で返る**ことを実機で確認済み
/// （Referer と `Sec-Fetch-*` / `Accept` は引き続き送る）。
/// ストアへ送る UA。**クライアントを名乗る**（`ThundokuShelf/<version> (+リポジトリ URL)`）。
///
/// 以前は Chrome を名乗っていた（DLsite は非ブラウザ UA を弾くため）。購入履歴が
/// この UA でも 200 で取れることを実測で確認したので、なりすましはやめた。
///
/// サイト側が弾くようになったら、環境変数 `THUNDOKU_DLSITE_UA` にブラウザ UA を入れて
/// 起動すれば元の挙動に戻せる（`crate::ua`）。
const USER_AGENT: &str = concat!(
    "ThundokuShelf/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/MegaBlackLabel/thundoku-shelf-app)"
);

/// 送る UA。既定は上の定数（`THUNDOKU_DLSITE_UA` で差し替えられる → `crate::ua`）。
fn user_agent() -> String {
    crate::ua::for_store(USER_AGENT, crate::ua::ENV_DLSITE)
}

/// ダウンロード要求を送ってよいホスト（**完全一致**）とパス。
///
/// `bookshelf_items.download_url`（= `down_url`）は Drive の JSON バックアップから
/// 復元でき、改変したバックアップを復元させると外部ホストへリクエストを飛ばせる。
/// 実測の `down_url` は `https://www.dlsite.com/{store}/download/=/product_id/{id}.html`
/// なので、サブドメインまで許す必要が無い（Cookie は宛先別に絞ってあるため漏れはしないが、
/// 未認証のリクエストも飛ばさないほうが安全）。
///
/// **パスも `/{store}/download/` に限る**: ストアは同期対象の [`STORES`] だけを並べる
/// （同期で一覧に入らないストアの URL は、改変された `download_url` と見なして拒否する）。
/// 有効な store を増やすときは [`STORES`] と**両方**を直す（取りこぼすとダウンロードが
/// `BlockedUrl` で止まる。`download_rules_cover_every_store` が検知する）。
const DLSITE_DOWNLOAD_RULES: &[crate::download_url::HostRule] = &[
    crate::download_url::HostRule::exact("www.dlsite.com", Some("/maniax/download/")),
    crate::download_url::HostRule::exact("www.dlsite.com", Some("/home/download/")),
    crate::download_url::HostRule::exact("www.dlsite.com", Some("/books/download/")),
    crate::download_url::HostRule::exact("www.dlsite.com", Some("/ai/download/")),
];

/// 302 の転送先（署名 `jwt` + セッション Cookie を送る先）。
///
/// 実測の CDN は `download.dlsite.com` だけなので**完全一致**にする（`www` / `login` は
/// Cookie の収集元だが CDN ではない。署名 `jwt` を送る先を実測値に限る）。
const DLSITE_CDN_RULES: &[crate::download_url::HostRule] =
    &[crate::download_url::HostRule::exact(
        "download.dlsite.com",
        None,
    )];

/// CDN 取得で追うリダイレクトの上限（各ホップは [`DLSITE_CDN_RULES`] で検証する）。
const DLSITE_REDIRECT_LIMIT: usize = 3;

#[derive(Debug, thiserror::Error)]
pub enum DlsiteError {
    #[error("セッション切れ・未ログイン")]
    SessionExpired,
    #[error("不正アクセス（{0}）")]
    Unauthorized(u16),
    #[error("ダウンロード不可（販売終了・未購入等）")]
    NotDownloadable,
    #[error("HTTP {0}")]
    Http(u16),
    #[error("parsing failed: {0}")]
    Parse(String),
    /// 送信先が許可リストに無い（資格情報を送らない）。
    #[error("blocked download url: {0}")]
    BlockedUrl(String),
    #[error("db: {0}")]
    Database(#[from] sqlx::Error),
    #[error("transport: {0}")]
    Transport(#[from] TbfError),
}

impl From<serde_json::Error> for DlsiteError {
    fn from(e: serde_json::Error) -> Self {
        DlsiteError::Parse(e.to_string())
    }
}

/// ストア（`www.dlsite.com`）。Cookie を集める側のホスト。
pub const SITE_HOST: &str = "www.dlsite.com";
/// ログイン（`login.dlsite.com`）。SSO の Cookie はここでも発行される。
pub const LOGIN_HOST: &str = "login.dlsite.com";

/// DLsite のセッション（`www.dlsite.com` / `login.dlsite.com` の Cookie 群）。
///
/// Cookie は**収集元ホストごと**に持つ。1 つに潰すと、片方にしか送るべきでない Cookie が
/// もう片方や別システム（CDN）へ飛ぶ。送信先ごとに取り出せるようにホスト別で保つ。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct DlsiteSession {
    /// 収集元ホスト →（Cookie 名 → 値）。宛先ごとに絞って送る（`cookie_header_for`）。
    origins: HostScopedCookies,
}

impl DlsiteSession {
    /// 収集元ホスト →（Cookie 名 → 属性つき Cookie）。ログイン時に WebView から作る。
    pub fn new(origins: BTreeMap<String, BTreeMap<String, CookieEntry>>) -> Self {
        Self {
            origins: HostScopedCookies::from_named(origins),
        }
    }

    /// ログイン時に WebView から収集した形（ホスト → Cookie の並び）から作る。
    pub fn from_collected(origins: BTreeMap<String, Vec<CookieEntry>>) -> Self {
        Self {
            origins: HostScopedCookies::new(origins),
        }
    }

    /// `www.dlsite.com` の Cookie だけで作る。
    pub fn from_site_cookies(cookies: BTreeMap<String, String>) -> Self {
        Self {
            origins: HostScopedCookies::from_origin(SITE_HOST, cookies),
        }
    }

    pub fn logged_in(&self) -> bool {
        !self.origins.is_empty()
    }

    /// 収集元ホストの Cookie 値（認証済み判定・ログ用）。収集元をまたがない。
    pub fn cookie_value(&self, host: &str, name: &str) -> Option<&str> {
        self.origins.value(host, name)
    }

    /// 宛先 URL 向けの Cookie ヘッダ。**その URL のホスト向けに収集した Cookie だけ**を返し、
    /// `Path` / `Secure` / 期限も満たすものだけを載せる（`HostScopedCookies::header_for_url`）。
    ///
    /// 収集元すべてをまとめる API は**持たない**: `down_url` の proxy のように宛先が実行時に
    /// 決まる送信先へ、片方の収集元にしか送るべきでない Cookie を載せないため。
    pub fn cookie_header_for_url(&self, url: &str) -> String {
        self.origins.header_for_url(url)
    }

    pub fn cookies_count(&self) -> usize {
        self.origins.count()
    }
}

/// 購入履歴の 1 ページ分（1 ストア）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DlsitePurchasesPage {
    /// このページの作品。
    pub items: Vec<DlsitePurchase>,
    /// そのストアの最終ページ番号（ページャから。取れなければ `None`）。
    pub last_page: Option<usize>,
}

/// 購入履歴ページの 1 作品（`#buy_history_this table.work_list_main tr` 由来）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DlsitePurchase {
    pub content_id: String,
    /// ダウンロード URL のストアセグメント由来（`maniax` / `home` / `ai` 等）。
    pub site_id: String,
    pub title: String,
    pub work_type: String,
    pub genre_icons: Vec<String>,
    pub maker_name: String,
    pub maker_id: Option<String>,
    pub price: Option<String>,
    pub purchase_date: Option<String>,
    pub thumbnail_url: Option<String>,
    pub down_url: Option<String>,
}

/// `product/info/ajax` の主要フィールド（作品メタ）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DlsiteWorkMeta {
    pub site_id: String,
    pub work_type: String,
    pub maker_id: Option<String>,
    pub work_name: String,
    pub regist_date: Option<String>,
    pub price: Option<i64>,
    pub down_url: Option<String>,
    pub custom_genres: Vec<String>,
    pub options: String,
    pub age_category: Option<i64>,
    pub title_name: Option<String>,
    /// 作品画像（`//img.dlsite.jp/...img_main.jpg`）。ユーザーページの静的 HTML には
    /// サムネイル URL が入らない（data: プレースホルダのみ）ため、表紙はこれを使う。
    pub work_image: Option<String>,
}

/// DLsite クライアント（`tbf::transport` を再利用、`Transport` でモック可能）。
pub struct DlsiteClient {
    transport: Box<dyn Transport>,
    session: DlsiteSession,
}

impl DlsiteClient {
    pub fn with_transport(transport: Box<dyn Transport>, session: DlsiteSession) -> Self {
        Self { transport, session }
    }

    /// 宛先 URL 向けのヘッダ。`Cookie` は**その URL のホスト向けに収集したものだけ**を載せる
    /// （収集元をまたいで 1 本にまとめない。`Path` / `Secure` / 期限も見る）。
    fn cookie_headers_for(&self, url: &str) -> Vec<(String, String)> {
        vec![
            ("Cookie".to_string(), self.session.cookie_header_for_url(url)),
            ("User-Agent".to_string(), user_agent()),
            ("Referer".to_string(), "https://www.dlsite.com/".to_string()),
            // DLsite は non-browser リクエストをアプリ認証で弾く（Sec-Fetch / Accept が
            // 無いと購入履歴が regist/user へ 302 される）ため、ブラウザ相当のヘッダを送る。
            (
                "Accept".to_string(),
                "text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8".to_string(),
            ),
            ("Accept-Language".to_string(), "ja,en;q=0.9".to_string()),
            ("Sec-Fetch-Dest".to_string(), "document".to_string()),
            ("Sec-Fetch-Mode".to_string(), "navigate".to_string()),
            ("Sec-Fetch-Site".to_string(), "same-origin".to_string()),
            ("Upgrade-Insecure-Requests".to_string(), "1".to_string()),
        ]
    }

    fn ajax_headers_for(&self, host: &str) -> Vec<(String, String)> {
        let mut h = self.cookie_headers_for(host);
        if let Some((_, v)) = h.iter_mut().find(|(k, _)| k == "Accept") {
            *v = "application/json".to_string();
        }
        h.push(("X-Requested-With".to_string(), "XMLHttpRequest".to_string()));
        h
    }

    /// 1 ストアの購入履歴 1 ページを取得する（1 始まり）。
    ///
    /// 分割同期はこれを 1 ページずつ呼ぶ（1 回で全ページ取ると、購入数の多い
    /// アカウントでは 1 操作で大量のリクエストになる）。
    pub fn purchased_page(
        &mut self,
        store: &str,
        page: usize,
    ) -> Result<DlsitePurchasesPage, DlsiteError> {
        let url = format!(
            "https://www.dlsite.com/{store}/mypage/userbuy/=/type/all/start/all/sort/1/order/1/page/{page}"
        );
        let html = self.get_html(&url)?;
        // セッションが切れているとログインページが **200 で**返る（HTTP では気づけない）。
        // 「購入 0 件」と取り違えると、何も取り込まないまま「同期完了」になってしまう。
        if is_login_page(&html) {
            return Err(DlsiteError::SessionExpired);
        }
        let (items, last_page) = parse_userbuy_page(&html);
        Ok(DlsitePurchasesPage { items, last_page })
    }

    /// 作品メタを取得する（`product/info/ajax`、複数 ID はカンマ区切り一括）。
    pub fn product_info(
        &mut self,
        ids: &[&str],
    ) -> Result<HashMap<String, DlsiteWorkMeta>, DlsiteError> {
        let mut out = HashMap::new();
        for chunk in ids.chunks(20) {
            let joined = chunk.join(",");
            let url =
                format!("https://www.dlsite.com/maniax/product/info/ajax?product_id={joined}");
            let headers = self.ajax_headers_for(&url);
            let resp = self
                .transport
                .send(RequestSpec {
                    method: "GET".into(),
                    url,
                    headers,
                    body: None,
                    redirects: 3,
                })
                .map_err(DlsiteError::Transport)?;
            if resp.status == 401 || resp.status == 403 {
                return Err(DlsiteError::Unauthorized(resp.status));
            }
            if resp.status != 200 {
                return Err(DlsiteError::Http(resp.status));
            }
            let json = String::from_utf8_lossy(&resp.body).to_string();
            for (id, meta) in parse_product_info(&json) {
                out.insert(id, meta);
            }
        }
        Ok(out)
    }

    /// 作品ページ（`/maniax/work/=/product_id/{id}.html`）の「作者」行を取得する。
    /// DLsite は store を跨いでも作品ページが解決されるため maniax 固定でよい
    /// （`product_info` も maniax 固定）。作者行が無い作品は `None`。
    pub fn work_page_author(&mut self, content_id: &str) -> Result<Option<String>, DlsiteError> {
        Ok(self.work_page_meta(content_id)?.author)
    }

    /// 作品ページから「作者」と「ファイル容量」をまとめて取る（**取得は 1 回**）。
    ///
    /// ダウンロード前のサイズ確認（2 GiB 超）に使う。容量が読めない作品は `None`
    /// （＝サイズ不明。確認は出さない）。
    pub fn work_page_meta(
        &mut self,
        content_id: &str,
    ) -> Result<DlsiteWorkPageMeta, DlsiteError> {
        let url = format!("https://www.dlsite.com/maniax/work/=/product_id/{content_id}.html");
        let html = self.get_html(&url)?;
        Ok(DlsiteWorkPageMeta {
            author: parse_work_page_author(&html),
            file_size: parse_work_file_size(&html),
        })
    }

    /// `down_url` を 302 追跡して `download.dlsite.com` の実 ZIP を取得する。
    /// 302 の Set-Cookie で `jwt` を捕捉し、CDN へ直接（署名 Cookie 付き）取得する。
    pub fn download_with_progress(
        &mut self,
        down_url: &str,
        on_progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<Vec<u8>, DlsiteError> {
        // 1) down_url を manual（redirects=0）で叩いて 302 Location を取得
        //
        // 送信先は `bookshelf_items.download_url`（改変バックアップ由来もあり得る）と
        // その 302 の `Location`。Cookie を付ける前に双方を検証する。
        let proxy = crate::download_url::check(down_url, DLSITE_DOWNLOAD_RULES)
            .map_err(|error| DlsiteError::BlockedUrl(format!("{down_url}: {error}")))?;
        // proxy へは**宛先 URL のホスト向けに収集した Cookie だけ**を送る。www と login を
        // 1 本にまとめると、`down_url` が別サブドメインを指したときにもう片方の収集元の Cookie まで飛ぶ。
        let proxy_headers = self.cookie_headers_for(proxy.url.as_str());
        let proxy_spec = RequestSpec {
            method: "GET".into(),
            // 検証した URL（正規化済み）をそのまま送る（検証した宛先 = 送る宛先）
            url: proxy.url.as_str().into(),
            headers: proxy_headers,
            body: None,
            redirects: 0,
        };
        let proxy_resp = self
            .transport
            .send(proxy_spec)
            .map_err(DlsiteError::Transport)?;
        let cd_url = if proxy_resp.status == 302 {
            proxy_resp
                .header("location")
                .ok_or_else(|| DlsiteError::Parse("302 応答に location がありません".into()))?
                .to_string()
        } else if proxy_resp.status == 401 || proxy_resp.status == 403 {
            return Err(DlsiteError::Unauthorized(proxy_resp.status));
        } else if proxy_resp.status == 404 || proxy_resp.status == 410 {
            return Err(DlsiteError::NotDownloadable);
        } else {
            return Err(DlsiteError::Http(proxy_resp.status));
        };
        // 転送先も検証する（任意ホストへ Cookie を転送させない）。
        let cdn = crate::download_url::check(&cd_url, DLSITE_CDN_RULES)
            .map_err(|error| DlsiteError::BlockedUrl(format!("{cd_url}: {error}")))?;
        // 実機で観測したホストを残す（許可リストを実測値まで狭めるための材料）。
        log::info!("dlsite download: proxy={} cdn={}", proxy.host, cdn.host);
        // 2) CDN へ送る Cookie は**そのホスト向けに収集したもの + 署名 `jwt`** だけ。
        //    www の認証セッションは別システム（CDN）には要らないので送らない。
        //    `jwt` は 302 の Set-Cookie で渡される署名付きダウンロード鍵。
        //
        //    転送は**各ホップ検証**して自前で追う（`ureq` に任せると `Cookie` は落ちても
        //    他のヘッダをそのまま残すため、`Location` の差し替えで許可外ホストへ飛び得る）。
        //    資格情報はホップごとに組み立て直す（許可外のホストには載せない）。
        let jwt: Option<String> = proxy_resp
            .set_cookies()
            .into_iter()
            .find(|(name, _)| name == "jwt")
            .map(|(_, value)| value);
        let session = &self.session;
        let mut credentials = |destination: &url::Url| {
            let mut cookie = session.cookie_header_for_url(destination.as_str());
            if let Some(jwt) = &jwt
                && !cookie.contains("jwt=")
            {
                if !cookie.is_empty() {
                    cookie.push_str("; ");
                }
                cookie.push_str(&format!("jwt={jwt}"));
            }
            // Cookie が 1 つも無ければヘッダを付けない（空の Cookie は送らない）。
            if cookie.is_empty() {
                Vec::new()
            } else {
                vec![("Cookie".to_string(), cookie)]
            }
        };
        // 3) CDN（download.dlsite.com）へ直接取得
        let headers = vec![
            ("User-Agent".to_string(), user_agent()),
            ("Referer".to_string(), "https://www.dlsite.com/".to_string()),
            (
                "Accept".to_string(),
                "text/html,application/xhtml+xml,application/xml;q=0.9,*/*;q=0.8".to_string(),
            ),
            ("Accept-Language".to_string(), "ja,en;q=0.9".to_string()),
            ("Sec-Fetch-Dest".to_string(), "document".to_string()),
            ("Sec-Fetch-Mode".to_string(), "navigate".to_string()),
            ("Sec-Fetch-Site".to_string(), "cross-site".to_string()),
        ];
        let resp = crate::tbf::redirect::send_with_validated_redirects(
            self.transport.as_mut(),
            RequestSpec {
                method: "GET".into(),
                url: cdn.url.as_str().into(),
                headers,
                body: None,
                redirects: 0,
            },
            DLSITE_REDIRECT_LIMIT,
            DLSITE_CDN_RULES,
            &mut credentials,
            on_progress,
        )
        .map_err(DlsiteError::Transport)?;
        if resp.status == 401 || resp.status == 403 {
            return Err(DlsiteError::Unauthorized(resp.status));
        }
        if resp.status != 200 {
            return Err(DlsiteError::Http(resp.status));
        }
        if resp.body.starts_with(b"<!doctype") || resp.body.starts_with(b"<html") {
            return Err(DlsiteError::Parse(
                "HTML レスポンス（ファイルではない）".into(),
            ));
        }
        Ok(resp.body)
    }

    /// HTML ページを取得する（購入履歴・作品ページ）。Cookie は**宛先 URL のホスト向け**だけを
    /// 載せる（`login` の Cookie は載らない）。
    fn get_html(&mut self, url: &str) -> Result<String, DlsiteError> {
        let headers = self.cookie_headers_for(url);
        let resp = self
            .transport
            .send(RequestSpec {
                method: "GET".into(),
                url: url.into(),
                headers,
                body: None,
                redirects: 3,
            })
            .map_err(DlsiteError::Transport)?;
        if resp.status == 401 || resp.status == 403 {
            return Err(DlsiteError::Unauthorized(resp.status));
        }
        if resp.status != 200 {
            return Err(DlsiteError::Http(resp.status));
        }
        Ok(String::from_utf8_lossy(&resp.body).to_string())
    }
}

/// 購入履歴の HTML が**ログイン（会員登録）ページ**か。
///
/// DLsite はセッションが切れていると `login.dlsite.com/register?user=self` を **200 で**
/// 返す（＝HTTP ステータスでは気づけない）。これを「購入履歴 0 件」と取り違えると、
/// 何も取り込まないまま「同期完了（0 件）」と表示してしまう。
///
/// viviON ID の SSO 画面にだけ出るクラス名で判定する（ストアの購入履歴ページには
/// `#buy_history_this` があり、こちらは出ない）。
fn is_login_page(html: &str) -> bool {
    html.contains("contentLoginRegist") || html.contains("contentLoginLogin")
}

/// 戻り値: `(作品行のベクタ, 最終ページ番号)`。
pub fn parse_userbuy_page(html: &str) -> (Vec<DlsitePurchase>, Option<usize>) {
    let mut rows = Vec::new();
    let seg_re = regex::Regex::new(r"(?s)<tr\b.*?</tr>").unwrap();
    for seg in seg_re.find_iter(html) {
        let s = seg.as_str();
        // ヘッダ行（`.item_name`）等、作品行でないものは飛ばす
        if !s.contains("work_name") || !s.contains("product_id") {
            continue;
        }
        if let Some(p) = parse_row(s) {
            rows.push(p);
        }
    }
    (rows, parse_last_page(html))
}

/// 1 つの `<tr>` 行から購入作品を抽出する。
fn parse_row(s: &str) -> Option<DlsitePurchase> {
    fn cap(re: &str, hay: &str, idx: usize) -> Option<String> {
        regex::Regex::new(re)
            .ok()?
            .captures(hay)
            .map(|c| c.get(idx).unwrap().as_str().trim().to_string())
    }
    let content_id = cap(r"(?i)product_id\/(RJ\d+)\.html", s, 1)?;
    let title = cap(r#"class="work_name"[^>]*>\s*<a[^>]*>([^<]*)</a>"#, s, 1).unwrap_or_default();
    let maker_name =
        cap(r#"class="maker_name"[^>]*>\s*<a[^>]*>([^<]*)</a>"#, s, 1).unwrap_or_default();
    let maker_id = cap(r"(?i)maker_id\/(RG\d+)\.html", s, 1);
    let price = cap(r#"class="work_price"[^>]*>\s*([^<]*)"#, s, 1);
    let purchase_date = cap(r#"class="buy_date"[^>]*>\s*([^<]*)"#, s, 1);
    let thumbnail_url = cap(r#"<source[^>]*srcset="([^"]+)""#, s, 1)
        .and_then(|v| v.split_whitespace().next().map(String::from))
        .or_else(|| cap(r#"<img[^>]*data-src="([^"]+)""#, s, 1).filter(|u| !u.starts_with("data:")))
        .or_else(|| cap(r#"<img[^>]*src="([^"]+)""#, s, 1).filter(|u| !u.starts_with("data:")));
    let down_url = cap(r#"href="([^"]*\/download\/[^"]*)""#, s, 1);
    let site_id = cap(r#"https?://www\.dlsite\.com/([a-z0-9]+)/"#, s, 1).unwrap_or_default();
    // ジャンルアイコン: work_genre ブロック内の `icon_XXX` クラス群
    let genre_icons = regex::Regex::new(r#"(?s)class="work_genre"[^>]*>(.*?)</dd>"#)
        .ok()
        .and_then(|re| re.captures(s))
        .map(|c| {
            let block = &c[1];
            regex::Regex::new(r#"class="icon_([A-Za-z0-9]+)""#)
                .unwrap()
                .captures_iter(block)
                .map(|m| format!("icon_{}", &m[1]))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    // work_type はジャンルアイコンの中で既知の `work_type` コード（MNG/ICG 等）から導出
    let work_type = genre_icons
        .iter()
        .find_map(|i| {
            i.strip_prefix("icon_")
                .filter(|code| crate::dlsite::media_from_work_type(code).is_some())
                .map(String::from)
        })
        .unwrap_or_default();
    Some(DlsitePurchase {
        content_id,
        site_id,
        title,
        work_type,
        genre_icons,
        maker_name,
        maker_id,
        price,
        purchase_date,
        thumbnail_url,
        down_url,
    })
}

/// 作品ページ HTML の「ファイル容量」（例 `107.84MB`）をバイト数へ直す。
///
/// 行が無い / 値が読めない作品は `None`（＝サイズ不明。確認は出さない）。
/// 実機 HTML（2026-09-26 / RJ01412386）で検証済み:
/// `<th>ファイル容量</th><td><div class="main_genre">107.84MB</div></td>`
pub fn parse_work_file_size(html: &str) -> Option<u64> {
    // 「ファイル容量」行の `<td>` ブロックに限定する（他の行の数値と混ざらないように）。
    let row = regex::Regex::new(r"(?s)<th>\s*ファイル容量\s*</th>\s*<td>(.*?)</td>").ok()?;
    let block = row.captures(html)?.get(1)?.as_str();
    let value = regex::Regex::new(r"([0-9]+(?:\.[0-9]+)?\s*[KMGT]?B)").ok()?.captures(block)?;
    crate::store_size::parse_store_size(&value[1])
}

/// 作品ページ HTML の「作者」行（`<th>作者</th>` の直後の `<a>` テキスト）を抽出する。
/// 実機 HTML（2026-09-11 / RJ01412386）で検証済み。行が無い / 空の作品は `None`。
pub fn parse_work_page_author(html: &str) -> Option<String> {
    let re = regex::Regex::new(r"(?s)<th>\s*作者\s*</th>.*?<a[^>]*>\s*([^<]*?)\s*</a>").ok()?;
    re.captures(html)
        .map(|cap| cap[1].trim().to_string())
        .filter(|author| !author.is_empty())
}

/// 作品ページから取れる補足情報（作者とファイル容量）。
///
/// 作者は取り込み時に本棚へ入れ、ファイル容量はダウンロード前の確認（2 GiB 超）に使う。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DlsiteWorkPageMeta {
    pub author: Option<String>,
    /// 宣言されたファイル容量（バイト）。読めなければ `None`。
    pub file_size: Option<u64>,
}

/// 作品ページ HTML の `product/info/ajax` レスポンス（JSON）を解析する。
pub fn parse_product_info(json: &str) -> HashMap<String, DlsiteWorkMeta> {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or(serde_json::Value::Null);
    let mut out = HashMap::new();
    if let serde_json::Value::Object(map) = &v {
        for (id, o) in map {
            let meta = DlsiteWorkMeta {
                site_id: s(o, "site_id"),
                work_type: s(o, "work_type"),
                maker_id: s_opt(o, "maker_id"),
                work_name: s(o, "work_name"),
                regist_date: s_opt(o, "regist_date"),
                price: o.get("price").and_then(serde_json::Value::as_i64),
                down_url: s_opt(o, "down_url"),
                custom_genres: arr_str(o, "custom_genres"),
                options: s(o, "options"),
                age_category: o.get("age_category").and_then(serde_json::Value::as_i64),
                title_name: s_opt(o, "title_name"),
                work_image: s_opt(o, "work_image"),
            };
            out.insert(id.clone(), meta);
        }
    }
    out
}

/// ページング HTML から最終ページ番号（`page_no` 最大値 / `最後` リンク）を解析する。
pub fn parse_last_page(html: &str) -> Option<usize> {
    let page_re = regex::Regex::new(r"/page/(\d+)").unwrap();
    let max = page_re
        .captures_iter(html)
        .filter_map(|c| c.get(1).and_then(|m| m.as_str().parse::<usize>().ok()))
        .max();
    max.filter(|&m| m > 0)
}

fn s(v: &serde_json::Value, key: &str) -> String {
    v.get(key)
        .and_then(serde_json::Value::as_str)
        .unwrap_or("")
        .to_string()
}

fn s_opt(v: &serde_json::Value, key: &str) -> Option<String> {
    v.get(key)
        .and_then(serde_json::Value::as_str)
        .map(|s| s.to_string())
}

fn arr_str(v: &serde_json::Value, key: &str) -> Vec<String> {
    v.get(key)
        .and_then(serde_json::Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tbf::transport::{RequestSpec, ResponseSpec, Transport};

    fn session() -> DlsiteSession {
        DlsiteSession::from_site_cookies(BTreeMap::from([(
            "__DLsite_SID".to_string(),
            "abc".to_string(),
        )]))
    }

    /// リクエストの `Cookie` ヘッダ（無ければ空文字）。
    fn cookie_of(spec: &RequestSpec) -> String {
        spec.headers
            .iter()
            .find(|(k, _)| k == "Cookie")
            .map(|(_, v)| v.clone())
            .unwrap_or_default()
    }

    struct MockTransport {
        handler: Box<dyn FnMut(RequestSpec) -> Result<ResponseSpec, TbfError> + Send>,
    }
    impl Transport for MockTransport {
        fn send(&mut self, spec: RequestSpec) -> Result<ResponseSpec, TbfError> {
            (self.handler)(spec)
        }
    }

    fn row_html(
        content_id: &str,
        title: &str,
        icon_class: &str,
        maker: &str,
        maker_id: &str,
    ) -> String {
        let thumb = format!(
            "//img.dlsite.jp/resize/images2/work/doujin/RJ{base}/{content_id}_img_main_240x240.webp",
            base = &content_id[..8]
        );
        format!(
            r#"<tr><td class="buy_date">2026/05/04 17:03</td>
<td class="work_1col_thumb"><img src="{thumb}"></td>
<td class="work_content">
<dl class="work_1col">
<dt class="work_name"><a href="https://www.dlsite.com/maniax/work/=/product_id/{content_id}.html">{title}</a></dt>
<dd class="maker_name"><a href="https://www.dlsite.com/maniax/circle/profile/=/maker_id/{maker_id}.html">{maker}</a></dd>
<dd class="work_genre"><span class="icon_GEN" title="全年齢">全年齢</span><span class="{icon_class}" title="マンガ">マンガ</span></dd>
</dl></td>
<td class="re_dl"><a href="https://www.dlsite.com/maniax/download/=/product_id/{content_id}.html">DL</a></td>
<td class="work_price">440円</td></tr>"#
        )
    }

    /// セッションが切れているとログインページが **200 で**返る。0 件と取り違えないこと。
    ///
    /// マーカーは実物（`login.dlsite.com/register?user=self` を保存した 20,770 バイト）から
    /// 採っている。
    #[test]
    fn login_page_is_not_mistaken_for_an_empty_library() {
        let login = r#"<title>ユーザー登録 - ユーザー登録</title>
<div class="contentLoginRegist-item"><p class="contentLoginLogin-text">viviON IDに登録済みの方はこちらから</p></div>"#;
        assert!(
            is_login_page(login),
            "viviON ID のログインページを検知できない"
        );

        let empty_library =
            r#"<div id="buy_history_this"><table class="work_list_main"></table></div>"#;
        assert!(
            !is_login_page(empty_library),
            "購入 0 件のページをログインページと誤判定した"
        );
    }

    /// 購入履歴ページから作品行と最終ページ番号を抽出する。
    #[test]
    fn parse_userbuy_page_extracts_rows_and_last_page() {
        let html = format!(
            r#"<div id="buy_history_this"><table class="work_list_main">
<tr class="item_name"><td>..</td></tr>
{}
{}</table>
<table class="global_pagination"><td class="page_no">
<a href="/maniax/mypage/userbuy/.../page/1">1</a><a href="/maniax/mypage/userbuy/.../page/3">3</a>
</td></table></div>"#,
            row_html(
                "RJ01234567",
                "タイトルA",
                "icon_MNG",
                "サークルA",
                "RG12345"
            ),
            row_html(
                "RJ05678912",
                "タイトルB",
                "icon_ICG",
                "サークルB",
                "RG67890"
            ),
        );
        let (rows, last) = parse_userbuy_page(&html);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].content_id, "RJ01234567");
        assert_eq!(rows[0].title, "タイトルA");
        assert_eq!(rows[0].work_type, "MNG");
        assert_eq!(rows[0].genre_icons, vec!["icon_GEN", "icon_MNG"]);
        assert_eq!(rows[0].maker_id.as_deref(), Some("RG12345"));
        assert_eq!(rows[0].price.as_deref(), Some("440円"));
        assert_eq!(rows[0].purchase_date.as_deref(), Some("2026/05/04 17:03"));
        assert_eq!(
            rows[0].down_url.as_deref(),
            Some("https://www.dlsite.com/maniax/download/=/product_id/RJ01234567.html")
        );
        assert!(
            rows[0]
                .thumbnail_url
                .as_deref()
                .unwrap()
                .ends_with("RJ01234567_img_main_240x240.webp")
        );
        assert_eq!(rows[1].work_type, "ICG");
        assert_eq!(last, Some(3));
    }

    /// 作品ページ HTML の「作者」行を抽出する（実機 HTML 2026-09-11 / RJ01412386 と同じ整形）。
    #[test]
    fn parse_work_page_author_extracts_author() {
        let html = r#"<table cellspacing="0" id="work_outline">
    <tr>
    <th>販売日</th>
    <td><a href="/x">2025年06月17日</a></td>  </tr>

<tr>
  <th>作者</th>
    <td>
          <a
                  href="https://www.dlsite.com/home/fsr/=/keyword_creater/%22%E3%81%A8%E3%81%A8%E3%81%AD%E3%82%8D%22/ana_flg/all"
              >ととねろ</a>        </td>
</tr>
</table>"#;
        assert_eq!(parse_work_page_author(html).as_deref(), Some("ととねろ"));
        // 作者行が無い作品は None
        assert_eq!(parse_work_page_author("<table></table>"), None);
    }

    /// 作品ページ HTML の「ファイル容量」を抽出する（実機 HTML 2026-09-26 / RJ01412386 と同じ整形）。
    #[test]
    fn parse_work_file_size_extracts_capacity() {
        let html = r#"<tr>
    <th>ジャンル</th>
    <td><div class="main_genre"><a href="/x">男の娘</a></div></td>
  </tr>
          <tr>
    <th>ファイル容量</th>
    <td>
      <div class="main_genre">
                              107.84MB
                        </div>
    </td>
  </tr>
    </table>"#;
        assert_eq!(
            parse_work_file_size(html),
            crate::store_size::parse_store_size("107.84MB"),
            "「ファイル容量」をバイト数へ直していない"
        );
        // 該当行が無い / 値が空の作品は None（＝サイズ不明として扱う）
        assert_eq!(parse_work_file_size("<table></table>"), None);
        assert_eq!(
            parse_work_file_size("<th>ファイル容量</th><td><div class=\"main_genre\"></div></td>"),
            None
        );
    }

    /// `product/info/ajax` JSON をフィールドへマップする。
    #[test]
    fn product_info_maps_fields() {
        let json = r#"{"RJ01234567":{"site_id":"maniax","work_type":"MNG","maker_id":"RG12345","work_name":"タイトルA","regist_date":"2025-06-17 16:00:00","price":440,"down_url":"https://www.dlsite.com/maniax/download/=/product_id/RJ01234567.html","custom_genres":["dlsiteawards2025"],"options":"JPN#DLP","age_category":1,"title_name":"少年エルフ"}}"#;
        let map = parse_product_info(json);
        let m = &map["RJ01234567"];
        assert_eq!(m.work_type, "MNG");
        assert_eq!(m.site_id, "maniax");
        assert_eq!(m.maker_id.as_deref(), Some("RG12345"));
        assert_eq!(m.regist_date.as_deref(), Some("2025-06-17 16:00:00"));
        assert_eq!(m.price, Some(440));
        assert_eq!(
            m.down_url.as_deref(),
            Some("https://www.dlsite.com/maniax/download/=/product_id/RJ01234567.html")
        );
        assert_eq!(m.custom_genres, vec!["dlsiteawards2025"]);
        assert_eq!(m.age_category, Some(1));
        assert_eq!(m.title_name.as_deref(), Some("少年エルフ"));
    }

    /// `purchased_page` が 1 ストア 1 ページ分（行 + 最終ページ番号）を返し、
    /// Cookie ヘッダを送ることを検証する。
    #[test]
    fn purchased_page_returns_rows_last_page_and_sends_cookie() {
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
                let (rows, last) = if spec.url.contains("page/1") {
                    (
                        row_html("RJ01234567", "A", "icon_MNG", "C", "RG1").to_string(),
                        "2",
                    )
                } else {
                    (String::new(), "1")
                };
                let html = format!(
                    "<div id=\"buy_history_this\"><table class=\"work_list_main\"><tr class=\"item_name\"></tr>{rows}</table><table class=\"global_pagination\"><td class=\"page_no\"><a href=\"/page/{last}\">…</a></td></table></div>"
                );
                Ok(ResponseSpec {
                    status: 200,
                    headers: vec![],
                    body: html.into_bytes(),
                })
            }),
        };
        let mut client = DlsiteClient::with_transport(Box::new(transport), session());
        let first = client.purchased_page("maniax", 1).unwrap();
        assert_eq!(first.items.len(), 1);
        assert_eq!(first.items[0].content_id, "RJ01234567");
        assert_eq!(first.last_page, Some(2));

        let second = client.purchased_page("maniax", 2).unwrap();
        assert!(second.items.is_empty());
        assert_eq!(second.last_page, Some(1));

        let calls = calls.lock();
        assert_eq!(calls.len(), 2);
        assert!(calls[0].contains("/maniax/mypage/userbuy/"), "{}", calls[0]);
        for c in calls.iter() {
            assert!(c.contains("Cookie="), "no cookie header: {c}");
        }
    }

    /// `down_url` 302 の `jwt` Set-Cookie を捕捉して CDN へ送り、ZIP を取得する。
    /// **CDN へはセッション Cookie を送らない**（www の認証は CDN には要らない）。
    /// HTML レスポンスは拒否する。
    #[test]
    fn download_manually_follows_302_to_cdn_with_jwt() {
        use parking_lot::Mutex;
        use std::sync::Arc;
        let cdn_spec = Arc::new(Mutex::new(None::<RequestSpec>));
        let cdn_spec2 = cdn_spec.clone();
        let proxy_spec = Arc::new(Mutex::new(None::<RequestSpec>));
        let proxy_spec2 = proxy_spec.clone();
        let transport = MockTransport {
            handler: Box::new(move |spec: RequestSpec| {
                if spec.url.contains("/download/") {
                    *proxy_spec2.lock() = Some(spec);
                    Ok(ResponseSpec {
                        status: 302,
                        headers: vec![
                            ("location".into(), "https://download.dlsite.com/get/=/type/work/domain/doujin/dir/RJ0123/file/RJ01234567.zip/_/20250619?update_date=20250619".into()),
                            ("set-cookie".into(), "jwt=eyJh.eyJwYXRoIjovY29udGVudC9kb3VqaW4vcmlwL3oifQ.sig; path=/; domain=dlsite.com; secure".into()),
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
        let mut client = DlsiteClient::with_transport(
            Box::new(transport),
            DlsiteSession::from_site_cookies(BTreeMap::from([(
                "__DLsite_SID".to_string(),
                "abc".to_string(),
            )])),
        );
        let mut on = |_: u64, _: u64| true;
        let bytes = client
            .download_with_progress(
                "https://www.dlsite.com/maniax/download/=/product_id/RJ01234567.html",
                &mut on,
            )
            .unwrap();
        assert_eq!(bytes, b"PK\x03\x04zipdata");
        let spec = cdn_spec.lock().clone().expect("CDN request made");
        assert!(spec.url.starts_with("https://download.dlsite.com/"));
        // CDN へ送るのは 302 で受け取った署名 Cookie だけ。www の認証セッションは
        // 別システム（CDN）には要らないので送らない。
        assert_eq!(
            cookie_of(&spec),
            "jwt=eyJh.eyJwYXRoIjovY29udGVudC9kb3VqaW4vcmlwL3oifQ.sig",
            "CDN へは署名 Cookie だけを送る"
        );
        // サイト内（proxy）へは従来どおりセッション Cookie を送る
        let proxy = proxy_spec.lock().clone().expect("proxy request made");
        assert_eq!(cookie_of(&proxy), "__DLsite_SID=abc");
    }

    /// 宛先が違う Cookie は送らない（www と login の 2 収集元を持つセッション）。
    fn two_origin_session() -> DlsiteSession {
        DlsiteSession::new(BTreeMap::from([
            (
                SITE_HOST.to_string(),
                BTreeMap::from([(
                    "__DLsite_SID".to_string(),
                    CookieEntry::new("shop"),
                )]),
            ),
            (
                LOGIN_HOST.to_string(),
                BTreeMap::from([(
                    "login_ticket".to_string(),
                    CookieEntry::new("t"),
                )]),
            ),
        ]))
    }

    /// `down_url` を叩くトランスポート。proxy のリクエストを記録し、302 + 署名 `jwt` を返す。
    /// CDN（`download.dlsite.com`）は 200 + ZIP を返す。
    fn download_transport() -> (
        MockTransport,
        std::sync::Arc<parking_lot::Mutex<Option<RequestSpec>>>,
    ) {
        use parking_lot::Mutex;
        use std::sync::Arc;
        let proxy_spec = Arc::new(Mutex::new(None::<RequestSpec>));
        let proxy_spec2 = proxy_spec.clone();
        let transport = MockTransport {
            handler: Box::new(move |spec: RequestSpec| {
                if spec.url.contains("/download/") {
                    *proxy_spec2.lock() = Some(spec);
                    Ok(ResponseSpec {
                        status: 302,
                        headers: vec![
                            (
                                "location".into(),
                                "https://download.dlsite.com/get/=/type/work/file/RJ01234567.zip"
                                    .into(),
                            ),
                            (
                                "set-cookie".into(),
                                "jwt=sig; path=/; domain=dlsite.com; secure".into(),
                            ),
                        ],
                        body: vec![],
                    })
                } else {
                    Ok(ResponseSpec {
                        status: 200,
                        headers: vec![("content-type".into(), "application/zip".into())],
                        body: b"PK\x03\x04zip".to_vec(),
                    })
                }
            }),
        };
        (transport, proxy_spec)
    }

    /// ダウンロード proxy（`down_url`）へは、宛先ホスト向けの Cookie だけを送る。
    /// www の認証セッションに login の Cookie を混ぜない。
    #[test]
    fn download_proxy_sends_only_destination_cookies() {
        let (transport, proxy_spec) = download_transport();
        let mut client = DlsiteClient::with_transport(Box::new(transport), two_origin_session());
        let mut on = |_: u64, _: u64| true;
        client
            .download_with_progress(
                "https://www.dlsite.com/maniax/download/=/product_id/RJ01234567.html",
                &mut on,
            )
            .unwrap();
        let proxy = proxy_spec.lock().clone().expect("proxy request made");
        assert_eq!(cookie_of(&proxy), "__DLsite_SID=shop");
    }

    /// `down_url` が収集元（`www.dlsite.com`）以外を指す場合、**リクエストを送らずに**拒否する。
    ///
    /// 許可リストをサブドメインまで広げると、`download_url`（Drive の改変バックアップから
    /// 復元され得る）を `*.dlsite.com` の別ホストへ向けられたときに、Cookie は宛先別で
    /// 空になるものの**未認証のリクエストは飛ぶ**。実測の proxy は `www` なので、
    /// ホストは完全一致に絞る（fail-closed）。
    #[test]
    fn download_proxy_on_another_subdomain_is_blocked_without_sending() {
        let (transport, proxy_spec) = download_transport();
        let mut client = DlsiteClient::with_transport(Box::new(transport), two_origin_session());
        let mut on = |_: u64, _: u64| true;
        let err = client
            .download_with_progress(
                "https://dl.dlsite.com/maniax/download/=/product_id/RJ01234567.html",
                &mut on,
            )
            .unwrap_err();
        assert!(
            matches!(err, DlsiteError::BlockedUrl(_)),
            "拒否されていない: {err:?}"
        );
        assert!(
            proxy_spec.lock().is_none(),
            "拒否したのにリクエストを送っている"
        );
    }

    /// proxy のホストが正しくても、パスが `/{store}/download/` の外なら
    /// **リクエストを送らずに**拒否する（購入履歴など他のページへ Cookie を載せない）。
    ///
    /// ストアは同期対象の `STORES`（maniax / home / books / ai）に限る。それ以外のストアは
    /// 同期で一覧に入らないので、出てきた時点で改変された `download_url` と見なせる。
    #[test]
    fn download_proxy_path_outside_the_allowlist_is_blocked_without_sending() {
        for url in [
            // 同じストアでもダウンロード以外のパス
            "https://www.dlsite.com/maniax/mypage/userbuy/=/type/all/",
            // 同期対象外のストア
            "https://www.dlsite.com/girls/download/=/product_id/RJ01234567.html",
        ] {
            let (transport, proxy_spec) = download_transport();
            let mut client =
                DlsiteClient::with_transport(Box::new(transport), two_origin_session());
            let mut on = |_: u64, _: u64| true;
            let err = client.download_with_progress(url, &mut on).unwrap_err();
            assert!(
                matches!(err, DlsiteError::BlockedUrl(_)),
                "パス制限が効いていない（{url}）: {err:?}"
            );
            assert!(
                proxy_spec.lock().is_none(),
                "拒否したのにリクエストを送っている（{url}）"
            );
        }
    }

    /// 同期対象のストア（`STORES`）のダウンロード URL は、すべて許可リストを通ること。
    ///
    /// ストアを増やすときに `DLSITE_DOWNLOAD_RULES` を直し忘れると、そのストアの
    /// ダウンロードが `BlockedUrl` で止まる（同期は成功するので気付きにくい）。
    #[test]
    fn download_rules_cover_every_store() {
        for store in STORES {
            let url =
                format!("https://www.dlsite.com/{store}/download/=/product_id/RJ01234567.html");
            assert!(
                crate::download_url::check(&url, DLSITE_DOWNLOAD_RULES).is_ok(),
                "{store} のダウンロード URL が許可されていない: {url}"
            );
        }
    }

    /// 302 の転送先（CDN）は**実測値まで狭める**（`download.dlsite.com` 完全一致）。
    /// `www` や `login` は Cookie の収集元だが CDN ではないので、署名 `jwt` を送る先にしない。
    #[test]
    fn download_cdn_hosts_outside_the_observed_set_are_blocked() {
        assert!(
            crate::download_url::check(
                "https://download.dlsite.com/get/=/type/work/file/RJ1.zip",
                DLSITE_CDN_RULES
            )
            .is_ok(),
            "実測の CDN を弾いている"
        );
        for url in [
            "https://www.dlsite.com/x.zip",
            "https://login.dlsite.com/x.zip",
            "https://dlsite.com/x.zip",
            "https://download.dlsite.com.evil.example.com/x.zip",
        ] {
            assert!(
                crate::download_url::check(url, DLSITE_CDN_RULES).is_err(),
                "実測外のホストを通している: {url}"
            );
        }
    }

    /// 購入履歴（www）への送信に login の Cookie を混ぜない。
    #[test]
    fn site_requests_send_only_site_cookies() {
        use parking_lot::Mutex;
        use std::sync::Arc;
        let spec = Arc::new(Mutex::new(None::<RequestSpec>));
        let spec2 = spec.clone();
        let transport = MockTransport {
            handler: Box::new(move |s: RequestSpec| {
                *spec2.lock() = Some(s);
                Ok(ResponseSpec {
                    status: 200,
                    headers: vec![],
                    body: b"<div id=\"buy_history_this\"><table class=\"work_list_main\"></table></div>".to_vec(),
                })
            }),
        };
        let mut client = DlsiteClient::with_transport(Box::new(transport), two_origin_session());
        client.purchased_page("maniax", 1).unwrap();
        let sent = spec.lock().clone().expect("request made");
        assert_eq!(cookie_of(&sent), "__DLsite_SID=shop");
    }

    /// Cookie は収集元ホストごとに持ち、宛先が違う Cookie は送らない。
    ///
    /// 収集元（`www` / `login`）を潰すと、片方にしか送るべきでない Cookie がもう片方や
    /// CDN へ飛ぶ。
    #[test]
    fn session_cookies_are_scoped_to_their_origin() {
        let session = DlsiteSession::new(BTreeMap::from([
            (
                SITE_HOST.to_string(),
                BTreeMap::from([(
                    "__DLsite_SID".to_string(),
                    crate::session_cookies::CookieEntry::new("shop"),
                )]),
            ),
            (
                LOGIN_HOST.to_string(),
                BTreeMap::from([(
                    "login_ticket".to_string(),
                    crate::session_cookies::CookieEntry::new("t"),
                )]),
            ),
        ]));

        // 宛先が収集元でなければ 1 つも送らない（CDN はこれに当たる）
        assert_eq!(
            session.cookie_header_for_url("https://download.dlsite.com/get/x.zip"),
            ""
        );
        assert_eq!(session.cookie_header_for_url("https://evil.example.com/"), "");
        // 収集元は完全一致（サブドメインへは送らない）
        assert_eq!(
            session.cookie_header_for_url("https://www.dlsite.com/home/mypage"),
            "__DLsite_SID=shop"
        );
        assert_eq!(
            session.cookie_header_for_url("https://sub.www.dlsite.com/"),
            ""
        );
        assert_eq!(
            session.cookie_header_for_url("https://login.dlsite.com/login"),
            "login_ticket=t"
        );

        // 収集元をまたいだ取り出し（認証判定用）
        assert_eq!(
            session.cookie_value(SITE_HOST, "__DLsite_SID"),
            Some("shop")
        );
        assert_eq!(session.cookie_value(LOGIN_HOST, "__DLsite_SID"), None);
        assert_eq!(session.cookies_count(), 2);
        assert!(session.logged_in());
    }

    /// `parse_last_page` は数値リンクの最大値と `最後` リンクを検出する。
    #[test]
    fn parse_last_page_detects_last_link() {
        let html = r#"<table class="global_pagination"><td class="page_no"><a href=".../page/1">1</a><a href=".../page/3">3</a><a href=".../page/12">最後</a></td></table>"#;
        assert_eq!(parse_last_page(html), Some(12));
    }
}
