//! リダイレクトを**各ホップ検証して**追う。
//!
//! これまでストアのクライアントは `RequestSpec.redirects` に入れた回数だけ `ureq` に
//! 自動追跡させていた。`ureq` は転送時に `Cookie` を落とすが、他の資格情報
//! （`X-XSRF-TOKEN` など）はそのまま送るため、`Location` を差し替えられると許可外
//! ホストへトークンが残る。ここでは
//!
//! - 各ホップの `Location` を `url::Url` で解決し（`ureq` と同じ解釈）
//! - 資格情報は**宛先ごとに組み立て直し**、許可リスト外のホストへは載せない
//! - `https` 以外（ダウングレード）へは転送しない
//!
//! という規則で自前で追う。

use url::Url;

use crate::download_url::{self, HostRule};
use crate::tbf::TbfError;
use crate::tbf::transport::{RequestSpec, ResponseSpec, Transport};

/// ホップごとに組み立て直すヘッダー（比較は大文字小文字を区別しない）。
const CREDENTIAL_HEADERS: &[&str] = &["cookie", "x-xsrf-token", "authorization"];

/// リダイレクトを最大 `max_hops` 回まで、各ホップを検証しながら追う。
///
/// `credentials` は宛先 URL ごとに資格情報ヘッダーを組み立てる（呼び出し側が
/// 「そのホスト向けに収集した Cookie だけ」を返す）。許可リストに一致しないホストでは
/// 呼ばれず、資格情報なしで追う（署名付き CDN / S3 への転送は壊さず、セッションは渡さない）。
pub fn send_with_validated_redirects(
    transport: &mut dyn Transport,
    spec: RequestSpec,
    max_hops: usize,
    rules: &[HostRule],
    credentials: &mut dyn FnMut(&Url) -> Vec<(String, String)>,
    on_progress: &mut dyn FnMut(u64, u64) -> bool,
) -> Result<ResponseSpec, TbfError> {
    let mut current = Url::parse(&spec.url)
        .map_err(|_| TbfError::BlockedUrl(format!("URL を解釈できない: {}", spec.url)))?;
    // 起点もここで検証する（呼び出し側の検証と二重の関門。送信前に止める）。
    download_url::check_url(&current, rules)
        .map_err(|error| TbfError::BlockedUrl(format!("{current}: {error}")))?;

    // 資格情報は毎ホップ `credentials` で組み立て直すので、元のヘッダーからは落とす。
    let base: Vec<(String, String)> = spec
        .headers
        .iter()
        .filter(|(name, _)| {
            !CREDENTIAL_HEADERS
                .iter()
                .any(|header| name.eq_ignore_ascii_case(header))
        })
        .cloned()
        .collect();

    let mut remaining = max_hops;
    loop {
        let mut headers = base.clone();
        // 許可リスト内のホストにだけ資格情報を載せる（宛先ごとに組み立て直す）。
        // 許可外へは載せずに追う（署名付き CDN / S3 への転送を壊さない）。
        if download_url::check_url(&current, rules).is_ok() {
            headers.extend(credentials(&current));
        }
        let hop = RequestSpec {
            method: spec.method.clone(),
            url: current.as_str().to_string(),
            headers,
            body: spec.body.clone(),
            // ここで自前で追う（`ureq` に追わせると各ホップを検証できない）
            redirects: 0,
        };
        let response = transport.send_download(hop, on_progress)?;
        if !(300..400).contains(&response.status) {
            return Ok(response);
        }
        let Some(location) = response.header("location") else {
            // `Location` の無い 3xx はそのまま返す（呼び出し側が状態を見て判断する）。
            return Ok(response);
        };
        if remaining == 0 {
            return Err(TbfError::Upstream(format!(
                "リダイレクトが上限（{max_hops} 回）を超えました: {current}"
            )));
        }
        remaining -= 1;
        let next = current
            .join(location)
            .map_err(|_| TbfError::Upstream(format!("Location を解釈できない: {location}")))?;
        // ダウングレード（`https` 以外）へは転送しない。
        if next.scheme() != "https" {
            return Err(TbfError::BlockedUrl(format!(
                "https 以外へは転送しない: {next}"
            )));
        }
        current = next;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::Mutex;
    use std::sync::Arc;

    const RULES: &[HostRule] = &[HostRule::with_subdomains("dl.example.com", None)];

    #[derive(Default)]
    struct Script {
        requests: Vec<RequestSpec>,
        /// (status, location)
        responses: Vec<(u16, Option<String>)>,
    }

    /// スクリプト化した応答を順に返し、受け取ったリクエストを記録するモック。
    #[derive(Clone, Default)]
    struct ScriptedTransport {
        script: Arc<Mutex<Script>>,
    }

    impl ScriptedTransport {
        fn new(responses: &[(u16, &str)]) -> Self {
            let mut script = Script::default();
            for (status, location) in responses {
                script.responses.push((
                    *status,
                    if location.is_empty() {
                        None
                    } else {
                        Some((*location).to_string())
                    },
                ));
            }
            Self {
                script: Arc::new(Mutex::new(script)),
            }
        }

        fn requests(&self) -> Vec<RequestSpec> {
            self.script.lock().requests.clone()
        }
    }

    impl Transport for ScriptedTransport {
        fn send(&mut self, spec: RequestSpec) -> Result<ResponseSpec, TbfError> {
            let mut script = self.script.lock();
            let index = script.requests.len();
            script.requests.push(spec);
            let (status, location) = script
                .responses
                .get(index)
                .cloned()
                .unwrap_or((200, None));
            let headers = location
                .map(|value| vec![("location".to_string(), value)])
                .unwrap_or_default();
            Ok(ResponseSpec {
                status,
                headers,
                body: vec![7, 7],
            })
        }
    }

    fn spec(url: &str) -> RequestSpec {
        RequestSpec {
            method: "GET".into(),
            url: url.into(),
            headers: vec![
                ("User-Agent".to_string(), "ua".to_string()),
                ("Cookie".to_string(), "stale=1".to_string()),
                ("X-XSRF-TOKEN".to_string(), "stale-token".to_string()),
            ],
            body: None,
            // 呼び出し側の指定に関わらず、この関数が自前で追う（ureq には追わせない）
            redirects: 5,
        }
    }

    fn credential_headers(spec: &RequestSpec) -> Vec<(String, String)> {
        spec.headers
            .iter()
            .filter(|(name, _)| {
                CREDENTIAL_HEADERS
                    .iter()
                    .any(|header| name.eq_ignore_ascii_case(header))
            })
            .cloned()
            .collect()
    }

    /// 資格情報は毎ホップ、宛先ごとに組み立て直す（許可リスト内のホスト）。
    #[test]
    fn credentials_are_rebuilt_for_each_allowed_hop() {
        let mut transport = ScriptedTransport::new(&[
            (302, "https://cdn.dl.example.com/b.zip"),
            (200, ""),
        ]);
        let mut credentials = |url: &Url| {
            vec![(
                "Cookie".to_string(),
                format!("host={}", url.host_str().unwrap_or_default()),
            )]
        };
        let response = send_with_validated_redirects(
            &mut transport,
            spec("https://dl.example.com/a.zip"),
            5,
            RULES,
            &mut credentials,
            &mut |_, _| true,
        )
        .expect("許可リスト内のホップを追えるはず");

        assert_eq!(response.status, 200);
        let requests = transport.requests();
        assert_eq!(requests.len(), 2, "ホップ数が違う");
        assert_eq!(requests[0].url, "https://dl.example.com/a.zip");
        assert_eq!(requests[1].url, "https://cdn.dl.example.com/b.zip");
        assert_eq!(requests[0].redirects, 0, "ureq にリダイレクトを追わせている");
        assert_eq!(requests[1].redirects, 0, "ureq にリダイレクトを追わせている");
        assert_eq!(
            credential_headers(&requests[0]),
            vec![("Cookie".to_string(), "host=dl.example.com".to_string())]
        );
        assert_eq!(
            credential_headers(&requests[1]),
            vec![("Cookie".to_string(), "host=cdn.dl.example.com".to_string())],
            "転送先の資格情報を組み立て直していない"
        );
        // 資格情報以外のヘッダーは引き継ぐ
        assert!(
            requests[1]
                .headers
                .iter()
                .any(|(name, value)| name == "User-Agent" && value == "ua"),
            "資格情報以外のヘッダーが落ちている"
        );
    }

    /// 許可リスト外のホストへは資格情報を載せない（署名付き CDN への転送は追う）。
    #[test]
    fn credentials_are_not_sent_to_hosts_outside_the_rules() {
        let mut transport =
            ScriptedTransport::new(&[(302, "https://signed.invalid/b.zip"), (200, "")]);
        let mut built_for: Vec<String> = Vec::new();
        let mut credentials = |url: &Url| {
            built_for.push(url.host_str().unwrap_or_default().to_string());
            vec![("Cookie".to_string(), "leak=1".to_string())]
        };
        let response = send_with_validated_redirects(
            &mut transport,
            spec("https://dl.example.com/a.zip"),
            5,
            RULES,
            &mut credentials,
            &mut |_, _| true,
        )
        .expect("許可外でも資格情報なしで追う");

        assert_eq!(response.status, 200);
        assert_eq!(
            built_for,
            vec!["dl.example.com".to_string()],
            "許可外ホスト向けに資格情報を組み立てている"
        );
        let requests = transport.requests();
        assert_eq!(requests[1].url, "https://signed.invalid/b.zip");
        assert!(
            credential_headers(&requests[1]).is_empty(),
            "許可外ホストへ資格情報を送っている: {:?}",
            credential_headers(&requests[1])
        );
    }

    /// `https` から落とす転送（ダウングレード）は追わない。
    #[test]
    fn https_downgrade_is_rejected() {
        let mut transport = ScriptedTransport::new(&[(302, "http://cdn.dl.example.com/b.zip")]);
        let mut credentials = |_: &Url| Vec::new();
        let error = send_with_validated_redirects(
            &mut transport,
            spec("https://dl.example.com/a.zip"),
            5,
            RULES,
            &mut credentials,
            &mut |_, _| true,
        )
        .expect_err("http への転送を追っている");
        assert!(matches!(error, TbfError::BlockedUrl(_)), "{error:?}");
        assert_eq!(transport.requests().len(), 1, "落とす転送先へ送信している");
    }

    /// 相対 `Location` は現在の URL を基準に解決する（`ureq` と同じ）。
    #[test]
    fn a_relative_location_is_resolved_against_the_current_url() {
        let mut transport = ScriptedTransport::new(&[(302, "/other/b.zip"), (200, "")]);
        let mut credentials = |_: &Url| Vec::new();
        send_with_validated_redirects(
            &mut transport,
            spec("https://dl.example.com/a/one.zip"),
            5,
            RULES,
            &mut credentials,
            &mut |_, _| true,
        )
        .expect("相対 Location を解決できるはず");
        assert_eq!(
            transport.requests()[1].url,
            "https://dl.example.com/other/b.zip"
        );
    }

    /// ホップ数の上限を超えたら失敗する（無限ループと転送の連鎖を防ぐ）。
    #[test]
    fn too_many_hops_fail_closed() {
        let mut transport = ScriptedTransport::new(&[(302, "/1"), (302, "/2"), (302, "/3")]);
        let mut credentials = |_: &Url| Vec::new();
        let error = send_with_validated_redirects(
            &mut transport,
            spec("https://dl.example.com/a.zip"),
            2,
            RULES,
            &mut credentials,
            &mut |_, _| true,
        )
        .expect_err("上限を超えて追っている");
        assert!(matches!(error, TbfError::Upstream(_)), "{error:?}");
        // 起点 + 2 ホップまでしか送らない
        assert_eq!(transport.requests().len(), 3);
    }

    /// 起点が許可リスト外なら、そもそも送信しない。
    #[test]
    fn a_start_url_outside_the_rules_is_rejected_before_sending() {
        let mut transport = ScriptedTransport::new(&[(200, "")]);
        let mut credentials = |_: &Url| Vec::new();
        let error = send_with_validated_redirects(
            &mut transport,
            spec("https://evil.invalid/a.zip"),
            5,
            RULES,
            &mut credentials,
            &mut |_, _| true,
        )
        .expect_err("許可外の起点へ送っている");
        assert!(matches!(error, TbfError::BlockedUrl(_)), "{error:?}");
        assert!(transport.requests().is_empty());
    }

    /// `Location` の無い 3xx はそのまま返す（呼び出し側が状態を見て判断する）。
    #[test]
    fn a_redirect_without_a_location_is_returned_as_is() {
        let mut transport = ScriptedTransport::new(&[(302, "")]);
        let mut credentials = |_: &Url| Vec::new();
        let response = send_with_validated_redirects(
            &mut transport,
            spec("https://dl.example.com/a.zip"),
            5,
            RULES,
            &mut credentials,
            &mut |_, _| true,
        )
        .expect("そのまま返すはず");
        assert_eq!(response.status, 302);
        assert_eq!(transport.requests().len(), 1);
    }

    /// 進捗コールバック（中止）はそのまま本文取得へ伝わる。
    #[test]
    fn the_progress_callback_can_cancel_the_final_body_read() {
        let mut transport = ScriptedTransport::new(&[(200, "")]);
        let mut credentials = |_: &Url| Vec::new();
        let error = send_with_validated_redirects(
            &mut transport,
            spec("https://dl.example.com/a.zip"),
            5,
            RULES,
            &mut credentials,
            &mut |_, _| false,
        )
        .expect_err("中止が伝わっていない");
        assert!(matches!(error, TbfError::Cancelled), "{error:?}");
    }
}
