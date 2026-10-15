//! Fantia（`fantia.jp`）のセッションと HTTP クライアント。
//!
//! ログインはアプリ内 WebView（`crates/app/src/views/fantia_login.rs`）でユーザーが行う
//! （`/sessions/signin` はパスワードに加えて reCAPTCHA と 2 段階認証があるため、
//! アプリから資格情報を送る自動ログインはしない）。ここは、そのときに取得した Cookie の
//! 保持と、宛先ごとの取り出し、購入履歴ページの取得（同期は [`crate::fantia::sync`]）と
//! 商品本体のダウンロードを持つ。

use std::collections::BTreeMap;

use crate::session_cookies::{CookieEntry, HostScopedCookies};
use crate::tbf::TbfError;
use crate::tbf::transport::{RequestSpec, Transport, UreqTransport};

/// サイト（`fantia.jp`）。Cookie を集める側のホスト。
pub const SITE_HOST: &str = "fantia.jp";

/// ログイン済みの判定に使う Cookie 名。
///
/// Fantia のセッションはこの 1 つ（HttpOnly / Secure / SameSite=Lax。実測 2026-10-15）。
const SESSION_COOKIE: &str = "_session_id";

/// Fantia のセッション（`fantia.jp` の Cookie 群）。
///
/// Cookie は**収集元ホストごと**に持つ（`DlsiteSession` と同じ。1 つに潰すと、片方に
/// しか送るべきでない Cookie がもう片方や別システムへ飛ぶ。詳細は
/// [`crate::session_cookies`] を参照）。
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct FantiaSession {
    /// 収集元ホスト →（Cookie 名 → 値）。宛先ごとに絞って送る（`cookie_header_for_url`）。
    origins: HostScopedCookies,
}

impl FantiaSession {
    /// ログイン時に WebView から収集した形（ホスト → Cookie の並び）から作る。
    pub fn from_collected(origins: BTreeMap<String, Vec<CookieEntry>>) -> Self {
        Self {
            origins: HostScopedCookies::new(origins),
        }
    }

    /// `fantia.jp` の Cookie だけで作る。
    pub fn from_site_cookies(cookies: BTreeMap<String, String>) -> Self {
        Self {
            origins: HostScopedCookies::from_origin(SITE_HOST, cookies),
        }
    }

    /// ログイン済みか。
    ///
    /// **`_session_id` がサイトのホストに載っていること**を要求する（`origins` が空でない
    /// だけでは足りない: 未ログインでも `_ga` / `_gid` 等の Cookie が付く）。他のホスト
    /// （将来 CDN 等を足したとき）の Cookie では真にしない。
    pub fn logged_in(&self) -> bool {
        self.origins.value(SITE_HOST, SESSION_COOKIE).is_some()
    }

    /// 収集元ホストの Cookie 値（認証済み判定・ログ用）。収集元をまたがない。
    pub fn cookie_value(&self, host: &str, name: &str) -> Option<&str> {
        self.origins.value(host, name)
    }

    /// 宛先 URL 向けの Cookie ヘッダ。**その URL のホスト向けに収集した Cookie だけ**を返し、
    /// `Path` / `Secure` / 期限も満たすものだけを載せる（`HostScopedCookies::header_for_url`）。
    pub fn cookie_header_for_url(&self, url: &str) -> String {
        self.origins.header_for_url(url)
    }

    pub fn cookies_count(&self) -> usize {
        self.origins.count()
    }
}

/// ログイン状態の確認に使う API。未認証は 401 と `{"redirect_to":"/sessions/signin"}` を返す
/// （実測 2026-10-15。匿名の `_session_id` でも同じ）。
pub const ME_URL: &str = "https://fantia.jp/api/v1/me";

/// 送る UA。FANZA と同じく**クライアントを名乗る**（`crate::ua` の差し替えは今のところ無し）。
const USER_AGENT: &str = concat!(
    "ThundokuShelf/",
    env!("CARGO_PKG_VERSION"),
    " (+https://github.com/MegaBlackLabel/thundoku-shelf-app)"
);

#[derive(Debug, thiserror::Error)]
pub enum FantiaError {
    #[error("セッション切れ・未ログイン")]
    Unauthorized,
    #[error("HTTP {0}")]
    Http(u16),
    #[error("parsing failed: {0}")]
    Parse(String),
    #[error("db: {0}")]
    Database(#[from] sqlx::Error),
    #[error("transport: {0}")]
    Transport(#[from] TbfError),
    #[error("io: {0}")]
    Io(String),
}

/// Fantia の API クライアント。いまはログイン確認（[`ME_URL`]）と購入履歴の取得だけ。
pub struct FantiaClient {
    transport: Box<dyn Transport>,
    session: FantiaSession,
}

impl FantiaClient {
    pub fn with_transport(transport: Box<dyn Transport>, session: FantiaSession) -> Self {
        Self { transport, session }
    }

    /// ログイン済みか。
    ///
    /// **Cookie の有無では判定できない**: Fantia は未ログインでも `_session_id` を発行する
    /// （実測 2026-10-15。サインインページの GET だけで `Set-Cookie` される）。`/api/v1/me` が
    /// 200 ならログイン済み、401 / 403 なら未ログイン（`Ok(false)`）。
    pub fn logged_in(&mut self) -> Result<bool, FantiaError> {
        let response = self.get(ME_URL)?;
        match response.status {
            200 => Ok(true),
            401 | 403 => Ok(false),
            status => Err(FantiaError::Http(status)),
        }
    }

    /// 注文一覧ページ（`/mypage/users/purchases?page=N`）の HTML。`page` は 1 始まり。
    pub fn purchases_page_html(&mut self, page: usize) -> Result<String, FantiaError> {
        let url = crate::fantia::shop::purchases_page_url(page);
        self.get_html(&url)
    }

    /// 注文ページ（`/mypage/users/purchases/<orderId>`）の HTML。
    pub fn order_page_html(&mut self, order_id: &str) -> Result<String, FantiaError> {
        let url = crate::fantia::shop::order_page_url(order_id);
        self.get_html(&url)
    }

    /// 商品ページ（`/products/<productId>`）の HTML。
    pub fn product_page_html(&mut self, product_id: &str) -> Result<String, FantiaError> {
        let url = crate::fantia::shop::product_page_url(product_id);
        self.get_html(&url)
    }

    /// GET して 2xx の本文を返す。401 / 403 は [`FantiaError::Unauthorized`]。
    fn get_html(&mut self, url: &str) -> Result<String, FantiaError> {
        let response = self.get(url)?;
        match response.status {
            200 => Ok(String::from_utf8_lossy(&response.body).to_string()),
            401 | 403 => Err(FantiaError::Unauthorized),
            status => Err(FantiaError::Http(status)),
        }
    }

    /// ショップの商品本体を `path` へ取る（**ダウンロード**）。
    ///
    /// `/products/<id>/download` は 302 で CDN のファイルへ飛ぶので、`redirects: 0` で
    /// 叩いて `Location` を取り、その URL から本体を取得する。Cookie は**収集元ホストごと**
    /// に持っているため、CDN（`fantia.jp` 以外）へは何も送らない（`cookie_header_for_url` が
    /// 空を返す。これは意図どおり: セッション Cookie を別システムへ広げない）。
    ///
    /// 進捗とキャンセルの規則は `FanzaClient::download_to_file_with_progress` と同じで、
    /// `on_progress` が `false` を返したら転送を中断する。戻り値は書いたバイト数。
    /// 2xx でも HTML（ログイン画面・エラーページ）が返る場合があるので弾く。
    pub fn download_to_file(
        &mut self,
        product_id: &str,
        path: &std::path::Path,
        on_progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<u64, FantiaError> {
        let download_url = crate::fantia::shop::product_download_url(product_id);
        // 1) /download を manual（redirects=0）で叩いて 302 の `Location` を取る。
        //    Cookie を付ける前に宛先を決めない（リダイレクトを追うのはこの 1 回だけ）。
        let spec = RequestSpec {
            method: "GET".into(),
            url: download_url.clone(),
            headers: self.download_headers(&download_url),
            body: None,
            redirects: 0,
        };
        let response = self.transport.send(spec).map_err(FantiaError::Transport)?;
        let location = match response.status {
            301 | 302 | 303 | 307 | 308 => response.header("location").ok_or_else(|| {
                FantiaError::Parse("ダウンロード応答に location がありません".into())
            })?,
            401 | 403 => return Err(FantiaError::Unauthorized),
            status => return Err(FantiaError::Http(status)),
        };
        // 相対の `Location` はサイト基準で解決する（実測は CDN の絶対 URL）
        let destination = resolve_location(&download_url, location)?;
        // 2) 転送先（CDN）から本体を取得する。宛先向けの Cookie だけを載せる
        //    （ホストが違えば 1 つも載らない）。
        let file =
            std::fs::File::create(path).map_err(|error| FantiaError::Io(error.to_string()))?;
        let mut sink = std::io::BufWriter::new(file);
        let result = self
            .transport
            .send_download_to(
                RequestSpec {
                    method: "GET".into(),
                    url: destination.clone(),
                    headers: self.download_headers(&destination),
                    body: None,
                    redirects: 0,
                },
                &mut sink,
                on_progress,
            )
            .map_err(FantiaError::Transport)?;
        std::io::Write::flush(&mut sink).map_err(|error| FantiaError::Io(error.to_string()))?;
        match result.status {
            200..=299 => {}
            401 | 403 => return Err(FantiaError::Unauthorized),
            status => return Err(FantiaError::Http(status)),
        }
        // 2xx でも HTML が返ることがある（ログイン画面・エラーページ）→ 先頭バイトで弾く
        let head = read_head(path)?;
        if head.starts_with(b"<!doctype") || head.starts_with(b"<html") {
            return Err(FantiaError::Parse("HTML response (not a file)".into()));
        }
        Ok(result.bytes)
    }

    /// ダウンロード用のヘッダ。**その URL のホスト向けに収集した Cookie だけ**を載せる
    /// （空なら `Cookie` を付けない）。
    fn download_headers(&self, url: &str) -> Vec<(String, String)> {
        let mut headers = vec![
            ("User-Agent".to_string(), USER_AGENT.to_string()),
            ("Accept".to_string(), "*/*".to_string()),
        ];
        let cookie = self.session.cookie_header_for_url(url);
        if !cookie.is_empty() {
            headers.push(("Cookie".to_string(), cookie));
        }
        headers
    }

    /// セッション Cookie を載せて GET する（リダイレクトは追わない。認証切れは 302 ではなく
    /// 401 で返るため、追う必要が無い）。
    fn get(&mut self, url: &str) -> Result<crate::tbf::transport::ResponseSpec, FantiaError> {
        let spec = RequestSpec {
            method: "GET".into(),
            url: url.into(),
            headers: vec![
                ("Cookie".into(), self.session.cookie_header_for_url(url)),
                ("Accept".into(), "text/html,application/json".into()),
                ("User-Agent".into(), USER_AGENT.into()),
            ],
            body: None,
            redirects: 0,
        };
        Ok(self.transport.send(spec)?)
    }
}

/// `Location`（相対もあり得る）を絶対 URL にする（元の URL を基準に解決する）。
fn resolve_location(base: &str, location: &str) -> Result<String, FantiaError> {
    let base = url::Url::parse(base).map_err(|error| FantiaError::Parse(error.to_string()))?;
    let resolved = base
        .join(location)
        .map_err(|error| FantiaError::Parse(error.to_string()))?;
    Ok(resolved.to_string())
}

/// 書き込んだファイルの先頭 64 バイトを読み戻す（HTML 判定用）。
fn read_head(path: &std::path::Path) -> Result<Vec<u8>, FantiaError> {
    use std::io::Read as _;
    let mut head = [0u8; 64];
    let mut file = std::fs::File::open(path).map_err(|error| FantiaError::Io(error.to_string()))?;
    let read = file
        .read(&mut head)
        .map_err(|error| FantiaError::Io(error.to_string()))?;
    Ok(head[..read].to_vec())
}

/// 既定の transport（`ureq`）でログイン済みかを確かめる（ログイン画面から使う）。
///
/// 判定できないとき（通信失敗など）は **false**（ログイン済みとして保存しない側へ倒す）。
pub fn me_is_authenticated(session: &FantiaSession) -> bool {
    FantiaClient::with_transport(Box::new(UreqTransport::new()), session.clone())
        .logged_in()
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tbf::TbfError;
    use crate::tbf::transport::{RequestSpec, ResponseSpec, Transport};

    /// 未ログインでも `fantia.jp` には `_ga` / `_gid` 等の Cookie が付くため、
    /// 「Cookie が 1 つでもあればログイン済み」とは判定できないこと。
    #[test]
    fn logged_in_requires_the_session_cookie() {
        let anonymous = FantiaSession::from_site_cookies(BTreeMap::from([
            ("_ga".to_string(), "GA1.1.1".to_string()),
            ("_gid".to_string(), "GA1.1.2".to_string()),
        ]));
        assert!(
            !anonymous.logged_in(),
            "未ログイン（_session_id 無し）をログイン済みと判定している"
        );

        let logged_in = FantiaSession::from_site_cookies(BTreeMap::from([(
            SESSION_COOKIE.to_string(),
            "s".to_string(),
        )]));
        assert!(
            logged_in.logged_in(),
            "_session_id があってもログイン済みにならない"
        );
    }

    /// サイトのホスト**以外**の `_session_id` ではログイン済みにしない（収集元をまたがない）。
    #[test]
    fn logged_in_requires_the_cookie_on_the_site_host() {
        let elsewhere = FantiaSession::from_collected(BTreeMap::from([(
            "evil.example.com".to_string(),
            vec![CookieEntry::new("s").named(SESSION_COOKIE)],
        )]));
        assert!(
            !elsewhere.logged_in(),
            "別ホストの _session_id でログイン済みと判定している"
        );
    }

    /// `from_site_cookies` はサイトのホストに置き、そのホスト宛ての送信にだけ載せる。
    #[test]
    fn from_site_cookies_keeps_the_session_on_the_site_host() {
        let session = FantiaSession::from_site_cookies(BTreeMap::from([(
            SESSION_COOKIE.to_string(),
            "abc".to_string(),
        )]));

        // 認証判定（収集元の完全一致で引く）
        assert_eq!(session.cookie_value(SITE_HOST, SESSION_COOKIE), Some("abc"));
        assert_eq!(session.cookie_value("www.fantia.jp", SESSION_COOKIE), None);
        // 宛先 URL への送信
        assert_eq!(
            session.cookie_header_for_url("https://fantia.jp/mypage/users/purchases"),
            "_session_id=abc"
        );
        assert_eq!(
            session.cookie_header_for_url("https://evil.example.com/"),
            ""
        );
        assert_eq!(session.cookies_count(), 1);
        assert!(session.logged_in());
    }

    /// セッション保管（`SessionVault`）へ置けるよう、serde で往復できること。
    #[test]
    fn session_round_trips_through_serde() {
        let session = FantiaSession::from_site_cookies(BTreeMap::from([(
            SESSION_COOKIE.to_string(),
            "abc".to_string(),
        )]));
        let json = serde_json::to_string(&session).expect("シリアライズできる");
        let restored: FantiaSession = serde_json::from_str(&json).expect("デシリアライズできる");
        assert_eq!(restored, session);
        assert!(
            restored.logged_in(),
            "復元したセッションが未ログインになっている"
        );
    }

    // ---- ログイン確認（`/api/v1/me`） -------------------------------------------

    /// 応答をそのまま返し、送ったリクエストを記録する transport。
    struct MockTransport {
        status: u16,
        body: Vec<u8>,
        sent: std::sync::Arc<parking_lot::Mutex<Vec<RequestSpec>>>,
    }

    impl Transport for MockTransport {
        fn send(&mut self, spec: RequestSpec) -> Result<ResponseSpec, TbfError> {
            self.sent.lock().push(spec);
            Ok(ResponseSpec {
                status: self.status,
                headers: vec![],
                body: self.body.clone(),
            })
        }
    }

    fn client_with(
        status: u16,
        body: &str,
    ) -> (
        FantiaClient,
        std::sync::Arc<parking_lot::Mutex<Vec<RequestSpec>>>,
    ) {
        let sent = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let session = FantiaSession::from_site_cookies(BTreeMap::from([(
            SESSION_COOKIE.to_string(),
            "abc".to_string(),
        )]));
        let client = FantiaClient::with_transport(
            Box::new(MockTransport {
                status,
                body: body.as_bytes().to_vec(),
                sent: sent.clone(),
            }),
            session,
        );
        (client, sent)
    }

    /// `/api/v1/me` が 200 ならログイン済み。**セッション Cookie を載せて叩く**こと。
    #[test]
    fn me_returns_logged_in_on_200() {
        let (mut client, sent) = client_with(200, r#"{"id":1,"name":"x"}"#);
        assert!(
            client.logged_in().expect("200 は成功"),
            "200 をログイン済みと判定していない"
        );
        let requests = sent.lock();
        assert_eq!(requests[0].url, ME_URL, "確認先が違う");
        assert!(
            requests[0]
                .headers
                .iter()
                .any(|(name, value)| name.eq_ignore_ascii_case("cookie")
                    && value.contains("_session_id=abc")),
            "セッション Cookie を載せていない: {:?}",
            requests[0].headers
        );
    }

    /// 未ログインの `/api/v1/me` は 401（`{"redirect_to":"/sessions/signin"}`。実測 2026-10-15）。
    /// これはエラーではなく「ログインしていない」として扱う。
    #[test]
    fn me_returns_not_logged_in_on_401() {
        let (mut client, _) = client_with(401, r#"{"redirect_to":"/sessions/signin"}"#);
        assert!(
            !client.logged_in().expect("401 は未ログイン（成功扱い）"),
            "401 をログイン済みと判定している"
        );
    }

    /// それ以外の状態（500 等）はエラー（判定できないので未ログイン扱いにしない）。
    #[test]
    fn me_errors_on_other_status() {
        let (mut client, _) = client_with(500, "boom");
        assert!(client.logged_in().is_err(), "500 を成功として扱っている");
    }

    /// 購入履歴ページの HTML を取れる（セッション Cookie を載せ、URL はページ付き）。
    #[test]
    fn purchases_page_returns_the_html() {
        let (mut client, sent) = client_with(200, "<html>orders</html>");
        let html = client.purchases_page_html(2).expect("200 は成功");
        assert_eq!(html, "<html>orders</html>");
        let requests = sent.lock();
        assert_eq!(
            requests[0].url, "https://fantia.jp/mypage/users/purchases?page=2",
            "一覧の URL が違う"
        );
        assert!(
            requests[0]
                .headers
                .iter()
                .any(|(name, value)| name.eq_ignore_ascii_case("cookie")
                    && value.contains("_session_id=abc")),
            "セッション Cookie を載せていない"
        );
    }

    /// セッション切れ（401）は `Unauthorized`（再ログインの導線を出すため、未ログインと区別する）。
    #[test]
    fn purchases_page_maps_401_to_unauthorized() {
        let (mut client, _) = client_with(401, r#"{"redirect_to":"/sessions/signin"}"#);
        assert!(
            matches!(
                client.purchases_page_html(1),
                Err(FantiaError::Unauthorized)
            ),
            "401 を Unauthorized にしていない"
        );
    }

    // ---- ダウンロード（302 → CDN） ----------------------------------------------

    /// 呼ばれるたびに台本の応答を返し、送ったリクエストを記録する transport。
    /// 台本が尽きたら最後の 1 つを返し続ける。
    struct ScriptedTransport {
        responses: Vec<ResponseSpec>,
        sent: std::sync::Arc<parking_lot::Mutex<Vec<RequestSpec>>>,
    }

    impl Transport for ScriptedTransport {
        fn send(&mut self, spec: RequestSpec) -> Result<ResponseSpec, TbfError> {
            self.sent.lock().push(spec);
            if self.responses.len() > 1 {
                Ok(self.responses.remove(0))
            } else {
                Ok(self.responses[0].clone())
            }
        }
    }

    fn download_temp_path(name: &str) -> std::path::PathBuf {
        let path = std::env::temp_dir().join(format!(
            "thundoku-fantia-download-{}-{name}",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        path
    }

    /// 302 の `Location` を辿って本体を書き、進捗を報告すること。
    /// **CDN へは Cookie を送らない**（収集元ホストの完全一致でしか載せない）。
    #[test]
    fn download_follows_the_redirect_and_writes_the_body() {
        let body = b"PK\x03\x04 zip body".to_vec();
        let sent = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let transport = ScriptedTransport {
            responses: vec![
                ResponseSpec {
                    status: 302,
                    headers: vec![(
                        "Location".to_string(),
                        "https://cdn.fantia.jp/files/abc.zip?token=1".to_string(),
                    )],
                    body: Vec::new(),
                },
                ResponseSpec {
                    status: 200,
                    headers: vec![],
                    body: body.clone(),
                },
            ],
            sent: sent.clone(),
        };
        let session = FantiaSession::from_site_cookies(BTreeMap::from([(
            SESSION_COOKIE.to_string(),
            "abc".to_string(),
        )]));
        let mut client = FantiaClient::with_transport(Box::new(transport), session);

        let path = download_temp_path("follows-redirect");
        let mut progress = Vec::new();
        let bytes = client
            .download_to_file("111111", &path, &mut |downloaded: u64, total: u64| {
                progress.push((downloaded, total));
                true
            })
            .expect("ダウンロードできる");

        assert_eq!(bytes, body.len() as u64);
        assert_eq!(std::fs::read(&path).unwrap(), body, "本体が書かれていない");
        let requests = sent.lock();
        assert_eq!(requests.len(), 2, "リダイレクトを手動で 1 回だけ追う");
        assert_eq!(
            requests[0].url, "https://fantia.jp/products/111111/download",
            "ダウンロード URL が商品 id から組み立てられていない"
        );
        assert!(
            requests[0]
                .headers
                .iter()
                .any(|(name, value)| name.eq_ignore_ascii_case("cookie")
                    && value.contains("_session_id=abc")),
            "サイトへの要求にセッション Cookie を載せていない"
        );
        assert_eq!(requests[0].redirects, 0, "リダイレクトを自動で追っている");
        assert_eq!(
            requests[1].url,
            "https://cdn.fantia.jp/files/abc.zip?token=1"
        );
        assert!(
            !requests[1]
                .headers
                .iter()
                .any(|(name, _)| name.eq_ignore_ascii_case("cookie")),
            "CDN へセッション Cookie を送っている"
        );
        assert!(!progress.is_empty(), "進捗が 1 度も報告されていない");
        assert_eq!(
            progress.last().copied(),
            Some((body.len() as u64, body.len() as u64)),
            "最後の進捗が全長になっていない"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// 2xx でも HTML（ログイン画面・エラーページ）が返る場合はファイルとして受け取らない。
    #[test]
    fn download_rejects_an_html_body() {
        let sent = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let transport = ScriptedTransport {
            responses: vec![
                ResponseSpec {
                    status: 302,
                    headers: vec![(
                        "Location".to_string(),
                        "https://cdn.fantia.jp/login".to_string(),
                    )],
                    body: Vec::new(),
                },
                ResponseSpec {
                    status: 200,
                    headers: vec![],
                    body: b"<html><body>sign in</body></html>".to_vec(),
                },
            ],
            sent,
        };
        let session = FantiaSession::from_site_cookies(BTreeMap::from([(
            SESSION_COOKIE.to_string(),
            "abc".to_string(),
        )]));
        let mut client = FantiaClient::with_transport(Box::new(transport), session);

        let path = download_temp_path("rejects-html");
        let result = client.download_to_file("111111", &path, &mut |_, _| true);
        assert!(
            matches!(result, Err(FantiaError::Parse(_))),
            "HTML をファイルとして受け取っている: {result:?}"
        );
        let _ = std::fs::remove_file(&path);
    }

    /// セッション切れ（401 / 403）は `Unauthorized`。
    #[test]
    fn download_maps_401_and_403_to_unauthorized() {
        for status in [401u16, 403] {
            let sent = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
            let transport = ScriptedTransport {
                responses: vec![ResponseSpec {
                    status,
                    headers: vec![],
                    body: b"{\"redirect_to\":\"/sessions/signin\"}".to_vec(),
                }],
                sent: sent.clone(),
            };
            let session = FantiaSession::from_site_cookies(BTreeMap::from([(
                SESSION_COOKIE.to_string(),
                "abc".to_string(),
            )]));
            let mut client = FantiaClient::with_transport(Box::new(transport), session);

            let path = download_temp_path(&format!("unauthorized-{status}"));
            let result = client.download_to_file("111111", &path, &mut |_, _| true);
            assert!(
                matches!(result, Err(FantiaError::Unauthorized)),
                "{status} を Unauthorized にしていない: {result:?}"
            );
            assert_eq!(sent.lock().len(), 1, "認証エラーで先を叩いている");
            let _ = std::fs::remove_file(&path);
        }
    }

    /// 中継が 401 / 403 を返した場合（2 ホップ目）も `Unauthorized`。
    #[test]
    fn download_maps_a_cdn_401_to_unauthorized() {
        let sent = std::sync::Arc::new(parking_lot::Mutex::new(Vec::new()));
        let transport = ScriptedTransport {
            responses: vec![
                ResponseSpec {
                    status: 302,
                    headers: vec![(
                        "Location".to_string(),
                        "https://cdn.fantia.jp/files/abc.zip".to_string(),
                    )],
                    body: Vec::new(),
                },
                ResponseSpec {
                    status: 403,
                    headers: vec![],
                    body: b"forbidden".to_vec(),
                },
            ],
            sent,
        };
        let session = FantiaSession::from_site_cookies(BTreeMap::from([(
            SESSION_COOKIE.to_string(),
            "abc".to_string(),
        )]));
        let mut client = FantiaClient::with_transport(Box::new(transport), session);

        let path = download_temp_path("cdn-unauthorized");
        let result = client.download_to_file("111111", &path, &mut |_, _| true);
        assert!(
            matches!(result, Err(FantiaError::Unauthorized)),
            "CDN の 403 を Unauthorized にしていない: {result:?}"
        );
        let _ = std::fs::remove_file(&path);
    }
}
