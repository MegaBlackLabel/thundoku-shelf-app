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
    /// Default implementation falls back to [`Self::send`] and reports the
    /// whole body at once (no live progress).
    fn send_download(
        &mut self,
        spec: RequestSpec,
        on_progress: &mut dyn FnMut(u64, u64),
    ) -> Result<ResponseSpec, TbfError> {
        let resp = self.send(spec)?;
        let total = resp.body.len() as u64;
        on_progress(total, total);
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

fn read_body(reader: &mut impl std::io::Read) -> Vec<u8> {
    let mut body = Vec::new();
    let _ = reader.read_to_end(&mut body);
    body
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
                body: read_body(&mut resp.into_reader()),
            }),
            Err(ureq::Error::Status(status, resp)) => Ok(ResponseSpec {
                status,
                headers: collect_headers(&resp),
                body: read_body(&mut resp.into_reader()),
            }),
            Err(other) => Err(TbfError::Network(other.to_string())),
        }
    }

    fn send_download(
        &mut self,
        spec: RequestSpec,
        on_progress: &mut dyn FnMut(u64, u64),
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
                let mut out = Vec::new();
                let mut buf = vec![0u8; 64 * 1024];
                let mut downloaded = 0u64;
                // Report progress at most once per whole percent — per-chunk
                // callbacks flood the channel and stall the download when the
                // consumer is throttled (UI thread).
                let mut last_pct = u32::MAX;
                loop {
                    match reader.read(&mut buf) {
                        Ok(0) => break,
                        Ok(n) => {
                            downloaded += n as u64;
                            out.extend_from_slice(&buf[..n]);
                            if total > 0 {
                                let pct = (downloaded * 100).checked_div(total).unwrap_or(0) as u32;
                                if pct != last_pct {
                                    last_pct = pct;
                                    on_progress(downloaded, total);
                                }
                            }
                        }
                        Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                        Err(_) => break,
                    }
                }
                Ok(ResponseSpec {
                    status,
                    headers,
                    body: out,
                })
            }
            Err(ureq::Error::Status(status, resp)) => Ok(ResponseSpec {
                status,
                headers: collect_headers(&resp),
                body: read_body(&mut resp.into_reader()),
            }),
            Err(other) => Err(TbfError::Network(other.to_string())),
        }
    }
}
