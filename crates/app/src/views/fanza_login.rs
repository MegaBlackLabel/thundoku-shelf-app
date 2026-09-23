//! FANZA同人（www.dmm.co.jp/dc/doujin）のログインをアプリ内 WebView（gpui-wry）で行うビュー。
//!
//! FANZA の購入済み作品ページ（mylibrary）を WebView で開く。ユーザーが年齢確認（はい）→
//! accounts.dmm.co.jp のパスワードログインを WebView 内で完了して www.dmm.co.jp に戻ったら、
//! www.dmm.co.jp / accounts.dmm.co.jp のセッション Cookie を自動取得して永続化する。

use gpui_kit::component::{Icon, IconName};
use gpui_kit::{
    Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement, Render, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_wry::WebView;
use thundoku_core::session_cookies::CookieEntry;

/// ログイン完了イベント（Cookie を取得して永続化した後に発行）。
pub struct FanzaLoginDone;

/// ログインキャンセルイベント（閉じるボタンでモーダルを閉じたときに発行）。
pub struct FanzaLoginCancelled;

/// セッション Cookie を集める起点（ストアとアカウント）。
const SESSION_ORIGINS: [&str; 2] = ["https://www.dmm.co.jp", "https://accounts.dmm.co.jp"];

pub struct FanzaLoginView {
    webview: Option<Entity<WebView>>,
    check_generation: u64,
    /// WebView を表示したいか。生成が非同期（Windows はタスク）なので、生成完了時に反映する。
    visible: bool,
}

impl FanzaLoginView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            webview: None,
            check_generation: 0,
            visible: false,
        };
        // 生成は App の借用外（Windows はタスク）で行われる。理由は
        // `super::create_login_webview` のドキュメント参照。
        super::create_login_webview(
            &mut this,
            window,
            cx,
            // incognito（non-persistent）WebView: Cookie はメモリのみ。
            // 永続ストアだと再ログイン時に SSO で自動再ログインされるため使わない。
            true,
            // 購入済み作品ページを起点にする。年齢確認 → accounts ログイン → 復帰で
            // セッションが確立する。
            "https://www.dmm.co.jp/dc/-/mylibrary/",
            |this, webview, window, cx| {
                this.webview = Some(webview);
                this.apply_webview_bounds(window, cx);
                // 生成前に show() されていたら、その意図をここで反映する。
                if this.visible
                    && let Some(webview) = &this.webview
                {
                    webview.update(cx, |view, _| view.show());
                }
                // 生成完了を 1 回描画へ反映する（配置と可視化のため）。
                cx.notify();
            },
        );
        this.start_url_check(cx);
        this
    }

    fn start_url_check(&mut self, cx: &mut Context<Self>) {
        self.check_generation += 1;
        let generation = self.check_generation;
        // 強参照を持つと、ログイン画面を閉じてもビュー（と WebView）が解放されない。
        // 弱参照にし、ビューが drop されたら update が Err を返すのでタスクを終了する。
        let handle = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                // WebView2 がメッセージループを回している間は待つ（借用が衝突するため）。
                crate::app_state::wait_while_webview_pumping(cx.background_executor()).await;
                let Ok(done) = handle.update(cx, |this, cx| {
                    if this.check_generation != generation {
                        return true;
                    }
                    this.check_login(cx)
                }) else {
                    // Err = ビューが drop された（監視する相手がいない）
                    break;
                };
                if done {
                    break;
                }
            }
        })
        .detach();
    }

    /// URL が www.dmm.co.jp に戻り（年齢確認・ログインページを抜けて）、セッション
    /// Cookie が取れたら保存・通知する。完了で true。
    fn check_login(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(webview) = self.webview.as_ref() else {
            return false;
        };
        let Ok(url) = webview.read(cx).raw().url() else {
            return false;
        };
        // URL のクエリ/フラグメントには認可コードやトークンが載り得るため落とす。
        log::debug!(
            "fanza login check: url={}",
            url.split(['?', '#']).next().unwrap_or(&url)
        );
        let host_ok = url
            .parse::<url::Url>()
            .map(|u| u.host_str().is_some_and(|h| h == "www.dmm.co.jp"))
            .unwrap_or(false);
        // 年齢確認（/age_check/）やログイン（accounts）中はまだ完了扱いにしない。
        let on_age_check = url.contains("age_check");
        let on_login = url.contains("accounts.dmm.co.jp") || url.contains("/service/login");
        if !host_ok || on_age_check || on_login {
            return false;
        }
        // Cookie 取得も内部でメッセージループを回す（理由は `crate::app_state::webview_pumping()` のドキュメント参照）。
        let _pumping = crate::app_state::WebviewPumpGuard::enter();
        // www と accounts の Cookie を**収集元ごとに分けて**持つ（1 つに潰すと、片方に
        // しか送るべきでない Cookie がもう片方や CDN へ飛ぶ）。
        let mut origins: std::collections::BTreeMap<
            String,
            std::collections::BTreeMap<String, CookieEntry>,
        > = std::collections::BTreeMap::new();
        for url in SESSION_ORIGINS {
            let cookies = webview
                .read(cx)
                .raw()
                .cookies_for_url(url)
                .unwrap_or_default();
            log::debug!("fanza login: cookies_for_url({url}) -> {}", cookies.len());
            let Ok(parsed) = thundoku_core::download_url::parse(url) else {
                continue;
            };
            let entry = origins.entry(parsed.host.to_string()).or_default();
            for cookie in cookies {
                entry
                    .entry(cookie.name().to_string())
                    .or_insert_with(|| super::cookie_entry(&cookie));
            }
        }
        let session = thundoku_core::fanza::client::FanzaSession::new(origins);
        if !session.logged_in() {
            log::warn!("fanza login: セッション Cookie を取得できませんでした");
            return false;
        }
        log::info!("fanza login: {} cookies captured", session.cookies_count());
        crate::app_state::save_fanza_session(cx, &session);
        if let Some(webview) = &self.webview {
            webview.update(cx, |view, _| view.hide());
        }
        self.visible = false;
        cx.emit(FanzaLoginDone);
        true
    }

    pub fn show(&mut self, cx: &mut Context<Self>) {
        self.visible = true;
        if let Some(webview) = &self.webview {
            webview.update(cx, |view, _| view.show());
        }
        self.check_generation += 1;
        self.start_url_check(cx);
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.check_generation += 1;
        self.visible = false;
        if let Some(webview) = self.webview.take() {
            webview.update(cx, |view, _| view.hide());
        }
    }

    /// WebView をモーダル領域（中央 640x480）へ配置する。
    ///
    /// 生成が非同期（Windows はタスク）なので、生成直後にも呼ぶ。`render` 任せだと
    /// 生成完了後に再描画が走らず、**配置されないまま表示**される。
    fn apply_webview_bounds(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(webview) = &self.webview else {
            return;
        };
        let bounds = {
            let window_bounds = window.bounds();
            let width = 640_f32;
            let height = 480_f32;
            let left = (window_bounds.size.width.as_f32() - width) / 2.0;
            let top = (window_bounds.size.height.as_f32() - height) / 2.0;
            gpui_kit::bounds(
                gpui_kit::Point {
                    x: px(left),
                    y: px(top),
                },
                gpui_kit::Size {
                    width: px(width),
                    height: px(height),
                },
            )
        };
        webview.update(cx, |view, _| {
            let _ = view.raw().set_bounds(lb_wry::Rect {
                position: lb_wry::dpi::Position::Physical(lb_wry::dpi::PhysicalPosition::new(
                    bounds.origin.x.as_f32() as i32,
                    bounds.origin.y.as_f32() as i32,
                )),
                size: lb_wry::dpi::Size::Physical(lb_wry::dpi::PhysicalSize::new(
                    bounds.size.width.as_f32() as u32,
                    bounds.size.height.as_f32() as u32,
                )),
            });
        });
    }
}

impl EventEmitter<FanzaLoginDone> for FanzaLoginView {}
impl EventEmitter<FanzaLoginCancelled> for FanzaLoginView {}

impl Render for FanzaLoginView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.apply_webview_bounds(window, cx);
div()
            .id("fanza-login-backdrop")
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
            .bg(gpui_kit::hsla(0.0, 0.0, 0.0, 0.45))
            .child(
                div()
                    .id("fanza-login-cancel")
                    .absolute()
                    .top_3()
                    .right_3()
                    .w(px(36.0))
                    .h(px(36.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .bg(gpui_kit::rgba(0xffffff26))
                    .text_color(gpui_kit::white())
                    .hover(|style| style.bg(gpui_kit::rgba(0xffffff40)))
                    .cursor_pointer()
                    .on_click({
                        let handle = cx.weak_entity();
                        move |_, _window, cx| {
                            if let Some(handle) = handle.upgrade() {
                                handle.update(cx, |this, cx| {
                                    this.close(cx);
                                    cx.emit(FanzaLoginCancelled);
                                });
                            }
                        }
                    })
                    .child(Icon::new(IconName::Close).size(px(18.0))),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::AppContext as _;
    use gpui_kit::TestAppContext;

    /// テスト用のウィンドウルート（描くものは無い）。
    struct TestRoot;

    impl Render for TestRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    /// ログイン画面を閉じて強参照を全て drop したらビューが解放されること。
    /// 修正前は URL 監視タスクが `Entity` を持ち続け、WebView ごと残っていた。
    #[gpui_kit::test]
    async fn view_is_released_when_its_handles_are_dropped(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        // テストウィンドウには native handle が無いので WebView は作られない。
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(640.0),
                height: gpui_kit::px(480.0),
            },
            |_, _| TestRoot,
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        let weak = visual.update(|window, cx| {
            let view = cx.new(|cx| FanzaLoginView::new(window, cx));
            let weak = view.downgrade();
            drop(view); // モーダルを閉じた状態（強参照なし）
            weak
        });
        cx.run_until_parked();
        assert!(
            weak.upgrade().is_none(),
            "FanzaLoginView が解放されていない（URL 監視タスクのリーク）"
        );
    }
}
