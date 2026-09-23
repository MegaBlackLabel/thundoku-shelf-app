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

use gpui_kit::AppContext as _;
use gpui_kit::gpui::{App, Context, Entity, Window};
use gpui_wry::WebView;

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

/// 借用を切ってタスクへ渡せるウィンドウハンドル（Windows のみ）。
///
/// `raw_window_handle::WindowHandle<'_>` は `Window` を借用するため、`spawn_in` のタスクへ
/// 持っていけない。WebView2 の生成は**メッセージループを回す**ので、**App の借用外**（タスク）
/// で行う必要があり、そのために HWND の値だけを取り出して持ち運ぶ（`isize` なので `Send` も満たす）。
#[cfg(windows)]
#[derive(Clone, Copy)]
pub(crate) struct OwnedHwnd(isize);

#[cfg(windows)]
impl OwnedHwnd {
    /// ウィンドウから HWND を取り出す（Windows 以外のハンドル種別では `None`）。
    pub(crate) fn new(window: &Window) -> Option<Self> {
        use raw_window_handle::{HasWindowHandle, RawWindowHandle};
        // `Window` には同名の固有メソッド（gpui の `AnyWindowHandle` を返す）があるため、
        // 生のハンドルを返すトレイトメソッドを明示的に選ぶ。
        match HasWindowHandle::window_handle(window).ok()?.as_raw() {
            RawWindowHandle::Win32(handle) => Some(Self(handle.hwnd.get())),
            _ => None,
        }
    }
}

#[cfg(windows)]
impl raw_window_handle::HasWindowHandle for OwnedHwnd {
    fn window_handle(
        &self,
    ) -> Result<raw_window_handle::WindowHandle<'_>, raw_window_handle::HandleError> {
        let hwnd = std::num::NonZero::new(self.0).ok_or(raw_window_handle::HandleError::Unavailable)?;
        Ok(unsafe {
            raw_window_handle::WindowHandle::borrow_raw(
                raw_window_handle::RawWindowHandle::Win32(
                    raw_window_handle::Win32WindowHandle::new(hwnd),
                ),
            )
        })
    }
}

/// ログイン用 WebView を作り、生成できたら `on_ready` でビューへ渡す。
///
/// **Windows では生成をタスク（= App の借用外）で行う**: `WebViewBuilder::build` は内部で
/// `webview2_com::wait_with_pump` を呼んで**メッセージループを回す**ため、その間に走った
/// 他タスクの `update` が gpui の借用と衝突する（`RefCell already borrowed`。実測: ログイン
/// モーダルを開いた瞬間に 58 行）。entity の構築中（`Context::new` の中）は App を借用して
/// いるので、生成だけをタスクへ出す（ハンドルは [`OwnedHwnd`] で持ち運ぶ）。Windows 以外は
/// メッセージループの再入が無いのでその場で作る。
///
/// 生成中に `this.webview` が `None` の間は、各ビューの `check_login` が `false` を返す
/// （= 監視ループは次の tick に回す）ため、順序の入れ替わりは起きない。
///
/// `on_ready` には**ウィンドウも渡す**。WebView の配置（`set_bounds`）は各ビューの
/// `render` で行われるため、生成完了後に再描画を促さないと**配置されないまま表示**される。
/// 呼び出し側で bounds を当てて `cx.notify()` する。
pub(crate) fn create_login_webview<T: 'static>(
    // Windows では生成をタスクへ出すため、ここでは使わない（`update_in` 側で受け取る）。
    #[allow(unused_variables)] this: &mut T,
    window: &mut Window,
    cx: &mut Context<T>,
    incognito: bool,
    initial_url: &'static str,
    on_ready: impl FnOnce(&mut T, Entity<WebView>, &mut Window, &mut Context<T>) + 'static,
) {
    let build = move || {
        let builder = lb_wry::WebViewBuilder::new().with_incognito(incognito);
        #[cfg(debug_assertions)]
        let builder = builder.with_devtools(true);
        builder
    };

    #[cfg(windows)]
    {
        let Some(hwnd) = OwnedHwnd::new(window) else {
            log::error!("webview: ウィンドウハンドルを取得できません");
            return;
        };
        cx.spawn_in(window, async move |weak, cx| {
            // ここは App の借用を持たないので、メッセージループを回しても衝突しない。
            let webview = match build().build(&hwnd) {
                Ok(webview) => webview,
                Err(error) => {
                    log::error!("webview: build failed: {error:?} | {error}");
                    return;
                }
            };
            let _ = weak.update_in(cx, |this, window, cx| {
                let entity = attach_webview(webview, initial_url, window, cx);
                on_ready(this, entity, window, cx);
            });
        })
        .detach();
    }

    #[cfg(not(windows))]
    {
        // `Window` には同名の固有メソッド（gpui の `AnyWindowHandle` を返す）があるため、
        // 生のハンドルを返すトレイトメソッドを明示的に選ぶ。
        use raw_window_handle::HasWindowHandle;
        let built = HasWindowHandle::window_handle(window)
            .ok()
            .and_then(|handle| build().build(&handle).ok());
        match built {
            Some(webview) => {
                let entity = attach_webview(webview, initial_url, window, cx);
                on_ready(this, entity, window, cx);
            }
            None => log::error!("webview: build failed"),
        }
    }
}

/// 生成済み WebView を gpui の entity にして、初期 URL を開いて隠す。
fn attach_webview(
    webview: lb_wry::WebView,
    initial_url: &str,
    window: &mut Window,
    cx: &mut App,
) -> Entity<WebView> {
    let entity = cx.new(|cx| WebView::new(webview, window, cx));
    entity.update(cx, |view, _| {
        view.load_url(initial_url);
        view.hide();
    });
    entity
}

/// WebView（wry）の Cookie を、属性つきの保存用 Cookie へ変換する。
///
/// `Domain` は WebView2 が host-only を `www.dlsite.com`、domain 指定を `.dlsite.com` の形で
/// 返す（そのまま保つ。送信先の判定は `HostScopedCookies::header_for_url` が行う）。
/// 期限は Unix 秒へ落とす（セッション Cookie は `None`）。
pub(crate) fn cookie_entry(
    cookie: &lb_wry::cookie::Cookie<'_>,
) -> thundoku_core::session_cookies::CookieEntry {
    let expires = match cookie.expires() {
        Some(lb_wry::cookie::Expiration::DateTime(datetime)) => Some(datetime.unix_timestamp()),
        // `Session` / 期限なし = セッション Cookie
        _ => None,
    };
    thundoku_core::session_cookies::CookieEntry::with_attributes(
        cookie.value().to_string(),
        cookie.domain().map(str::to_string),
        cookie.path().map(str::to_string),
        // 属性が無い（他プラットフォームの実装）ときは Secure 扱いにする（fail-closed。
        // 送信先は全て https なので、実際の挙動は変わらない）
        cookie.secure().unwrap_or(true),
        expires,
    )
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
