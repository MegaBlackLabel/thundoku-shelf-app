//! Google OAuth PKCE + token exchange tests (no external network; loopback
//! tested against a real ephemeral socket).

use parking_lot::Mutex;
use std::net::TcpListener;
use std::sync::Arc;

use thundoku_core::google::{
    DEFAULT_REDIRECT_PORT, GoogleClient, OAuthTokens, build_authorize_url, build_challenge,
    receive_callback,
};
use thundoku_core::tbf::{RequestSpec, ResponseSpec, Transport};

type MockFn =
    Box<dyn FnMut(RequestSpec) -> Result<ResponseSpec, thundoku_core::tbf::TbfError> + Send>;

struct Mock(MockFn);
impl Transport for Mock {
    fn send(&mut self, spec: RequestSpec) -> Result<ResponseSpec, thundoku_core::tbf::TbfError> {
        (self.0)(spec)
    }
}

#[test]
fn pkce_challenge_matches_rfc7636_vector() {
    // RFC 7636 Appendix B test vector.
    assert_eq!(
        build_challenge("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
        "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
    );
}

#[test]
fn authorize_url_contains_required_params() {
    let url = build_authorize_url(
        "my-client-id.apps.googleusercontent.com",
        "http://127.0.0.1:38387",
        "challenge-123",
        "state-456",
    );
    assert!(url.starts_with("https://accounts.google.com/o/oauth2/v2/auth?"));
    assert!(url.contains("client_id=my-client-id.apps.googleusercontent.com"));
    assert!(url.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A38387"));
    assert!(url.contains("response_type=code"));
    assert!(url.contains("access_type=offline"));
    assert!(url.contains("prompt=consent"));
    assert!(url.contains("state=state-456"));
    assert!(url.contains("code_challenge=challenge-123"));
    assert!(url.contains("code_challenge_method=S256"));
    assert!(url.contains("https%3A%2F%2Fwww.googleapis.com%2Fauth%2Fdrive.readonly"));
    assert!(url.contains("openid"));
}

#[test]
fn token_exchange_sends_pkce_without_client_secret() {
    let captured = Arc::new(Mutex::new(Vec::<RequestSpec>::new()));
    let log = captured.clone();
    let mut client = GoogleClient::with_transport(
        Box::new(Mock(Box::new(move |spec| {
            log.lock().push(spec.clone());
            Ok(ResponseSpec {
                status: 200,
                headers: vec![],
                body: br#"{
                    "access_token": "acc-1",
                    "refresh_token": "ref-1",
                    "expires_in": 3600,
                    "token_type": "Bearer",
                    "id_token": "header.eyJpc3MiOiJodHRwczovL2FjY291bnRzLmdvb2dsZS5jb20iLCJzdWIiOiIxMjM0NSJ9.signature"
                }"#
                .to_vec(),
            })
        }))),
        "client-1",
        None,
    );

    let tokens = client
        .exchange_code("auth-code", "verifier-1", "http://127.0.0.1:38387")
        .unwrap();
    assert_eq!(tokens.access_token, "acc-1");
    assert_eq!(tokens.refresh_token.as_deref(), Some("ref-1"));
    assert!(tokens.expires_at > 0);

    let req = &captured.lock()[0];
    assert_eq!(req.url, "https://oauth2.googleapis.com/token");
    assert_eq!(req.method, "POST");
    let body = String::from_utf8(req.body.clone().unwrap()).unwrap();
    assert!(body.contains("grant_type=authorization_code"));
    assert!(body.contains("code=auth-code"));
    assert!(body.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A38387"));
    assert!(body.contains("client_id=client-1"));
    assert!(body.contains("code_verifier=verifier-1"));
    assert!(!body.contains("client_secret"));
    assert!(
        req.headers
            .iter()
            .any(|(k, v)| k == "Content-Type" && v.contains("application/x-www-form-urlencoded"))
    );
}

#[test]
fn refresh_tokens_updates_access_token() {
    let mut client = GoogleClient::with_transport(
        Box::new(Mock(Box::new(|spec| {
            let body = String::from_utf8(spec.body.clone().unwrap()).unwrap();
            assert!(body.contains("grant_type=refresh_token"));
            assert!(body.contains("refresh_token=ref-1"));
            Ok(ResponseSpec {
                status: 200,
                headers: vec![],
                body: br#"{"access_token":"acc-2","expires_in":3600,"token_type":"Bearer"}"#
                    .to_vec(),
            })
        }))),
        "client-1",
        None,
    );
    client.restore_tokens(OAuthTokens {
        access_token: "acc-1".into(),
        refresh_token: Some("ref-1".into()),
        expires_at: 0, // already expired
    });
    let token = client.access_token().unwrap();
    assert_eq!(token, "acc-2");
    assert!(client.tokens().unwrap().expires_at > 0);
}

#[test]
fn access_token_without_tokens_is_not_authorized() {
    let mut client =
        GoogleClient::with_transport(Box::new(Mock(Box::new(|_| panic!("no http")))), "c", None);
    assert!(matches!(
        client.access_token(),
        Err(thundoku_core::google::GoogleError::NotAuthorized)
    ));
}

#[test]
fn profile_maps_userinfo_fields() {
    let mut client = GoogleClient::with_transport(
        Box::new(Mock(Box::new(|spec| {
            assert_eq!(spec.url, "https://www.googleapis.com/oauth2/v3/userinfo");
            assert!(
                spec.headers
                    .iter()
                    .any(|(k, v)| k == "Authorization" && v == "Bearer acc-1")
            );
            Ok(ResponseSpec {
                status: 200,
                headers: vec![],
                body: r#"{"sub":"12345","email":"u@example.com","name":"テスト ユーザー","picture":"https://example.com/p.png"}"#
                    .as_bytes()
                    .to_vec(),
            })
        }))),
        "c",
        None,
    );
    client.restore_tokens(OAuthTokens {
        access_token: "acc-1".into(),
        refresh_token: None,
        expires_at: 4_102_444_800, // year 2100
    });
    let profile = client.profile().unwrap();
    assert_eq!(profile.sub, "12345");
    assert_eq!(profile.email, "u@example.com");
    assert_eq!(profile.name, "テスト ユーザー");
    assert_eq!(
        profile.picture.as_deref(),
        Some("https://example.com/p.png")
    );
}

#[test]
fn logout_clears_tokens() {
    let mut client =
        GoogleClient::with_transport(Box::new(Mock(Box::new(|_| panic!("no http")))), "c", None);
    client.restore_tokens(OAuthTokens {
        access_token: "a".into(),
        refresh_token: None,
        expires_at: 4_102_444_800,
    });
    assert!(client.is_authenticated());
    client.logout();
    assert!(!client.is_authenticated());
}

#[test]
fn loopback_receives_code_and_rejects_state_mismatch() {
    // real ephemeral loopback: bind, receive in a thread, connect from test
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let state = "expected-state".to_string();
    let handle = std::thread::spawn(move || receive_callback(listener, &state, None));

    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    use std::io::Write;
    write!(
        stream,
        "GET /callback?code=auth-code-123&state=expected-state HTTP/1.1\r\nHost: localhost\r\n\r\n"
    )
    .unwrap();
    let mut response = String::new();
    use std::io::Read;
    stream.read_to_string(&mut response).unwrap();
    assert!(response.contains("認証完了"));
    assert_eq!(handle.join().unwrap().unwrap(), "auth-code-123");

    // state mismatch
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let handle = std::thread::spawn(move || receive_callback(listener, "other-state", None));
    let mut stream = std::net::TcpStream::connect(addr).unwrap();
    write!(
        stream,
        "GET /callback?code=abc&state=wrong HTTP/1.1\r\nHost: x\r\n\r\n"
    )
    .unwrap();
    assert!(matches!(
        handle.join().unwrap(),
        Err(thundoku_core::google::GoogleError::Auth(_))
    ));
}

#[test]
fn default_redirect_port_is_38387() {
    assert_eq!(DEFAULT_REDIRECT_PORT, 38387);
}

#[test]
fn begin_finish_authorize_roundtrip() {
    use std::io::Write as _;

    let mut client = GoogleClient::with_transport(
        Box::new(Mock(Box::new(|spec| {
            if spec.url.contains("oauth2.googleapis.com/token") {
                Ok(ResponseSpec {
                    status: 200,
                    headers: vec![],
                    body: br#"{"access_token":"acc-1","refresh_token":"ref-1","expires_in":3600,"token_type":"Bearer"}"#
                        .to_vec(),
                })
            } else {
                Ok(ResponseSpec {
                    status: 200,
                    headers: vec![],
                    body: r#"{"sub":"s-1","email":"e@example.com","name":"N"}"#
                        .as_bytes()
                        .to_vec(),
                })
            }
        }))),
        "client-1",
        None,
    );
    let pending = client.begin_authorize().unwrap();
    assert!(pending.url.contains("accounts.google.com/o/oauth2/v2/auth"));
    let port = pending
        .redirect_uri
        .rsplit(':')
        .next()
        .unwrap()
        .parse::<u16>()
        .unwrap();
    let state = pending.state.clone();
    let handle = std::thread::spawn(move || client.finish_authorize(pending));

    let mut stream = std::net::TcpStream::connect(("127.0.0.1", port)).unwrap();
    write!(
        stream,
        "GET /callback?code=code-1&state={state} HTTP/1.1\r\nHost: localhost\r\n\r\n"
    )
    .unwrap();
    let profile = handle.join().unwrap().unwrap();
    assert_eq!(profile.email, "e@example.com");
    assert_eq!(profile.sub, "s-1");
}

#[test]
fn pending_cancel_returns_cancelled() {
    let mut client = GoogleClient::with_transport(
        Box::new(Mock(Box::new(|_| panic!("no http")))),
        "client-1",
        None,
    );
    let pending = client.begin_authorize().unwrap();
    let cancel = pending.cancel_handle();
    let handle = std::thread::spawn(move || client.finish_authorize(pending));
    cancel.store(true, std::sync::atomic::Ordering::Relaxed);
    assert!(matches!(
        handle.join().unwrap(),
        Err(thundoku_core::google::GoogleError::Cancelled)
    ));
}
