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
/// `drive.file` のみ（アプリが作成・利用者が選択したファイルだけ）。
///
/// 以前は `drive.readonly`（Drive 全体の閲覧・ダウンロード）も要求していたが、
/// 本アプリが触るのは自分で作った `thundoku-shelf/` フォルダとその中の
/// バックアップファイルだけなので、`drive.file` で足りる。トークンが漏れた場合の
/// 影響を「アプリが作成したファイル」に限定する。appdata は使わない。
const OAUTH_SCOPE: &str = "openid email https://www.googleapis.com/auth/drive.file";
/// Refresh tokens this many seconds before expiry.
const REFRESH_SKEW_SECONDS: i64 = 60;

#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct OAuthTokens {
    pub access_token: String,
    pub refresh_token: Option<String>,
    /// Unix seconds when the access token expires.
    pub expires_at: i64,
}

/// `sub` は所有者（`books.owner_sub`）の判定に使うため keyring に永続化する
/// （[`save_profile`]）。
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GoogleProfile {
    pub sub: String,
    pub email: String,
    pub name: String,
    pub picture: Option<String>,
}

/// ログ用のラベル。**`sub` を含めない**。
///
/// v3 の pack の鍵は乱数のルート鍵（PRK）で、`sub` は PRK のラップを解く材料
/// （`KEK_sub` と `owner_id`）なので実質の復号秘密である。ログに書くと pack と
/// `thundoku-keys.json` を入手した第三者へ復号材料を渡すことになる（Windows は
/// `%TEMP%/thundoku-shelf/thundoku.log` に既定 debug レベルで残る）。有無だけを伝える。
pub fn profile_log_label(profile: Option<&GoogleProfile>) -> &'static str {
    if profile.is_some() {
        "あり"
    } else {
        "なし"
    }
}

/// 取得済みプロフィールを keyring に保存する（**ブロッキング**。UI スレッドでは呼ばない）。
///
/// 起動直後はプロフィールをネットワーク取得しない（設定画面を開くまで `userinfo` を
/// 叩かない）ため、`books.owner_sub` の判定に使う `sub` を次回起動でオフライン復元
/// できるようにする。
pub fn save_profile(
    store: &crate::secrets::SecretStore,
    profile: &GoogleProfile,
) -> Result<(), crate::secrets::SecretError> {
    let json = serde_json::to_string(profile)
        .map_err(|error| crate::secrets::SecretError::Encoding(error.to_string()))?;
    store.save(crate::secrets::USER_GOOGLE_PROFILE, &json)
}

/// 保存済みプロフィール。未保存・壊れている・項目が欠けている場合は `None`
/// （復元できなくても起動は続ける）。
pub fn saved_profile(store: &crate::secrets::SecretStore) -> Option<GoogleProfile> {
    let json = store
        .load(crate::secrets::USER_GOOGLE_PROFILE)
        .ok()
        .flatten()?;
    match serde_json::from_str(&json) {
        Ok(profile) => Some(profile),
        Err(error) => {
            log::warn!("google profile: 保存値を復元できないため無視します: {error}");
            None
        }
    }
}

/// 保存済みプロフィールを削除する（ログアウト時）。**削除できたか**を返す。
///
/// 呼び出し側が結果を必要としない場合（利用者への表示が別経路のとき）は `let _ =` で
/// 捨ててよいが、起動時の「前回のログアウトで消せなかった資格情報を復元しない」判定は
/// この戻り値で行う（セキュリティ評価 F04）。
pub fn delete_saved_profile(store: &crate::secrets::SecretStore) -> bool {
    match store.delete(crate::secrets::USER_GOOGLE_PROFILE) {
        Ok(()) => true,
        Err(error) => {
            log::warn!("google profile: 保存値を削除できません: {error}");
            false
        }
    }
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

/// Parse the `userinfo` endpoint response.
///
/// `sub` は必須（欠落 / 空 / 文字列でない値は認証エラー）。`sub` は
/// `books.owner_sub` の判定と、PRK のラップを解く鍵（`KEK_sub`）・`owner_id`
/// （keyring のスロット名）に使う実質の秘密であり、空文字を成功として通すと
/// 自分の鍵を引けない組み合わせができる（`docs/spec/10-pack-keys.md` §4）。
/// 表示に使う `email` / `name` は欠けても空文字で埋める。
pub fn parse_userinfo(body: &[u8]) -> Result<GoogleProfile, GoogleError> {
    let payload: Value = serde_json::from_slice(body)
        .map_err(|e| GoogleError::Auth(format!("invalid userinfo JSON: {e}")))?;
    let sub = payload
        .get("sub")
        .and_then(Value::as_str)
        .filter(|sub| !sub.trim().is_empty())
        .ok_or_else(|| GoogleError::Auth("userinfo response has no sub".into()))?
        .to_string();
    Ok(GoogleProfile {
        sub,
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

/// ループバックのコールバック（`code`）を待つ。
///
/// **クライアントの状態を触らない**ので、待っている間 `GoogleClient` のロックを
/// 保持しない。保持すると最大 5 分の待ちの間 UI 側の `google.lock()` が止まり、
/// ログインモーダルの ✕ も効かなくなる（アプリが固まったように見える）。
pub fn wait_for_code(pending: &PendingGoogleAuth) -> Result<String, GoogleError> {
    receive_callback(&pending.listener, &pending.state, Some(&pending.cancel))
}

/// Accept exactly one connection on the loopback listener and return the
/// authorization `code` (verifying `state`). Responds with a short HTML
/// page telling the user to close the tab.
/// Non-blocking accept with polling: returns `GoogleError::Cancelled` when
/// `cancel` is set, and times out after 5 minutes.
pub fn receive_callback(
    listener: &TcpListener,
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
/// システムブラウザで URL を開く。
///
/// Windows は **`ShellExecuteW`（OS の API）**を使う。**コマンドライン経由にしない**:
/// `cmd /C start "" <URL>` は `cmd` が `&` をコマンド区切り、`%XX` を環境変数として
/// 解釈するため、認可 URL が最初の `&` で切れて渡る（実測: `cmd /C echo <URL>` の出力は
/// `?client_id=…` まで）。必須パラメータが落ちると Google は 400 `invalid_request` を返す。
/// `explorer <URL>` も不可（URL を渡すとエクスプローラーが開くだけで既定ブラウザが
/// 開かない。実測 2026-09-23）。`ShellExecuteW` は URL をそのままシェルへ渡す。
pub fn open_browser(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let operation = wide("open");
        let target = wide(url);
        // SAFETY: 3 つの文字列はこの関数の間だけ生きる NUL 終端 UTF-16 で、
        // ポインタ引数は ShellExecuteW が呼び出し中しか読まない。
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                operation.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        // ShellExecute の戻り値は「32 以下なら失敗」と決まっている。
        if (result as isize) <= 32 {
            return Err(std::io::Error::other(format!(
                "ShellExecuteW failed ({})",
                result as isize
            )));
        }
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let program = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(program)
            .arg(url)
            .spawn()
            .map(|_| ())
    }
}

/// NUL 終端の UTF-16（Windows の `*W` API 用）。
#[cfg(target_os = "windows")]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
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
        let code = crate::google::wait_for_code(&pending)?;
        self.complete_authorize(&pending, &code)
    }

    /// Exchange the received `code` for tokens and fetch the profile
    /// (`&mut self` が要る側だけ。待つ側は `wait_for_code`）。
    pub fn complete_authorize(
        &mut self,
        pending: &PendingGoogleAuth,
        code: &str,
    ) -> Result<GoogleProfile, GoogleError> {
        log::info!("google callback received, exchanging code");
        let tokens = self.exchange_code(code, &pending.verifier, &pending.redirect_uri)?;
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
        parse_userinfo(&response.body)
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

    /// ログ用ラベルは `sub` を含まない（ログ = pack 復号材料の漏洩経路にしない）。
    #[test]
    fn profile_log_label_never_contains_sub() {
        let profile = GoogleProfile {
            sub: "1234567890-secret-sub".to_string(),
            email: "user@example.com".to_string(),
            name: "ユーザー".to_string(),
            picture: None,
        };
        let label = profile_log_label(Some(&profile));
        assert!(
            !label.contains(&profile.sub),
            "ログ用ラベルに sub が入っている: {label}"
        );
        assert_ne!(profile_log_label(None), label);
    }

    /// `sub` は `books.owner_sub` と pack の鍵（PRK のラップ）に使う秘密なので、
    /// 欠落した userinfo 応答を「sub = 空文字」の成功として通さない。
    #[test]
    fn parse_userinfo_rejects_missing_sub() {
        let error = parse_userinfo(br#"{"email":"u@example.com","name":"n"}"#)
            .expect_err("sub が無い応答は認証エラー");
        assert!(matches!(error, GoogleError::Auth(_)), "{error:?}");
    }

    /// 空文字・空白だけ・null の `sub` も通さない（欠落と同じ扱い）。
    #[test]
    fn parse_userinfo_rejects_empty_sub() {
        for body in [
            &br#"{"sub":"","email":"u@example.com"}"#[..],
            &br#"{"sub":"   "}"#[..],
            &br#"{"sub":null}"#[..],
            &br#"{"sub":12345}"#[..],
        ] {
            assert!(
                parse_userinfo(body).is_err(),
                "空の sub を通してはいけない: {}",
                String::from_utf8_lossy(body)
            );
        }
    }

    #[test]
    fn parse_userinfo_maps_fields() {
        let profile =
            parse_userinfo(br#"{"sub":"s-1","email":"e@example.com","name":"N"}"#).unwrap();
        assert_eq!(profile.sub, "s-1");
        assert_eq!(profile.email, "e@example.com");
        assert_eq!(profile.name, "N");
        assert_eq!(profile.picture, None);
    }

    /// userinfo 応答の経路全体（`profile()`）でも空 sub はエラーになること。
    #[test]
    fn profile_endpoint_without_sub_is_an_auth_error() {
        /// `sub` を含まない userinfo を返す transport。
        struct NoSubTransport;

        impl Transport for NoSubTransport {
            fn send(&mut self, _req: RequestSpec) -> Result<ResponseSpec, TbfError> {
                Ok(ResponseSpec {
                    status: 200,
                    headers: Vec::new(),
                    body: br#"{"email":"u@example.com","name":"n"}"#.to_vec(),
                })
            }
        }

        let mut client = GoogleClient::with_transport(Box::new(NoSubTransport), "cid", None);
        client.restore_tokens(OAuthTokens {
            access_token: "acc-1".to_string(),
            refresh_token: None,
            expires_at: 4_102_444_800,
        });
        let error = client.profile().expect_err("sub が無い userinfo はエラー");
        assert!(matches!(error, GoogleError::Auth(_)), "{error:?}");
    }

    /// メモリバックエンド（`SecretStore`）はプロセス内で共有されるため、
    /// `USER_GOOGLE_PROFILE` スロットを使うテストはこの Mutex で直列化する。
    static PROFILE_SLOT: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn lock_profile_slot() -> std::sync::MutexGuard<'static, ()> {
        PROFILE_SLOT
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

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

    /// ループバックのコールバック（`code`）を受け取る。
    ///
    /// ポートは動的（`127.0.0.1:0`）にする: `bind_loopback()` の固定ポート 38387 は
    /// 起動中のアプリが握っていることがあり（`SO_REUSEADDR` で二重 bind できてしまう）、
    /// テストがコールバックを取り合って固まる。
    #[test]
    fn callback_code_is_received_from_loopback() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let sender = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .write_all(b"GET /?code=abc&state=S HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
                .unwrap();
        });
        let code = receive_callback(&listener, "S", None).unwrap();
        sender.join().unwrap();
        assert_eq!(code, "abc");
    }

    /// キャンセル済みなら待たずに `Cancelled` を返す（✕ で即座に止まる）。
    #[test]
    fn cancelled_callback_returns_cancelled() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let cancel = AtomicBool::new(true);
        let error = receive_callback(&listener, "S", Some(&cancel)).expect_err("cancelled");
        assert!(matches!(error, GoogleError::Cancelled), "{error}");
    }

    /// `wait_for_code` はクライアントを要らない（＝コールバック待ちの間
    /// `GoogleClient` のロックを保持しない）。ロックを保持すると、待っている間に
    /// UI 側の `google.lock()` が止まり、モーダルの ✕ が効かなくなる。
    #[test]
    fn wait_for_code_receives_the_callback_without_a_client() {
        // 固定ポート（`bind_loopback`）は使わない: 起動中のアプリと二重 bind し得る。
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let pending = PendingGoogleAuth {
            redirect_uri: "http://127.0.0.1:0".to_string(),
            url: String::new(),
            listener,
            verifier: "verifier".to_string(),
            state: "S".to_string(),
            cancel: Arc::new(AtomicBool::new(false)),
        };
        let port = pending.listener.local_addr().unwrap().port();
        let sender = std::thread::spawn(move || {
            let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
            stream
                .write_all(b"GET /?code=xyz&state=S HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
                .unwrap();
        });
        let code = wait_for_code(&pending).unwrap();
        sender.join().unwrap();
        assert_eq!(code, "xyz");
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

    /// プロフィール（sub）は keyring に永続化され、keychain 無しの経路でも読み戻せること。
    ///
    /// 起動直後はプロフィールをネットワーク取得しない（設定画面を開くまで `userinfo` を
    /// 叩かない）。所有者（`books.owner_sub`）の判定に使う sub を起動時から使えるように
    /// するため、一度取得したプロフィールは保存しておく必要がある。
    #[test]
    fn saved_profile_round_trips_and_deletes() {
        let _guard = lock_profile_slot();
        crate::secrets::SecretStore::use_memory_backend();
        let store = crate::secrets::SecretStore::new();
        let _ = delete_saved_profile(&store);

        let profile = GoogleProfile {
            sub: "sub-1".to_string(),
            email: "user@example.com".to_string(),
            name: "ユーザー".to_string(),
            picture: Some("https://example.com/p.png".to_string()),
        };
        save_profile(&store, &profile).expect("保存できる");

        assert_eq!(
            saved_profile(&store),
            Some(profile),
            "保存したプロフィールが読み戻せること"
        );

        let _ = delete_saved_profile(&store);
        assert_eq!(saved_profile(&store), None, "削除後は読み戻せないこと");
    }

    /// 壊れた保存値（JSON でない・項目欠け）は `None` に落とす（起動を止めない）。
    #[test]
    fn saved_profile_ignores_broken_value() {
        let _guard = lock_profile_slot();
        crate::secrets::SecretStore::use_memory_backend();
        let store = crate::secrets::SecretStore::new();
        store
            .save(crate::secrets::USER_GOOGLE_PROFILE, "{not json")
            .unwrap();

        assert_eq!(saved_profile(&store), None);

        store
            .save(crate::secrets::USER_GOOGLE_PROFILE, r#"{"sub":"only-sub"}"#)
            .unwrap();
        assert_eq!(saved_profile(&store), None, "項目が欠けた値も無視する");
        let _ = delete_saved_profile(&store);
    }
}
