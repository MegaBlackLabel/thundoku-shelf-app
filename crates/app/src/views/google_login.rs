//! Google OAuth ログインをアプリ内 WebView（gpui-wry）で行うビュー。
//!
//! BOOTH と同じく、Google の認可ページを WebView で開き、ユーザーが
//! WebView 内で Google アカウントにログインしてリダイレクトがループバック
//! アドレス（127.0.0.1）に戻ったら、code をループバック受信 → トークン交換
//! → プロフィール取得まで自動で行う（システムブラウザは開かない）。

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gpui::{
    AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _, IntoElement,
    ParentElement, ReadGlobal as _, Render, StatefulInteractiveElement as _, Styled as _, Window,
    div, px,
};
use gpui_component::{Icon, IconName};
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
        let window_handle = window.window_handle().ok()?;
        let webview = builder.build_as_child(&window_handle).ok()?;
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
                        cx.emit(GoogleLoginDone(profile));
                    }
                    // キャンセルは ✕ ボタンが既に GoogleLoginCancelled を発行済み
                    Err(GoogleError::Cancelled) => {}
                    Err(error) => cx.emit(GoogleLoginFailed(error.to_string())),
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
            .id("google-login-backdrop")
            .absolute()
            .top_0()
            .right_0()
            .bottom_0()
            .left_0()
            .bg(gpui::hsla(0.0, 0.0, 0.0, 0.45))
            .flex()
            .items_center()
            .justify_center()
            // 閉じるボタンはウィンドウ右上（WebView 領域の外側）に配置
            .child(
                div()
                    .id("google-login-cancel")
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
                    .bg(gpui::hsla(0.0, 0.0, 0.0, 0.75))
                    .text_color(gpui::white())
                    .text_sm()
                    .child(message)
                    .into_any_element()
            } else {
                div().into_any_element()
            })
    }
}
