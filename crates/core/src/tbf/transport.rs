//! HTTP transport abstraction: `ureq` in production, scripted mocks in tests.

use crate::tbf::TbfError;

#[derive(Debug, Clone)]
pub struct RequestSpec {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
    /// Maximum redirects to follow; 0 = manual (return the 3xx as-is).
    pub redirects: u32,
}

#[derive(Debug, Clone, Default)]
pub struct ResponseSpec {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl ResponseSpec {
    /// Case-insensitive single header lookup (last occurrence wins).
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .rev()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    /// Parse every `Set-Cookie` header into (name, value) pairs.
    pub fn set_cookies(&self) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (k, v) in &self.headers {
            if k.eq_ignore_ascii_case("set-cookie") {
                let Some(segment) = v.split(';').next() else {
                    continue;
                };
                let Some((name, value)) = segment.split_once('=') else {
                    continue;
                };
                let name = name.trim();
                if !name.is_empty() {
                    out.push((name.to_string(), value.trim().to_string()));
                }
            }
        }
        out
    }
}

pub trait Transport: Send {
    fn send(&mut self, spec: RequestSpec) -> Result<ResponseSpec, TbfError>;

    /// Download a response body while reporting `(downloaded, total)` bytes.
    ///
    /// The callback returns whether the transfer should continue. Returning
    /// `false` aborts it (`TbfError::Cancelled`) and discards the bytes read so
    /// far — a partial body must never be imported.
    ///
    /// Default implementation falls back to [`Self::send`] and reports the
    /// whole body at once (no live progress).
    fn send_download(
        &mut self,
        spec: RequestSpec,
        on_progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<ResponseSpec, TbfError> {
        let resp = self.send(spec)?;
        let total = resp.body.len() as u64;
        if !on_progress(total, total) {
            return Err(TbfError::Cancelled);
        }
        Ok(resp)
    }
}

impl Transport for Box<dyn FnMut(RequestSpec) -> Result<ResponseSpec, TbfError> + Send> {
    fn send(&mut self, spec: RequestSpec) -> Result<ResponseSpec, TbfError> {
        self(spec)
    }
}

/// Production transport backed by `ureq` (rustls).
pub struct UreqTransport {
    /// Agent following redirects (default 5).
    agent: ureq::Agent,
    /// Agent with redirects disabled, for manual 3xx inspection.
    manual_redirect_agent: ureq::Agent,
}

impl Default for UreqTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl UreqTransport {
    pub fn new() -> Self {
        let build_agent = || {
            ureq::AgentBuilder::new()
                .timeout_connect(std::time::Duration::from_secs(5))
                .timeout_read(std::time::Duration::from_secs(15))
                .build()
        };
        Self {
            agent: build_agent(),
            // リダイレクト手動追跡用もタイムアウト必須（無いとハングする）
            manual_redirect_agent: ureq::AgentBuilder::new()
                .redirects(0)
                .timeout_connect(std::time::Duration::from_secs(5))
                .timeout_read(std::time::Duration::from_secs(15))
                .build(),
        }
    }
}

fn collect_headers(resp: &ureq::Response) -> Vec<(String, String)> {
    let mut headers = Vec::new();
    for name in resp.headers_names() {
        for value in resp.all(&name) {
            headers.push((name.clone(), value.to_string()));
        }
    }
    headers
}

/// API 応答（JSON / HTML）本文の上限。同期の応答がこれを超えることはない。
pub(crate) const MAX_API_BODY_BYTES: u64 = 16 * 1024 * 1024;

/// ダウンロード本文の上限。取り込み対象は最大でも数百 MB（ZIP の展開後は 1.3GB 級）。
/// 無制限に積むと、細工した Content-Length や無限ストリームでメモリを使い切られる。
pub(crate) const MAX_DOWNLOAD_BODY_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// 本文読み出しの結末。
pub(crate) enum BodyOutcome {
    /// 上限内で読み切った。
    Read(Vec<u8>),
    /// 進捗コールバックが中止を要求した。
    Cancelled,
    /// 上限（バイト）を超えた。
    TooLarge(u64),
    /// 読み出しが途中で失敗した（切れた本文を成功として返さない）。
    Io(String),
}

fn with_limit(outcome: BodyOutcome) -> Result<Vec<u8>, TbfError> {
    match outcome {
        BodyOutcome::Read(body) => Ok(body),
        BodyOutcome::Cancelled => Err(TbfError::Cancelled),
        BodyOutcome::TooLarge(limit) => Err(TbfError::Upstream(format!(
            "応答本文が上限（{limit} バイト）を超えました"
        ))),
        BodyOutcome::Io(message) => Err(TbfError::Network(format!(
            "応答本文の読み出しに失敗しました: {message}"
        ))),
    }
}

/// 本文を上限つきで読む（進捗通知なし）。
fn read_body_capped(reader: &mut impl std::io::Read, limit: u64) -> BodyOutcome {
    read_body_with_progress(reader, 0, limit, &mut |_, _| true)
}

/// 本文を読みながら `on_progress(downloaded, total)` を通知する。
///
/// - 進捗は**1% 刻み**で間引く（チャンクごとに通知すると受信側の UI を詰まらせ、
///   ダウンロード自体がストールする）
/// - コールバックが `false` を返すと読み込みを中断する（途中まで読んだバイト列は
///   呼び出し側に渡さない。部分的な本文を取り込まない）
/// - `total` が 0（Content-Length 無し）のときは **1MiB 刻み**で通知する
///   （% は出せないが、キャンセルは効かせる。読み切ったら最後のサイズも一度通知する）
/// - 読み出しが `limit` を超えたら `TooLarge`（**宣言サイズではなく実際に読めた
///   バイト数**で判定する）
pub(crate) fn read_body_with_progress(
    reader: &mut impl std::io::Read,
    total: u64,
    limit: u64,
    on_progress: &mut dyn FnMut(u64, u64) -> bool,
) -> BodyOutcome {
    /// Content-Length が無いときに進捗（＝キャンセル確認）を挟む間隔。
    const UNKNOWN_TOTAL_STEP: u64 = 1024 * 1024;

    let mut out = Vec::new();
    let mut buf = vec![0u8; 64 * 1024];
    let mut downloaded = 0u64;
    let mut last_pct = u32::MAX;
    let mut notified_at = 0u64;
    loop {
        match reader.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                downloaded += n as u64;
                if downloaded > limit {
                    return BodyOutcome::TooLarge(limit);
                }
                out.extend_from_slice(&buf[..n]);
                let notify = if total > 0 {
                    let pct = (downloaded * 100).checked_div(total).unwrap_or(0) as u32;
                    if pct != last_pct {
                        last_pct = pct;
                        true
                    } else {
                        false
                    }
                } else {
                    downloaded - notified_at >= UNKNOWN_TOTAL_STEP
                };
                if notify {
                    notified_at = downloaded;
                    if !on_progress(downloaded, total) {
                        return BodyOutcome::Cancelled;
                    }
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            // 途中で切れた本文を成功として返さない（壊れた JSON / 途中までの
            // ファイルを取り込まない）。
            Err(e) => return BodyOutcome::Io(e.to_string()),
        }
    }
    // Content-Length が無いときは、読み切ったサイズを最後に一度だけ通知する
    // （UI が「完了」を出せるようにする。中止なら本文は返さない）。
    if total == 0 && downloaded > 0 && downloaded != notified_at && !on_progress(downloaded, 0) {
        return BodyOutcome::Cancelled;
    }
    BodyOutcome::Read(out)
}

impl Transport for UreqTransport {
    fn send(&mut self, spec: RequestSpec) -> Result<ResponseSpec, TbfError> {
        // `redirects` is an agent-level setting in ureq; keep one agent per
        // mode to preserve connection pooling.
        let agent = if spec.redirects == 0 {
            &self.manual_redirect_agent
        } else {
            &self.agent
        };
        let mut request = agent.request(&spec.method, &spec.url);
        for (name, value) in &spec.headers {
            request = request.set(name, value);
        }
        let body = spec.body.unwrap_or_default();
        match request.send_bytes(&body) {
            Ok(resp) => Ok(ResponseSpec {
                status: resp.status(),
                headers: collect_headers(&resp),
                body: with_limit(read_body_capped(
                    &mut resp.into_reader(),
                    MAX_API_BODY_BYTES,
                ))?,
            }),
            Err(ureq::Error::Status(status, resp)) => Ok(ResponseSpec {
                status,
                headers: collect_headers(&resp),
                body: with_limit(read_body_capped(
                    &mut resp.into_reader(),
                    MAX_API_BODY_BYTES,
                ))?,
            }),
            Err(other) => Err(TbfError::Network(other.to_string())),
        }
    }

    fn send_download(
        &mut self,
        spec: RequestSpec,
        on_progress: &mut dyn FnMut(u64, u64) -> bool,
    ) -> Result<ResponseSpec, TbfError> {
        let agent = if spec.redirects == 0 {
            &self.manual_redirect_agent
        } else {
            &self.agent
        };
        let mut request = agent.request(&spec.method, &spec.url);
        for (name, value) in &spec.headers {
            request = request.set(name, value);
        }
        let body = spec.body.unwrap_or_default();
        match request.send_bytes(&body) {
            Ok(resp) => {
                let status = resp.status();
                let headers = collect_headers(&resp);
                let total = resp
                    .header("content-length")
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0);
                let mut reader = resp.into_reader();
                let body = match read_body_with_progress(
                    &mut reader,
                    total,
                    MAX_DOWNLOAD_BODY_BYTES,
                    on_progress,
                ) {
                    BodyOutcome::Read(body) => body,
                    BodyOutcome::Cancelled => return Err(TbfError::Cancelled),
                    BodyOutcome::TooLarge(limit) => {
                        return Err(TbfError::Upstream(format!(
                            "ダウンロードが上限（{limit} バイト）を超えました"
                        )));
                    }
                    BodyOutcome::Io(message) => {
                        return Err(TbfError::Network(format!(
                            "ダウンロードの読み出しに失敗しました: {message}"
                        )));
                    }
                };
                Ok(ResponseSpec {
                    status,
                    headers,
                    body,
                })
            }
            Err(ureq::Error::Status(status, resp)) => Ok(ResponseSpec {
                status,
                headers: collect_headers(&resp),
                body: with_limit(read_body_capped(
                    &mut resp.into_reader(),
                    MAX_API_BODY_BYTES,
                ))?,
            }),
            Err(other) => Err(TbfError::Network(other.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> RequestSpec {
        RequestSpec {
            method: "GET".into(),
            url: "https://example.com/file.bin".into(),
            headers: Vec::new(),
            body: None,
            redirects: 3,
        }
    }

    /// 進捗コールバックが `false` を返したら中止する（本文は返さない）。
    #[test]
    fn send_download_aborts_when_the_callback_returns_false() {
        let mut transport: Box<dyn FnMut(RequestSpec) -> Result<ResponseSpec, TbfError> + Send> =
            Box::new(|_spec: RequestSpec| {
                Ok(ResponseSpec {
                    status: 200,
                    headers: Vec::new(),
                    body: vec![1, 2, 3],
                })
            });
        let error = transport
            .send_download(spec(), &mut |_, _| false)
            .expect_err("中止を要求したのに本文が返っている");
        assert!(
            matches!(error, TbfError::Cancelled),
            "中止が Cancelled として伝わっていない: {error:?}"
        );
    }

    /// 中止しなければ本文全体と (total, total) の進捗を返す（既定実装）。
    #[test]
    fn send_download_reports_the_full_body_when_not_cancelled() {
        let mut transport: Box<dyn FnMut(RequestSpec) -> Result<ResponseSpec, TbfError> + Send> =
            Box::new(|_spec: RequestSpec| {
                Ok(ResponseSpec {
                    status: 200,
                    headers: Vec::new(),
                    body: vec![1, 2, 3],
                })
            });
        let mut progress: Vec<(u64, u64)> = Vec::new();
        let response = transport
            .send_download(spec(), &mut |downloaded, total| {
                progress.push((downloaded, total));
                true
            })
            .expect("中止していないのに失敗した");
        assert_eq!(response.body, vec![1, 2, 3]);
        assert_eq!(progress, vec![(3, 3)]);
    }

    /// 1% 刻みで通知し、同じ % のうちは呼ばない（UI を詰まらせない）。
    #[test]
    fn read_body_with_progress_reports_once_per_percent() {
        let total = 64 * 1024 * 2;
        let mut reader = std::io::Cursor::new(vec![0u8; total as usize]);
        let mut calls: Vec<u64> = Vec::new();
        let outcome = read_body_with_progress(
            &mut reader,
            total as u64,
            MAX_DOWNLOAD_BODY_BYTES,
            &mut |downloaded, _| {
                calls.push(downloaded);
                true
            },
        );
        let BodyOutcome::Read(body) = outcome else {
            panic!("上限内なのに本文が読めていない");
        };
        assert_eq!(body.len(), total as usize);
        assert_eq!(calls, vec![64 * 1024, total as u64], "通知が 1% 刻みでない");
    }

    /// 中止すると途中まで読んだバイト列は返さず、読み進めもしない。
    #[test]
    fn read_body_with_progress_discards_partial_data_on_cancel() {
        let total = 64 * 1024 * 4;
        let mut reader = std::io::Cursor::new(vec![7u8; total as usize]);
        let mut seen: Vec<u64> = Vec::new();
        let outcome = read_body_with_progress(
            &mut reader,
            total as u64,
            MAX_DOWNLOAD_BODY_BYTES,
            &mut |downloaded, _| {
                seen.push(downloaded);
                false
            },
        );
        assert!(
            matches!(outcome, BodyOutcome::Cancelled),
            "中止したのに途中のデータが返っている"
        );
        assert_eq!(seen, vec![64 * 1024], "最初のチャンクで通知していない");
        assert_eq!(
            reader.position(),
            64 * 1024,
            "中止後も読み進めている（転送が止まっていない）"
        );
    }

    /// 上限を超えた本文は成功として返さない（宣言サイズではなく実バイト数で判定）。
    #[test]
    fn read_body_with_progress_stops_at_the_limit() {
        let mut reader = std::io::Cursor::new(vec![0u8; 200 * 1024]);
        let outcome = read_body_with_progress(&mut reader, 0, 64 * 1024, &mut |_, _| true);
        assert!(
            matches!(outcome, BodyOutcome::TooLarge(limit) if limit == 64 * 1024),
            "上限超過が失敗として伝わっていない"
        );
    }

    /// `Content-Length` が無い応答でも進捗（＝中止）が効く。
    ///
    /// 以前は `total == 0` のときコールバックを一度も呼んでおらず、Content-Length を
    /// 返さないサイトではキャンセルできなかった（読むしかない）。
    #[test]
    fn read_body_with_progress_notifies_without_content_length() {
        let total = 4 * 1024 * 1024;
        let mut reader = std::io::Cursor::new(vec![0u8; total]);
        let mut calls: Vec<(u64, u64)> = Vec::new();
        let outcome = read_body_with_progress(
            &mut reader,
            0,
            MAX_DOWNLOAD_BODY_BYTES,
            &mut |downloaded, reported_total| {
                calls.push((downloaded, reported_total));
                // 2 回目の通知で中止する
                calls.len() < 2
            },
        );
        assert!(
            matches!(outcome, BodyOutcome::Cancelled),
            "Content-Length 無しで中止できない"
        );
        assert_eq!(
            calls,
            vec![(1024 * 1024, 0), (2 * 1024 * 1024, 0)],
            "1MiB 刻みで通知していない"
        );
    }

    /// `Content-Length` が無くても、読み切ったら最後のサイズを一度通知する。
    #[test]
    fn read_body_with_progress_reports_the_final_size_without_content_length() {
        let total = 1024 * 1024 + 4096;
        let mut reader = std::io::Cursor::new(vec![0u8; total]);
        let mut calls: Vec<(u64, u64)> = Vec::new();
        let outcome = read_body_with_progress(
            &mut reader,
            0,
            MAX_DOWNLOAD_BODY_BYTES,
            &mut |downloaded, reported_total| {
                calls.push((downloaded, reported_total));
                true
            },
        );
        let BodyOutcome::Read(body) = outcome else {
            panic!("本文が読めていない");
        };
        assert_eq!(body.len(), total);
        assert_eq!(calls, vec![(1024 * 1024, 0), (total as u64, 0)]);
    }
}
