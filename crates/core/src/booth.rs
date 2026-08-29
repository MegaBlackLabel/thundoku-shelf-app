//! BOOTH（booth.pm）のセッション管理と購入品取得。
//!
//! ログインはアプリ内 WebView（gpui-wry）で pixiv OAuth を実施し、
//! 取得した booth.pm のセッション Cookie を保持・永続化する。
//! 購入品一覧はライブラリページ、購入日は購入履歴ページから取得する。

use std::collections::HashMap;

use serde_json::Value;

/// BOOTH のセッション（booth.pm ドメインの Cookie 群）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct BoothSession {
    /// booth.pm のセッション Cookie（name → value）
    pub cookies: HashMap<String, String>,
}

impl BoothSession {
    /// ログイン済みか（booth.pm の Cookie が 1 つ以上ある）。
    pub fn logged_in(&self) -> bool {
        !self.cookies.is_empty()
    }

    /// HTTP リクエスト用の `Cookie: name=value; ...` ヘッダー値。
    pub fn cookie_header(&self) -> String {
        self.cookies
            .iter()
            .map(|(name, value)| format!("{name}={value}"))
            .collect::<Vec<_>>()
            .join("; ")
    }
}

#[derive(Debug, thiserror::Error)]
pub enum BoothError {
    #[error("network error: {0}")]
    Network(String),
    #[error("not logged in (session expired)")]
    NotLoggedIn,
    #[error("not found (item page removed)")]
    NotFound,
    #[error("invalid response: {0}")]
    InvalidResponse(String),
}

/// ライブラリ（購入品一覧）の 1 商品。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoothLibraryItem {
    /// 商品 ID（booth.pm/ja/items/{id}）
    pub item_id: u64,
    pub title: String,
    /// ショップ名（サークル名）
    pub shop_name: String,
    pub file_name: Option<String>,
    /// サムネイル URL（300x300）
    pub thumbnail_url: Option<String>,
    /// ダウンロード URL（booth.pm/downloadables/{id}）
    pub download_url: Option<String>,
}

/// 購入履歴の 1 注文（商品名 + 注文日時）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoothOrder {
    pub item_title: String,
    /// 注文日時（"2026/01/01 19:36:23"）
    pub ordered_at: String,
}

/// 商品詳細 API（/ja/items/{id} + Accept: application/json）の表紙情報。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BoothItemDetail {
    pub item_id: u64,
    pub title: String,
    /// オリジナル画像 URL（大きい表紙）
    pub images: Vec<String>,
    pub shop_name: Option<String>,
}

/// BOOTH の購入品取得クライアント（ログイン済みセッションを使用）。
pub struct BoothClient {
    cookie_header: String,
    agent: ureq::Agent,
}

const USER_AGENT: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36";

impl BoothClient {
    pub fn new(session: &BoothSession) -> Self {
        Self {
            cookie_header: session.cookie_header(),
            agent: ureq::AgentBuilder::new()
                .timeout_connect(std::time::Duration::from_secs(5))
                .timeout_read(std::time::Duration::from_secs(15))
                .build(),
        }
    }

    /// サーバー側のセッションを無効化する（ログアウト）。
    ///
    /// 2 段階:
    /// 1. plaza（pixiv アカウント）: `POST accounts.booth.pm/users/sign_out`
    ///    を同じ CSRF トークンで叩く。`_plaza_session` が生きていると
    ///    booth.pm 再訪のたびに自動再ログインされるため、こちらも破棄する。
    /// 2. booth.pm（Rails アプリ）: `POST booth.pm/users/sign_out` +
    ///    `_method=delete`。CSRF トークンは `booth.pm/ja` の
    ///    `<meta name="csrf-token" content="...">` から取得する。
    pub fn logout(&self) -> Result<(), BoothError> {
        let page = self.get("https://booth.pm/ja", "text/html; charset=utf-8")?;
        let csrf = extract_csrf_token(&page)
            .ok_or_else(|| BoothError::Network("csrf token not found in page".into()))?;
        let cookie_ok = !self.cookie_header.is_empty();

        // 1) plaza（pixiv アカウント）側のセッション破棄。失敗しても続行する
        //    （booth.pm 側だけでもログアウトとして機能するため）。
        {
            let mut request = self
                .agent
                .post("https://accounts.booth.pm/users/sign_out")
                .set("User-Agent", USER_AGENT)
                .set("Accept", "*/*")
                .set("X-CSRF-Token", &csrf)
                .set("Referer", "https://accounts.booth.pm")
                .set("Origin", "https://accounts.booth.pm");
            if cookie_ok {
                request = request.set("Cookie", &self.cookie_header);
            }
            match request.send_form(&[("_method", "delete")]) {
                Ok(response) => {
                    log::info!(
                        "booth logout(plaza): status={} location={:?}",
                        response.status(),
                        response.header("location").unwrap_or_default()
                    );
                }
                Err(e) => {
                    log::info!("booth logout(plaza): failed ({e})");
                }
            }
        }

        // 2) booth.pm（Rails アプリ）側のセッション破棄。
        let mut request = self
            .agent
            .post("https://booth.pm/users/sign_out")
            .set("User-Agent", USER_AGENT)
            .set("Accept", "*/*")
            .set("X-CSRF-Token", &csrf)
            .set("Referer", "https://booth.pm/ja")
            .set("Origin", "https://booth.pm");
        if cookie_ok {
            request = request.set("Cookie", &self.cookie_header);
        }
        let response = request
            .send_form(&[("_method", "delete")])
            .map_err(|e| match e {
                ureq::Error::Status(code, _) if code == 401 || code == 403 => {
                    BoothError::NotLoggedIn
                }
                _ => BoothError::Network(e.to_string()),
            })?;
        let status = response.status();
        let location = response
            .header("location")
            .map(|v| v.to_string())
            .unwrap_or_default();
        log::info!("booth logout(booth): status={status} location={location:?}");
        Ok(())
    }

    fn get(&self, url: &str, accept: &str) -> Result<String, BoothError> {
        let mut request = self
            .agent
            .get(url)
            .set("User-Agent", USER_AGENT)
            .set("Accept", accept)
            .set("Accept-Language", "ja,en-US;q=0.9,en;q=0.8");
        if !self.cookie_header.is_empty() {
            request = request.set("Cookie", &self.cookie_header);
        }
        let response = request.call().map_err(|e| match e {
            // Cloudflare のクリアランス（cf_clearance）失効やセッション切れは
            // 403/401 になる。再ログインを促すため NotLoggedIn に分類する
            ureq::Error::Status(code, _) if code == 401 || code == 403 => BoothError::NotLoggedIn,
            // 商品ページが消えている（「お探しの本は見つかりませんでした」）
            ureq::Error::Status(404, _) => BoothError::NotFound,
            _ => BoothError::Network(e.to_string()),
        })?;
        if response.status() == 302 && url.contains("sign_in") {
            return Err(BoothError::NotLoggedIn);
        }
        response
            .into_string()
            .map_err(|e| BoothError::Network(e.to_string()))
    }

    /// ライブラリ（購入品一覧）を全ページ取得する。
    ///
    /// ページネーション（/library?page=N のリンク）を HTML から検出し、
    /// 存在するページを最後まで辿る（10 件/ページ）。
    pub fn library(&self) -> Result<Vec<BoothLibraryItem>, BoothError> {
        let mut items = Vec::new();
        let mut page = 1;
        let mut max_page = 1;
        loop {
            let html = self.get(
                &format!("https://accounts.booth.pm/library?page={page}"),
                "text/html",
            )?;
            // ページネーションリンク（/library?page=N）から最大ページ数を検出
            let mut new_max = max_page;
            for cap in regex::Regex::new(r#"/library\?page=(\d+)"#)
                .unwrap()
                .captures_iter(&html)
            {
                if let Ok(p) = cap[1].parse::<usize>() {
                    new_max = new_max.max(p);
                }
            }
            max_page = new_max;
            log::info!(
                "booth library: page={page} html_len={} head={:?}",
                html.len(),
                html.chars().take(220).collect::<String>()
            );
            // ログインページが返った場合はセッション切れ。
            // 0 件の成功として扱うと「同期したのに 0 件」が隠れるため
            // 明示的に NotLoggedIn を返す（UI が再ログインを促す）。
            if html.contains("<title>ログイン - BOOTH</title>")
                || html.contains("ログイン</h1>")
                || html.len() < 20_000
            {
                log::warn!(
                    "booth library: セッション無効（ログインページを受信）page={page} len={}",
                    html.len()
                );
                return Err(BoothError::NotLoggedIn);
            }
            // パーサー修正のための実物ダンプ（リンク先が無い・取得できない本の
            // 構造を確認する）。アプリの同期後にこのファイルを解析してパーサーを直す。
            let dump_path = std::env::temp_dir().join(format!("booth_library_page_{page}.html"));
            if let Err(e) = std::fs::write(&dump_path, &html) {
                log::warn!("booth library: html dump failed: {e}");
            } else {
                log::info!("booth library: html dump -> {}", dump_path.display());
            }
            let parsed = parse_library(&html);
            log::info!(
                "booth library: page={page} parsed={} max_page={max_page}",
                parsed.len()
            );
            if parsed.is_empty() {
                break;
            }
            items.extend(parsed);
            if page >= max_page {
                break; // 最終ページまで到達
            }
            page += 1;
            if page > 100 {
                break; // 安全弁
            }
        }
        Ok(items)
    }

    /// 購入履歴を全ページ取得する（商品名 + 注文日時）。
    pub fn orders(&self) -> Result<Vec<BoothOrder>, BoothError> {
        let mut orders = Vec::new();
        let mut page = 1;
        loop {
            let html = self.get(
                &format!("https://accounts.booth.pm/orders?page={page}"),
                "text/html",
            )?;
            // パーサー修正のための実物ダンプ（ライブラリに載らない購入品の構造確認）
            let dump_path = std::env::temp_dir().join(format!("booth_orders_page_{page}.html"));
            if let Err(e) = std::fs::write(&dump_path, &html) {
                log::warn!("booth orders: html dump failed: {e}");
            } else {
                log::info!("booth orders: html dump -> {}", dump_path.display());
            }
            let parsed = parse_orders(&html);
            if parsed.is_empty() {
                break;
            }
            let new_count = parsed.len();
            orders.extend(parsed);
            if new_count < 12 {
                break;
            }
            page += 1;
            if page > 100 {
                break;
            }
        }
        Ok(orders)
    }

    /// 商品ファイルをダウンロードする。
    ///
    /// BOOTH の仕様: `GET https://booth.pm/downloadables/{id}` はセッション Cookie を
    /// 検証して 302 リダイレクトを返し、Location は署名付きの一時 S3 URL
    /// （180 秒有効）。ureq の自動リダイレクト追跡で最終的にファイル本体が返る。
    pub fn download(&self, download_url: &str) -> Result<Vec<u8>, BoothError> {
        self.download_with_progress(download_url, &mut |_, _| {})
    }

    /// 進捗コールバック付きで商品ファイルをダウンロードする。
    /// `on_progress(downloaded, total)` — total は Content-Length が無い場合は 0。
    pub fn download_with_progress(
        &self,
        download_url: &str,
        on_progress: &mut dyn FnMut(u64, u64),
    ) -> Result<Vec<u8>, BoothError> {
        let mut request = self
            .agent
            .get(download_url)
            .set("User-Agent", USER_AGENT)
            .set("Accept", "application/octet-stream, */*");
        if !self.cookie_header.is_empty() {
            request = request.set("Cookie", &self.cookie_header);
        }
        let response = request
            .call()
            .map_err(|e| BoothError::Network(e.to_string()))?;
        let content_type = response
            .header("content-type")
            .map(|v| v.to_string())
            .unwrap_or_default();
        // ファイルではなく HTML（エラーページ・ダウンロード画面）が返った場合は
        // 明示的に失敗させる（見かけ上の成功で HTML を取り込んでしまうのを防ぐ）。
        if content_type.contains("text/html") {
            return Err(BoothError::Network(format!(
                "HTML が返りました（リンクが無効の可能性）: {download_url} (content-type={content_type})"
            )));
        }
        let total = response
            .header("Content-Length")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        use std::io::Read;
        let mut reader = response.into_reader();
        let mut bytes = Vec::new();
        let mut buffer = [0u8; 65536];
        let mut downloaded = 0u64;
        loop {
            let count = reader
                .read(&mut buffer)
                .map_err(|e| BoothError::Network(e.to_string()))?;
            if count == 0 {
                break;
            }
            downloaded += count as u64;
            bytes.extend_from_slice(&buffer[..count]);
            on_progress(downloaded, total);
        }
        Ok(bytes)
    }

    /// 商品詳細 API から表紙画像（オリジナルサイズ）を取得する。
    pub fn item_detail(&self, item_id: u64) -> Result<BoothItemDetail, BoothError> {
        let body = self.get(
            &format!("https://booth.pm/ja/items/{item_id}"),
            "application/json",
        )?;
        let value: Value =
            serde_json::from_str(&body).map_err(|e| BoothError::InvalidResponse(e.to_string()))?;
        let title = value
            .get("name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string();
        let images = value
            .get("images")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|img| img.get("original").and_then(|o| o.as_str()))
                    .map(|s| s.to_string())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let shop_name = value
            .get("shop")
            .and_then(|s| s.get("name"))
            .and_then(|v| v.as_str())
            .map(|s| s.to_string());
        Ok(BoothItemDetail {
            item_id,
            title,
            images,
            shop_name,
        })
    }
}

/// ライブラリ HTML から購入品を抽出する。
fn parse_library(html: &str) -> Vec<BoothLibraryItem> {
    let mut items = Vec::new();
    // アイテムブロック: <a href="https://booth.pm/ja/items/{id}">
    // （商品ページが閉じた本は `https://{shop}.booth.pm/items/{id}` のリンクになる。
    //  サブドメイン・/ja/ の有無を任意にして拾えるようにする）
    let re = regex::Regex::new(
        r#"<a[^>]*href="https?://(?:[a-z0-9-]+\.)?booth\.pm/(?:ja/)?items/(\d+)"[^>]*><div class="text-text-default font-bold[^"]*"[^>]*>(.*?)</div></a>"#,
    )
    .unwrap();
    for cap in re.captures_iter(html) {
        let item_id = cap[1].parse::<u64>().unwrap_or(0);
        let title = strip_tags(&cap[2]);
        if item_id == 0 || title.is_empty() {
            continue;
        }
        items.push(BoothLibraryItem {
            item_id,
            title,
            shop_name: String::new(),
            file_name: None,
            thumbnail_url: None,
            download_url: None,
        });
    }
    // 各アイテムにショップ名・ファイル名・DL URL・サムネイルを補完する
    for item in items.iter_mut() {
        let start = html.find(&format!("items/{}", item.item_id)).unwrap_or(0);
        // UTF-8 の文字境界でクランプする（マルチバイト文字の途中で切るとパニックする）
        let mut end = (start + 4000).min(html.len());
        while end > start && !html.is_char_boundary(end) {
            end -= 1;
        }
        let block = &html[start..end];
        item.shop_name = extract_shop_name(block);
        item.file_name = extract_file_name(block);
        item.download_url = extract_download_url(block);
        item.thumbnail_url = extract_thumbnail_url(block);
        // リンク先が見つからないアイテム（ファイルはあるが downloadables の
        // data-href が消えている本）を特定する。ダウンロード可能な場合は
        // ブロック内の別の手がかり（JSON・data 属性）から URL を復元するため、
        // ブロックの実物をログに残す。
        if item.download_url.is_none() && item.file_name.is_some() || (item.download_url.is_none() && item.thumbnail_url.is_some()) {
            log::warn!(
                "booth library: download_url なし item_id={} title={:?} file_name={:?} block_head={:?}",
                item.item_id,
                item.title,
                item.file_name,
                block.chars().take(400).collect::<String>()
            );
        }
    }
    items
}

/// 取得したページの Rails CSRF トークン（<meta name="csrf-token" content="...">）。
fn extract_csrf_token(html: &str) -> Option<String> {
    html.split(r#"csrf-token" content=""#)
        .nth(1)?
        .split('"')
        .next()
        .map(|t| t.to_string())
}

fn strip_tags(input: &str) -> String {
    regex::Regex::new(r"<[^>]+>")
        .unwrap()
        .replace_all(input, "")
        .trim()
        .to_string()
}

/// ショップ名（<img alt="ショップ名" class="rounded-[50%]...">）
fn extract_shop_name(block: &str) -> String {
    regex::Regex::new(r#"<img alt="([^"]+)" class="rounded-\[50%\]"#)
        .unwrap()
        .captures(block)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .unwrap_or_default()
}

/// ファイル名（タイトル直後の min-w-0 break-words ブロック内のテキスト）
fn extract_file_name(block: &str) -> Option<String> {
    // ファイル名は「.pdf」「.zip」等の拡張子を含む最初のテキスト行
    regex::Regex::new(r#"class="min-w-0 break-words whitespace-pre-line"[^>]*>(.*?)</div>"#)
        .unwrap()
        .captures(block)
        .and_then(|c| c.get(1))
        .map(|m| strip_tags(m.as_str()))
        .filter(|s| !s.is_empty())
}

/// ダウンロード URL（js-download-button の data-href）。
///
/// ライブラリのボタンは 2 種類ある:
/// - `data-test="browsable"`（`?browse=1` 付き）: ブラウザプレビュー用。
///   この URL から取得すると HTML（プレビュー画面）が返る。
/// - `data-test="downloadable"`: ファイル本体。こちらを優先する。
/// - `other-downloads-button`（deeplink）: アプリ起動用。無視する。
fn extract_download_url(block: &str) -> Option<String> {
    // 1) 本体（downloadable）の data-href を優先
    let downloadable = regex::Regex::new(
        r#"data-href[" ]?="(https://booth\.pm/downloadables/\d+[^"]*)"[^>]*data-test="downloadable""#,
    )
    .unwrap()
    .captures(block)
    .and_then(|c| c.get(1))
    .map(|m| m.as_str().to_string());
    if downloadable.is_some() {
        return downloadable;
    }
    // 2) ブラウザ用（browsable）しか無い本はクエリ（?browse=1）を除去して
    //    ファイル URL として使う（同一 downloadables/{id} が返る）。
    regex::Regex::new(r#"data-href="(https://booth\.pm/downloadables/\d+)\?browse=1""#)
        .unwrap()
        .captures(block)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
        .or_else(|| {
            // 後方互換: サーバー側の構造変化に備えて、downloadables を含む
            // data-href を最初の 1 件にフォールバック（?browse=1 除去）。
            regex::Regex::new(r#"data-href="(https://booth\.pm/downloadables/\d+[^"]*)""#)
                .unwrap()
                .captures(block)
                .and_then(|c| c.get(1))
                .map(|m| m.as_str().to_string())
                .map(|url| url.split('?').next().unwrap_or(&url).to_string())
        })
}

/// サムネイル URL（l-library-item-thumbnail）
fn extract_thumbnail_url(block: &str) -> Option<String> {
    regex::Regex::new(r#"<img class="l-library-item-thumbnail" src="([^"]+)""#)
        .unwrap()
        .captures(block)
        .and_then(|c| c.get(1))
        .map(|m| m.as_str().to_string())
}

/// 購入履歴 HTML から（商品名, 注文日時）を抽出する。
fn parse_orders(html: &str) -> Vec<BoothOrder> {
    // テキスト化して「発送完了 | 商品名 | 注文日時: YYYY/MM/DD HH:MM:SS」を探す
    let text = regex::Regex::new(r"<[^>]+>")
        .unwrap()
        .replace_all(html, "|");
    let text = regex::Regex::new(r"\|+").unwrap().replace_all(&text, "|");
    let re = regex::Regex::new(
        r"発送完了\s*\|\s*(.+?)\s*\|\s*注文日時:\s*(\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2})",
    )
    .unwrap();
    re.captures_iter(&text)
        .map(|cap| BoothOrder {
            item_title: cap[1].trim().to_string(),
            ordered_at: cap[2].to_string(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_logged_in_and_cookie_header() {
        let session = BoothSession::default();
        assert!(!session.logged_in(), "empty session is not logged in");
        assert_eq!(session.cookie_header(), "");

        let session = BoothSession {
            cookies: HashMap::from([
                ("_booth_session".to_string(), "abc123".to_string()),
                ("locale".to_string(), "ja".to_string()),
            ]),
        };
        assert!(session.logged_in());
        let header = session.cookie_header();
        assert!(header.contains("_booth_session=abc123"));
        assert!(header.contains("locale=ja"));
    }

    #[test]
    fn session_roundtrips_via_json() {
        let session = BoothSession {
            cookies: HashMap::from([("_booth_session".to_string(), "abc".to_string())]),
        };
        let json = serde_json::to_string(&session).unwrap();
        let restored: BoothSession = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, session);
    }

    #[test]
    fn parse_library_extracts_items() {
        let html = r#"<a target="_blank" class="no-underline" rel="noopener" href="https://booth.pm/ja/items/7825209"><div class="text-text-default font-bold text-16 mb-8 break-all">サンプル技術書 (電子版)</div></a><a target="_blank" class="no-underline w-fit flex gap-4 items-center" rel="noopener" href="https://yorimiya.booth.pm/"><img alt="YORIMIYA STUDIO" class="rounded-[50%] w-24 h-24" src="https://booth.pximg.net/c/48x48/users/1/icon_image/a.png" /><div class="text-14 text-text-gray600 break-all">YORIMIYA STUDIO</div></a></div></div><div class="mt-16"><div class="mt-16 desktop:flex desktop:justify-between desktop:items-center"><div class="min-w-0 break-words whitespace-pre-line"><div>sample-book.pdf</div></div><div><div class="js-download-button" data-href="https://booth.pm/downloadables/8191306?browse=1" data-is-browsable="true" data-label="ブラウザで開く"></div></div></div><img class="l-library-item-thumbnail" src="https://booth.pximg.net/c/300x300_a2_g5/68bc3986-7344-4f3f-bb59-62fa48e0f508/i/7825209/cover.jpg" width="80" height="80" />"#;
        let items = parse_library(html);
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.item_id, 7825209);
        assert_eq!(item.title, "サンプル技術書 (電子版)");
        assert_eq!(item.shop_name, "YORIMIYA STUDIO");
        assert_eq!(item.file_name.as_deref(), Some("sample-book.pdf"));
        assert_eq!(
            item.download_url.as_deref(),
            Some("https://booth.pm/downloadables/8191306?browse=1")
        );
        assert!(
            item.thumbnail_url
                .as_deref()
                .unwrap_or_default()
                .contains("300x300")
        );
    }

    #[test]
    fn parse_orders_extracts_title_and_date() {
        let html = r#"<div>発送完了</div><div>【C107】サンプル本 (電子版)</div><div>注文日時: 2026/01/01 19:36:23</div><div>発送完了</div><div>別の本</div><div>注文日時: 2025/05/18 15:22:13</div>"#;
        let orders = parse_orders(html);
        assert_eq!(orders.len(), 2);
        assert_eq!(orders[0].item_title, "【C107】サンプル本 (電子版)");
        assert_eq!(orders[0].ordered_at, "2026/01/01 19:36:23");
        assert_eq!(orders[1].item_title, "別の本");
        assert_eq!(orders[1].ordered_at, "2025/05/18 15:22:13");
    }

    #[test]
    fn item_detail_parses_images() {
        let body = r#"{"name":"テスト本","shop":{"name":"テストショップ"},"images":[{"original":"https://booth.pximg.net/abc/i/1/cover_base_resized.jpg"},{"original":"https://booth.pximg.net/abc/i/1/page2_base_resized.jpg"}]}"#;
        let value: Value = serde_json::from_str(body).unwrap();
        let detail = BoothItemDetail {
            item_id: 1,
            title: value["name"].as_str().unwrap().to_string(),
            images: value["images"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|i| i["original"].as_str())
                .map(|s| s.to_string())
                .collect(),
            shop_name: value["shop"]["name"].as_str().map(|s| s.to_string()),
        };
        assert_eq!(detail.title, "テスト本");
        assert_eq!(detail.shop_name.as_deref(), Some("テストショップ"));
        assert_eq!(detail.images.len(), 2);
        assert!(detail.images[0].contains("cover_base_resized.jpg"));
    }
}
