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
    /// Cookie 名。`from_origin` / `from_origin_entries` は名前をキーから埋める
    /// （保存形式では各 Cookie が名前を持つ）。
    #[serde(default)]
    pub name: String,
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
    /// 値だけの Cookie（host-only / `Path` 指定なし / 非 Secure / 期限なし）。名前は収納時に付く。
    pub fn new(value: impl Into<String>) -> Self {
        Self {
            name: String::new(),
            value: value.into(),
            domain: None,
            path: None,
            secure: false,
            expires: None,
        }
    }

    /// WebView が返す属性から作る（core は cookie クレートに依存しないよう素の値で受ける）。
    /// 名前は [`Self::named`] で付ける。
    pub fn with_attributes(
        value: String,
        domain: Option<String>,
        path: Option<String>,
        secure: bool,
        expires: Option<i64>,
    ) -> Self {
        Self {
            name: String::new(),
            value,
            domain,
            path,
            secure,
            expires,
        }
    }

    /// 名前を付ける（WebView が返す `name` / 収納先のキー）。
    pub fn named(mut self, name: impl Into<String>) -> Self {
        self.name = name.into();
        self
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
                #[serde(default)]
                name: String,
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
                name,
                value,
                domain,
                path,
                secure,
                expires,
            } => Self::with_attributes(value, domain, path, secure, expires).named(name),
        })
    }
}

/// 収集元ホスト → Cookie の並び。
///
/// **名前だけで引かない**: `foo=root; Path=/` と `foo=dc; Path=/dc/` のように同名でも
/// `Path`（や `Domain`）が違う Cookie は別物として両方保つ。送信時に宛先で選ぶ。
/// 保存形式は「ホスト → Cookie の配列」で、各 Cookie が名前を持つ。名前をキーにした
/// 旧形式（`{"host":{"name":"value"}}` / 名前キー + 属性）も読める（再ログインを強いない）。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HostScopedCookies {
    origins: BTreeMap<String, Vec<CookieEntry>>,
}

/// 保存形式の読み取り（現行 = 配列、旧 = 名前キーのオブジェクト）。
impl<'de> Deserialize<'de> for HostScopedCookies {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Raw {
            /// 旧形式: ホスト →（Cookie 名 → 値 or 属性つき）。
            Named(BTreeMap<String, BTreeMap<String, CookieEntry>>),
            /// 現行: ホスト → Cookie の配列。
            Listed(BTreeMap<String, Vec<CookieEntry>>),
        }

        Ok(match Raw::deserialize(deserializer)? {
            Raw::Named(origins) => Self::from_named(origins),
            Raw::Listed(origins) => Self::new(origins),
        })
    }
}

/// 保存形式の書き出し（ホスト → Cookie の配列）。
impl Serialize for HostScopedCookies {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.origins.serialize(serializer)
    }
}

impl HostScopedCookies {
    /// 収集した形（ホスト → Cookie の並び）から作る。
    pub fn new(origins: BTreeMap<String, Vec<CookieEntry>>) -> Self {
        Self { origins }
    }

    /// 名前をキーにした形（旧形式の保存データ / 呼び出し側の便宜）から作る。
    /// キーの名前を各 Cookie へ移す。
    pub fn from_named(origins: BTreeMap<String, BTreeMap<String, CookieEntry>>) -> Self {
        Self::new(
            origins
                .into_iter()
                .map(|(host, cookies)| {
                    let cookies = cookies
                        .into_iter()
                        .map(|(name, cookie)| cookie.named(name))
                        .collect();
                    (host, cookies)
                })
                .collect(),
        )
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

    /// 1 つの収集元だけを持つ（キーの Cookie 名を各 Cookie へ移す）。
    pub fn from_origin_entries(
        host: &str,
        cookies: BTreeMap<String, CookieEntry>,
    ) -> Self {
        Self {
            origins: BTreeMap::from([(
                host.to_string(),
                cookies
                    .into_iter()
                    .map(|(name, cookie)| cookie.named(name))
                    .collect(),
            )]),
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
    /// 並びはブラウザと同じ「長い `Path` が先、同じなら名前順」（同名で別 `Path` の Cookie を
    /// 取り違えないようにする）。URL を解釈できないときは空（fail-closed）。
    pub fn header_for_url(&self, url: &str) -> String {
        let Some((host, path, https)) = split_url(url) else {
            return String::new();
        };
        let now = now_unix();
        let mut matched: Vec<(&CookieEntry, usize)> = self
            .origins
            .iter()
            .filter(|(origin, _)| origin.eq_ignore_ascii_case(host))
            .flat_map(|(_, cookies)| cookies.iter())
            .filter(|cookie| cookie.applies_to(host, path, https, now))
            .map(|cookie| (cookie, cookie.path.as_deref().unwrap_or("/").len()))
            .collect();
        // 長い Path を先に、同じなら名前順（安定した並び）
        matched.sort_by(|a, b| {
            b.1.cmp(&a.1)
                .then_with(|| a.0.name.cmp(&b.0.name))
                .then_with(|| a.0.path.cmp(&b.0.path))
        });
        join(matched.into_iter().map(|(cookie, _)| (&cookie.name, &cookie.value)))
    }

    /// 収集元ホストの Cookie 値（認証済み判定・ログ用）。収集元をまたがない。
    /// 同名が複数あるときは最初の 1 つ（並びは保存順）。
    pub fn value(&self, host: &str, name: &str) -> Option<&str> {
        self.origins
            .get(host)?
            .iter()
            .find(|cookie| cookie.name == name)
            .map(|cookie| cookie.value.as_str())
    }

    pub fn count(&self) -> usize {
        self.origins.values().map(Vec::len).sum()
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
        HostScopedCookies::from_named(BTreeMap::from([
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
            json["www.dmm.co.jp"][0]["name"],
            serde_json::json!("login_id")
        );
        assert_eq!(
            json["www.dmm.co.jp"][0]["value"],
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
        // 保存済みセッション（属性つき・名前キー）の実物の形を読み続ける
        // （`DlsiteSession` の保存 JSON。再ログインを強いない）。
        let stored = serde_json::json!({
            "origins": {
                "www.dlsite.com": {
                    "__DLsite_SID": {
                        "value": "sid",
                        "domain": "dlsite.com",
                        "path": "/",
                        "secure": true,
                        "expires": null
                    }
                }
            }
        });
        let loaded: crate::dlsite::client::DlsiteSession =
            serde_json::from_value(stored).unwrap();
        assert_eq!(
            loaded.cookie_header_for_url("https://www.dlsite.com/maniax/mypage/userbuy/"),
            "__DLsite_SID=sid"
        );
    }

    /// 同名でも `Path` が違う Cookie は**両方**保持し、宛先のパスで選ぶ。
    ///
    /// 名前だけをキーにすると `foo=root; Path=/` と `foo=dc; Path=/dc/` の片方しか残らず、
    /// 必要なパスへ送るべき値が失われる。RFC 6265 の path-match では `/dc/x` に両方が
    /// 一致するので、長い `Path` を先に送る（ブラウザと同じ順）。
    #[test]
    fn same_name_cookies_with_different_paths_are_kept_and_selected_by_path() {
        let cookies = HostScopedCookies::new(BTreeMap::from([(
            "www.example.com".to_string(),
            vec![
                CookieEntry::with_attributes("root".into(), None, Some("/".into()), true, None)
                    .named("foo"),
                CookieEntry::with_attributes("dc".into(), None, Some("/dc/".into()), true, None)
                    .named("foo"),
                CookieEntry::with_attributes("b".into(), None, Some("/dc/".into()), true, None)
                    .named("bar"),
            ],
        )]));

        // `/dc/` に一致するものだけ（長い Path が先）
        assert_eq!(
            cookies.header_for_url("https://www.example.com/dc/x"),
            "bar=b; foo=dc; foo=root"
        );
        // ルートにしか一致しない
        assert_eq!(
            cookies.header_for_url("https://www.example.com/other"),
            "foo=root"
        );
        assert_eq!(cookies.count(), 3, "同名でも Path が違えば別に数える");
        // 値の取り出しは名前で引く（同名なら保存順の最初）
        assert_eq!(cookies.value("www.example.com", "foo"), Some("root"));
    }

    /// 同名・別 `Path` の Cookie も書き出し → 読み直しで保たれる（保存形式の往復）。
    #[test]
    fn same_name_cookies_round_trip_through_the_saved_shape() {
        let cookies = HostScopedCookies::new(BTreeMap::from([(
            "www.example.com".to_string(),
            vec![
                CookieEntry::with_attributes("root".into(), None, Some("/".into()), true, None)
                    .named("foo"),
                CookieEntry::with_attributes("dc".into(), None, Some("/dc/".into()), true, None)
                    .named("foo"),
            ],
        )]));
        let json = serde_json::to_value(&cookies).unwrap();
        let back: HostScopedCookies = serde_json::from_value(json).unwrap();
        assert_eq!(back, cookies);
        assert_eq!(back.count(), 2);
    }
}
