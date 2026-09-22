//! DLsite（www.dlsite.com）のログインをアプリ内 WebView（gpui-wry）で行うビュー。
//!
//! viviON ID のログイン（`login.dlsite.com/login?user=self`）を WebView で開く。ユーザーが
//! ログインを完了して `www.dlsite.com`（ストア）に到達すると、`www.dlsite.com` の
//! セッション Cookie（`__DLsite_SID` / `uhashjp` / `jwt` 等）を自動取得して永続化する。
//! ログイン後も `login.dlsite.com` のまま（初回ガイド等）なら、ストアへ遷移して
//! ストア Cookie の発行を促す。

use std::collections::BTreeMap;

use gpui_kit::component::{Icon, IconName};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement, Render, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_wry::WebView;
use raw_window_handle::HasWindowHandle;
use thundoku_core::dlsite::client::{DlsiteSession, LOGIN_HOST, SITE_HOST};

use crate::app_state::WebviewPumpGuard;

/// セッション Cookie を集める起点（ストアとログイン）。
const SESSION_ORIGINS: [&str; 2] = ["https://www.dlsite.com", "https://login.dlsite.com"];

/// ログイン完了イベント（Cookie を取得して永続化した後に発行）。
pub struct DlsiteLoginDone;

/// ログインキャンセルイベント（閉じるボタンでモーダルを閉じたときに発行）。
pub struct DlsiteLoginCancelled;

pub struct DlsiteLoginView {
    webview: Option<Entity<WebView>>,
    check_generation: u64,
}

impl DlsiteLoginView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let webview = Self::try_create_webview(window, cx);
        let mut this = Self {
            webview,
            check_generation: 0,
        };
        this.start_url_check(cx);
        this
    }

    fn try_create_webview(window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<WebView>> {
        // incognito（non-persistent）WebView: Cookie はメモリのみ。
        // 永続ストアだと再ログイン時に SSO で自動再ログインされるため使わない。
        let builder = lb_wry::WebViewBuilder::new().with_incognito(true);
        #[cfg(debug_assertions)]
        let builder = builder.with_devtools(true);
        let window_handle = match window.window_handle() {
            Ok(h) => h,
            Err(e) => {
                log::error!("dlsite login: window_handle() failed: {e:?}");
                return None;
            }
        };
        // WebView2 の生成は内部でメッセージループを回す（理由は `crate::app_state::webview_pumping()` のドキュメント参照）。
        // その間に他の定期タスクが App を更新すると gpui の借用と衝突して落ちるため、
        // ここでカウンタを立てて知らせる。
        let _pumping = WebviewPumpGuard::enter();
        let webview = match builder.build(&window_handle) {
            Ok(w) => w,
            Err(e) => {
                log::error!("dlsite login: wry build() failed: {e:?} | {e}");
                return None;
            }
        };
        let entity = cx.new(|cx| WebView::new(webview, window, cx));
        entity.update(cx, |view, _| {
            // ストアのログインルートを起点にする。これは viviON のログインフォーム
            // （login.dlsite.com の oauth2 consumer SSO）を最初に表示し、ログイン完了で
            // www.dlsite.com/home/mypage へ戻って認証セッション（__DLsite_SID + uid_jp /
            // uhashjp）を確立する。
            // 直接 login.dlsite.com/login?user=self は consumer SSO を完了せずゲストのまま、
            // www.dlsite.com/ を最初に出すとログインフォームが出ない（手動でログインを
            // クリックが必要）ため、このストアログインルートを使う。
            view.load_url(
                "https://www.dlsite.com/home/login/=/skip_register/1/_query/https://www.dlsite.com/home/mypage",
            );
            view.hide();
        });
        Some(entity)
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
                // WebView2 がメッセージループを回している間は更新しない（次の tick に回す）。
                if crate::app_state::webview_pumping() > 0 {
                    continue;
                }
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

    /// ログインが完了してストア（www.dlsite.com）に到達し、ストアセッション Cookie
    /// （`__DLsite_SID` 等）が取れたら保存・通知する。完了で true。
    fn check_login(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(webview) = self.webview.as_ref() else {
            return false;
        };
        let Ok(url) = webview.read(cx).raw().url() else {
            return false;
        };
        // URL のクエリ/フラグメントには認可コードやトークンが載り得るため落とす。
        log::debug!(
            "dlsite login check: url={}",
            url.split(['?', '#']).next().unwrap_or(&url)
        );
        // www ストア + login の Cookie を**収集元ごとに分けて**持つ（1 つに潰すと、片方に
        // しか送るべきでない Cookie がもう片方や CDN へ飛ぶ）。www の認証セッション
        // （__DLsite_SID に加えて uid_jp / uhashjp）は「ログイン完了時の oauth2
        // コールバック」で確立される。ここでは一切 auto-navigation しない（SSO 連鎖を
        // 中断するとゲスト __DLsite_SID のままになり、同期が regist/user へ 302 されて
        // 0 件になる）。
        // Cookie 取得も内部でメッセージループを回す（理由は `crate::app_state::webview_pumping()` のドキュメント参照）。
        let _pumping = WebviewPumpGuard::enter();
        let mut origins: BTreeMap<String, BTreeMap<String, String>> = BTreeMap::new();
        for url in SESSION_ORIGINS {
            let cookies = webview
                .read(cx)
                .raw()
                .cookies_for_url(url)
                .unwrap_or_default();
            log::debug!("dlsite login: cookies_for_url({url}) -> {}", cookies.len());
            let Ok(parsed) = thundoku_core::download_url::parse(url) else {
                continue;
            };
            let entry = origins.entry(parsed.host.to_string()).or_default();
            for cookie in cookies {
                entry
                    .entry(cookie.name().to_string())
                    .or_insert_with(|| cookie.value().to_string());
            }
        }
        let session = DlsiteSession::new(origins);
        // 認証済み: __DLsite_SID に加えて uid_jp / uhashjp（ログイン中の認証 ID）が揃ったら完了。
        // 未ログインだと www 側はゲスト __DLsite_SID だけが付く（oauth2 コールバック未完了）。
        let has = |name: &str| {
            session.cookie_value(SITE_HOST, name).is_some()
                || session.cookie_value(LOGIN_HOST, name).is_some()
        };
        if !has("__DLsite_SID") || !(has("uhashjp") || has("uid_jp")) {
            log::debug!(
                "dlsite login: 未認証（__DLsite_SID={}, uhashjp={}, uid_jp={}）",
                has("__DLsite_SID"),
                has("uhashjp"),
                has("uid_jp")
            );
            return false;
        }
        log::info!("dlsite login: {} cookies captured", session.cookies_count());
        crate::app_state::save_dlsite_session(cx, &session);
        if let Some(webview) = &self.webview {
            webview.update(cx, |view, _| view.hide());
        }
        cx.emit(DlsiteLoginDone);
        true
    }

    pub fn show(&mut self, cx: &mut Context<Self>) {
        if let Some(webview) = &self.webview {
            webview.update(cx, |view, _| view.show());
        }
        self.check_generation += 1;
        self.start_url_check(cx);
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.check_generation += 1;
        if let Some(webview) = self.webview.take() {
            webview.update(cx, |view, _| view.hide());
        }
    }
}

impl EventEmitter<DlsiteLoginDone> for DlsiteLoginView {}
impl EventEmitter<DlsiteLoginCancelled> for DlsiteLoginView {}

impl Render for DlsiteLoginView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // モーダル領域（中央 640x480）に WebView を配置する
        let bounds = {
            let window_bounds = window.bounds();
            let width = 640.0_f32;
            let height = 480.0_f32;
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
            .id("dlsite-login-backdrop")
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
            .bg(gpui_kit::hsla(0.0, 0.0, 0.0, 0.45))
            .child(
                div()
                    .id("dlsite-login-cancel")
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
                                    cx.emit(DlsiteLoginCancelled);
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
                width: gpui_kit::px(640.0),
                height: gpui_kit::px(480.0),
            },
            |_, _| TestRoot,
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        let weak = visual.update(|window, cx| {
            let view = cx.new(|cx| DlsiteLoginView::new(window, cx));
            let weak = view.downgrade();
            drop(view); // モーダルを閉じた状態（強参照なし）
            weak
        });
        cx.run_until_parked();
        assert!(
            weak.upgrade().is_none(),
            "DlsiteLoginView が解放されていない（URL 監視タスクのリーク）"
        );
    }
}
