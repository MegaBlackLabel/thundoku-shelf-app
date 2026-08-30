//! 技術書典（techbookfest.org）のログインをアプリ内 WebView（gpui-wry）で行うビュー。
//!
//! 技術書典のログインページを WebView で開き、ユーザーが WebView 内で
//! メールアドレス・パスワードでログインしてログイン後のページに遷移したら、
//! セッション Cookie を自動取得して永続化する（BOOTH と同じ方式）。

use gpui::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement, Render, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_component::{Icon, IconName};
use gpui_wry::WebView;
use raw_window_handle::HasWindowHandle;
use thundoku_core::tbf::TbfSession;

/// ログイン完了イベント（セッション Cookie を取得して永続化した後に発行）。
pub struct TbfLoginDone;

/// ログインキャンセルイベント（閉じるボタンでモーダルを閉じたときに発行）。
pub struct TbfLoginCancelled;

pub struct TbfLoginView {
    webview: Option<Entity<WebView>>,
    /// URL 監視タイマーの世代（重複チェック防止）
    check_generation: u64,
}

impl TbfLoginView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let webview = Self::try_create_webview(window, cx);
        if let Some(webview) = &webview {
            webview.update(cx, |view, _| {
                view.load_url("https://techbookfest.org/user/signin");
                // モーダルを開くまでは隠しておく（AuthDialog の show で表示）
                view.hide();
            });
        }
        let mut this = Self {
            webview,
            check_generation: 0,
        };
        this.start_url_check(cx);
        this
    }

    fn try_create_webview(window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<WebView>> {
        let builder = lb_wry::WebViewBuilder::new();
        #[cfg(debug_assertions)]
        let builder = builder.with_devtools(true);
        let window_handle = window.window_handle().ok()?;
        let webview = builder.build(&window_handle).ok()?;
        let entity = cx.new(|cx| WebView::new(webview, window, cx));
        entity.update(cx, |view, _| view.hide());
        Some(entity)
    }

    /// WebView を表示する（ログインモーダルを開く）。
    pub fn show(&mut self, cx: &mut Context<Self>) {
        if let Some(webview) = &self.webview {
            webview.update(cx, |view, _| view.show());
        }
        self.check_generation += 1;
        self.start_url_check(cx);
    }

    /// キャンセル/閉じる。
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.check_generation += 1; // 監視を止める
        if let Some(webview) = self.webview.take() {
            webview.update(cx, |view, _| view.hide());
        }
    }

    /// 1 秒ごとに WebView の URL を確認し、ログイン後のページに遷移したら
    /// セッション Cookie を取得して保存・通知する。
    fn start_url_check(&mut self, cx: &mut Context<Self>) {
        self.check_generation += 1;
        let generation = self.check_generation;
        let handle = cx.entity();
        cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                let done = handle.update(cx, |this, cx| {
                    if this.check_generation != generation {
                        return true; // 新しい監視が始まっている
                    }
                    this.check_login(cx)
                });
                if done {
                    break;
                }
            }
        })
        .detach();
    }

    /// 現在の URL がログイン後ページに遷移していたら Cookie を取得して保存する。
    /// ログイン完了で true を返す。
    fn check_login(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(webview) = self.webview.as_ref() else {
            // WebView なし（テスト環境）ではログインを試みない
            return false;
        };
        let Ok(url) = webview.read(cx).raw().url() else {
            return false;
        };
        let is_tbf = url
            .parse::<url::Url>()
            .map(|u| {
                u.host_str()
                    .is_some_and(|h| h.ends_with("techbookfest.org"))
            })
            .unwrap_or(false);
        // ログインページ自体から遷移していない間は待つ。
        let on_login_page = url.contains("/user/signin");
        if !is_tbf || on_login_page {
            return false;
        }
        // セッション Cookie（XSRF-TOKEN 以外）が入っていれば成功
        let cookies = webview
            .read(cx)
            .raw()
            .cookies_for_url("https://techbookfest.org")
            .unwrap_or_default();
        let session = TbfSession::from_cookies(
            cookies
                .iter()
                .map(|cookie| (cookie.name().to_string(), cookie.value().to_string()))
                .collect(),
        );
        if !session.is_logged_in() {
            return false;
        }
        log::info!("tbf login: {} cookies captured", session.cookies.len());
        crate::app_state::save_tbf_session(cx, &session);
        // WebView を隠して完了を通知する
        if let Some(webview) = &self.webview {
            webview.update(cx, |view, _| view.hide());
        }
        cx.emit(TbfLoginDone);
        true
    }
}

impl EventEmitter<TbfLoginDone> for TbfLoginView {}
impl EventEmitter<TbfLoginCancelled> for TbfLoginView {}

impl Render for TbfLoginView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // モーダル領域（中央 480x640）に WebView を配置する
        let bounds = {
            let window_bounds = window.bounds();
            let width = 480.0_f32;
            let height = 640.0_f32;
            let left = (window_bounds.size.width.as_f32() - width) / 2.0;
            let top = (window_bounds.size.height.as_f32() - height) / 2.0;
            gpui::bounds(
                gpui::Point {
                    x: px(left),
                    y: px(top),
                },
                gpui::Size {
                    width: px(width),
                    height: px(height),
                },
            )
        };
        if let Some(webview) = &self.webview {
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
        div()
            .id("tbf-login-backdrop")
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45))
            // 閉じるボタンはウィンドウ右上（WebView 領域の外側）に配置
            .child(
                div()
                    .id("tbf-login-cancel")
                    .absolute()
                    .top_3()
                    .right_3()
                    .w(px(36.0))
                    .h(px(36.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .bg(gpui::rgba(0xffffff26))
                    .text_color(gpui::white())
                    .hover(|style| style.bg(gpui::rgba(0xffffff40)))
                    .cursor_pointer()
                    .on_click({
                        let handle = cx.weak_entity();
                        move |_, _window, cx| {
                            if let Some(handle) = handle.upgrade() {
                                handle.update(cx, |this, cx| {
                                    this.close(cx);
                                    cx.emit(TbfLoginCancelled);
                                });
                            }
                        }
                    })
                    .child(Icon::new(IconName::Close).size(px(18.0))),
            )
    }
}
