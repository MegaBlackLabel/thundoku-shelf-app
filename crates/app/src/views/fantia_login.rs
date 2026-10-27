//! Fantia（fantia.jp）のログインをアプリ内 WebView（gpui-webview）で行うビュー。
//!
//! Fantia のログイン（`https://fantia.jp/sessions/signin`）はパスワードに加えて
//! **reCAPTCHA** があり、続けて **2 段階認証**のステップがある（実測 2026-10-15）。
//! アプリから資格情報を送る自動ログインはできないため、ユーザーが WebView 内で完了する。
//!
//! 完了の判定は次の 2 つを**両方**満たすこと:
//!
//! 1. 表示中の URL のホストが `fantia.jp` で、パスが `/sessions` 配下**ではない**
//!    （サインインと 2 段階認証の画面がそこにあり、居る間はまだ完了していない）
//! 2. `_session_id`（Fantia のセッション Cookie。これ 1 つがセッションの全体）を取得できている
//!
//! **この完了判定は実機のログインでは未検証**（根拠は公開ページと Cookie 名の実測だけ）。
//! ログイン直後の遷移先が `/sessions` 配下でないこと・ログイン後も `/sessions` 配下に
//! 留まる画面が無いことを、実機で確かめる必要がある。

use gpui_kit::component::{Icon, IconName};
use gpui_kit::{
    App, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement, ParentElement,
    Render, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_webview::WebView;
use thundoku_core::fantia::client::{FantiaSession, SITE_HOST};

/// セッション Cookie を集める起点（Fantia は 1 ホストに集約されている）。
const SESSION_ORIGINS: [&str; 1] = ["https://fantia.jp"];

/// ログインの入口（公開ページ。実測 2026-10-15）。
const SIGNIN_URL: &str = "https://fantia.jp/sessions/signin";

/// ログインと 2 段階認証の画面が置かれるパスの接頭辞。
const SESSIONS_PATH: &str = "/sessions";

/// 完了判定の URL 条件。
///
/// `fantia.jp`（サイトのホスト）の上で、`/sessions`（サインインと 2 段階認証の画面）
/// 配下**ではない**こと。両方の画面はログインの途中なので、ここに居る間は完了にしない。
///
/// **実機のログインでは未検証**（根拠は公開ページと Cookie 名の実測だけ）。ログイン直後の
/// 遷移先が `/sessions` 配下でないこと・ログイン後に `/sessions` 配下へ留まる画面が無い
/// ことを実機で確かめる必要がある。解釈できない URL は `false`（fail-closed）。
fn is_completion_page(url: &str) -> bool {
    let Ok(parsed) = url.parse::<url::Url>() else {
        return false;
    };
    if parsed.host_str() != Some(SITE_HOST) {
        return false;
    }
    let path = parsed.path();
    let on_sessions = path == SESSIONS_PATH
        || path
            .strip_prefix(SESSIONS_PATH)
            .is_some_and(|rest| rest.starts_with('/'));
    !on_sessions
}

/// ログイン完了イベント（Cookie を取得して永続化した後に発行）。
pub struct FantiaLoginDone;

/// ログインキャンセルイベント（閉じるボタンでモーダルを閉じたときに発行）。
pub struct FantiaLoginCancelled;

/// ログイン確認（`/api/v1/me`）の最短間隔。完了ページに留まっている間、毎秒叩かない。
const VERIFY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

pub struct FantiaLoginView {
    webview: Option<Entity<WebView>>,
    check_generation: u64,
    /// WebView を表示したいか。生成が非同期（Windows はタスク）なので、生成完了時に反映する。
    visible: bool,
    /// 収集した Cookie が本当にログイン済みかを確かめる（`/api/v1/me`）。
    ///
    /// **Cookie の有無では判定できない**: Fantia は未ログインでも `_session_id` を発行する
    /// （実測 2026-10-15。匿名でも `/api/v1/me` は 401 を返す）。テストでは差し替える。
    verify: fn(&FantiaSession) -> bool,
    /// 最後に確認した時刻（間隔を空けるため）。
    last_verify: Option<std::time::Instant>,
}

impl FantiaLoginView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            webview: None,
            check_generation: 0,
            visible: false,
            verify: thundoku_core::fantia::client::me_is_authenticated,
            last_verify: None,
        };
        // 生成は App の借用外（Windows はタスク）で行われる。理由は
        // `super::create_login_webview` のドキュメント参照。
        super::create_login_webview(
            &mut this,
            window,
            cx,
            // incognito（non-persistent）WebView: Cookie はメモリのみ。
            // 永続ストアだとログアウト後も端末にセッションが残り、次回起動で自動ログインする。
            true,
            // ログインの入口（`/sessions/signin`）から始める。2 段階認証はこのページから
            // 続けて出るので、ここから始めればユーザーは画面の指示どおり進められる。
            SIGNIN_URL,
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
                    // この画面では使わない（BOOTH だけが購入ライブラリへ寄る）
                    Ok(super::CheckStep::Visit(_, _)) => continue,
                    // サインイン / 2 段階認証の画面に居る（まだ完了しない）
                    Ok(super::CheckStep::Wait) => continue,
                    Ok(super::CheckStep::Stop) => break,
                    // Err = ビューが drop された（監視する相手がいない）
                    Err(_) => break,
                };
                // (b) 借用の外: Cookie 収集（`cookies_for_url` がメッセージループを回す）
                let cookies =
                    super::collect_session_cookies(&tick.webview, &SESSION_ORIGINS, "fantia");
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
    /// - 世代が変わっていたら `Stop`（閉じ直されたので監視を終える）
    /// - WebView が無ければ `Wait`（テスト環境・生成待ち）
    /// - URL が `fantia.jp` の `/sessions` 配下（サインイン / 2 段階認証）なら `Wait`
    /// - それ以外の `fantia.jp` なら `Collect`
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
        // URL のクエリ/フラグメントにはトークンが載り得るため落とす。
        log::debug!(
            "fantia login check: url={}",
            url.split(['?', '#']).next().unwrap_or(&url)
        );
        // サインイン（/sessions/signin）と 2 段階認証の画面に居る間はまだ完了していない
        // （未認証でも `_session_id` は発行され得るため、Cookie の有無だけでは判定できない）。
        if !is_completion_page(&url) {
            return super::CheckStep::Wait;
        }
        super::CheckStep::Collect(super::CheckTick {
            webview: webview.handle(),
        })
    }

    /// tick の後半（借用内・**wry を触らない**）。収集結果を解釈して保存・通知する。
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
        let session = FantiaSession::from_collected(collected);
        // `_session_id` は Fantia のセッションそのもの（未ログインでも `fantia.jp` には
        // `_ga` 等の Cookie が付くので、名前まで見る）。
        if !session.logged_in() {
            log::debug!("fantia login: _session_id が未取得（未ログイン）");
            return false;
        }
        // **本当にログイン済みかは `/api/v1/me` で確かめる**: Fantia は未ログインでも
        // `_session_id` を発行するため、Cookie の有無だけでは匿名と区別できない（実測
        // 2026-10-15。`/api/v1/me` は未認証で 401 `{"redirect_to":"/sessions/signin"}`）。
        // 完了ページに留まっている間に叩き続けないよう、確認は間隔を空ける。
        if self
            .last_verify
            .is_some_and(|last| last.elapsed() < VERIFY_INTERVAL)
        {
            return false;
        }
        self.last_verify = Some(std::time::Instant::now());
        if !(self.verify)(&session) {
            log::debug!("fantia login: /api/v1/me が未認証（まだログインしていない）");
            return false;
        }
        log::info!(
            "fantia login: {} cookies captured（/api/v1/me でログイン確認済み）",
            session.cookies_count()
        );
        crate::app_state::save_fantia_session(cx, &session);
        self.visible = false;
        if let Some(webview) = &self.webview {
            webview.update(cx, |view, _| view.hide());
        }
        cx.emit(FantiaLoginDone);
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

impl EventEmitter<FantiaLoginDone> for FantiaLoginView {}
impl EventEmitter<FantiaLoginCancelled> for FantiaLoginView {}

impl Render for FantiaLoginView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 閉じるボタンは WebView の右上・すぐ外側（他のログインモーダルと同じ位置）。
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
            .id("fantia-login-backdrop")
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
            .bg(gpui_kit::hsla(0.0, 0.0, 0.0, 0.45))
            .child(
                div()
                    .id("fantia-login-cancel")
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
                                    cx.emit(FantiaLoginCancelled);
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
    use gpui_kit::ReadGlobal as _;
    use gpui_kit::TestAppContext;
    use thundoku_core::session_cookies::CookieEntry;

    /// テスト用のウィンドウルート（描くものは無い）。
    struct TestRoot;

    impl Render for TestRoot {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    /// 完了判定の URL 条件: `fantia.jp` の `/sessions` 配下（サインイン / 2 段階認証）では
    /// 完了にしないこと。**この条件は実機のログインで未検証**なので、意図をテストで固定する。
    #[test]
    fn completion_page_is_fantia_outside_sessions() {
        // ログインの途中（サインイン / 2 段階認証）
        assert!(!is_completion_page("https://fantia.jp/sessions/signin"));
        assert!(!is_completion_page("https://fantia.jp/sessions"));
        assert!(!is_completion_page("https://fantia.jp/sessions/two_factor"));
        // ログインが完了してサイト内へ戻った
        assert!(is_completion_page("https://fantia.jp/"));
        assert!(is_completion_page(
            "https://fantia.jp/mypage/users/purchases"
        ));
        // 別ホスト（reCAPTCHA 等）や紛らわしいホストは完了にしない
        assert!(!is_completion_page("https://www.google.com/recaptcha"));
        assert!(!is_completion_page("https://fantia.jp.evil.example.com/"));
        // 解釈できない URL は完了にしない（fail-closed）
        assert!(!is_completion_page("not a url"));
    }

    /// ログイン画面を閉じて強参照を全て drop したらビューが解放されること。
    /// URL 監視タスクが `Entity` を持ち続けると、WebView ごと残る。
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
            let view = cx.new(|cx| FantiaLoginView::new(window, cx));
            let weak = view.downgrade();
            drop(view); // モーダルを閉じた状態（強参照なし）
            weak
        });
        cx.run_until_parked();
        assert!(
            weak.upgrade().is_none(),
            "FantiaLoginView が解放されていない（URL 監視タスクのリーク）"
        );
    }

    /// `_session_id` を含む Cookie を収集できたら、セッションを保存して完了イベントを出し、
    /// 監視を終えること。
    #[gpui_kit::test]
    async fn collected_session_id_saves_and_emits_done(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(640.0),
                height: gpui_kit::px(480.0),
            },
            |_, _| TestRoot,
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();

        // 完了イベントの購読（Done が来た回数を数える）。
        //
        // - `emit` は effect として update の終わりに配送される（即時ではない）ので、
        //   `finish_check` を呼ぶ update と数を読むタイミングを分ける。
        // - `Subscription` は drop すると解除されるので、レコーダー自身に持たせ、
        //   そのハンドルも update をまたいで生かす。
        struct DoneRecorder {
            count: std::rc::Rc<std::cell::Cell<usize>>,
            _subscription: Option<gpui_kit::Subscription>,
        }

        // `recorder` のハンドルは、購読を生かしておくためにテストの間ずっと持つ
        // （drop すると購読が解除される）。読まないので `_` を付ける。
        let (view, _recorder, count) = visual.update(|window, cx| {
            let view = cx.new(|cx| FantiaLoginView::new(window, cx));
            // ログイン確認（`/api/v1/me`）はテストでは差し替える（ネットワークを触らない）
            view.update(cx, |this, _| this.verify = |_| true);
            let count = std::rc::Rc::new(std::cell::Cell::new(0));
            let recorder = cx.new(|_| DoneRecorder {
                count: count.clone(),
                _subscription: None,
            });
            recorder.update(cx, |this, cx| {
                this._subscription = Some(cx.subscribe(
                    &view,
                    |this: &mut DoneRecorder,
                     _: Entity<FantiaLoginView>,
                     _: &FantiaLoginDone,
                     _: &mut Context<DoneRecorder>| {
                        this.count.set(this.count.get() + 1);
                    },
                ));
            });
            (view, recorder, count)
        });

        let (saved, stopped) = visual.update(|_window, cx| {
            let generation = view.read(cx).check_generation;
            // ログイン完了時に WebView から収集される形の Cookie（属性つき・名前つき）。
            let collected = std::collections::BTreeMap::from([(
                SITE_HOST.to_string(),
                vec![
                    CookieEntry::with_attributes("s".to_string(), None, None, true, None)
                        .named("_session_id"),
                ],
            )]);
            let stopped = view.update(cx, |this, cx| this.finish_check(generation, collected, cx));
            let saved = *crate::app_state::AppState::global(cx)
                .fantia_logged_in
                .lock();
            (saved, stopped)
        });

        assert_eq!(count.get(), 1, "FantiaLoginDone を出していない");
        assert!(saved, "ログイン済みセッションを保存していない");
        assert!(stopped, "完了したのに監視を続けている");
    }

    /// `_session_id` が無い（未ログイン）なら、保存も完了イベントも出さず監視を続けること。
    #[gpui_kit::test]
    async fn collected_without_session_id_keeps_waiting(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(640.0),
                height: gpui_kit::px(480.0),
            },
            |_, _| TestRoot,
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        let (saved, stopped) = visual.update(|window, cx| {
            let view = cx.new(|cx| FantiaLoginView::new(window, cx));
            let generation = view.read(cx).check_generation;
            let collected = std::collections::BTreeMap::from([(
                SITE_HOST.to_string(),
                vec![
                    CookieEntry::with_attributes("v".to_string(), None, None, true, None)
                        .named("_ga"),
                ],
            )]);
            let stopped = view.update(cx, |this, cx| this.finish_check(generation, collected, cx));
            let saved = *crate::app_state::AppState::global(cx)
                .fantia_logged_in
                .lock();
            (saved, stopped)
        });
        assert!(!saved, "未ログインをログイン済みとして保存している");
        assert!(!stopped, "未ログインなのに監視を終えている");
    }

    /// `_session_id` があっても、`/api/v1/me` が未認証（401）なら保存しないこと。
    ///
    /// Fantia は**未ログインでも `_session_id` を発行する**（実測 2026-10-15）ため、
    /// Cookie の有無だけでは匿名と区別できない。
    #[gpui_kit::test]
    async fn session_cookie_without_authentication_keeps_waiting(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(640.0),
                height: gpui_kit::px(480.0),
            },
            |_, _| TestRoot,
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        let (saved, stopped) = visual.update(|window, cx| {
            let view = cx.new(|cx| FantiaLoginView::new(window, cx));
            // `/api/v1/me` が未認証を返す状況（匿名の `_session_id`）
            view.update(cx, |this, _| this.verify = |_| false);
            let generation = view.read(cx).check_generation;
            let collected = std::collections::BTreeMap::from([(
                SITE_HOST.to_string(),
                vec![
                    CookieEntry::with_attributes("s".to_string(), None, None, true, None)
                        .named("_session_id"),
                ],
            )]);
            let stopped = view.update(cx, |this, cx| this.finish_check(generation, collected, cx));
            let saved = *crate::app_state::AppState::global(cx)
                .fantia_logged_in
                .lock();
            (saved, stopped)
        });
        assert!(
            !saved,
            "未認証（/api/v1/me が 401）のセッションをログイン済みとして保存している"
        );
        assert!(!stopped, "未認証なのに監視を終えている");
    }
}
