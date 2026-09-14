//! Google OAuth 2.0 installed-app flow (PKCE S256) with a loopback receiver.
//! Desktop app talks to Google directly — no Workers involved.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::tbf::{RequestSpec, ResponseSpec, TbfError, Transport, UreqTransport};

pub const GOOGLE_OAUTH_AUTH: &str = "https://accounts.google.com/o/oauth2/v2/auth";
pub const GOOGLE_OAUTH_TOKEN: &str = "https://oauth2.googleapis.com/token";
pub const GOOGLE_USERINFO: &str = "https://www.googleapis.com/oauth2/v3/userinfo";
pub const DEFAULT_REDIRECT_PORT: u16 = 38387;
/// `drive.readonly` + `drive.file` — the app never uses `appdata`.
const OAUTH_SCOPE: &str = "openid email https://www.googleapis.com/auth/drive.readonly https://www.googleapis.com/auth/drive.file";
/// Refresh tokens this many seconds before expiry.
const REFRESH_SKEW_SECONDS: i64 = 60;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix seconds when the access token expires.
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleProfile {
    pub sub: String,
    pub email: String,
    pub name: String,
    pub picture: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum GoogleError {
    #[error("io error: {0}")]
    Io(String),
    #[error("authorization failed: {0}")]
    Auth(String),
    #[error("token exchange failed: {0}")]
    Token(String),
    #[error("network error: {0}")]
    Network(String),
    #[error("refresh token unavailable: re-authorize required")]
    NoRefreshToken,
    /// リフレッシュトークンが失効・取り消しされている（再ログインが必要）。
    ///
    /// Google 側で revoke された / 長期間未使用で失効した場合に返る（`invalid_grant`）。
    /// 生の応答（JSON）を画面に出さず、再ログインへ誘導するための型。
    #[error("refresh token expired or revoked: re-authorize required")]
    RefreshTokenRevoked,
    #[error("not authorized")]
    NotAuthorized,
    #[error("authorization cancelled")]
    Cancelled,
}

impl From<TbfError> for GoogleError {
    fn from(value: TbfError) -> Self {
        GoogleError::Network(value.to_string())
    }
}

fn base64_url_no_pad(bytes: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// RFC 7636 code verifier: 32 random bytes, base64url without padding.
pub fn generate_verifier() -> String {
    let mut bytes = [0u8; 32];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    base64_url_no_pad(&bytes)
}

/// OAuth `state` nonce.
pub fn generate_state() -> String {
    let mut bytes = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut bytes);
    base64_url_no_pad(&bytes)
}

/// S256 challenge: base64url(SHA-256(verifier)).
pub fn build_challenge(verifier: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(verifier.as_bytes());
    base64_url_no_pad(&digest)
}

/// Authorize URL for the installed-app flow (PKCE, offline access).
pub fn build_authorize_url(
    client_id: &str,
    redirect_uri: &str,
    challenge: &str,
    state: &str,
) -> String {
    let scope = percent_encode(OAUTH_SCOPE);
    let redirect = percent_encode(redirect_uri);
    format!(
        "{GOOGLE_OAUTH_AUTH}?client_id={client_id}&redirect_uri={redirect}&response_type=code&scope={scope}&access_type=offline&prompt=consent&state={state}&code_challenge={challenge}&code_challenge_method=S256"
    )
}

/// Parse the `/token` endpoint response; `expires_at` is computed from the
/// current wall clock.
pub fn parse_token_response(body: &[u8]) -> Result<OAuthTokens, GoogleError> {
    let payload: Value = serde_json::from_slice(body)
        .map_err(|e| GoogleError::Token(format!("invalid JSON: {e}")))?;
    if let Some(error) = payload.get("error").and_then(Value::as_str) {
        let description = payload
            .get("error_description")
            .and_then(Value::as_str)
            .unwrap_or("");
        return Err(GoogleError::Token(format!("{error}: {description}")));
    }
    let access_token = payload
        .get("access_token")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| GoogleError::Token("missing access_token".into()))?
        .to_string();
    let refresh_token = payload
        .get("refresh_token")
        .and_then(Value::as_str)
        .map(String::from);
    let expires_in = payload
        .get("expires_in")
        .and_then(Value::as_i64)
        .unwrap_or(3600);
    let expires_at = chrono::Utc::now().timestamp() + expires_in;
    Ok(OAuthTokens {
        access_token,
        refresh_token,
        expires_at,
    })
}

/// Accept exactly one connection on the loopback listener and return the
/// authorization `code` (verifying `state`). Responds with a short HTML
/// page telling the user to close the tab.
/// Non-blocking accept with polling: returns `GoogleError::Cancelled` when
/// `cancel` is set, and times out after 5 minutes.
pub fn receive_callback(
    listener: TcpListener,
    expected_state: &str,
    cancel: Option<&AtomicBool>,
) -> Result<String, GoogleError> {
    listener
        .set_nonblocking(true)
        .map_err(|e| GoogleError::Io(e.to_string()))?;
    let started = Instant::now();
    let timeout = Duration::from_secs(300);
    let (mut stream, _) = loop {
        if cancel.is_some_and(|c| c.load(Ordering::Relaxed)) {
            return Err(GoogleError::Cancelled);
        }
        if started.elapsed() > timeout {
            return Err(GoogleError::Auth("authorization timed out".into()));
        }
        match listener.accept() {
            Ok(accepted) => break accepted,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(GoogleError::Io(e.to_string())),
        }
    };
    // macOS では accept したソケットが listener の非ブロッキング設定を継承する。
    // そのままだと read がデータ到着前に即 EAGAIN を返し、有効なコールバックでも
    // 認証が失敗する（起動直後のブラウザからの 1 発目で起きやすい）。
    stream
        .set_nonblocking(false)
        .map_err(|e| GoogleError::Io(e.to_string()))?;
    stream
        .set_read_timeout(Some(Duration::from_secs(120)))
        .map_err(|e| GoogleError::Io(e.to_string()))?;
    // リクエストは TCP の分割や書き手の複数 write で部分的に届き得る。1 回の
    // read で打ち切るとクエリの途中で切れて state が空になり、有効なコールバック
    // でも認証失敗になる。ヘッダ終端（\r\n\r\n）まで読み切る。
    let mut buffer = [0u8; 4096];
    let mut filled = 0usize;
    let request = loop {
        if filled == buffer.len() {
            break String::from_utf8_lossy(&buffer).into_owned();
        }
        let read = stream
            .read(&mut buffer[filled..])
            .map_err(|e| GoogleError::Io(e.to_string()))?;
        if read == 0 {
            break String::from_utf8_lossy(&buffer[..filled]).into_owned();
        }
        filled += read;
        if buffer[..filled].windows(4).any(|w| w == b"\r\n\r\n") {
            break String::from_utf8_lossy(&buffer[..filled]).into_owned();
        }
    };
    let path = request
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .unwrap_or("");
    let Some(query) = path.split_once('?').map(|(_, q)| q) else {
        respond(
            &mut stream,
            "認証に失敗しました。このタブを閉じてください。",
        );
        return Err(GoogleError::Auth("missing query parameters".into()));
    };
    let params: Vec<(String, String)> = query
        .split('&')
        .filter_map(|pair| {
            let (key, value) = pair.split_once('=')?;
            Some((percent_decode(key), percent_decode(value)))
        })
        .collect();
    let get = |name: &str| {
        params
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    if let Some(error) = get("error") {
        log::error!("google callback error: {error}");
        respond(
            &mut stream,
            "認証に失敗しました。このタブを閉じてください。",
        );
        return Err(GoogleError::Auth(format!("OAuth error: {error}")));
    }
    if get("state").as_deref() != Some(expected_state) {
        respond(
            &mut stream,
            "認証に失敗しました。このタブを閉じてください。",
        );
        return Err(GoogleError::Auth("state mismatch".into()));
    }
    let code = match get("code") {
        Some(code) => code,
        None => {
            respond(
                &mut stream,
                "認証に失敗しました。このタブを閉じてください。",
            );
            return Err(GoogleError::Auth("missing code".into()));
        }
    };
    respond(&mut stream, "認証完了。このタブを閉じてください。");
    Ok(code)
}

fn respond(stream: &mut TcpStream, message: &str) {
    let body = format!("<html><body><p>{message}</p></body></html>");
    let response = format!(
        "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    );
    let _ = stream.write_all(response.as_bytes());
}

fn bind_loopback() -> Result<TcpListener, GoogleError> {
    use socket2::{Domain, Protocol, Socket, Type};
    use std::net::{Ipv4Addr, SocketAddr};

    // Google Cloud Console に登録した redirect_uri（http://127.0.0.1:38387）と
    // 必ず一致させるため、SO_REUSEADDR を設定して固定ポートに確実にバインドする。
    // （動的ポートにフォールバックすると redirect_uri が一致せず invalid_request になる）
    let preferred: SocketAddr = SocketAddr::new(Ipv4Addr::LOCALHOST.into(), DEFAULT_REDIRECT_PORT);
    if let Ok(socket) = Socket::new(Domain::IPV4, Type::STREAM, Some(Protocol::TCP)) {
        let _ = socket.set_reuse_address(true);
        if socket.bind(&preferred.into()).is_ok() {
            let _ = socket.listen(128);
            return Ok(socket.into());
        }
    }
    TcpListener::bind("127.0.0.1:0").map_err(|e| GoogleError::Io(e.to_string()))
}
fn percent_encode(value: &str) -> String {
    use percent_encoding::{AsciiSet, NON_ALPHANUMERIC};
    // RFC 3986 unreserved characters stay unencoded.
    const ENCODE_SET: &AsciiSet = &NON_ALPHANUMERIC
        .remove(b'-')
        .remove(b'.')
        .remove(b'_')
        .remove(b'~');
    percent_encoding::utf8_percent_encode(value, ENCODE_SET).to_string()
}
/// Open the authorize URL in the system browser.
pub fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map(|_| ())
    }
    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn()
            .map(|_| ())
    }
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map(|_| ())
    }
}

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

/// In-flight OAuth authorization (loopback listener + PKCE state).
/// Created by `GoogleClient::begin_authorize`, completed by `finish_authorize`.
pub struct PendingGoogleAuth {
    listener: TcpListener,
    verifier: String,
    /// OAuth `state` value (validated on callback).
    pub state: String,
    /// Loopback redirect URI (for debugging/UI).
    pub redirect_uri: String,
    /// Authorize URL to open in a browser (or WebView).
    pub url: String,
    cancel: Arc<AtomicBool>,
}

impl PendingGoogleAuth {
    /// Cancel the in-flight authorization. `finish_authorize` returns
    /// `GoogleError::Cancelled` shortly after.
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::Relaxed);
    }

    /// Shared cancellation handle (e.g. for a UI button to hold).
    pub fn cancel_handle(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }
}

pub struct GoogleClient {
    transport: Box<dyn Transport>,
    client_id: String,
    client_secret: Option<String>,
    tokens: Option<OAuthTokens>,
    /// リフレッシュトークンが失効・取り消し済みと判明した。
    ///
    /// 一度分かれば再ログインまで何度試しても同じ結果になるので、**再試行しない**。
    /// これが無いと、失効後に Drive 系の呼び出し（ファイルごとなど）が
    /// リフレッシュを連打して UI を巻き込む（実測: 数秒で数十回リクエストしていた）。
    refresh_revoked: bool,
}

impl GoogleClient {
    pub fn new(client_id: impl Into<String>, client_secret: Option<String>) -> Self {
        Self::with_transport(Box::new(UreqTransport::new()), client_id, client_secret)
    }

    pub fn with_transport(
        transport: Box<dyn Transport>,
        client_id: impl Into<String>,
        client_secret: Option<String>,
    ) -> Self {
        Self {
            transport,
            client_id: client_id.into(),
            client_secret,
            tokens: None,
            refresh_revoked: false,
        }
    }

    pub fn tokens(&self) -> Option<&OAuthTokens> {
        self.tokens.as_ref()
    }

    /// 保存済みトークンがあるか（ネットワークに行かずに確認する）。
    pub fn has_tokens(&self) -> bool {
        self.tokens.is_some()
    }

    pub fn restore_tokens(&mut self, tokens: OAuthTokens) {
        self.tokens = Some(tokens);
        // 新しいセッションなので失効の記憶を戻す
        self.refresh_revoked = false;
    }

    pub fn logout(&mut self) {
        self.tokens = None;
        self.refresh_revoked = false;
    }

    pub fn is_authenticated(&self) -> bool {
        self.tokens.is_some()
    }

    pub fn client_id(&self) -> &str {
        &self.client_id
    }

    /// Full installed-app flow: loopback listener -> system browser ->
    /// callback -> token exchange -> profile.
    pub fn authorize(&mut self) -> Result<GoogleProfile, GoogleError> {
        let pending = self.begin_authorize()?;
        // 認可 URL には state（CSRF ノンス）と code_challenge が含まれる。
        // ログに残すと不正コールバックの成立に使われ得るため、ホストと path だけにする。
        log::info!(
            "google authorize start: {}",
            pending
                .url
                .split(['?', '#'])
                .next()
                .unwrap_or("https://accounts.google.com/o/oauth2/v2/auth")
        );
        open_browser(&pending.url).map_err(|e| GoogleError::Io(e.to_string()))?;
        self.finish_authorize(pending)
    }

    /// Start the OAuth flow: bind the loopback listener and build the
    /// authorize URL (without opening any browser). The caller decides how
    /// to present the URL (system browser or in-app WebView).
    pub fn begin_authorize(&self) -> Result<PendingGoogleAuth, GoogleError> {
        let listener = bind_loopback()?;
        let redirect_uri = format!(
            "http://127.0.0.1:{}",
            listener
                .local_addr()
                .map_err(|e| GoogleError::Io(e.to_string()))?
                .port()
        );
        let verifier = generate_verifier();
        let challenge = build_challenge(&verifier);
        let state = generate_state();
        let url = build_authorize_url(&self.client_id, &redirect_uri, &challenge, &state);
        Ok(PendingGoogleAuth {
            listener,
            verifier,
            state,
            redirect_uri,
            url,
            cancel: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Wait for the callback (or cancellation), exchange the code, and fetch
    /// the profile. Blocking; polled every 100 ms for `cancel`.
    pub fn finish_authorize(
        &mut self,
        pending: PendingGoogleAuth,
    ) -> Result<GoogleProfile, GoogleError> {
        let code = receive_callback(pending.listener, &pending.state, Some(&pending.cancel))?;
        log::info!("google callback received, exchanging code");
        let tokens = self.exchange_code(&code, &pending.verifier, &pending.redirect_uri)?;
        self.tokens = Some(tokens);
        self.profile()
    }

    /// Exchange an authorization code for tokens. Desktop clients use PKCE
    /// only; `client_secret` is only sent when explicitly configured.
    pub fn exchange_code(
        &mut self,
        code: &str,
        verifier: &str,
        redirect_uri: &str,
    ) -> Result<OAuthTokens, GoogleError> {
        log::info!(
            "token exchange: client_id={}, client_secret={}",
            self.client_id,
            if self.client_secret.is_some() {
                "configured"
            } else {
                "MISSING"
            }
        );
        let mut form = format!(
            "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
            percent_encode(code),
            percent_encode(redirect_uri),
            percent_encode(&self.client_id),
            percent_encode(verifier),
        );
        if let Some(secret) = &self.client_secret {
            form.push_str(&format!("&client_secret={}", percent_encode(secret)));
        }
        let response = self.post_token_form(&form)?;
        if !(200..300).contains(&response.status) {
            log::error!(
                "token exchange failed: status {}, body: {}",
                response.status,
                String::from_utf8_lossy(&response.body).trim()
            );
            return Err(GoogleError::Token(format!(
                "token endpoint status {}: {}",
                response.status,
                String::from_utf8_lossy(&response.body).trim()
            )));
        }
        log::info!("token exchange succeeded (status {})", response.status);
        parse_token_response(&response.body)
    }

    /// Refresh using the stored refresh token; keeps the old refresh token
    /// when the endpoint omits a new one.
    pub fn refresh_tokens(&mut self) -> Result<(), GoogleError> {
        // 失効が判明済みなら、ネットワークに出ずに即返す（連打を防ぐ）
        if self.refresh_revoked {
            return Err(GoogleError::RefreshTokenRevoked);
        }
        let refresh_token = self
            .tokens
            .as_ref()
            .and_then(|t| t.refresh_token.clone())
            .ok_or(GoogleError::NoRefreshToken)?;
        let mut form = format!(
            "grant_type=refresh_token&refresh_token={}&client_id={}",
            percent_encode(&refresh_token),
            percent_encode(&self.client_id),
        );
        if let Some(secret) = &self.client_secret {
            form.push_str(&format!("&client_secret={}", percent_encode(secret)));
        }
        let response = self.post_token_form(&form)?;
        if !(200..300).contains(&response.status) {
            let body = String::from_utf8_lossy(&response.body);
            log::error!(
                "token refresh failed: status {}, body: {}",
                response.status,
                body.trim()
            );
            // `invalid_grant` はリフレッシュトークンが失効・取り消しされている状態。
            // 生の応答を画面に出さず、呼び出し側が再ログインへ誘導できるようにする。
            if body.contains("invalid_grant") {
                // 再ログインまで何度試しても同じなので記憶する
                self.refresh_revoked = true;
                return Err(GoogleError::RefreshTokenRevoked);
            }
            return Err(GoogleError::Token(format!(
                "token endpoint status {}: {}",
                response.status,
                body.trim()
            )));
        }
        let mut tokens = parse_token_response(&response.body)?;
        if tokens.refresh_token.is_none() {
            tokens.refresh_token = Some(refresh_token);
        }
        self.tokens = Some(tokens);
        Ok(())
    }

    /// Current access token, refreshing first when expired (with skew).
    pub fn access_token(&mut self) -> Result<String, GoogleError> {
        let Some(tokens) = &self.tokens else {
            return Err(GoogleError::NotAuthorized);
        };
        if chrono::Utc::now().timestamp() >= tokens.expires_at - REFRESH_SKEW_SECONDS {
            self.refresh_tokens()?;
        }
        Ok(self
            .tokens
            .as_ref()
            .expect("tokens set above")
            .access_token
            .clone())
    }

    /// Current profile from the userinfo endpoint.
    pub fn profile(&mut self) -> Result<GoogleProfile, GoogleError> {
        let token = self.access_token()?;
        let response = self.request_raw(
            "GET",
            GOOGLE_USERINFO,
            &[("Authorization", &format!("Bearer {token}"))],
            None,
            5,
        )?;
        if !(200..300).contains(&response.status) {
            return Err(GoogleError::Auth(format!(
                "userinfo status {}",
                response.status
            )));
        }
        let payload: Value = serde_json::from_slice(&response.body)
            .map_err(|e| GoogleError::Auth(format!("invalid userinfo JSON: {e}")))?;
        Ok(GoogleProfile {
            sub: payload
                .get("sub")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            email: payload
                .get("email")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            name: payload
                .get("name")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string(),
            picture: payload
                .get("picture")
                .and_then(Value::as_str)
                .map(String::from),
        })
    }

    fn post_token_form(&mut self, form: &str) -> Result<ResponseSpec, GoogleError> {
        self.request_raw(
            "POST",
            GOOGLE_OAUTH_TOKEN,
            &[("Content-Type", "application/x-www-form-urlencoded")],
            Some(form.as_bytes().to_vec()),
            5,
        )
    }

    fn request_raw(
        &mut self,
        method: &str,
        url: &str,
        headers: &[(&str, &str)],
        body: Option<Vec<u8>>,
        redirects: u32,
    ) -> Result<ResponseSpec, GoogleError> {
        self.transport
            .send(RequestSpec {
                method: method.to_string(),
                url: url.to_string(),
                headers: headers
                    .iter()
                    .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
                    .collect(),
                body,
                redirects,
            })
            .map_err(GoogleError::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tbf::{RequestSpec, ResponseSpec, Transport};

    struct CaptureTransport {
        captured: std::sync::Arc<std::sync::Mutex<Option<RequestSpec>>>,
    }

    impl Transport for CaptureTransport {
        fn send(&mut self, req: RequestSpec) -> Result<ResponseSpec, TbfError> {
            *self.captured.lock().unwrap() = Some(req);
            Ok(ResponseSpec {
                status: 200,
                headers: Vec::new(),
                body: br#"{"access_token":"tok","expires_in":3600,"token_type":"Bearer","refresh_token":"ref"}"#
                    .to_vec(),
            })
        }
    }

    type Captured = std::sync::Arc<std::sync::Mutex<Option<RequestSpec>>>;

    fn client_with(secret: Option<&str>) -> (GoogleClient, Captured) {
        let captured: Captured = Default::default();
        let client = GoogleClient::with_transport(
            Box::new(CaptureTransport {
                captured: captured.clone(),
            }),
            "cid",
            secret.map(String::from),
        );
        (client, captured)
    }

    #[test]
    fn exchange_includes_client_secret_when_configured() {
        let (mut client, transport) = client_with(Some("csecret"));
        client
            .exchange_code("code", "verifier", "http://127.0.0.1:38387")
            .expect("exchange");
        let req = transport.lock().unwrap().take().unwrap();
        let body = String::from_utf8(req.body.unwrap()).unwrap();
        assert!(
            body.contains("client_secret=csecret"),
            "client_secret must be sent when configured: {body}"
        );
    }

    #[test]
    fn exchange_omits_client_secret_when_not_configured() {
        let (mut client, transport) = client_with(None);
        client
            .exchange_code("code", "verifier", "http://127.0.0.1:38387")
            .expect("exchange");
        let req = transport.lock().unwrap().take().unwrap();
        let body = String::from_utf8(req.body.unwrap()).unwrap();
        assert!(
            !body.contains("client_secret="),
            "client_secret must not be sent when unconfigured: {body}"
        );
    }

    /// 失効が分かったあとは再試行しないこと。
    ///
    /// 実機で、失効後に Drive 系の呼び出しが**リフレッシュを連打**して（数秒で数十回）
    /// アプリが固まる症状が出たため、一度失敗したら再ログインまでネットワークに出ない。
    #[test]
    fn revoked_token_is_not_retried() {
        /// 呼ばれた回数を数えつつ、常に 400 `invalid_grant` を返す transport。
        struct CountingTransport {
            calls: std::sync::Arc<std::sync::atomic::AtomicUsize>,
        }

        impl Transport for CountingTransport {
            fn send(&mut self, _req: RequestSpec) -> Result<ResponseSpec, TbfError> {
                self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                Ok(ResponseSpec {
                    status: 400,
                    headers: Vec::new(),
                    body: br#"{"error":"invalid_grant","error_description":"expired"}"#.to_vec(),
                })
            }
        }

        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let mut client = GoogleClient::with_transport(
            Box::new(CountingTransport {
                calls: calls.clone(),
            }),
            "cid",
            None,
        );
        client.restore_tokens(OAuthTokens {
            access_token: "old".to_string(),
            expires_at: 0,
            refresh_token: Some("dead".to_string()),
        });

        for _ in 0..5 {
            assert!(matches!(
                client.refresh_tokens(),
                Err(GoogleError::RefreshTokenRevoked)
            ));
        }
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "失効後はネットワークに出ない（連打しない）"
        );

        // 再ログイン（logout → 新しいトークン）なら、また試せる
        client.logout();
        client.restore_tokens(OAuthTokens {
            access_token: "new".to_string(),
            expires_at: 0,
            refresh_token: Some("fresh".to_string()),
        });
        let _ = client.refresh_tokens();
        assert_eq!(
            calls.load(std::sync::atomic::Ordering::SeqCst),
            2,
            "新しいセッションでは再試行する"
        );
    }

    /// リフレッシュトークンが失効・取り消しされている場合（`invalid_grant`）は、
    /// 生の JSON を画面に出さずに専用のエラーとして返すこと。
    /// 呼び出し側が再ログインへ誘導できるようにするため。
    #[test]
    fn refresh_reports_a_revoked_token_as_a_typed_error() {
        /// 常に 400 `invalid_grant` を返す transport。
        struct RevokedTransport;
        impl Transport for RevokedTransport {
            fn send(&mut self, _req: RequestSpec) -> Result<ResponseSpec, TbfError> {
                Ok(ResponseSpec {
                    status: 400,
                    headers: Vec::new(),
                    body: br#"{"error":"invalid_grant","error_description":"Token has been expired or revoked."}"#
                        .to_vec(),
                })
            }
        }

        let mut client = GoogleClient::with_transport(Box::new(RevokedTransport), "cid", None);
        client.restore_tokens(OAuthTokens {
            access_token: "old".to_string(),
            expires_at: 0,
            refresh_token: Some("dead".to_string()),
        });

        let error = client.refresh_tokens().expect_err("失効を検出する");

        assert!(
            matches!(error, GoogleError::RefreshTokenRevoked),
            "失効は専用のエラーで返す: {error:?}"
        );
        let message = error.to_string();
        assert!(
            !message.contains("invalid_grant") && !message.contains('{'),
            "生の応答を文言に出さない: {message}"
        );
    }
}
