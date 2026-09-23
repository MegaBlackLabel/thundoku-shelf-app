//! 収集元ホスト別の Cookie バッグ（ストアのセッション Cookie の置き場）。
//!
//! 収集元（`www.dmm.co.jp` と `accounts.dmm.co.jp`、`www.dlsite.com` と
//! `login.dlsite.com`）を 1 つに潰すと、片方にしか送るべきでない Cookie が
//! もう片方や**別システム（ダウンロード CDN）**へ飛ぶ。宛先ホストごとに
//! 取り出せるようにホスト別で保つ。
//!
//! シリアライズは「ホスト →（Cookie 名 → 値）」の素のマップ
//! （`#[serde(transparent)]`）。保存済みセッションの形をこの型に依存させない。

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

/// 収集元ホスト →（Cookie 名 → 値）。
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(transparent)]
pub struct HostScopedCookies {
    origins: BTreeMap<String, BTreeMap<String, String>>,
}

impl HostScopedCookies {
    pub fn new(origins: BTreeMap<String, BTreeMap<String, String>>) -> Self {
        Self { origins }
    }

    /// 1 つの収集元（ログインで Cookie を拾ったホスト）だけを持つ。
    pub fn from_origin(host: &str, cookies: BTreeMap<String, String>) -> Self {
        Self {
            origins: BTreeMap::from([(host.to_string(), cookies)]),
        }
    }

    /// 宛先ホスト向けの `Cookie` ヘッダ。**そのホスト向けに収集したものだけ**を返す
    /// （収集元と一致するか、その収集元の子ドメイン）。収集元でなければ空。
    ///
    /// 収集元すべてを 1 本にまとめる API は**持たない**: まとめると、片方にしか送るべきでない
    /// Cookie（`accounts.dmm.co.jp` / `login.dlsite.com` のもの）が、宛先が不定の
    /// ダウンロード proxy や CDN へ飛ぶ。宛先が既知なら必ずこの関数を使う。
    pub fn header_for(&self, host: &str) -> String {
        join(
            self.origins
                .iter()
                .filter(|(origin, _)| is_origin_of(origin, host))
                .flat_map(|(_, cookies)| cookies.iter()),
        )
    }

    /// 収集元ホストの Cookie 値（認証済み判定・ログ用）。収集元をまたがない。
    pub fn value(&self, host: &str, name: &str) -> Option<&str> {
        self.origins.get(host)?.get(name).map(String::as_str)
    }

    pub fn count(&self) -> usize {
        self.origins.values().map(BTreeMap::len).sum()
    }

    pub fn is_empty(&self) -> bool {
        self.count() == 0
    }
}

/// `host` が収集元 `origin` そのもの、またはその子ドメインか。
///
/// 判定は `download_url::host_within` と**同じ実装**を使う（大小文字の扱いが
/// URL 検証側と食い違うと、検証は通るのに Cookie が空になって黙って未認証になる）。
fn is_origin_of(origin: &str, host: &str) -> bool {
    crate::download_url::host_within(host, origin)
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

    fn bag() -> HostScopedCookies {
        HostScopedCookies::new(BTreeMap::from([
            (
                "www.dmm.co.jp".to_string(),
                BTreeMap::from([("login_id".to_string(), "abc".to_string())]),
            ),
            (
                "accounts.dmm.co.jp".to_string(),
                BTreeMap::from([("acct".to_string(), "1".to_string())]),
            ),
        ]))
    }

    /// 宛先が収集元でなければ 1 つも送らない（CDN がこれに当たる）。
    /// 収集元そのもの / その子ドメインには送る。
    #[test]
    fn header_for_is_scoped_to_the_destination_host() {
        let cookies = bag();

        assert_eq!(cookies.header_for("doujin.contents.doujin.dmm.co.jp"), "");
        assert_eq!(cookies.header_for("download.dlsite.com"), "");
        assert_eq!(cookies.header_for("evil.example.com"), "");

        assert_eq!(cookies.header_for("www.dmm.co.jp"), "login_id=abc");
        assert_eq!(cookies.header_for("sub.www.dmm.co.jp"), "login_id=abc");
        assert_eq!(cookies.header_for("accounts.dmm.co.jp"), "acct=1");
    }

    /// 宛先ホストの大文字小文字は区別しない（URL 検証側 `download_url::host_matches` と揃える）。
    /// 区別すると、`Location` が `HTTPS://WWW.DLsite.COM/...` のような URL を返したときに
    /// 検証は通るのに Cookie が空になる（＝黙って未認証になる）。
    #[test]
    fn header_for_ignores_host_case() {
        let cookies = bag();

        assert_eq!(cookies.header_for("WWW.DMM.CO.JP"), "login_id=abc");
        assert_eq!(cookies.header_for("Sub.WWW.dmm.CO.jp"), "login_id=abc");
        assert_eq!(cookies.header_for("ACCOUNTS.DMM.CO.JP"), "acct=1");
        assert_eq!(cookies.header_for("DOUJIN.CONTENTS.DOUJIN.DMM.CO.JP"), "");
    }

    /// 値の取り出しは収集元をまたがない。
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

    /// 保存済みセッションの形は「ホスト → Cookie」の素のマップのまま
    /// （この型を挟んでも保存済みの値が読めなくならないこと）。
    #[test]
    fn serializes_as_a_plain_host_map() {
        let json = serde_json::to_value(bag()).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "accounts.dmm.co.jp": { "acct": "1" },
                "www.dmm.co.jp": { "login_id": "abc" },
            })
        );
        let back: HostScopedCookies = serde_json::from_value(json).unwrap();
        assert_eq!(back, bag());
    }
}
