//! DLsite の購入済み作品クライアント。
//!
//! 購入一覧はストア別の `mypage/userbuy/.../page/{n}` HTML（公開 JSON API は無い）を
//! スクレイプする。作品メタは `product/info/ajax`（Cookie + XHR ヘッダ）で取得する。
//! ダウンロードは `down_url` を 302 追跡して `download.dlsite.com` の実 ZIP を取得する
//! （`jwt` 署名 Cookie を 302 の Set-Cookie で捕捉して CDN へ送る）。`Transport` は
//! `tbf::transport` を再利用（モック可能）。

use std::collections::HashMap;

use crate::tbf::TbfError;
use crate::tbf::transport::{RequestSpec, Transport};

/// 列挙するストアフロア。`soft`（PC ソフト）/ `app`（スマホゲーム）はゲーム系・対象外。
pub const STORES: [&str; 4] = ["maniax", "home", "books", "ai"];
/// 購入履歴の 1 ページあたりの行数境界（実測は未確認だが、過剰アクセス防止の上限）。
pub const MAX_PAGES_PER_STORE: usize = 200;

/// DLsite は非ブラウザ UA を弾くため、ブラウザ UA + Referer を送る（BoothClient と同流儀）。
const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

/// ダウンロード要求を送ってよいホスト。
///
/// `bookshelf_items.download_url`（= `down_url`）は Drive の JSON バックアップから
/// 復元でき、改変したバックアップを復元させると外部ホストへセッション Cookie が
/// 送られる。自サイトだけを許可する。
const DLSITE_DOWNLOAD_RULES: &[crate::download_url::HostRule] =
    &[crate::download_url::HostRule::with_subdomains(
        "dlsite.com",
        None,
    )];

/// 302 の転送先（署名 `jwt` + セッション Cookie を送る先）。Cookie は
/// `domain=.dlsite.com` で発行されるため、その範囲（本体 + CDN）だけを許可する。
const DLSITE_CDN_RULES: &[crate::download_url::HostRule] =
    &[crate::download_url::HostRule::with_subdomains(
        "dlsite.com",
        None,
    )];

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

/// DLsite のセッション（`www.dlsite.com` / `login.dlsite.com` の Cookie 群）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct DlsiteSession {
    cookies: HashMap<String, String>,
}

impl DlsiteSession {
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

    fn cookie_headers(&self) -> Vec<(String, String)> {
        vec![
            ("Cookie".to_string(), self.session.cookie_header()),
            ("User-Agent".to_string(), USER_AGENT.to_string()),
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

    fn ajax_headers(&self) -> Vec<(String, String)> {
        let mut h = self.cookie_headers();
        if let Some((_, v)) = h.iter_mut().find(|(k, _)| k == "Accept") {
            *v = "application/json".to_string();
        }
        h.push(("X-Requested-With".to_string(), "XMLHttpRequest".to_string()));
        h
    }

    /// 購入済み作品一覧を全ストア × 全ページから取得する。
    /// 各ページは `parse_userbuy_page` で行 + `page_no` を解析し、
    /// `page_no` 最大値 / `最後` リンクで終端、行 0 件でフォールバック打ち切り。
    pub fn purchased(&mut self) -> Result<Vec<DlsitePurchase>, DlsiteError> {
        let mut out = Vec::new();
        // 同一作品は複数ストア（maniax/home/books/ai）の userbuy に重複して現れるため、
        // content_id で初出のみ採る（site_id / work_type は同一なので初出で十分）。
        let mut seen = std::collections::HashSet::new();
        for store in STORES {
            let mut page = 1usize;
            loop {
                if page > MAX_PAGES_PER_STORE {
                    break;
                }
                let url = format!(
                    "https://www.dlsite.com/{store}/mypage/userbuy/=/type/all/start/all/sort/1/order/1/page/{page}"
                );
                let html = self.get_html(&url)?;
                let (rows, last) = parse_userbuy_page(&html);
                let empty = rows.is_empty();
                for row in rows {
                    if seen.insert(row.content_id.clone()) {
                        out.push(row);
                    }
                }
                if empty {
                    break;
                }
                if let Some(lp) = last
                    && page >= lp
                {
                    break;
                }
                page += 1;
            }
        }
        Ok(out)
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
            let resp = self
                .transport
                .send(RequestSpec {
                    method: "GET".into(),
                    url,
                    headers: self.ajax_headers(),
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
        let url = format!("https://www.dlsite.com/maniax/work/=/product_id/{content_id}.html");
        let html = self.get_html(&url)?;
        Ok(parse_work_page_author(&html))
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
        crate::download_url::check(down_url, DLSITE_DOWNLOAD_RULES)
            .map_err(|error| DlsiteError::BlockedUrl(format!("{down_url}: {error}")))?;
        let proxy_spec = RequestSpec {
            method: "GET".into(),
            url: down_url.into(),
            headers: self.cookie_headers(),
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
        crate::download_url::check(&cd_url, DLSITE_CDN_RULES)
            .map_err(|error| DlsiteError::BlockedUrl(format!("{cd_url}: {error}")))?;
        // 2) 302 の Set-Cookie で `jwt`（署名付きダウンロード鍵）を捕捉して Cookie へ足す
        let mut cookie = self.session.cookie_header();
        for (k, v) in proxy_resp.set_cookies() {
            if k == "jwt" && !cookie.contains("jwt=") {
                if !cookie.is_empty() {
                    cookie.push_str("; ");
                }
                cookie.push_str(&format!("{k}={v}"));
            }
        }
        // 3) CDN（download.dlsite.com）へ直接、`jwt` + セッション Cookie 付きで取得
        let headers = vec![
            ("Cookie".to_string(), cookie),
            ("User-Agent".to_string(), USER_AGENT.to_string()),
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

    /// HTML ページを取得する（購入履歴）。
    fn get_html(&mut self, url: &str) -> Result<String, DlsiteError> {
        let resp = self
            .transport
            .send(RequestSpec {
                method: "GET".into(),
                url: url.into(),
                headers: self.cookie_headers(),
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

/// 購入履歴ページ HTML から作品行 + 最終ページ番号を解析する。
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

/// 作品ページ HTML の「作者」行（`<th>作者</th>` の直後の `<a>` テキスト）を抽出する。
/// 実機 HTML（2026-09-11 / RJ01412386）で検証済み。行が無い / 空の作品は `None`。
pub fn parse_work_page_author(html: &str) -> Option<String> {
    let re = regex::Regex::new(r"(?s)<th>\s*作者\s*</th>.*?<a[^>]*>\s*([^<]*?)\s*</a>").ok()?;
    re.captures(html)
        .map(|cap| cap[1].trim().to_string())
        .filter(|author| !author.is_empty())
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
        DlsiteSession::new(HashMap::from([("__DLsite_SID".into(), "abc".into())]))
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

    /// `purchased` が複数ストア × ページを走査し、Cookie ヘッダを送ることを検証する。
    #[test]
    fn purchased_pages_all_stores_and_sends_cookie() {
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
        let list = client.purchased().unwrap();
        assert!(list.iter().any(|p| p.content_id == "RJ01234567"));
        let calls = calls.lock();
        assert!(calls.len() >= 4); // maniax,home,books,ai × (page1..2)
        for c in calls.iter() {
            assert!(c.contains("Cookie="), "no cookie header: {c}");
        }
    }

    /// `down_url` 302 の `jwt` Set-Cookie を捕捉して CDN へ送り、ZIP を取得する。
    /// HTML レスポンスは拒否する。
    #[test]
    fn download_manually_follows_302_to_cdn_with_jwt() {
        use parking_lot::Mutex;
        use std::sync::Arc;
        let cdn_spec = Arc::new(Mutex::new(None::<RequestSpec>));
        let cdn_spec2 = cdn_spec.clone();
        let transport = MockTransport {
            handler: Box::new(move |spec: RequestSpec| {
                if spec.url.contains("/download/") {
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
            DlsiteSession::new(HashMap::from([("__DLsite_SID".into(), "abc".into())])),
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
        let cookie = spec
            .headers
            .iter()
            .find(|(k, _)| k == "Cookie")
            .map(|(_, v)| v.clone())
            .unwrap_or_default();
        assert!(
            cookie.contains("__DLsite_SID=abc"),
            "session cookie missing: {cookie}"
        );
        assert!(
            cookie.contains("jwt=eyJh.eyJwYXRoIjovY29udGVudC9kb3VqaW4vcmlwL3oifQ.sig"),
            "jwt cookie missing: {cookie}"
        );
    }

    /// `parse_last_page` は数値リンクの最大値と `最後` リンクを検出する。
    #[test]
    fn parse_last_page_detects_last_link() {
        let html = r#"<table class="global_pagination"><td class="page_no"><a href=".../page/1">1</a><a href=".../page/3">3</a><a href=".../page/12">最後</a></td></table>"#;
        assert_eq!(parse_last_page(html), Some(12));
    }
}
