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

use url::Url;

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

/// 検証済み URL。**送信にはこの `url` を使う**。
///
/// 検証と実送信で同じ `url::Url`（WHATWG）の正規化結果を共有するための型。
/// 生の文字列をそのまま送ると、`ureq` が送信時にもう一度解釈し直すため、
/// 「検証したホスト・パス」と「実際に送るホスト・パス」が食い違い得る。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedUrl {
    /// 正規化済みの URL（`url::Url::as_str()` を送信に使う）。
    pub url: Url,
    /// 小文字化済みのホスト（`url.host_str()` 由来）。
    pub host: String,
    /// クエリ・フラグメントを除き、ドットセグメントを解決したパス（先頭は `/`）。
    pub path: String,
}

/// 生のパスに「パーサが解決してしまう」表現が無いか。
///
/// - 生の `.` / `..` セグメント（`/downloadables/../../x` は実送信で `/x` になる）
/// - `%2e` 系の符号化ドット（同じく解決される）
///
/// 正規のダウンロード URL には現れないので、解決せず**拒否**する（fail-closed）。
fn raw_path_looks_ambiguous(url: &str) -> bool {
    let rest = url.split_once("://").map(|(_, rest)| rest).unwrap_or("");
    let path = match rest.find('/') {
        Some(index) => &rest[index..],
        None => "",
    };
    let path = path.split(['?', '#']).next().unwrap_or("");
    if path.to_ascii_lowercase().contains("%2e") {
        return true;
    }
    path.split('/')
        .any(|segment| segment == "." || segment == "..")
}

/// `https://host[:443]/path` を `url::Url`（WHATWG）で解析し、正規化済みの URL を返す。
///
/// 検証を手動の文字列分割でやると実送信の解釈と食い違う。`https://audit.invalid\.allowed.example/x`
/// は authority の終端が `\` なので実ホストは `audit.invalid` になり、符号化ドット
/// （`%2e%2e`）は解決されて許可プレフィックスの外へ出る。ここで同じパーサを通し、
/// **返した URL をそのまま送信に使う**ことで「検証した宛先 = 送る宛先」にする。
pub fn parse(url: &str) -> Result<ParsedUrl, UrlError> {
    // 生のバックスラッシュは URL に現れない（special URL では区切りの意味になる）。
    if url.contains('\\') {
        return Err(UrlError::Malformed);
    }
    if !url
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
    {
        return Err(UrlError::NotHttps);
    }
    if raw_path_looks_ambiguous(url) {
        return Err(UrlError::Malformed);
    }
    let parsed = Url::parse(url).map_err(|_| UrlError::Malformed)?;
    if parsed.scheme() != "https" {
        return Err(UrlError::NotHttps);
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(UrlError::Malformed);
    }
    // `:443` は既定ポートなので `port()` は `None` になる（明示も許す）。
    if let Some(port) = parsed.port()
        && port != 443
    {
        return Err(UrlError::UnexpectedPort(port.to_string()));
    }
    let host = parsed
        .host_str()
        .ok_or(UrlError::Malformed)?
        .to_ascii_lowercase();
    if host.is_empty() {
        return Err(UrlError::Malformed);
    }
    Ok(ParsedUrl {
        path: parsed.path().to_string(),
        url: parsed,
        host,
    })
}

/// 解析済み URL を許可ルールに照らす（**リダイレクトの各ホップ**の検証に使う）。
///
/// 正規化済みの `url.path()` で判定するので、エンコードの違いで許可プレフィックスを
/// 迂回することはない（検証したパス = 実際に送るパス）。
pub fn check_url(url: &Url, rules: &[HostRule]) -> Result<(), UrlError> {
    if url.scheme() != "https" {
        return Err(UrlError::NotHttps);
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(UrlError::Malformed);
    }
    if let Some(port) = url.port()
        && port != 443
    {
        return Err(UrlError::UnexpectedPort(port.to_string()));
    }
    let host = url.host_str().ok_or(UrlError::Malformed)?;
    let path = url.path();
    let mut host_matched = false;
    for rule in rules {
        if !host_matches(rule, host) {
            continue;
        }
        host_matched = true;
        match rule.path_prefix {
            Some(prefix) if !path.starts_with(prefix) => continue,
            _ => return Ok(()),
        }
    }
    if host_matched {
        return Err(UrlError::PathNotAllowed(path.to_string()));
    }
    Err(UrlError::HostNotAllowed(host.to_string()))
}

/// 許可ルールに照らして検証する。通れば**正規化済み URL**を返す（送信にはこれを使う）。
///
/// 同じホストに複数のルールを並べられる（DLsite のストア別のように、パスだけが違う
/// ルール）。そのため**ホストが一致したルールを全部見て**、どれかのパスに一致すれば許可し、
/// ホストは一致したのにパスがどれにも一致しないときだけ `PathNotAllowed` を返す
/// （最初のホスト一致で確定させると、2 本目以降のパスが効かない）。
pub fn check(url: &str, rules: &[HostRule]) -> Result<ParsedUrl, UrlError> {
    let parsed = parse(url)?;
    check_url(&parsed.url, rules)?;
    Ok(parsed)
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

    /// 同じホストに複数のルールを並べたとき（DLsite のストア別のように）、**どれかの
    /// パスに一致すれば許可**し、どれにも一致しなければ拒否する。
    #[test]
    fn several_rules_for_the_same_host_are_all_considered() {
        const STORES: &[HostRule] = &[
            HostRule::exact("www.dlsite.com", Some("/maniax/download/")),
            HostRule::exact("www.dlsite.com", Some("/home/download/")),
        ];
        // 2 本目のルールで一致するものも通る
        assert!(check("https://www.dlsite.com/home/download/=/product_id/RJ1.html", STORES).is_ok());
        assert!(check("https://www.dlsite.com/maniax/download/=/product_id/RJ1.html", STORES).is_ok());
        // ホストは一致するがパスがどこにも一致しないときは拒否（理由もパス）
        assert!(matches!(
            check("https://www.dlsite.com/maniax/mypage/userbuy/=/x/", STORES),
            Err(UrlError::PathNotAllowed(_))
        ));
        // ホストが一致しないときはホストで拒否
        assert!(matches!(
            check("https://dl.dlsite.com/maniax/download/=/x/", STORES),
            Err(UrlError::HostNotAllowed(_))
        ));
    }

    /// `.` / `..` のセグメントを含むパスは**拒否**する（fail-closed）。
    ///
    /// 生の文字列の接頭辞を見るだけでは `https://host/downloadables/../../x` が通ってしまい、
    /// 実際に送られるリクエスト（URL パーサがドットセグメントを解決する）は `/x` になるため、
    /// パスの許可リストを迂回できる。正規のダウンロード URL にドットセグメントは現れないので、
    /// 解決する代わりに拒否する。
    #[test]
    fn dot_segments_in_the_path_are_rejected() {
        for url in [
            "https://booth.pm/downloadables/../../x",
            "https://booth.pm/downloadables/./1",
            "https://booth.pm/..",
        ] {
            assert!(
                matches!(check(url, RULES), Err(UrlError::Malformed)),
                "ドットセグメントを通している: {url}"
            );
        }
        // クエリの中の `.` はパスではないので影響しない
        assert!(check("https://booth.pm/downloadables/1?x=../y", RULES).is_ok());
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

    /// 実送信は `url::Url`（WHATWG）で解釈されるため、検証も同じ解釈に合わせる。
    /// バックスラッシュは special URL の authority 終端として扱われ、実ホストが変わる。
    /// 手動分割では `audit.invalid\.contents.doujin.dmm.co.jp` が許可サブドメインに
    /// 見えてしまう（＝実送信先 `audit.invalid` へ署名 Cookie が飛ぶ）。
    #[test]
    fn backslash_in_the_authority_is_not_a_subdomain_of_the_allowed_host() {
        const CDN: &[HostRule] = &[HostRule::with_subdomains("contents.doujin.dmm.co.jp", None)];
        let url = "https://audit.invalid\\.contents.doujin.dmm.co.jp/book.zip";
        assert!(
            matches!(
                check(url, CDN),
                Err(UrlError::Malformed) | Err(UrlError::HostNotAllowed(_))
            ),
            "バックスラッシュで許可ホストを偽装できている: {url}"
        );
    }

    /// パーセント符号化されたドットセグメントは WHATWG が解決するため、正規化後の
    /// パスは許可プレフィックスの外に出る（`/downloadables/%2e%2e/x` → `/x`）。
    /// 生の接頭辞だけで見ると通ってしまうので fail-closed で拒否する。
    #[test]
    fn percent_encoded_dot_segments_in_the_path_are_rejected() {
        for url in [
            "https://booth.pm/downloadables/%2e%2e/audit-only",
            "https://booth.pm/downloadables/%2E%2E/audit-only",
            "https://booth.pm/downloadables/.%2e/audit-only",
        ] {
            assert!(
                matches!(check(url, RULES), Err(UrlError::Malformed)),
                "符号化ドットでパスの許可リストを迂回できている: {url}"
            );
        }
    }

    /// 検証結果は正規化済み URL を持ち、送信にはこれを使う（検証した宛先 = 送る宛先）。
    #[test]
    fn check_returns_the_canonical_url_that_must_be_sent() {
        let parsed =
            check("https://BOOTH.pm:443/downloadables/1?x=1", RULES).expect("許可されるはず");
        // ホストの小文字化・既定ポートの除去が済んだ形（この文字列を送る）
        assert_eq!(parsed.url.as_str(), "https://booth.pm/downloadables/1?x=1");
        assert_eq!(parsed.host, "booth.pm");
        assert_eq!(parsed.path, "/downloadables/1");
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
