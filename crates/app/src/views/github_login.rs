//! GitHub の Device Flow（RFC 8628）でログインするモーダル。
//!
//! WebView は使わない。GitHub の Authorization Code フローは `client_secret` を要求し、
//! デスクトップアプリのバイナリに埋め込むのは GitHub のガイドライン違反になる。
//! Device Flow なら `client_id` だけで完結するため、こちらを使う。
//!
//! 流れ: device code を取得 → user_code を画面に出して既定ブラウザで
//! `https://github.com/login/device` を開く → バックグラウンドでポーリング →
//! 承認されたらトークンを keyring に保存して `github_login_done` を立てる。

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, IconName};
use gpui_kit::{
    ClipboardItem, Context, EventEmitter, InteractiveElement as _, IntoElement, ParentElement,
    ReadGlobal as _, Render, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};
use thundoku_core::github::{DEFAULT_SCOPE, DeviceCode, DeviceFlowPoll, GithubError};

use crate::app_state::{AppState, default_github_client_id, save_github_token};

/// ログイン成功（トークン保存とプロフィール取得まで完了）。
///
/// 注: 完了時の成功・失敗は `cx.emit` では通知しない（RefCell 再入でパニックしうるため）。
/// `AppState` の `github_login_done` / `github_login_error` を直接更新し、
/// Workspace の監視タスクがモーダルを閉じる（Google と同じ経路）。この型は
/// Google の 3 イベントと同じ形に揃えるために置いている。
pub struct GithubLoginDone;

/// ログイン失敗（エラーメッセージ付き）。実際に emit されるのは「client_id 未設定」の
/// ようにフローを開始できない場合だけ。進行中の失敗は `github_login_error` に入る。
pub struct GithubLoginFailed(pub String);

/// ログインキャンセル（✕ ボタンで閉じたとき）。
pub struct GithubLoginCancelled;

/// モーダルの表示状態。
enum Phase {
    /// device code を取得中。
    Requesting,
    /// ユーザーがブラウザでコードを入力するのを待っている。
    Waiting { code: DeviceCode },
    /// 失敗（メッセージを出して再試行できる）。
    Failed(String),
}

pub struct GithubLoginView {
    phase: Phase,
    /// ポーリングを止めるためのフラグ（✕ で立てる）。
    cancel: Option<Arc<AtomicBool>>,
}

impl GithubLoginView {
    pub fn new(_window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut this = Self {
            phase: Phase::Requesting,
            cancel: None,
        };
        this.start(cx);
        this
    }

    /// 端末フローを開始する（再試行でも呼ぶ）。
    fn start(&mut self, cx: &mut Context<Self>) {
        let client_id = default_github_client_id();
        if client_id.is_empty() {
            // client_id はビルド時に埋め込む。未設定なら GitHub ログインは無効。
            let message = "GitHub ログインが設定されていません（client_id 未設定）".to_string();
            self.phase = Phase::Failed(message.clone());
            cx.emit(GithubLoginFailed(message));
            return;
        }

        let github = AppState::global(cx).github.clone();
        let cancel = Arc::new(AtomicBool::new(false));
        self.cancel = Some(cancel.clone());
        self.phase = Phase::Requesting;
        // 弱参照: 画面を閉じたあとに完了しても、ビューを復活させない。
        let handle = cx.entity().downgrade();

        cx.spawn(async move |_, cx| {
            let executor = cx.background_executor().clone();

            // 1) device code を取る（ブロッキング HTTP は背景で実行する）。
            let request = executor.spawn({
                let github = github.clone();
                let client_id = client_id.clone();
                async move {
                    let mut guard = github.lock();
                    guard
                        .as_mut()
                        .map(|client| client.begin_device_flow(&client_id, DEFAULT_SCOPE))
                }
            });
            let code = match request.await {
                Some(Ok(code)) => code,
                Some(Err(error)) => {
                    finish_with_error(&handle, cx, error);
                    return;
                }
                None => {
                    finish_with_error(
                        &handle,
                        cx,
                        GithubError::Auth("GitHub クライアントが未設定です".to_string()),
                    );
                    return;
                }
            };

            // 2) コードを見せてブラウザを開く。
            if thundoku_core::google::open_browser(&code.verification_uri).is_err() {
                log::warn!(
                    "github login: ブラウザを開けませんでした: {}",
                    code.verification_uri
                );
            }
            let waiting_code = code.clone();
            let _ = handle.update(cx, |this, cx| {
                this.phase = Phase::Waiting {
                    code: waiting_code.clone(),
                };
                cx.notify();
            });

            // 3) 承認されるまでポーリングする（`interval` 秒間隔、`slow_down` で延びる）。
            let mut interval = code.interval.max(1);
            loop {
                if cancel.load(Ordering::SeqCst) {
                    return;
                }
                executor.timer(Duration::from_secs(interval)).await;
                if cancel.load(Ordering::SeqCst) {
                    return;
                }

                let poll = executor.spawn({
                    let github = github.clone();
                    let client_id = client_id.clone();
                    let device_code = code.device_code.clone();
                    async move {
                        let mut guard = github.lock();
                        guard
                            .as_mut()
                            .map(|client| client.poll_device_flow(&client_id, &device_code))
                    }
                });

                match poll.await {
                    Some(Ok(DeviceFlowPoll::Pending)) => {}
                    Some(Ok(DeviceFlowPoll::SlowDown { interval: next })) => {
                        // GitHub は新しい interval を返す。念のため単調増加にする。
                        interval = next.max(interval + 5);
                    }
                    Some(Ok(DeviceFlowPoll::Authorized(token))) => {
                        // 4) トークンを保存し、表示名のためにユーザーを引く。
                        let user = executor.spawn({
                            let github = github.clone();
                            async move {
                                let mut guard = github.lock();
                                guard.as_mut().and_then(|client| client.current_user().ok())
                            }
                        });
                        let user = user.await;
                        let _ = handle.update(cx, |_, cx| {
                            let state = AppState::global(cx);
                            save_github_token(cx, &token);
                            if let Some(user) = user {
                                *state.github_profile.lock() = Some(user);
                            }
                            // cx.emit は RefCell 再入でパニックしうるため、グローバル状態を
                            // 直接更新して Workspace の監視タスクに閉じさせる（Google と同じ）。
                            state.github_login_done.store(true, Ordering::SeqCst);
                        });
                        return;
                    }
                    Some(Err(GithubError::DeviceCodeExpired)) => {
                        finish_with_error(
                            &handle,
                            cx,
                            GithubError::Auth(
                                "コードの有効期限が切れました。もう一度お試しください".to_string(),
                            ),
                        );
                        return;
                    }
                    Some(Err(GithubError::AccessDenied)) => {
                        // ユーザーがブラウザで拒否した。エラー表示はせず閉じる。
                        let _ = handle.update(cx, |_, cx| {
                            AppState::global(cx)
                                .github_login_done
                                .store(true, Ordering::SeqCst);
                        });
                        return;
                    }
                    Some(Err(error)) => {
                        finish_with_error(&handle, cx, error);
                        return;
                    }
                    None => {
                        finish_with_error(
                            &handle,
                            cx,
                            GithubError::Auth("GitHub クライアントが未設定です".to_string()),
                        );
                        return;
                    }
                }
            }
        })
        .detach();
    }

    /// モーダルを表示する（フローは `new` で開始済み）。
    pub fn show(&mut self, cx: &mut Context<Self>) {
        cx.notify();
    }

    /// キャンセル/閉じる。ポーリングを止める。
    pub fn close(&mut self, cx: &mut Context<Self>) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::SeqCst);
        }
        self.cancel = None;
        cx.notify();
    }
}

/// 失敗を画面とグローバル状態の両方に反映する。
fn finish_with_error(
    handle: &gpui_kit::WeakEntity<GithubLoginView>,
    cx: &mut gpui_kit::AsyncApp,
    error: GithubError,
) {
    let message = error.to_string();
    log::warn!("github login failed: {message}");
    let _ = handle.update(cx, |this, cx| {
        this.phase = Phase::Failed(message.clone());
        {
            let state = AppState::global(cx);
            *state.github_login_error.lock() = Some(message);
            state.github_login_done.store(true, Ordering::SeqCst);
        }
        cx.notify();
    });
}

impl EventEmitter<GithubLoginDone> for GithubLoginView {}
impl EventEmitter<GithubLoginFailed> for GithubLoginView {}
impl EventEmitter<GithubLoginCancelled> for GithubLoginView {}

impl Render for GithubLoginView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let muted = theme.muted_foreground;
        let border = theme.border;

        let body = match &self.phase {
            Phase::Requesting => div()
                .text_sm()
                .text_color(muted)
                .child("コードを取得しています…")
                .into_any_element(),
            Phase::Waiting { code } => {
                let user_code = code.user_code.clone();
                let verification_uri = code.verification_uri.clone();
                div()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted)
                            .child("ブラウザが開きます。次のコードを入力してください。"),
                    )
                    .child(
                        div()
                            .debug_selector(|| "github-login-code".into())
                            .text_size(px(26.0))
                            .font_weight(gpui_kit::FontWeight::BOLD)
                            .child(user_code.clone()),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(
                                Button::new("github-login-copy")
                                    .cursor_pointer()
                                    .outline()
                                    .label("コードをコピー")
                                    .on_click({
                                        let user_code = user_code.clone();
                                        move |_, _window, cx| {
                                            cx.write_to_clipboard(ClipboardItem::new_string(
                                                user_code.clone(),
                                            ));
                                        }
                                    }),
                            )
                            .child(
                                div()
                                    .debug_selector(|| "github-login-open-browser".into())
                                    .child(
                                        Button::new("github-login-open-browser")
                                            .cursor_pointer()
                                            .outline()
                                            .label("ブラウザで開く")
                                            .on_click({
                                                let url = verification_uri.clone();
                                                move |_, _window, _cx| {
                                                    let _ =
                                                        thundoku_core::google::open_browser(&url);
                                                }
                                            }),
                                    ),
                            ),
                    )
                    .child(
                        div().text_xs().text_color(muted).child(
                            "入力が終わると自動でログインします（この画面は閉じられます）。",
                        ),
                    )
                    .into_any_element()
            }
            Phase::Failed(message) => div()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .debug_selector(|| "github-login-error".into())
                        .text_sm()
                        .text_color(gpui_kit::rgb(0xdc2626))
                        .child(message.clone()),
                )
                .child(
                    Button::new("github-login-retry")
                        .cursor_pointer()
                        .primary()
                        .label("やり直す")
                        .on_click({
                            let handle = cx.weak_entity();
                            move |_, _window, cx| {
                                if let Some(handle) = handle.upgrade() {
                                    handle.update(cx, |this, cx| {
                                        this.close(cx);
                                        this.start(cx);
                                    });
                                }
                            }
                        }),
                )
                .into_any_element(),
        };

        div()
            .id("github-login-backdrop")
            .debug_selector(|| "github-login-backdrop".into())
            // モーダルの下にあるレイヤ（サイドバー / 本棚 / workspace の dim）へ
            // クリックを伝播させない。`occlude` が無いと下の要素が反応してしまう。
            .occlude()
            .on_click(|_, _window, cx| cx.stop_propagation())
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
                    .id("github-login-card")
                    // カード内のクリックでモーダルが閉じないようにする（下の dim オーバーレイが
                    // CloseAuth を受けてしまう）。
                    .on_click(|_, _window, cx| cx.stop_propagation())
                    .w(px(420.0))
                    .p_5()
                    .rounded_lg()
                    .border_1()
                    .border_color(border)
                    .bg(theme.popover)
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(gpui_kit::FontWeight::BOLD)
                                    .child("GitHub にログイン"),
                            )
                            .child(
                                div()
                                    .id("github-login-cancel")
                                    .debug_selector(|| "github-login-cancel".into())
                                    .w(px(28.0))
                                    .h(px(28.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .cursor_pointer()
                                    .hover(|style| style.bg(theme.muted))
                                    .on_click({
                                        let handle = cx.weak_entity();
                                        move |_, _window, cx| {
                                            if let Some(handle) = handle.upgrade() {
                                                handle.update(cx, |this, cx| {
                                                    this.close(cx);
                                                    cx.emit(GithubLoginCancelled);
                                                });
                                            }
                                        }
                                    })
                                    .child(Icon::new(IconName::Close).size(px(16.0))),
                            ),
                    )
                    .child(body)
                    .child(
                        div()
                            .text_xs()
                            .text_color(muted)
                            .child("レポート機能で使うアクセストークンだけを保存します。"),
                    ),
            )
    }
}
