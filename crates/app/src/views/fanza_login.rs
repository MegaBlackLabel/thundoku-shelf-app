//! FANZA同人（www.dmm.co.jp/dc/doujin）のログインをアプリ内 WebView（gpui-wry）で行うビュー。
//!
//! FANZA の購入済み作品ページ（mylibrary）を WebView で開く。ユーザーが年齢確認（はい）→
//! accounts.dmm.co.jp のパスワードログインを WebView 内で完了して www.dmm.co.jp に戻ったら、
//! www.dmm.co.jp / accounts.dmm.co.jp のセッション Cookie を自動取得して永続化する。

use gpui_kit::component::{Icon, IconName};
use gpui_kit::{
    App, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement, ParentElement,
    Render, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_wry::WebView;

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
                // (a) 借用内: 世代・URL の確認とハンドルの取得（**wry を触らない = pump しない**）
                let tick = match handle.update(cx, |this, cx| this.begin_check(generation, cx)) {
                    Ok(super::CheckStep::Collect(tick)) => tick,
                    Ok(super::CheckStep::Wait) => continue,
                    Ok(super::CheckStep::Stop) => break,
                    // Err = ビューが drop された（監視する相手がいない）
                    Err(_) => break,
                };
                // (b) 借用の外: Cookie 収集（`cookies_for_url` がメッセージループを回す）
                let cookies =
                    super::collect_session_cookies(&tick.webview, &SESSION_ORIGINS, "fanza");
                drop(tick); // 親ウィンドウより長生きさせない（tick 内で解放）
                // (c) 借用内: 収集結果を反映（**wry を触らない**）
                let Ok(done) =
                    handle.update(cx, |this, cx| this.finish_check(generation, cookies, cx))
                else {
                    break;
                };
                if done {
                    break;
                }
            }
        })
        .detach();
    }

    /// tick の前半（借用内・**wry を触らない**）。
    ///
    /// URL が `www.dmm.co.jp` に戻り（年齢確認・ログインページを抜けて）いる間だけ収集する。
    fn begin_check(&self, generation: u64, cx: &App) -> super::CheckStep {
        if self.check_generation != generation {
            return super::CheckStep::Stop;
        }
        let Some(webview) = self.webview.as_ref() else {
            return super::CheckStep::Wait;
        };
        let webview = webview.read(cx);
        let Ok(url) = webview.raw().url() else {
            return super::CheckStep::Wait;
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
            return super::CheckStep::Wait;
        }
        super::CheckStep::Collect(super::CheckTick {
            webview: webview.handle(),
        })
    }

    /// tick の後半（借用内・**wry を触らない**）。収集結果を解釈して保存・通知する。
    ///
    /// `www` と `accounts` の Cookie を**収集元ごとに分けて**持つ（1 つに潰すと、片方に
    /// しか送るべきでない Cookie がもう片方や CDN へ飛ぶ）。
    fn finish_check(
        &mut self,
        generation: u64,
        collected: super::CollectedCookies,
        cx: &mut Context<Self>,
    ) -> bool {
        if self.check_generation != generation {
            // 収集中に閉じ直された。この tick の結果は捨てる。
            return true;
        }
        let session = thundoku_core::fanza::client::FanzaSession::from_collected(collected);
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
        // `AuthDialog::render` から**描画のたび**に呼ばれる。表示中に監視を再起動すると
        // tick の 1 秒タイマーが毎回リセットされ、URL チェックが永久に走らない
        // （＝ログインできてもモーダルが閉じない）。表示状態が変わったときだけ起動する。
        let was_visible = self.visible;
        self.visible = true;
        if let Some(webview) = &self.webview {
            webview.update(cx, |view, _| view.show());
        }
        if !was_visible {
            self.check_generation += 1;
            self.start_url_check(cx);
        }
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
        let window_bounds = window.bounds();
        let geometry = super::login_modal_geometry(
            window_bounds.size.width.as_f32(),
            window_bounds.size.height.as_f32(),
            640.0,
            480.0,
        );
        webview.update(cx, |view, _| {
            let _ = view.raw().set_bounds(lb_wry::Rect {
                position: lb_wry::dpi::Position::Physical(lb_wry::dpi::PhysicalPosition::new(
                    geometry.webview_left as i32,
                    geometry.webview_top as i32,
                )),
                size: lb_wry::dpi::Size::Physical(lb_wry::dpi::PhysicalSize::new(
                    geometry.webview_width as u32,
                    geometry.webview_height as u32,
                )),
            });
        });
    }
}

impl EventEmitter<FanzaLoginDone> for FanzaLoginView {}
impl EventEmitter<FanzaLoginCancelled> for FanzaLoginView {}

impl Render for FanzaLoginView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 閉じるボタンは WebView の右上・すぐ外側（技術書典のログインモーダルと同じ位置）。
        // WebView（native 子ウィンドウ）は GPUI 要素より常に最前面に描画されるため、
        // ボタンをモーダル領域の中に置くと隠れる。
        let geometry = {
            let window_bounds = window.bounds();
            super::login_modal_geometry(
                window_bounds.size.width.as_f32(),
                window_bounds.size.height.as_f32(),
                640.0,
                480.0,
            )
        };
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
                    .left(px(geometry.close_left))
                    .top(px(geometry.close_top))
                    .w(px(geometry.close_size))
                    .h(px(geometry.close_size))
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

    /// `AuthDialog::render` は**描画のたび**に `show()` を呼ぶ。表示中の `show()` で監視を
    /// 起動し直すと tick の 1 秒タイマーが毎回リセットされ、描画が 1 秒より速い間は
    /// URL チェックが**一度も走らない**（実測: DLsite ログイン後の同期で描画が増えた間に
    /// FANZA / BOOTH / 技術書典のモーダルが閉じなくなった）。表示状態が変わったときだけ
    /// 監視を起動する（世代が進まない）ことを見る。
    #[gpui_kit::test]
    async fn show_does_not_restart_the_monitor_while_visible(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(640.0),
                height: gpui_kit::px(480.0),
            },
            |_, _| TestRoot,
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        let (first, second, after_close) = visual.update(|window, cx| {
            let view = cx.new(|cx| FanzaLoginView::new(window, cx));
            view.update(cx, |view, cx| view.show(cx)); // 描画相当（1 回目）
            let first = view.read(cx).check_generation;
            view.update(cx, |view, cx| view.show(cx)); // 描画相当（2 回目）
            let second = view.read(cx).check_generation;
            view.update(cx, |view, cx| {
                view.close(cx);
                view.show(cx); // 閉じた後に開き直したら監視を起こし直す
            });
            let after_close = view.read(cx).check_generation;
            (first, second, after_close)
        });
        assert_eq!(
            first, second,
            "表示中の show() で監視が再起動している（1 秒タイマーが毎回リセットされ、URL チェックが走らない）"
        );
        assert!(
            after_close > second,
            "閉じた後の show() では監視を起動し直す必要がある"
        );
    }
}
