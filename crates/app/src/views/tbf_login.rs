//! 技術書典（techbookfest.org）のログインをアプリ内 WebView（gpui-wry）で行うビュー。
//!
//! 技術書典のログインページを WebView で開き、ユーザーが WebView 内で
//! メールアドレス・パスワードでログインしてログイン後のページに遷移したら、
//! セッション Cookie を自動取得して永続化する（BOOTH と同じ方式）。

use gpui_kit::component::{Icon, IconName};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement, Render, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
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
        let window_handle = match window.window_handle() {
            Ok(h) => h,
            Err(e) => {
                log::error!("tbf login: window_handle() failed: {e:?}");
                return None;
            }
        };
        let webview = match builder.build(&window_handle) {
            Ok(w) => w,
            Err(e) => {
                log::error!("tbf login: wry build() failed: {e:?} | {e}");
                return None;
            }
        };
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
        // 強参照を持つと、ログイン画面を閉じてもビュー（と WebView）が解放されない。
        // 弱参照にし、ビューが drop されたら update が Err を返すのでタスクを終了する。
        let handle = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(1))
                    .await;
                let Ok(done) = handle.update(cx, |this, cx| {
                    if this.check_generation != generation {
                        return true; // 新しい監視が始まっている
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
        let window_bounds = window.bounds();
        let window_w = window_bounds.size.width.as_f32();
        let window_h = window_bounds.size.height.as_f32();
        let width = 480.0_f32;
        let height = 640.0_f32;
        let left = (window_w - width) / 2.0;
        let top = (window_h - height) / 2.0;
        let bounds = gpui_kit::bounds(
            gpui_kit::Point {
                x: px(left),
                y: px(top),
            },
            gpui_kit::Size {
                width: px(width),
                height: px(height),
            },
        );
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
        // 閉じるボタンは WebView の右上・すぐ外側に置く。WebView（ネイティブ子ウィンドウ）
        // は GPUI 要素より常に最前面に描画され、ボタンを領域内に置くと隠れるため、
        // 描画領域の外・直近（右外側、右に収まらない場合は左外側）へ配置する。
        let close_size = 36.0_f32;
        let close_gap = 10.0_f32;
        let mut close_left = left + width + close_gap;
        let close_top = top + close_gap;
        if close_left + close_size > window_w {
            close_left = left - close_gap - close_size;
        }
        div()
            .id("tbf-login-backdrop")
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
            .bg(gpui_kit::hsla(0.0, 0.0, 0.0, 0.45))
            .child(
                div()
                    .id("tbf-login-cancel")
                    .absolute()
                    .left(px(close_left))
                    .top(px(close_top))
                    .w(px(close_size))
                    .h(px(close_size))
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
                                    cx.emit(TbfLoginCancelled);
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
                width: gpui_kit::px(480.0),
                height: gpui_kit::px(640.0),
            },
            |_, _| TestRoot,
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        let weak = visual.update(|window, cx| {
            let view = cx.new(|cx| TbfLoginView::new(window, cx));
            let weak = view.downgrade();
            drop(view); // モーダルを閉じた状態（強参照なし）
            weak
        });
        cx.run_until_parked();
        assert!(
            weak.upgrade().is_none(),
            "TbfLoginView が解放されていない（URL 監視タスクのリーク）"
        );
    }
}
