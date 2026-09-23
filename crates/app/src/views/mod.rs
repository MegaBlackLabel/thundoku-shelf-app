pub mod about;
pub mod auth;
pub mod bookshelf;
pub mod booth_login;
pub mod checklist;
pub mod dlsite_login;
pub mod fanza_login;
pub mod github_login;
pub mod google_login;
pub mod history;
pub mod licenses;
pub mod notes;
pub mod reader;
pub mod report;
pub mod settings;
pub mod tag_edit;
pub mod tbf_login;

/// ホバー中の背景色。
///
/// テーマの `muted` / `secondary` はダークで同色（どちらも 15%）なので、既に
/// `muted` を背景にしている要素ではホバーしても色が変わらない。背景と区別できる
/// よう、ダークは明るめ・ライトは濃いめの明示グレーを返す。
pub(crate) fn hover_bg(theme: &gpui_kit::component::Theme) -> gpui_kit::Rgba {
    match theme.mode {
        gpui_kit::component::ThemeMode::Dark => gpui_kit::rgb(0x3e3e3e),
        gpui_kit::component::ThemeMode::Light => gpui_kit::rgb(0xcfcfcf),
    }
}

/// URL が `domain` そのもの、またはそのサブドメインを指しているか。
///
/// WebView のログイン完了判定（目的のサイトへ戻ったか）に使う。`ends_with("booth.pm")` は
/// `evilbooth.pm` も通してしまうため、ラベル境界を見る `download_url::host_within` に委譲する。
/// 認証情報の送信先の検証は `download_url::check` の許可リストが別途行う（こちらは判定だけ）。
pub(crate) fn url_is_on_host(url: &str, domain: &str) -> bool {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| {
            parsed
                .host_str()
                .map(|host| thundoku_core::download_url::host_within(host, domain))
        })
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// ログイン完了判定はラベル境界を見る（`evilbooth.pm` / `eviltechbookfest.org` を通さない）。
    #[test]
    fn url_is_on_host_requires_a_label_boundary() {
        for url in [
            "https://booth.pm/ja",
            "https://accounts.booth.pm/library",
            "https://BOOTH.PM/ja",
        ] {
            assert!(url_is_on_host(url, "booth.pm"), "{url}");
        }
        for url in [
            "https://evilbooth.pm/ja",
            "https://booth.pm.evil.example.com/ja",
            "https://example.com/?next=https://booth.pm/",
            "not a url",
        ] {
            assert!(!url_is_on_host(url, "booth.pm"), "{url}");
        }

        assert!(url_is_on_host(
            "https://techbookfest.org/user/signin",
            "techbookfest.org"
        ));
        assert!(!url_is_on_host(
            "https://eviltechbookfest.org/user/signin",
            "techbookfest.org"
        ));
    }
}
