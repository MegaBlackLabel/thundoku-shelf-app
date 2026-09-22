//! Google OAuth ログインを**システムブラウザ**で行うモーダル。
//!
//! RFC 8252（ネイティブアプリは外部ユーザーエージェントを使う）と、Google が
//! 埋め込みユーザーエージェントを拒否する方針（`disallowed_useragent`）に従い、
//! 認可ページはアプリ内 WebView ではなく OS の既定ブラウザで開く。認証情報の入力面を
//! アプリのプロセスから分離でき、WebView に残る認証状態も持たない。
//!
//! ループバック受信（`127.0.0.1`）・PKCE S256・`state` 検証は従来のまま
//! （`GoogleClient::begin_authorize` / `finish_authorize`）。ブラウザが開けなかった
//! 場合に備えて認可 URL を画面に出し、手で開いてもらう経路を残す。

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::button::Button;
use gpui_kit::component::{Icon, IconName};
use gpui_kit::{
    ClipboardItem, Context, EventEmitter, InteractiveElement as _, IntoElement, ParentElement,
    ReadGlobal as _, Render, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use thundoku_core::google::{GoogleError, GoogleProfile, PendingGoogleAuth};

use crate::app_state::AppState;

/// ログイン成功イベント（プロフィール取得まで完了）。
pub struct GoogleLoginDone(pub GoogleProfile);

/// ログイン失敗イベント（エラーメッセージ付き）。
pub struct GoogleLoginFailed(pub String);

/// ログインキャンセルイベント（✕ ボタンで閉じたとき）。
pub struct GoogleLoginCancelled;

pub struct GoogleLoginView {
    /// 認可フローのキャンセルフラグ（✕ ボタンで立てる）
    cancel: Option<Arc<AtomicBool>>,
    /// 認可 URL（ブラウザを開けなかったときに手で開いてもらう）
    url: Option<String>,
    /// 表示用のエラー（開始できなかった・ブラウザを開けなかった）
    error: Option<String>,
}

impl GoogleLoginView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        // AppState の GoogleClient から認可フローを開始し、認可 URL を既定ブラウザで開く。
        let google = AppState::global(cx).google.clone();
        let pending = google
            .lock()
            .as_ref()
            .and_then(|client| client.begin_authorize().ok());
        let mut this = Self {
            cancel: None,
            url: None,
            error: None,
        };
        match pending {
            Some(pending) => {
                this.cancel = Some(pending.cancel_handle());
                this.url = Some(pending.url.clone());
                // `authorize()` と同じく、URL には state と code_challenge が入るため
                // ログにはホストと path だけを残す。
                log::info!(
                    "google login: ブラウザで認可を開始 {}",
                    pending
                        .url
                        .split(['?', '#'])
                        .next()
                        .unwrap_or("https://accounts.google.com/o/oauth2/v2/auth")
                );
                if let Err(error) = thundoku_core::google::open_browser(&pending.url) {
                    // 自動で開けなくても、URL を出せば手で続行できる。
                    log::warn!("google login: ブラウザを開けません: {error}");
                    this.error = Some(
                        "ブラウザを自動で開けませんでした。下の URL を開いてください。".to_string(),
                    );
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

    /// ループバック受信 → トークン交換 → プロフィール取得をバックグラウンドで実行する。
    fn start_finish(
        &mut self,
        pending: PendingGoogleAuth,
        google: Arc<parking_lot::Mutex<Option<thundoku_core::google::GoogleClient>>>,
        cx: &mut Context<Self>,
    ) {
        // 弱参照にする: 監視タスクはアプリ寿命で動き続けるため、強参照を持つと
        // ログイン画面を閉じてもビューが解放されない。
        let handle = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = cx.background_executor().spawn({
                let google = google.clone();
                async move {
                    // コールバック待ち（最大 5 分）の間はクライアントのロックを取らない。
                    // 取ると待っている間に UI 側の `google.lock()`（設定画面の表示・
                    // Drive 同期）が止まり、モーダルの ✕ も効かなくなる（固まって見える）。
                    // 交換とプロフィール取得のときだけロックする。
                    let code = thundoku_core::google::wait_for_code(&pending)?;
                    let mut guard = google.lock();
                    guard
                        .as_mut()
                        .map(|client| client.complete_authorize(&pending, &code))
                        .unwrap_or(Err(GoogleError::Auth(
                            "Google クライアントが未設定です".into(),
                        )))
                }
            });
            let result = result.await;
            // ビューが閉じられていたら（弱参照が切れていたら）何もしない
            let _ = handle.update(cx, |this, cx| {
                this.cancel = None;
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
                        // プロフィール（sub）は keyring に保存する（次回起動の所有者判定用）。
                        crate::app_state::save_google_profile(cx, &profile);
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

    /// キャンセル/閉じる。
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        cx.notify();
    }
}

impl EventEmitter<GoogleLoginDone> for GoogleLoginView {}
impl EventEmitter<GoogleLoginFailed> for GoogleLoginView {}
impl EventEmitter<GoogleLoginCancelled> for GoogleLoginView {}

impl Render for GoogleLoginView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;
        let url = self.url.clone();
        let handle = cx.weak_entity();

        div()
            .id("google-login-card")
            .debug_selector(|| "google-login-card".into())
            // 下のレイヤ（workspace の dim オーバーレイ）へクリックを伝播させない
            // （伝播すると CloseAuth が走ってモーダルが閉じてしまう）。
            .occlude()
            .on_click(|_, _window, cx| cx.stop_propagation())
            .w(px(448.0))
            .rounded_xl()
            .border_1()
            .border_color(border)
            .bg(theme.popover)
            .shadow_lg()
            .p_6()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .child(
                        div()
                            .text_xl()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child("Google でログイン"),
                    )
                    .child(
                        div()
                            .id("google-login-cancel")
                            .debug_selector(|| "google-login-cancel".into())
                            .cursor_pointer()
                            .child(Icon::new(IconName::Close).size(px(18.0)))
                            .on_click(move |_, _window, cx| {
                                if let Some(handle) = handle.upgrade() {
                                    handle.update(cx, |this, cx| {
                                        this.close(cx);
                                        cx.emit(GoogleLoginCancelled);
                                    });
                                }
                            }),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(muted)
                    .child("ブラウザで Google アカウントにログインしてください。認証が終わると自動で続行します。"),
            )
            .child(if let Some(message) = self.error.clone() {
                div()
                    .debug_selector(|| "google-login-error".into())
                    .text_xs()
                    .text_color(gpui_kit::rgb(0xdc2626))
                    .child(message)
                    .into_any_element()
            } else {
                div().into_any_element()
            })
            .child(if let Some(url) = url {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("ブラウザが開かないときは、この URL を開いてください。"),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            // URL は長い（PKCE と state を含む）ので、カードからはみ出させず
                            // 省略表示にする（全文は「URL をコピー」「ブラウザで開く」で使える）。
                            // `min_w_0` が無いと flex 子の最小幅で押し広げられてあふれる。
                            .child(
                                div()
                                    .text_xs()
                                    .min_w_0()
                                    .flex_1()
                                    .truncate()
                                    .child(url.clone()),
                            )
                            .child(
                                Button::new("google-login-copy-url")
                                    .cursor_pointer()
                                    .outline()
                                    .label("URL をコピー")
                                    .on_click({
                                        let url = url.clone();
                                        move |_, _window, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                url.clone(),
                                            ));
                                        }
                                    }),
                            )
                            .child(
                                Button::new("google-login-open-browser")
                                    .cursor_pointer()
                                    .outline()
                                    .label("ブラウザで開く")
                                    .on_click({
                                        let url = url.clone();
                                        move |_, _window, _cx| {
                                            if let Err(error) =
                                                thundoku_core::google::open_browser(&url)
                                            {
                                                log::warn!(
                                                    "google login: ブラウザを開けません: {error}"
                                                );
                                            }
                                        }
                                    }),
                            ),
                    )
                    .into_any_element()
            } else {
                div().into_any_element()
            })
            .child(
                div()
                    .text_xs()
                    .text_color(muted)
                    .child("この画面の ✕ でいつでも中止できます。"),
            )
    }
}
