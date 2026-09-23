//! BOOTH（booth.pm）のログインをアプリ内 WebView（gpui-wry）で行うビュー。
//!
//! booth.pm のログインページ（pixiv OAuth）を WebView で開き、ユーザーが
//! WebView 内でログインを完了して booth.pm に戻ったら、booth.pm のセッション
//! Cookie を自動取得して永続化する（Cookie の手動コピーは不要）。

use gpui_kit::component::{Icon, IconName};
use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement, Render, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use gpui_wry::WebView;
use raw_window_handle::HasWindowHandle;

/// ログイン完了イベント（Cookie を取得して永続化した後に発行）。
pub struct BoothLoginDone;

/// ログインキャンセルイベント（閉じるボタンでモーダルを閉じたときに発行）。
pub struct BoothLoginCancelled;

pub struct BoothLoginView {
    webview: Option<Entity<WebView>>,
    /// URL 監視タイマーの世代（重複チェック防止）
    check_generation: u64,
}

impl BoothLoginView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // wry の WebView を作成して booth.pm のログインページを開く。
        // テスト環境など native window handle が取得できない場合は
        // WebView なしで作成する（backdrop のみ表示）。
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
        // 永続ストアだと pixiv/booth のセッションがアプリ再起動をまたいで
        // 残り、ログアウト後に再ログイン WebView を開くと pixiv の SSO で
        // 自動再ログインされてしまうため（ログアウトが効かないように見える）。
        let builder = lb_wry::WebViewBuilder::new().with_incognito(true);
        #[cfg(debug_assertions)]
        let builder = builder.with_devtools(true);
        let window_handle = match window.window_handle() {
            Ok(h) => h,
            Err(e) => {
                log::error!("booth login: window_handle() failed: {e:?}");
                return None;
            }
        };
        // WebView2 の生成は内部でメッセージループを回す（理由は `crate::app_state::webview_pumping()` のドキュメント参照）。
        // その間に他の定期タスクが App を更新すると gpui の借用と衝突して落ちるため、
        // ここでカウンタを立てて知らせる。
        let _pumping = crate::app_state::WebviewPumpGuard::enter();
        let webview = match builder.build(&window_handle) {
            Ok(w) => w,
            Err(e) => {
                log::error!("booth login: wry build() failed: {e:?} | {e}");
                return None;
            }
        };
        let entity = cx.new(|cx| WebView::new(webview, window, cx));
        entity.update(cx, |view, _| {
            view.load_url("https://booth.pm/users/sign_in");
            // モーダルを開くまでは隠しておく（AuthDialog の show で表示）
            view.hide();
        });
        Some(entity)
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
                // WebView2 がメッセージループを回している間は更新しない（次の tick に回す）。
                if crate::app_state::webview_pumping() > 0 {
                    continue;
                }
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

    /// 現在の URL が booth.pm に戻っていたら Cookie を取得して保存する。
    /// ログイン完了で true を返す。
    fn check_login(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(webview) = self.webview.as_ref() else {
            // WebView なし（テスト環境）ではログインを試みない
            return false;
        };
        let Ok(url) = webview.read(cx).raw().url() else {
            return false;
        };
        // URL のクエリ/フラグメントには認可コードやトークンが載り得るため落とす。
        log::debug!(
            "booth login check: url={}",
            url.split(['?', '#']).next().unwrap_or(&url)
        );
        let is_booth = super::url_is_on_host(&url, "booth.pm");
        // ログインページ自体から pixiv に遷移している間は待つ。
        // booth.pm に戻り、かつログインページでなければセッション確立とみなす。
        let on_sign_in = url.contains("/users/sign_in");
        if !is_booth || on_sign_in {
            return false;
        }
        // Cookie 取得も内部でメッセージループを回す（理由は `crate::app_state::webview_pumping()` のドキュメント参照）。
        let _pumping = crate::app_state::WebviewPumpGuard::enter();
        // booth.pm と accounts.booth.pm のセッション Cookie を取得する
        //（accounts.booth.pm の _plaza_session_* はログアウトに必要）
        //
        // 収集元ホストごとに分けて持つ（1 つに潰すと、accounts 側にしか送るべきでない
        // Cookie が booth.pm や画像 CDN へ飛ぶ）。
        let mut origins: std::collections::BTreeMap<
            String,
            std::collections::BTreeMap<String, String>,
        > = std::collections::BTreeMap::new();
        for url in ["https://booth.pm", "https://accounts.booth.pm"] {
            let cookies = webview
                .read(cx)
                .raw()
                .cookies_for_url(url)
                .unwrap_or_default();
            log::debug!("booth login: cookies_for_url({url}) -> {}", cookies.len());
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
        let session = thundoku_core::booth::BoothSession::new(origins);
        if !session.logged_in() {
            log::warn!("booth login: セッション Cookie を取得できませんでした");
            return false;
        }
        log::info!("booth login: {} cookies captured", session.cookies_count());
        crate::app_state::save_booth_session(cx, &session);
        // WebView を隠して完了を通知する
        if let Some(webview) = &self.webview {
            webview.update(cx, |view, _| view.hide());
        }
        cx.emit(BoothLoginDone);
        true
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
}

impl EventEmitter<BoothLoginDone> for BoothLoginView {}
impl EventEmitter<BoothLoginCancelled> for BoothLoginView {}

impl Render for BoothLoginView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // モーダル領域（中央 480x640）に WebView を配置する
        let bounds = {
            let window_bounds = window.bounds();
            let width = 480.0_f32;
            let height = 640.0_f32;
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
        // GPUI 側は backdrop（暗い背景）と閉じるボタンのみ描画する。
        // WebView 自体は GPUI ウィンドウの上に重なる。
        // （呼び出し側が deferred レイヤーに置いて最前面表示する）
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
