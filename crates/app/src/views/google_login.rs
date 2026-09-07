//! Google OAuth ログインをアプリ内 WebView（gpui-wry）で行うビュー。
//!
//! BOOTH と同じく、Google の認可ページを WebView で開き、ユーザーが
//! WebView 内で Google アカウントにログインしてリダイレクトがループバック
//! アドレス（127.0.0.1）に戻ったら、code をループバック受信 → トークン交換
//! → プロフィール取得まで自動で行う（システムブラウザは開かない）。

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gpui_kit::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement, ReadGlobal as _, Render, StatefulInteractiveElement as _, Styled as _, Window,
    div, px,
};
use gpui_kit::component::{Icon, IconName};
use gpui_wry::WebView;
use raw_window_handle::HasWindowHandle;
use thundoku_core::google::{GoogleError, GoogleProfile, PendingGoogleAuth};

use crate::app_state::AppState;

/// ログイン成功イベント（プロフィール取得まで完了）。
pub struct GoogleLoginDone(pub GoogleProfile);

/// ログイン失敗イベント（エラーメッセージ付き）。
pub struct GoogleLoginFailed(pub String);

/// ログインキャンセルイベント（✕ ボタンで閉じたとき）。
pub struct GoogleLoginCancelled;

pub struct GoogleLoginView {
    webview: Option<Entity<WebView>>,
    /// 認可フローのキャンセルフラグ（✕ ボタンで立てる）
    cancel: Option<Arc<AtomicBool>>,
    /// WebView を開けなかった場合のエラー（表示用）
    error: Option<String>,
}

impl GoogleLoginView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        // AppState の GoogleClient から認可フローを開始（ブラウザは開かない）
        // バックグラウンドタスクからも使うため Arc を保持する
        let google = AppState::global(cx).google.clone();
        let pending = google
            .lock()
            .as_ref()
            .and_then(|client| client.begin_authorize().ok());
        let mut this = Self {
            webview: None,
            cancel: None,
            error: None,
        };
        match pending {
            Some(pending) => {
                let cancel = pending.cancel_handle();
                let url = pending.url.clone();
                this.cancel = Some(cancel);
                this.webview = Self::try_create_webview(window, cx);
                if let Some(webview) = &this.webview {
                    webview.update(cx, |view, _| {
                        view.load_url(&url);
                        view.show();
                    });
                }
                this.start_finish(pending, google, cx);
            }
            None => {
                let message = "Google クライアントが設定されていません".to_string();
                this.error = Some(message.clone());
                cx.emit(GoogleLoginFailed(message));
            }
        }
        this
    }

    fn try_create_webview(window: &mut Window, cx: &mut Context<Self>) -> Option<Entity<WebView>> {
        let builder = lb_wry::WebViewBuilder::new();
        #[cfg(debug_assertions)]
        let builder = builder.with_devtools(true);
        let window_handle = match window.window_handle() {
            Ok(h) => {
                h
            }
            Err(e) => {
                log::error!("google login: window_handle() failed: {e:?}");
                return None;
            }
        };
        let webview = match builder.build(&window_handle) {
            Ok(w) => {
                w
            }
            Err(e) => {
                log::error!("google login: wry build() failed: {e:?} | {e}");
                return None;
            }
        };
        let entity = cx.new(|cx| WebView::new(webview, window, cx));
        entity.update(cx, |view, _| view.hide());
        Some(entity)
    }

    /// ループバック受信 → トークン交換 → プロフィール取得をバックグラウンドで実行する。
    fn start_finish(
        &mut self,
        pending: PendingGoogleAuth,
        google: Arc<parking_lot::Mutex<Option<thundoku_core::google::GoogleClient>>>,
        cx: &mut Context<Self>,
    ) {
        let handle = cx.entity();
        cx.spawn(async move |_, cx| {
            let result = cx.background_executor().spawn({
                let google = google.clone();
                async move {
                    let mut guard = google.lock();
                    guard
                        .as_mut()
                        .map(|client| client.finish_authorize(pending))
                        .unwrap_or(Err(GoogleError::Auth(
                            "Google クライアントが未設定です".into(),
                        )))
                }
            });
            let result = result.await;
            handle.update(cx, |this, cx| {
                this.cancel = None;
                // wry の WebView は GPUI のレイヤーとは別にウィンドウに重なっているため、
                // 完了時（成功・失敗・キャンセル）に必ず隠す
                if let Some(webview) = this.webview.take() {
                    webview.update(cx, |view, _| view.hide());
                }
                match result {
                    Ok(profile) => {
                        // トークンを keyring に永続化する（再起動時の復元用）。
                        // refresh_token も含まれるため、期限切れ後も自動リフレッシュできる。
                        let state = AppState::global(cx);
                        if let Some(tokens) = state
                            .google
                            .lock()
                            .as_ref()
                            .and_then(|client| client.tokens().cloned())
                            && let Ok(json) = serde_json::to_string(&tokens)
                        {
                            let _ = state
                                .secrets
                                .save(thundoku_core::secrets::USER_GOOGLE, &json);
                        }
                        // cx.emit は AsyncApp::update の RefCell 再入でパニックするため、
                        // AppState のグローバル状態を直接更新する。
                        // （Workspace の監視タスクが google_login_done を検知して show_auth を閉じる）
                        *state.google_profile.lock() = Some(profile.clone());
                        *state.google_logged_in.lock() = true;
                        *state.google_login_error.lock() = None;
                        state
                            .google_login_done
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                    // キャンセルは ✕ ボタンが既に GoogleLoginCancelled を発行済み
                    Err(GoogleError::Cancelled) => {}
                    Err(error) => {
                        let state = AppState::global(cx);
                        *state.google_login_error.lock() = Some(error.to_string());
                        state
                            .google_login_done
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                    }
                }
            });
        })
        .detach();
    }

    /// WebView を表示する（ログインモーダルを開く）。
    pub fn show(&mut self, cx: &mut Context<Self>) {
        if let Some(webview) = &self.webview {
            webview.update(cx, |view, _| view.show());
        }
    }

    /// キャンセル/閉じる。
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        if let Some(webview) = self.webview.take() {
            webview.update(cx, |view, _| view.hide());
        }
    }
}

impl EventEmitter<GoogleLoginDone> for GoogleLoginView {}
impl EventEmitter<GoogleLoginFailed> for GoogleLoginView {}
impl EventEmitter<GoogleLoginCancelled> for GoogleLoginView {}

impl Render for GoogleLoginView {
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
        // 閉じるボタンは WebView の右上・すぐ外側に置く。技術書典ログインと同じ配置
        // （WebView はネイティブ子ウィンドウで GPUI 要素より常に最前面。領域内だと隠れる）。
        let close_size = 36.0_f32;
        let close_gap = 10.0_f32;
        let mut close_left = left + width + close_gap;
        let close_top = top + close_gap;
        if close_left + close_size > window_w {
            close_left = left - close_gap - close_size;
        }
        div()
            .id("google-login-backdrop")
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
            .bg(gpui_kit::hsla(0.0, 0.0, 0.0, 0.45))
            .flex()
            .items_center()
            .justify_center()
            .child(
                div()
                    .id("google-login-cancel")
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
                                    cx.emit(GoogleLoginCancelled);
                                });
                            }
                        }
                    })
                    .child(Icon::new(IconName::Close).size(px(18.0))),
            )
            .child(if let Some(message) = self.error.clone() {
                div()
                    .max_w(px(420.0))
                    .p_4()
                    .rounded_lg()
                    .bg(gpui_kit::hsla(0.0, 0.0, 0.0, 0.75))
                    .text_color(gpui_kit::white())
                    .text_sm()
                    .child(message)
                    .into_any_element()
            } else {
                div().into_any_element()
            })
    }
}
