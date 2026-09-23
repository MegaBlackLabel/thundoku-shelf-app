//! 収集元ホスト別の Cookie バッグ（ストアのセッション Cookie の置き場）。
//!
//! 収集元（`www.dmm.co.jp` と `accounts.dmm.co.jp`、`www.dlsite.com` と
//! `login.dlsite.com`、`booth.pm` と `accounts.booth.pm`）を 1 つに潰すと、片方にしか
//! 送るべきでない Cookie がもう片方や**別システム（ダウンロード CDN）**へ飛ぶ。
//! 宛先 URL ごとに取り出せるようにホスト別で保つ。
//!
//! 各 Cookie は WebView が返す属性（`Domain` / `Path` / `Secure` / `Expires`）を持ち、
//! 送信先は**収集元ホストの完全一致**に限る（ブラウザの domain 一致より狭い。CDN のような
//! 別システムへセッション Cookie を広げないという決定を維持する）。そのうえで宛先 URL の
//! `Path` / スキーム（`Secure`）/ 現在時刻（期限）を満たさない Cookie は送らない。
//!
//! シリアライズは「ホスト →（Cookie 名 → 値 or 属性付きオブジェクト）」。値だけの旧形式も
//! 読める（保存済みセッションを捨てないため。`CookieEntry` の `Deserialize` を参照）。

use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize};

/// 1 つの Cookie（送信先の判定に使う属性だけを保存する）。
///
/// `HttpOnly` / `SameSite` は**送信先の判定に使わない**ため保存しない（HttpOnly は
/// Cookie の取得経路＝WebView の Cookie ストアを塞ぐものではないし、SameSite は
/// クロスサイトのリクエスト文脈が無いアプリ内の HTTP 呼び出しには効かない）。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CookieEntry {
    pub value: String,
    /// `Domain` 属性。WebView2 は host-only も domain 指定も**先頭ドット無し**で返す
    /// （`www.dlsite.com` / `dlsite.com`）。判定は [`domain_matches`] を参照。
    pub domain: Option<String>,
    /// `Path` 属性（無ければ `/` として扱う）。
    pub path: Option<String>,
    /// `Secure`（`https` 以外では送らない）。
    pub secure: bool,
    /// 期限（Unix 秒）。`None` = セッション Cookie。
    pub expires: Option<i64>,
}

impl CookieEntry {
    /// 値だけの Cookie（host-only / `Path` 指定なし / 非 Secure / 期限なし）。
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            value: value.into(),
            domain: None,
            path: None,
            secure: false,
            expires: None,
        }
    }

    /// WebView が返す属性から作る（core は cookie クレートに依存しないよう素の値で受ける）。
    pub fn with_attributes(
        value: String,
        domain: Option<String>,
        path: Option<String>,
        secure: bool,
        expires: Option<i64>,
    ) -> Self {
        Self {
            value,
            domain,
            path,
            secure,
            expires,
        }
    }

    /// 宛先（ホスト / パス / https か / 現在時刻）に送ってよいか。
    fn applies_to(&self, host: &str, path: &str, https: bool, now_unix: i64) -> bool {
        if self.secure && !https {
            return false;
        }
        if let Some(expires) = self.expires
            && now_unix >= expires
        {
            return false;
        }
        if let Some(domain) = &self.domain
            && !domain_matches(domain, host)
        {
            return false;
        }
        path_matches(self.path.as_deref().unwrap_or("/"), path)
    }
}

/// 旧形式（値だけの文字列）も読めるようにする。
///
/// 属性を足す前に保存されたセッションは `{"origins":{"host":{"name":"value"}}}` の形なので、
/// それを「host-only / `Path` なし / 非 Secure / 期限なし」として読む（再ログインを強いない）。
impl<'de> Deserialize<'de> for CookieEntry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            Value(String),
            Full {
                value: String,
                #[serde(default)]
                domain: Option<String>,
                #[serde(default)]
                path: Option<String>,
                #[serde(default)]
                secure: bool,
                #[serde(default)]
                expires: Option<i64>,
            },
        }

        Ok(match Raw::deserialize(deserializer)? {
            Raw::Value(value) => Self::new(value),
            Raw::Full {
                value,
                domain,
                path,
                secure,
                expires,
            } => Self::with_attributes(value, domain, path, secure, expires),
        })
    }
}

/// 収集元ホスト →（Cookie 名 → Cookie）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct HostScopedCookies {
    origins: BTreeMap<String, BTreeMap<String, CookieEntry>>,
}

impl HostScopedCookies {
    pub fn new(origins: BTreeMap<String, BTreeMap<String, CookieEntry>>) -> Self {
        Self { origins }
    }

    /// 1 つの収集元（ログインで Cookie を拾ったホスト）だけを、値だけの Cookie で作る。
    pub fn from_origin(host: &str, cookies: BTreeMap<String, String>) -> Self {
        Self::from_origin_entries(
            host,
            cookies
                .into_iter()
                .map(|(name, value)| (name, CookieEntry::new(value)))
                .collect(),
        )
    }

    /// 1 つの収集元だけを持つ。
    pub fn from_origin_entries(
        host: &str,
        cookies: BTreeMap<String, CookieEntry>,
    ) -> Self {
        Self {
            origins: BTreeMap::from([(host.to_string(), cookies)]),
        }
    }

    /// 宛先 URL 向けの `Cookie` ヘッダ。**その URL のホスト向けに収集した Cookie だけ**を返し、
    /// さらに `Path` / `Secure` / 期限を満たすものだけを載せる。
    ///
    /// ホストは**収集元との完全一致**（大小文字は区別しない）。ブラウザは `Domain` 属性に応じて
    /// サブドメインにも送るが、ここでは送らない（ダウンロード CDN のような別システムへ
    /// セッション Cookie を広げないという決定を維持する。Cookie は宛先別に絞ってあり、
    /// CDN へは 302 で受け取る署名 Cookie だけを送る）。
    ///
    /// URL を解釈できないときは空（fail-closed）。
    pub fn header_for_url(&self, url: &str) -> String {
        let Some((host, path, https)) = split_url(url) else {
            return String::new();
        };
        let now = now_unix();
        join(
            self.origins
                .iter()
                .filter(|(origin, _)| origin.eq_ignore_ascii_case(host))
                .flat_map(|(_, cookies)| cookies.iter())
                .filter(|(_, cookie)| cookie.applies_to(host, path, https, now))
                .map(|(name, cookie)| (name, &cookie.value)),
        )
    }

    /// 収集元ホストの Cookie 値（認証済み判定・ログ用）。収集元をまたがない。
    pub fn value(&self, host: &str, name: &str) -> Option<&str> {
        self.origins
            .get(host)?
            .get(name)
            .map(|cookie| cookie.value.as_str())
    }

    pub fn count(&self) -> usize {
        self.origins.values().map(BTreeMap::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.count() == 0
    }
}

/// URL を `(host, path, https か)` に分解する。
///
/// ここは**選択**のための緩い分解で、送信可否の検証は `crate::download_url::check`
/// （許可リスト）が別途行う。`userinfo` 付きやホスト無しは解釈しない（＝ Cookie を載せない）。
fn split_url(url: &str) -> Option<(&str, &str, bool)> {
    let (scheme, rest) = url.split_once("://")?;
    let https = scheme.eq_ignore_ascii_case("https");
    let authority_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
    let (authority, tail) = rest.split_at(authority_end);
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|c| c.is_ascii_digit()) => host,
        _ => authority,
    };
    if host.is_empty() || host.contains('@') {
        return None;
    }
    let path_end = tail.find(['?', '#']).unwrap_or(tail.len());
    let path = if tail.starts_with('/') {
        &tail[..path_end]
    } else {
        "/"
    };
    Some((host, path, https))
}

/// RFC 6265 の path-match。`/foo` は `/foo` と `/foo/bar` に一致し、`/foobar` には一致しない。
fn path_matches(cookie_path: &str, request_path: &str) -> bool {
    cookie_path == "/"
        || request_path == cookie_path
        || (request_path.starts_with(cookie_path)
            && (cookie_path.ends_with('/')
                || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')))
}

/// Cookie の `Domain` 属性が宛先ホストに適用できるか。
///
/// RFC 6265 では `Domain` の先頭ドットは**有っても無くても**サブドメインに適用される
/// （ドットは旧式で無視される）。WebView2 は domain 指定の Cookie を**先頭ドット無し**
/// （`dlsite.com`）で返すため、ドット無しを host-only と解釈すると、ログイン後の同期が
/// **Cookie を 1 つも載せずに**走り「セッション切れ・未ログイン」になる（実測: DLsite）。
/// host-only の Cookie は WebView2 がそのホストそのもの（`www.dlsite.com`）を返すので、
/// どちらも「ホスト自身 or そのサブドメイン」で判定して差し支えない。
fn domain_matches(cookie_domain: &str, host: &str) -> bool {
    let domain = cookie_domain.strip_prefix('.').unwrap_or(cookie_domain);
    crate::download_url::host_within(host, domain)
}

/// 現在時刻（Unix 秒）。期限の判定に使う。
fn now_unix() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Cookie 名と値の組を `Cookie` ヘッダの形に連結する（順序は BTreeMap 順で安定）。
fn join<'a>(pairs: impl Iterator<Item = (&'a String, &'a String)>) -> String {
    pairs
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>()
        .join("; ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(name: &str, value: &str) -> (String, CookieEntry) {
        (name.to_string(), CookieEntry::new(value))
    }

    fn bag() -> HostScopedCookies {
        HostScopedCookies::new(BTreeMap::from([
            (
                "www.dmm.co.jp".to_string(),
                BTreeMap::from([entry("login_id", "abc")]),
            ),
            (
                "accounts.dmm.co.jp".to_string(),
                BTreeMap::from([entry("acct", "1")]),
            ),
        ]))
    }

    /// 宛先が収集元でなければ 1 つも送らない（CDN がこれに当たる）。
    /// **完全一致**なので、収集元のサブドメインにも送らない。
    #[test]
    fn header_for_url_is_scoped_to_the_destination_host() {
        let cookies = bag();

        assert_eq!(
            cookies.header_for_url("https://doujin.contents.doujin.dmm.co.jp/x.zip"),
            ""
        );
        assert_eq!(cookies.header_for_url("https://download.dlsite.com/get/x.zip"), "");
        assert_eq!(cookies.header_for_url("https://evil.example.com/"), "");
        // 収集元のサブドメインにも送らない（host-only な Cookie を別ホストへ出さない）
        assert_eq!(cookies.header_for_url("https://sub.www.dmm.co.jp/"), "");
        assert_eq!(
            cookies.header_for_url("https://accounts.dmm.co.jp.evil.example.com/"),
            ""
        );
        // URL として解釈できないものは載せない（fail-closed）
        assert_eq!(cookies.header_for_url("www.dmm.co.jp"), "");
        assert_eq!(cookies.header_for_url("https://user@www.dmm.co.jp/"), "");

        assert_eq!(
            cookies.header_for_url("https://www.dmm.co.jp/dc/doujin/api/mylibraries/"),
            "login_id=abc"
        );
        assert_eq!(
            cookies.header_for_url("https://accounts.dmm.co.jp/library?page=1"),
            "acct=1"
        );
    }

    /// 宛先ホストの大文字小文字は区別しない（URL 検証側 `download_url::host_matches` と揃える）。
    #[test]
    fn header_for_url_ignores_host_case() {
        let cookies = bag();

        assert_eq!(
            cookies.header_for_url("https://WWW.DMM.CO.JP/x"),
            "login_id=abc"
        );
        assert_eq!(
            cookies.header_for_url("https://ACCOUNTS.DMM.CO.JP/x"),
            "acct=1"
        );
        assert_eq!(
            cookies.header_for_url("https://DOUJIN.CONTENTS.DOUJIN.DMM.CO.JP/x"),
            ""
        );
    }

    /// `Path` 属性を満たさないリクエストには送らない。
    #[test]
    fn header_for_url_honors_the_cookie_path() {
        let cookies = HostScopedCookies::from_origin_entries(
            "booth.pm",
            BTreeMap::from([
                (
                    "scoped".to_string(),
                    CookieEntry::with_attributes(
                        "yes".into(),
                        None,
                        Some("/downloadables".into()),
                        false,
                        None,
                    ),
                ),
                ("wide".to_string(), CookieEntry::new("always")),
            ]),
        );

        assert_eq!(
            cookies.header_for_url("https://booth.pm/downloadables/12345"),
            "scoped=yes; wide=always"
        );
        // `/downloadablesXX` は path-match しない（境界は `/`）
        assert_eq!(
            cookies.header_for_url("https://booth.pm/downloadablesXX"),
            "wide=always"
        );
        assert_eq!(cookies.header_for_url("https://booth.pm/ja/items/1"), "wide=always");
        // クエリは path に含めない
        assert_eq!(
            cookies.header_for_url("https://booth.pm/downloadables/1?x=1"),
            "scoped=yes; wide=always"
        );
    }

    /// `Secure` は `https` 以外へ送らない（アプリの送信先は全て https なので実質は常に真）。
    #[test]
    fn header_for_url_honors_secure() {
        let cookies = HostScopedCookies::from_origin_entries(
            "booth.pm",
            BTreeMap::from([
                (
                    "secure_one".to_string(),
                    CookieEntry::with_attributes("s".into(), None, None, true, None),
                ),
                ("plain".to_string(), CookieEntry::new("p")),
            ]),
        );

        assert_eq!(
            cookies.header_for_url("https://booth.pm/ja"),
            "plain=p; secure_one=s"
        );
        assert_eq!(cookies.header_for_url("http://booth.pm/ja"), "plain=p");
    }

    /// 期限切れの Cookie は送らない（セッション Cookie = 期限なしは送る）。
    #[test]
    fn header_for_url_drops_expired_cookies() {
        let cookies = HostScopedCookies::from_origin_entries(
            "booth.pm",
            BTreeMap::from([
                (
                    "expired".to_string(),
                    CookieEntry::with_attributes("old".into(), None, None, false, Some(1)),
                ),
                (
                    "session".to_string(),
                    CookieEntry::with_attributes("live".into(), None, None, false, None),
                ),
            ]),
        );

        assert_eq!(cookies.header_for_url("https://booth.pm/ja"), "session=live");
    }

    /// `Domain` 属性が宛先に合わない Cookie は送らない（収集元の完全一致に加えた二重の防波堤）。
    #[test]
    fn header_for_url_honors_the_domain_attribute() {
        let cookies = HostScopedCookies::from_origin_entries(
            "www.dlsite.com",
            BTreeMap::from([
                (
                    "wide".to_string(),
                    CookieEntry::with_attributes(
                        "d".into(),
                        Some(".dlsite.com".into()),
                        None,
                        false,
                        None,
                    ),
                ),
                (
                    "narrow".to_string(),
                    CookieEntry::with_attributes(
                        "n".into(),
                        Some("login.dlsite.com".into()),
                        None,
                        false,
                        None,
                    ),
                ),
            ]),
        );

        // 収集元（www）へは domain が `.dlsite.com` のものだけが載る
        assert_eq!(cookies.header_for_url("https://www.dlsite.com/home/mypage"), "wide=d");
    }

    /// WebView2 は domain 指定の Cookie を**先頭ドット無し**（`dlsite.com`）で返す。
    ///
    /// RFC 6265 では `Domain=dlsite.com` はサブドメインにも適用されるので、収集元
    /// （`www.dlsite.com`）へ送る。ここを host-only と誤解すると、ログイン後の同期が
    /// **Cookie を 1 つも載せずに**走り「セッション切れ・未ログイン」になる（実測: DLsite）。
    #[test]
    fn header_for_url_includes_dotless_domain_cookies() {
        let cookies = HostScopedCookies::from_origin_entries(
            "www.dlsite.com",
            BTreeMap::from([
                (
                    "__DLsite_SID".to_string(),
                    CookieEntry::with_attributes(
                        "sid".into(),
                        Some("dlsite.com".into()),
                        Some("/".into()),
                        true,
                        None,
                    ),
                ),
                (
                    "host_only".to_string(),
                    CookieEntry::with_attributes(
                        "h".into(),
                        Some("www.dlsite.com".into()),
                        None,
                        true,
                        None,
                    ),
                ),
                (
                    "other_site".to_string(),
                    CookieEntry::with_attributes(
                        "o".into(),
                        Some("example.com".into()),
                        None,
                        true,
                        None,
                    ),
                ),
            ]),
        );

        let header = cookies.header_for_url("https://www.dlsite.com/maniax/mypage/userbuy/");
        assert!(header.contains("__DLsite_SID=sid"), "{header}");
        assert!(header.contains("host_only=h"), "{header}");
        assert!(!header.contains("other_site=o"), "{header}");
    }

    /// 収集元をまたがない（値の取り出しも同じ）。
    #[test]
    fn value_does_not_cross_origins() {
        let cookies = bag();
        assert_eq!(cookies.value("www.dmm.co.jp", "login_id"), Some("abc"));
        assert_eq!(cookies.value("accounts.dmm.co.jp", "login_id"), None);
        assert_eq!(cookies.value("download.dlsite.com", "login_id"), None);
        assert_eq!(cookies.count(), 2);
        assert!(!cookies.is_empty());
        assert!(HostScopedCookies::default().is_empty());
    }

    /// 保存形式は「ホスト → Cookie」。**属性を足す前の値だけの形**も読める
    /// （再ログインを強いない）。書き出しは属性つきの形になる。
    #[test]
    fn serializes_with_attributes_and_reads_the_legacy_shape() {
        // 旧形式（値だけの文字列）→ 新しい型として読める
        let legacy = serde_json::json!({
            "accounts.dmm.co.jp": { "acct": "1" },
            "www.dmm.co.jp": { "login_id": "abc" },
        });
        let loaded: HostScopedCookies = serde_json::from_value(legacy.clone()).unwrap();
        assert_eq!(loaded, bag());
        // 読み直したものは値が保たれ、属性は空（host-only / Path なし / 非 Secure / 期限なし）
        assert_eq!(loaded.value("www.dmm.co.jp", "login_id"), Some("abc"));

        // 書き出しは属性つき（読み直しても同じ）
        let json = serde_json::to_value(bag()).unwrap();
        let back: HostScopedCookies = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back, bag());
        assert_eq!(
            json["www.dmm.co.jp"]["login_id"]["value"],
            serde_json::json!("abc")
        );

        // 属性つきは属性ごと往復する
        let with_attrs = HostScopedCookies::from_origin_entries(
            "booth.pm",
            BTreeMap::from([(
                "_plaza_session".to_string(),
                CookieEntry::with_attributes(
                    "x".into(),
                    Some(".booth.pm".into()),
                    Some("/".into()),
                    true,
                    Some(1_800_000_000),
                ),
            )]),
        );
        let json = serde_json::to_value(&with_attrs).unwrap();
        let back: HostScopedCookies = serde_json::from_value(json).unwrap();
        assert_eq!(back, with_attrs);
    }
}
