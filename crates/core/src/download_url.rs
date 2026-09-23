//! ダウンロード URL の検証（**資格情報を送る前**の関門）。
//!
//! 各ストアのクライアントは、セッション Cookie / XSRF トークンを付けたリクエストを
//! 送る。このとき送信先を検証していなかったため、以下が成立していた:
//!
//! - `bookshelf_items.download_url` は Drive の JSON バックアップから復元でき、
//!   改変したバックアップを利用者に復元させると、**外部ホストへセッション Cookie が
//!   送られる**（取得に失敗しても Cookie は渡る）
//! - 302 の `Location` を手動で追跡する経路（DLsite / FANZA）はスキームもホストも
//!   検証せず、`Location` を差し替えられると任意ホストへ Cookie を転送させられる
//!
//! したがって「認証付き送信の直前」と「リダイレクト先へ送る直前」に、
//! 解析した URL が `https` かつ許可ホスト（と必要なパス）であることを検証する。

use std::fmt;

/// 許可する送信先。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostRule {
    pub host: &'static str,
    /// `true` なら `host` を接尾辞として扱う（`dmm.co.jp` が `x.dmm.co.jp` にも一致）。
    /// Cookie のスコープ（`domain=.dmm.co.jp`）に合わせるときに使う。
    pub subdomains: bool,
    /// `Some(prefix)` のとき、パスがその接頭辞で始まることを要求する。
    pub path_prefix: Option<&'static str>,
}

impl HostRule {
    /// ホスト完全一致。
    pub const fn exact(host: &'static str, path_prefix: Option<&'static str>) -> Self {
        Self {
            host,
            subdomains: false,
            path_prefix,
        }
    }

    /// ホストとそのサブドメイン。
    pub const fn with_subdomains(host: &'static str, path_prefix: Option<&'static str>) -> Self {
        Self {
            host,
            subdomains: true,
            path_prefix,
        }
    }
}

/// `host` が `domain` そのもの、またはそのサブドメインか（大文字小文字は区別しない）。
///
/// `<something>.domain` の形だけを許す（`evilbooth.pm` のような接尾辞の部分一致は不可）。
/// ログイン完了判定（WebView が目的のサイトへ戻ったか）と Cookie の収集元判定が使う。
pub fn host_within(host: &str, domain: &str) -> bool {
    let (host, domain) = (host.as_bytes(), domain.as_bytes());
    if host.len() == domain.len() {
        return host.eq_ignore_ascii_case(domain);
    }
    if host.len() < domain.len() {
        return false;
    }
    // `<something>.domain` の形だけを許す（`evilhost` のような部分一致は不可）。
    host[host.len() - domain.len() - 1] == b'.'
        && host[host.len() - domain.len()..].eq_ignore_ascii_case(domain)
}

/// ルールが `host` に一致するか（大文字小文字は区別しない）。
fn host_matches(rule: &HostRule, host: &str) -> bool {
    if rule.subdomains {
        host_within(host, rule.host)
    } else {
        host.len() == rule.host.len() && host.as_bytes().eq_ignore_ascii_case(rule.host.as_bytes())
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum UrlError {
    /// `https://` 以外のスキーム。
    NotHttps,
    /// ホストが空、または `userinfo` 付き（`https://user@host/...`）。
    Malformed,
    /// 443 以外のポート指定。
    UnexpectedPort(String),
    HostNotAllowed(String),
    PathNotAllowed(String),
}

impl fmt::Display for UrlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotHttps => write!(f, "https 以外の URL"),
            Self::Malformed => write!(f, "URL を解釈できない"),
            Self::UnexpectedPort(port) => write!(f, "許可しないポート（{port}）"),
            Self::HostNotAllowed(host) => write!(f, "許可しないホスト（{host}）"),
            Self::PathNotAllowed(path) => write!(f, "許可しないパス（{path}）"),
        }
    }
}

/// 検証済み URL の構成要素。
#[derive(Debug, PartialEq, Eq)]
pub struct ParsedUrl<'a> {
    pub host: &'a str,
    /// クエリ・フラグメントを除いたパス（先頭は `/`）。
    pub path: &'a str,
}

/// `https://host[:443]/path` として解析する。`/` を伴わない authority も許す。
pub fn parse(url: &str) -> Result<ParsedUrl<'_>, UrlError> {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("HTTPS://"))
        .ok_or(UrlError::NotHttps)?;
    // authority は最初の `/` `?` `#` まで
    let end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(end);
    if authority.is_empty() || authority.contains('@') {
        return Err(UrlError::Malformed);
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) => (host, Some(port)),
        None => (authority, None),
    };
    if host.is_empty() {
        return Err(UrlError::Malformed);
    }
    if let Some(port) = port
        && port != "443"
    {
        return Err(UrlError::UnexpectedPort(port.to_string()));
    }
    // パス（クエリ・フラグメントは落とす）
    let path_end = tail.find(['?', '#']).unwrap_or(tail.len());
    let path = if tail.starts_with('/') {
        &tail[..path_end]
    } else {
        "/"
    };
    Ok(ParsedUrl { host, path })
}

/// 許可ルールに照らして検証する。通れば解析結果を返す。
pub fn check<'a>(url: &'a str, rules: &[HostRule]) -> Result<ParsedUrl<'a>, UrlError> {
    let parsed = parse(url)?;
    for rule in rules {
        if !host_matches(rule, parsed.host) {
            continue;
        }
        if let Some(prefix) = rule.path_prefix
            && !parsed.path.starts_with(prefix)
        {
            return Err(UrlError::PathNotAllowed(parsed.path.to_string()));
        }
        return Ok(parsed);
    }
    Err(UrlError::HostNotAllowed(parsed.host.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    const RULES: &[HostRule] = &[HostRule::exact("booth.pm", Some("/downloadables/"))];

    #[test]
    fn accepts_an_allowed_url() {
        let parsed = check("https://booth.pm/downloadables/12345?x=1", RULES).unwrap();
        assert_eq!(parsed.host, "booth.pm");
        assert_eq!(parsed.path, "/downloadables/12345");
    }

    /// `host_within` は「そのドメイン or そのサブドメイン」だけを真にする
    /// （`evilbooth.pm` のような接尾辞の部分一致は偽、大文字小文字は区別しない）。
    ///
    /// ログイン完了判定（WebView が目的のサイトに戻ったか）と Cookie の収集元判定が
    /// これを使う。`ends_with("booth.pm")` は `evilbooth.pm` を通してしまう。
    #[test]
    fn host_within_requires_a_label_boundary() {
        assert!(host_within("booth.pm", "booth.pm"));
        assert!(host_within("accounts.booth.pm", "booth.pm"));
        assert!(host_within("BOOTH.PM", "booth.pm"));
        assert!(host_within("Accounts.Booth.pm", "booth.pm"));
        assert!(host_within("techbookfest.org", "techbookfest.org"));

        assert!(!host_within("evilbooth.pm", "booth.pm"));
        assert!(!host_within("booth.pm.evil.example.com", "booth.pm"));
        assert!(!host_within("eviltechbookfest.org", "techbookfest.org"));
        assert!(!host_within("booth.p", "booth.pm"));
        assert!(!host_within("", "booth.pm"));
    }

    /// 接尾辞の部分一致（`evildmm.co.jp`）は許可しない
    #[test]
    fn subdomain_rules_require_a_label_boundary() {
        const CDN: &[HostRule] = &[HostRule::with_subdomains("dmm.co.jp", None)];
        for host in [
            "dmm.co.jp",
            "www.dmm.co.jp",
            "doujin.contents.doujin.dmm.co.jp",
        ] {
            assert!(check(&format!("https://{host}/x"), CDN).is_ok(), "{host}");
        }
        // 接尾辞の部分一致（`evildmm.co.jp`）は許可しない
        assert!(matches!(
            check("https://evildmm.co.jp/x", CDN),
            Err(UrlError::HostNotAllowed(_))
        ));
        // 完全一致ルールはサブドメインを許可しない
        assert!(matches!(
            check("https://download.booth.pm/downloadables/1", RULES),
            Err(UrlError::HostNotAllowed(_))
        ));
    }

    #[test]
    fn rejects_other_hosts_even_with_the_same_suffix() {
        for url in [
            "https://evil.example.com/downloadables/1",
            "https://booth.pm.evil.example.com/downloadables/1",
            "https://evil.example.com/?u=https://booth.pm/downloadables/1",
        ] {
            assert!(
                matches!(check(url, RULES), Err(UrlError::HostNotAllowed(_))),
                "ホスト検証を通過している: {url}"
            );
        }
    }

    #[test]
    fn rejects_plain_http_and_odd_ports() {
        assert_eq!(
            check("http://booth.pm/downloadables/1", RULES),
            Err(UrlError::NotHttps)
        );
        assert!(matches!(
            check("https://booth.pm:8443/downloadables/1", RULES),
            Err(UrlError::UnexpectedPort(_))
        ));
        // 443 の明示は許す
        assert!(check("https://booth.pm:443/downloadables/1", RULES).is_ok());
    }

    #[test]
    fn rejects_userinfo_and_other_paths() {
        assert_eq!(
            check("https://user@booth.pm/downloadables/1", RULES),
            Err(UrlError::Malformed)
        );
        assert!(matches!(
            check("https://booth.pm/ja/items/1", RULES),
            Err(UrlError::PathNotAllowed(_))
        ));
    }

    #[test]
    fn relative_urls_are_not_accepted_for_requests() {
        // 相対 URL は「送信先」ではない（サーバーに渡す前に絶対化する）。
        assert_eq!(check("/api/product-dlc/1", RULES), Err(UrlError::NotHttps));
    }
}
