//! BOOTH（booth.pm）のログインをアプリ内 WebView（gpui-wry）で行うビュー。
//!
//! booth.pm のログインページ（pixiv OAuth）を WebView で開き、ユーザーが
//! WebView 内でログインを完了して booth.pm に戻ったら、booth.pm のセッション
//! Cookie を自動取得して永続化する（Cookie の手動コピーは不要）。

use gpui_kit::component::{Icon, IconName};
use gpui_kit::{
    App, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement, ParentElement,
    Render, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_wry::WebView;

/// セッション Cookie を集める URL（`accounts.booth.pm` の `_plaza_session_*` は
/// ログアウトにも必要なので、booth / プラザ / pixiv の 3 つを集める）。
///
/// **必ずパスまで指定する**。`cookies_for_url` は `Path` を見るため、`https://host` の形だと
/// `Path=/` の Cookie しか返らず、`/library` や `/login` に置かれたセッションを取り落とす
/// （実測: `https://accounts.pixiv.net` で集めると 0 件、`https://accounts.booth.pm` でも 0 件
/// だったのが、パス付きにすると拾える）。
const SESSION_ORIGINS: [&str; 3] = [
    "https://booth.pm/ja",
    "https://accounts.booth.pm/library?page=1",
    "https://accounts.pixiv.net/",
];

/// ログイン成立後に**一度だけ**開く購入ライブラリ。
///
/// 同期が叩くのは `accounts.booth.pm/library` で、その Cookie はライブラリを開いた
/// 時点で SSO が発行する。ここを開かずに `booth.pm` だけで集めると、同期先向けの
/// Cookie が 0 件になり「ログインページを受信」で失敗する。
const PLAZA_LIBRARY_URL: &str = "https://accounts.booth.pm/library?page=1";

/// ログイン完了イベント（Cookie を取得して永続化した後に発行）。
pub struct BoothLoginDone;

/// ログインキャンセルイベント（閉じるボタンでモーダルを閉じたときに発行）。
pub struct BoothLoginCancelled;

pub struct BoothLoginView {
    webview: Option<Entity<WebView>>,
    /// URL 監視タイマーの世代（重複チェック防止）
    check_generation: u64,
    /// WebView を表示したいか。生成が非同期（Windows はタスク）なので、生成完了時に反映する。
    visible: bool,
    /// ログイン成立後に購入ライブラリ（`accounts.booth.pm/library`）へ寄ったか。
    ///
    /// セッションは `booth.pm` で確立するが、同期が叩く購入ライブラリはプラザ側
    /// （`accounts.booth.pm`）の Cookie を要求する。ブラウザならライブラリを開いた時点で
    /// SSO が発行するため、WebView でも一度開いてから集める（実測: booth.pm だけで
    /// 集めると `accounts.booth.pm` 向けが 0 件で、同期がログインページを受信していた）。
    plaza_visited: bool,
}

impl BoothLoginView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // wry の WebView を作成して booth.pm のログインページを開く。
        // テスト環境など native window handle が取得できない場合は
        // WebView なしで作成する（backdrop のみ表示）。
        let mut this = Self {
            webview: None,
            check_generation: 0,
            visible: false,
            plaza_visited: false,
        };
        // 生成は App の借用外（Windows はタスク）で行われる。理由は
        // `super::create_login_webview` のドキュメント参照。
        super::create_login_webview(
            &mut this,
            window,
            cx,
            // **永続** WebView（incognito にしない）。
            //
            // BOOTH のログインは booth.pm のセッションを作るが、購入ライブラリ
            // （accounts.booth.pm）は pixiv の SSO セッションを要求する。incognito
            // （メモリのみ・アプリ再起動をまたがない）だと SSO が通らず、同期が
            // 「ログインページを受信」で失敗する（実測: プラザへ寄ると
            // accounts.pixiv.net のログイン画面へリダイレクトされ、
            // accounts.booth.pm 向け Cookie が 0 件だった）。
            //
            // ログアウト時に SSO で自動再ログインされる問題は、保存データの消去
            // （`clear_login_webview_data`）で解決する（設定画面のログアウトで実行済み）。
            false,
            "https://booth.pm/users/sign_in",
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

    /// 1 秒ごとに WebView の URL を確認し、booth.pm に戻ってログインが
    /// 完了したら Cookie を取得して保存・通知する。
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
                    Ok(super::CheckStep::Visit(tick, url)) => {
                        // ログ用: プラザへ寄る**前**に、SSO の鍵になるホストの Cookie を数える。
                        // （incognito かどうか・SSO が通るかどうかを、憶測ではなくログで判断する）
                        // ★`cookies_for_url` は Path/Domain の合わない Cookie を取り落とす
                        //   （過少報告する）ため、「セッションが無い」判断には使えない。
                        //   WebView が保持している**全 Cookie** を属性つきでダンプする。
                        match tick.webview.raw().cookies() {
                            Ok(cookies) => {
                                log::info!("booth login: 寄る前の全 Cookie {} 件", cookies.len());
                                for cookie in &cookies {
                                    log::info!(
                                        "booth login: pre-cookie name={:?} domain={:?} path={:?}",
                                        cookie.name(),
                                        cookie.domain(),
                                        cookie.path()
                                    );
                                }
                            }
                            Err(error) => {
                                log::warn!("booth login: 全 Cookie を取得できない: {error}")
                            }
                        }
                        // (b') 借用の外: 購入ライブラリを一度開き、SSO に
                        //      `accounts.booth.pm` の Cookie を発行させる。
                        //      （同期が叩くのはこのホストなので、ここで寄らないと 0 件になる）
                        let _ = tick.webview.raw().load_url(url);
                        continue;
                    }
                    Ok(super::CheckStep::Wait) => continue,
                    Ok(super::CheckStep::Stop) => break,
                    // Err = ビューが drop された（監視する相手がいない）
                    Err(_) => break,
                };
                // (b) 借用の外: Cookie 収集（`cookies_for_url` がメッセージループを回す）
                let cookies =
                    super::collect_session_cookies(&tick.webview, &SESSION_ORIGINS, "booth");
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
    /// `booth.pm` に戻り、かつログインページでなくなったら収集する。
    fn begin_check(&mut self, generation: u64, cx: &App) -> super::CheckStep {
        if self.check_generation != generation {
            return super::CheckStep::Stop;
        }
        let Some(webview) = self.webview.as_ref() else {
            // WebView なし（テスト環境）ではログインを試みない
            return super::CheckStep::Wait;
        };
        let webview = webview.read(cx);
        let Ok(url) = webview.raw().url() else {
            return super::CheckStep::Wait;
        };
        // URL のクエリ/フラグメントには認可コードやトークンが載り得るため落とす。
        log::info!(
            "booth login check: url={}",
            url.split(['?', '#']).next().unwrap_or(&url)
        );
        // ログインページ自体から pixiv に遷移している間は待つ。
        if url.contains("/users/sign_in") {
            return super::CheckStep::Wait;
        }
        let is_booth = super::url_is_on_host(&url, "booth.pm");
        let is_plaza = super::url_is_on_host(&url, "accounts.booth.pm");
        // 1) booth.pm に戻ったら、まず購入ライブラリへ**一度だけ**寄る。
        //    同期が叩くのは `accounts.booth.pm/library` で、そちらの Cookie は
        //    ライブラリを開いた時点で SSO が発行する（booth.pm だけでは 0 件で、
        //    同期がログインページを受信してしまう）。
        if is_booth && !is_plaza && !self.plaza_visited {
            self.plaza_visited = true;
            log::info!(
                "booth login: セッション確立。購入ライブラリへ一度寄ってから Cookie を集める"
            );
            return super::CheckStep::Visit(
                super::CheckTick {
                    webview: webview.handle(),
                },
                PLAZA_LIBRARY_URL,
            );
        }
        // 2) 購入ライブラリへ着いたら集める。寄り直しが失敗しても詰まらないよう、
        //    booth.pm に戻っていれば（一度寄った後なら）そこで集める。
        if is_plaza || (is_booth && self.plaza_visited) {
            return super::CheckStep::Collect(super::CheckTick {
                webview: webview.handle(),
            });
        }
        super::CheckStep::Wait
    }

    /// tick の後半（借用内・**wry を触らない**）。収集結果を解釈して保存・通知する。
    ///
    /// `booth.pm` と `accounts.booth.pm` の Cookie を**収集元ホストごとに分けて**持つ
    /// （1 つに潰すと、`accounts` 側にしか送るべきでない Cookie が `booth.pm` や画像 CDN へ飛ぶ）。
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
        let session = thundoku_core::booth::BoothSession::from_collected(collected);
        if !session.logged_in() {
            log::warn!("booth login: セッション Cookie を取得できませんでした");
            return false;
        }
        log::info!("booth login: {} cookies captured", session.cookies_count());
        log::info!(
            "booth login: 完了処理に入る webview.is_some={} visible={}",
            self.webview.is_some(),
            self.visible
        );
        crate::app_state::save_booth_session(cx, &session);
        // WebView を隠して完了を通知する
        if let Some(webview) = &self.webview {
            webview.update(cx, |view, _| view.hide());
            log::info!("booth login: WebView を hide() した（完了）");
        } else {
            log::warn!("booth login: 完了時に WebView が無い（hide 不要）");
        }
        self.visible = false;
        cx.emit(BoothLoginDone);
        true
    }

    /// WebView を表示する（ログインモーダルを開く）。
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

    /// キャンセル/閉じる。
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.check_generation += 1; // 監視を止める
        self.visible = false;
        match self.webview.take() {
            Some(webview) => {
                webview.update(cx, |view, _| view.hide());
                log::info!("booth login: close で WebView を take()+hide() した");
            }
            None => log::info!("booth login: close（WebView は既に無い）"),
        }
    }

    /// WebView をモーダル領域（中央 480x640）へ配置する。
    ///
    /// 生成が非同期（Windows はタスク）なので、生成直後にも呼ぶ。`render` 任せだと
    /// 生成完了後に再描画が走らず、**配置されないまま表示**される。
    fn apply_webview_bounds(&self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(webview) = &self.webview else {
            return;
        };
        let window_bounds = window.bounds();
        log::debug!(
            "booth login: window.bounds = {}x{}（scale={:?}）",
            window_bounds.size.width.as_f32(),
            window_bounds.size.height.as_f32(),
            window.scale_factor()
        );
        let geometry = super::login_modal_geometry(
            window_bounds.size.width.as_f32(),
            window_bounds.size.height.as_f32(),
            480.0,
            640.0,
        );
        log::debug!(
            "booth login: set_bounds -> left={} top={} w={} h={}",
            geometry.webview_left,
            geometry.webview_top,
            geometry.webview_width,
            geometry.webview_height
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

impl EventEmitter<BoothLoginDone> for BoothLoginView {}
impl EventEmitter<BoothLoginCancelled> for BoothLoginView {}

impl Render for BoothLoginView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 閉じるボタンは WebView の右上・すぐ外側（技術書典のログインモーダルと同じ位置）。
        // WebView（native 子ウィンドウ）は GPUI 要素より常に最前面に描画されるため、
        // ボタンをモーダル領域の中に置くと隠れる。
        let geometry = {
            let window_bounds = window.bounds();
            super::login_modal_geometry(
                window_bounds.size.width.as_f32(),
                window_bounds.size.height.as_f32(),
                480.0,
                640.0,
            )
        };
        self.apply_webview_bounds(window, cx);
div()
            .id("booth-login-backdrop")
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
            .bg(gpui_kit::hsla(0.0, 0.0, 0.0, 0.45))
            // 閉じるボタンはウィンドウ右上（WebView 領域の外側）に配置
            .child(
                div()
                    .id("booth-login-cancel")
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
                                    cx.emit(BoothLoginCancelled);
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
                width: gpui_kit::px(480.0),
                height: gpui_kit::px(640.0),
            },
            |_, _| TestRoot,
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        let weak = visual.update(|window, cx| {
            let view = cx.new(|cx| BoothLoginView::new(window, cx));
            let weak = view.downgrade();
            drop(view); // モーダルを閉じた状態（強参照なし）
            weak
        });
        cx.run_until_parked();
        assert!(
            weak.upgrade().is_none(),
            "BoothLoginView が解放されていない（URL 監視タスクのリーク）"
        );
    }
}
