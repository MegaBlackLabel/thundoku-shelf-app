//! SEC-02: 認証付き送信の送信先検証。
//!
//! セッション Cookie / XSRF を付けたリクエストは、送信先を検証してから送る。
//! 保存済みの `download_url`（Drive の JSON バックアップから復元でき、改変され
//! 得る）や 302 の `Location` を外部ホストへ向けても、**認証情報が送られない**
//! ことを、送信を記録するモックで確認する。

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::Arc;

use thundoku_core::booth::{BoothClient, BoothError, BoothSession};
use thundoku_core::dlsite::client::{DlsiteClient, DlsiteError, DlsiteSession};
use thundoku_core::fanza::client::{FanzaClient, FanzaError, FanzaSession};
use thundoku_core::tbf::transport::{RequestSpec, ResponseSpec, Transport};
use thundoku_core::tbf::{TbfClient, TbfError};

type Log = Arc<Mutex<Vec<RequestSpec>>>;

/// 送信したリクエストを記録する transport。`location` を返すと 302 を模す。
struct Recording {
    log: Log,
    location: Option<String>,
}

impl Recording {
    fn new(log: &Log, location: Option<&str>) -> Self {
        Self {
            log: log.clone(),
            location: location.map(str::to_string),
        }
    }
}

impl Transport for Recording {
    fn send(&mut self, spec: RequestSpec) -> Result<ResponseSpec, TbfError> {
        self.log.lock().push(spec);
        Ok(match &self.location {
            Some(location) => ResponseSpec {
                status: 302,
                headers: vec![("location".into(), location.clone())],
                body: Vec::new(),
            },
            None => ResponseSpec {
                status: 200,
                headers: vec![],
                body: b"ok".to_vec(),
            },
        })
    }
}

fn session_cookies() -> HashMap<String, String> {
    HashMap::from([("session".to_string(), "secret-cookie".to_string())])
}

fn sent_urls(log: &Log) -> Vec<String> {
    log.lock().iter().map(|spec| spec.url.clone()).collect()
}

/// BOOTH: `booth.pm/downloadables/` 以外へは（Cookie を付ける前に）拒否する。
#[test]
fn booth_never_attaches_the_session_outside_the_allowlist() {
    let session = BoothSession {
        cookies: session_cookies(),
    };
    let client = BoothClient::new(&session);

    for url in [
        "https://evil.example.com/downloadables/1",
        "https://booth.pm.evil.example.com/downloadables/1",
        "https://booth.pm/ja/items/1",
    ] {
        let error = client
            .download(url)
            .expect_err("許可リスト外の取得先は拒否される");
        assert!(matches!(error, BoothError::BlockedUrl(_)), "{url}: {error}");
    }
}

/// DLsite: 保存 URL（`down_url`）が外部ホストなら 1 件も送らない。
#[test]
fn dlsite_rejects_a_foreign_saved_url_without_sending() {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let mut client = DlsiteClient::with_transport(
        Box::new(Recording::new(&log, None)),
        DlsiteSession::from_site_cookies(session_cookies().into_iter().collect()),
    );

    let error = client
        .download_with_progress(
            "https://evil.example.com/maniax/download/=/product_id/RJ1.html",
            &mut |_, _| true,
        )
        .expect_err("外部ホストは拒否される");
    assert!(matches!(error, DlsiteError::BlockedUrl(_)), "{error}");
    assert!(sent_urls(&log).is_empty(), "1 件も送ってはいけない");
}

/// DLsite: 302 の `Location` が許可外なら、CDN へは送らない（自サイトのみ）。
#[test]
fn dlsite_rejects_a_redirect_to_an_unlisted_host() {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let mut client = DlsiteClient::with_transport(
        Box::new(Recording::new(&log, Some("https://evil.example.com/zip"))),
        DlsiteSession::from_site_cookies(session_cookies().into_iter().collect()),
    );

    let error = client
        .download_with_progress(
            "https://www.dlsite.com/maniax/download/=/product_id/RJ1.html",
            &mut |_, _| true,
        )
        .expect_err("許可外の転送先は拒否される");
    assert!(matches!(error, DlsiteError::BlockedUrl(_)), "{error}");
    assert_eq!(
        sent_urls(&log),
        vec!["https://www.dlsite.com/maniax/download/=/product_id/RJ1.html".to_string()],
        "自サイトへの 1 回目だけを送る"
    );
}

/// FANZA: proxy URL が外部ホストなら送らない。
#[test]
fn fanza_rejects_a_foreign_proxy_url_without_sending() {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let mut client = FanzaClient::with_transport(
        Box::new(Recording::new(&log, None)),
        FanzaSession::from_site_cookies(session_cookies().into_iter().collect()),
    );

    let error = client
        .download_with_progress("https://evil.example.com/dc/-/proxy/=/x", &mut |_, _| true)
        .expect_err("外部ホストは拒否される");
    assert!(matches!(error, FanzaError::BlockedUrl(_)), "{error}");
    assert!(sent_urls(&log).is_empty(), "1 件も送ってはいけない");
}

/// FANZA: 302 の `Location` が許可外なら CDN へ送らない（proxy のみ）。
#[test]
fn fanza_rejects_a_redirect_to_an_unlisted_host() {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let mut client = FanzaClient::with_transport(
        Box::new(Recording::new(&log, Some("https://evil.example.com/zip"))),
        FanzaSession::from_site_cookies(session_cookies().into_iter().collect()),
    );

    let error = client
        .download_with_progress(
            "https://www.dmm.co.jp/dc/-/proxy/=/transfer_type=download/product_id=d_1/",
            &mut |_, _| true,
        )
        .expect_err("許可外の転送先は拒否される");
    assert!(matches!(error, FanzaError::BlockedUrl(_)), "{error}");
    assert_eq!(
        sent_urls(&log).len(),
        1,
        "proxy への 1 回目だけを送る（CDN へは送らない）"
    );
}

/// 技術書典: 保存 URL が自サイト以外なら、解決リクエストを送らない。
#[test]
fn tbf_rejects_a_foreign_resolve_target_without_sending() {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let mut client = TbfClient::with_transport(Box::new(Recording::new(&log, None)));

    let error = client
        .resolve_download_url("https://evil.example.com/api/product-dlc/1/download")
        .expect_err("外部ホストは拒否される");
    assert!(matches!(error, TbfError::BlockedUrl(_)), "{error}");
    assert!(sent_urls(&log).is_empty(), "1 件も送ってはいけない");
}

/// 技術書典: 本体取得も許可ホスト（自サイト / 署名付き GCS）だけ。
#[test]
fn tbf_rejects_a_foreign_download_target_without_sending() {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    let mut client = TbfClient::with_transport(Box::new(Recording::new(&log, None)));

    for url in [
        "https://evil.example.com/tbf-tokyo-product-dlc/1.zip",
        "https://storage.googleapis.com/other-bucket/1.zip",
    ] {
        let error = client
            .download_with_progress(url, &mut |_, _| true)
            .expect_err("許可リスト外の取得先は拒否される");
        assert!(matches!(error, TbfError::BlockedUrl(_)), "{url}: {error}");
    }
    assert!(sent_urls(&log).is_empty(), "1 件も送ってはいけない");
}
