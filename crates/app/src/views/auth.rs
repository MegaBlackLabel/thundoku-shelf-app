//! 認証ダイアログ: 技術書典・Google・BOOTH をアプリ内 WebView（gpui-wry）でログインする。
//! 各プロバイダは WebView でログイン画面を開き、Cookie / 認可フローを自動処理する。

use gpui_kit::{AppContext as _, ReadGlobal as _, Styled as _};
use gpui_kit::{
    Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement, Render,
    StatefulInteractiveElement as _, Window, deferred, div, px,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName};
use thundoku_core::google::GoogleProfile;

use crate::app_state::AppState;
use crate::views::booth_login::{BoothLoginCancelled, BoothLoginDone, BoothLoginView};
use crate::views::fanza_login::{FanzaLoginCancelled, FanzaLoginDone, FanzaLoginView};
use crate::views::google_login::{
    GoogleLoginCancelled, GoogleLoginDone, GoogleLoginFailed, GoogleLoginView,
};
use crate::views::tbf_login::{TbfLoginCancelled, TbfLoginDone, TbfLoginView};

/// ログインプロバイダ（Web の LoginPrompt の provider 相当）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AuthProvider {
    TechBookFest,
    Google,
    Booth,
    Fanza,
}

pub struct AuthDialog {
    /// None = プロバイダ選択画面
    provider: Option<AuthProvider>,
    error: Option<String>,
    tbf_logged_in: bool,
    google_profile: Option<GoogleProfile>,
    /// BOOTH の WebView ログインモーダルを表示中か
    show_booth_login: bool,
    booth_login: Option<Entity<BoothLoginView>>,
    /// BoothLoginView の完了イベント購読（保持して drop を防ぐ）
    booth_subscription: Option<gpui_kit::Subscription>,
    /// FANZA の WebView ログインモーダルを表示中か
    show_fanza_login: bool,
    fanza_login: Option<Entity<FanzaLoginView>>,
    /// FanzaLoginView の完了イベント購読（保持して drop を防ぐ）
    fanza_subscription: Option<gpui_kit::Subscription>,
    /// Google の WebView ログインモーダルを表示中か
    show_google_login: bool,
    google_login: Option<Entity<GoogleLoginView>>,
    google_subscription: Option<gpui_kit::Subscription>,
    /// 技術書典の WebView ログインモーダルを表示中か
    show_tbf_login: bool,
    tbf_login: Option<Entity<TbfLoginView>>,
    tbf_subscription: Option<gpui_kit::Subscription>,
}

impl AuthDialog {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            provider: None,
            error: None,
            tbf_logged_in: false,
            google_profile: None,
            show_booth_login: false,
            booth_login: None,
            booth_subscription: None,
            show_fanza_login: false,
            fanza_login: None,
            fanza_subscription: None,
            show_google_login: false,
            google_login: None,
            google_subscription: None,
            show_tbf_login: false,
            tbf_login: None,
            tbf_subscription: None,
        }
    }

    /// 指定プロバイダのログイン画面で開く（サイドバー・設定から呼ぶ）。
    /// 未指定（None）なら技術書典（主プロバイダ）で開く。
    /// プロバイダ選択画面は廃止: モーダルは直接ログイン画面を表示する。
    pub fn open_with_provider(&mut self, provider: Option<AuthProvider>) {
        let provider = provider.unwrap_or(AuthProvider::TechBookFest);
        self.provider = Some(provider);
        self.error = None;
        // プロバイダを指定したら中間の「〜ログイン画面を開く」ボタンを経由せず、
        // そのまま WebView ログインモーダルを開く（Windows の dcomp 競合は
        // GPUI_DISABLE_DIRECT_COMPOSITION で解消済みのため遅延は不要）。
        match provider {
            AuthProvider::TechBookFest => self.show_tbf_login = true,
            AuthProvider::Google => self.show_google_login = true,
            AuthProvider::Booth => self.show_booth_login = true,
            AuthProvider::Fanza => self.show_fanza_login = true,
        }
    }

    /// 現在のプロバイダ（テスト用）。
    #[cfg(test)]
    pub(crate) fn provider(&self) -> Option<AuthProvider> {
        self.provider
    }

    /// BOOTH の WebView ログインモーダルを表示中か（テスト用）。
    #[cfg(test)]
    pub(crate) fn show_booth_login(&self) -> bool {
        self.show_booth_login
    }

    /// Google の WebView ログインモーダルを表示中か（テスト用）。
    #[cfg(test)]
    pub(crate) fn show_google_login(&self) -> bool {
        self.show_google_login
    }

    /// 技術書典の WebView ログインモーダルを表示中か（テスト用）。
    #[cfg(test)]
    pub(crate) fn show_tbf_login(&self) -> bool {
        self.show_tbf_login
    }

    /// WebView（wry）は実ウィンドウが必要なため、ログインモーダルを
    /// 開いたときだけ作成する（テスト環境で WebView を作らない）。
    fn ensure_states(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // WebView 生成はウィンドウ借用中の RefCell 再入で固まるため render 後に defer する。
        if self.show_booth_login && self.booth_login.is_none() {
            cx.defer_in(window, |this, window, cx| {
                let booth_login = cx.new(|cx| BoothLoginView::new(window, cx));
                // ログイン成功: WebView を閉じてモーダル全体も閉じる
                let _done = cx.subscribe(
                    &booth_login,
                    |this: &mut Self, _: Entity<BoothLoginView>, _: &BoothLoginDone, cx| {
                        this.show_booth_login = false;
                        // 次回は新しい WebView + 監視を開始する
                        this.booth_login = None;
                        this.booth_subscription = None;
                        cx.defer(|cx| cx.dispatch_action(&crate::actions::CloseAuth));
                    },
                );
                // キャンセル: WebView を破棄してログイン画面に戻る（次回は新規フロー）
                let _cancelled = cx.subscribe(
                    &booth_login,
                    |this: &mut Self, _: Entity<BoothLoginView>, _: &BoothLoginCancelled, _| {
                        this.show_booth_login = false;
                        this.booth_login = None;
                        this.booth_subscription = None;
                    },
                );
                this.booth_subscription = Some(_done);
                this.booth_login = Some(booth_login);
                cx.notify();
            });
        }
        // FANZA: WebView ログイン（テスト環境では WebView を作らない）
        if self.show_fanza_login && self.fanza_login.is_none() {
            cx.defer_in(window, |this, window, cx| {
                let fanza_login = cx.new(|cx| FanzaLoginView::new(window, cx));
                // 成功: WebView を閉じてモーダル全体も閉じる
                let _done = cx.subscribe(
                    &fanza_login,
                    |this: &mut Self, _: Entity<FanzaLoginView>, _: &FanzaLoginDone, cx| {
                        this.show_fanza_login = false;
                        this.fanza_login = None;
                        this.fanza_subscription = None;
                        cx.defer(|cx| cx.dispatch_action(&crate::actions::CloseAuth));
                    },
                );
                // キャンセル: WebView を破棄してログイン画面に戻る
                let _cancelled = cx.subscribe(
                    &fanza_login,
                    |this: &mut Self, _: Entity<FanzaLoginView>, _: &FanzaLoginCancelled, _| {
                        this.show_fanza_login = false;
                        this.fanza_login = None;
                        this.fanza_subscription = None;
                    },
                );
                this.fanza_subscription = Some(_done);
                this.fanza_login = Some(fanza_login);
                cx.notify();
            });
        }
        // Google: WebView（wry）は実ウィンドウが必要なため、ログインモーダルを
        // 開いたときだけ作成する（テスト環境で WebView を作らない）。
        // ウィンドウ借用中の WebView 生成は RefCell 再入でウィンドウ描画が固まるため、
        // render 後に defer_in で生成する。
        if self.show_google_login && self.google_login.is_none() {
            cx.defer_in(window, |this, window, cx| {
                let google_login = cx.new(|cx| GoogleLoginView::new(window, cx));
                // 成功: プロフィールを保存してモーダル全体を閉じる
                let _done = cx.subscribe(
                    &google_login,
                    |this: &mut Self, _: Entity<GoogleLoginView>, event: &GoogleLoginDone, cx| {
                        let profile = event.0.clone();
                        this.google_profile = Some(profile.clone());
                        let state = AppState::global(cx);
                        *state.google_profile.lock() = Some(profile);
                        *state.google_logged_in.lock() = true;
                        *state.google_login_error.lock() = None;
                        // WebView は完了処理で隠される。次回は新しい認可フローを開始する
                        this.google_login = None;
                        this.google_subscription = None;
                        // Workspace の状態更新は、RefCell already borrowed でアプリが固まるため
                        // ここでは行わない。グローバルフラグを立て、Workspace の監視タスクが
                        // show_auth をリセットする（Workspace::new で開始）。
                        AppState::global(cx)
                            .google_login_done
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                    },
                );
                // 失敗: エラーを保持してモーダル全体を閉じる（成功時と同じ挙動）
                let _failed = cx.subscribe(
                    &google_login,
                    |this: &mut Self, _: Entity<GoogleLoginView>, event: &GoogleLoginFailed, cx| {
                        this.show_google_login = false;
                        this.google_login = None;
                        this.google_subscription = None;
                        let state = AppState::global(cx);
                        *state.google_login_error.lock() = Some(event.0.clone());
                        AppState::global(cx)
                            .google_login_done
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                    },
                );
                // キャンセル: WebView を破棄してログイン画面に戻る（次回は新規フロー）
                let _cancelled = cx.subscribe(
                    &google_login,
                    |this: &mut Self, _: Entity<GoogleLoginView>, _: &GoogleLoginCancelled, _| {
                        this.show_google_login = false;
                        this.google_login = None;
                        this.google_subscription = None;
                    },
                );
                this.google_subscription = Some(_done);
                this.google_login = Some(google_login);
                cx.notify();
            });
        }
        // 技術書典: WebView ログイン（テスト環境では WebView を作らない）
        if self.show_tbf_login && self.tbf_login.is_none() {
            cx.defer_in(window, |this, window, cx| {
                let tbf_login = cx.new(|cx| TbfLoginView::new(window, cx));
                // 成功: モーダル全体を閉じる
                let _done = cx.subscribe(
                    &tbf_login,
                    |this: &mut Self, _: Entity<TbfLoginView>, _: &TbfLoginDone, cx| {
                        this.tbf_logged_in = true;
                        this.show_tbf_login = false;
                        // 次回は新しい WebView + 監視を開始する
                        this.tbf_login = None;
                        this.tbf_subscription = None;
                        cx.defer(|cx| cx.dispatch_action(&crate::actions::CloseAuth));
                    },
                );
                // キャンセル: WebView を破棄してログイン画面に戻る（次回は新規フロー）
                let _cancelled = cx.subscribe(
                    &tbf_login,
                    |this: &mut Self, _: Entity<TbfLoginView>, _: &TbfLoginCancelled, _| {
                        this.show_tbf_login = false;
                        this.tbf_login = None;
                        this.tbf_subscription = None;
                    },
                );
                this.tbf_subscription = Some(_done);
                this.tbf_login = Some(tbf_login);
                cx.notify();
            });
        }
    }

    pub fn login_google(&mut self, cx: &mut Context<Self>) {
        // WebView モーダルで Google 認可を開始する（システムブラウザは開かない）
        self.show_google_login = true;
        cx.notify();
    }

    pub fn login_tbf(&mut self, cx: &mut Context<Self>) {
        // WebView モーダルで技術書典ログインを開始する
        self.show_tbf_login = true;
        cx.notify();
    }
}

impl Render for AuthDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_states(window, cx);
        // 各プロバイダの WebView ログインモーダル表示中は WebView を可視化する
        if self.show_booth_login
            && let Some(booth) = &self.booth_login
        {
            booth.update(cx, |view, cx| view.show(cx));
        }
        if self.show_google_login
            && let Some(google) = &self.google_login
        {
            google.update(cx, |view, cx| view.show(cx));
        }
        if self.show_tbf_login
            && let Some(tbf) = &self.tbf_login
        {
            tbf.update(cx, |view, cx| view.show(cx));
        }
        if self.show_fanza_login
            && let Some(fanza) = &self.fanza_login
        {
            fanza.update(cx, |view, cx| view.show(cx));
        }
        let error = self.error.clone();
        let tbf_logged_in = self.tbf_logged_in;
        let google_profile = self.google_profile.clone();
        let provider = self.provider;
        let handle = cx.entity();

        // Web の LoginPrompt 相当: 中央モーダル（max-w-md = 448px）+ バックドロップ。
        // バックドロップは workspace 側が描画し、クリックで CloseAuth する。
        // Web の LoginPrompt 相当: 背景は透明（workspace の dim オーバーレイを
        // 通して見せる）。モーダル自体は中央・max-w-md 相当。
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            // 各プロバイダの WebView ログインモーダル（deferred で最前面表示）
            // ログインビューは defer_in で render 後に生成されるため、まだ無いフレームは空を出す。
            .child(if self.show_booth_login {
                match &self.booth_login {
                    Some(booth) => deferred(booth.clone()).into_any_element(),
                    None => div().into_any_element(),
                }
            } else if self.show_google_login {
                match &self.google_login {
                    Some(google) => deferred(google.clone()).into_any_element(),
                    None => div().into_any_element(),
                }
            } else if self.show_tbf_login {
                match &self.tbf_login {
                    Some(tbf) => deferred(tbf.clone()).into_any_element(),
                    None => div().into_any_element(),
                }
            } else if self.show_fanza_login {
                match &self.fanza_login {
                    Some(fanza) => deferred(fanza.clone()).into_any_element(),
                    None => div().into_any_element(),
                }
            } else {
                div().into_any_element()
            })
            .child(if self.show_booth_login || self.show_google_login || self.show_tbf_login || self.show_fanza_login {
                // WebView ログインモーダル表示中は auth-modal（中央モーダル）を
                // 重ねない。deferred の WebView が最前面に出るため不要な見た目になる。
                div().into_any_element()
            } else {
                div()
                    .id("auth-modal")
                    .debug_selector(|| "auth-modal".into())
                    .w(px(448.0))
                    .max_h(px(560.0))
                    .rounded_xl()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().background)
                    .shadow_lg()
                    .p_6()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .cursor_pointer()
                    .on_click(|_, _window, cx| cx.stop_propagation())
                    .overflow_y_scrollbar()
                    // ヘッダー: タイトル + ✕（Web の XIcon 相当）
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .child(div().text_xl().font_weight(FontWeight::SEMIBOLD).child(
                                match provider {
                                    Some(AuthProvider::Google) => "Googleでログイン",
                                    Some(AuthProvider::TechBookFest) => "技術書典でログイン",
                                    Some(AuthProvider::Booth) => "BOOTHでログイン",
                                    Some(AuthProvider::Fanza) => "FANZAでログイン",
                                    None => "ログイン",
                                },
                            ))
                            .child(
                                div()
                                    .id("auth-close-btn")
                                    .w(px(32.0))
                                    .h(px(32.0))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded_md()
                                    .text_color(cx.theme().muted_foreground)
                                    .hover(|style| style.bg(cx.theme().secondary))
                                    .cursor_pointer()
                                    .on_click(|_, _window, cx| {
                                        cx.defer(move |cx| {
                                            cx.dispatch_action(&crate::actions::CloseAuth)
                                        });
                                    })
                                    .child(Icon::new(IconName::Close).size(px(16.0))),
                            ),
                    )
                    // エラー表示（Web の destructive アラート相当）
                    .child(if let Some(message) = error.clone() {
                        div()
                            .p_3()
                            .rounded_lg()
                            .bg(cx.theme().danger.opacity(0.1))
                            .border_1()
                            .border_color(cx.theme().danger.opacity(0.2))
                            .text_color(cx.theme().danger)
                            .text_sm()
                            .child(message)
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    })
                    // ログイン済み表示
                    .child(if tbf_logged_in {
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child("技術書典にログイン済みです")
                            .into_any_element()
                    } else if google_profile.is_some() {
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "Google ログイン済み: {}",
                                google_profile
                                    .as_ref()
                                    .map(|p| p.email.clone())
                                    .unwrap_or_default()
                            ))
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    })
                    // 本体: プロバイダ選択 or フォーム（Web の LoginPrompt 相当）
                    .child(match provider {
                        // 選択画面は廃止（モーダルは直接ログイン画面を表示する）
                        None => div().into_any_element(),
                        // Google: WebView で Google 認可（モーダル内で完結）
                        Some(AuthProvider::Google) => div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        "Google のログインはアプリ内ブラウザ（WebView）で行います。\nログイン画面で Google アカウントにログインすると、セッションが自動で保存されます。",
                                    ),
                            )
                            .child(
                                Button::new("auth-google-open")
                                    .cursor_pointer()
                                    .primary()
                                    .label("Googleログイン画面を開く")
                                    .on_click({
                                        let handle = handle.clone();
                                        move |_, _window, cx| {
                                            handle.update(cx, |this, cx| {
                                                this.show_google_login = true;
                                                cx.notify();
                                            });
                                        }
                                    }),
                            )
                            .into_any_element(),
                        // FANZA同人: WebView でログイン（モーダル内で完結）
                        Some(AuthProvider::Fanza) => div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        "FANZA のログインはアプリ内ブラウザ（WebView）で行います。\nログイン画面でメールアドレスとパスワードを入力すると、セッションが自動で保存されます。",
                                    ),
                            )
                            .child(
                                Button::new("auth-fanza-open")
                                    .cursor_pointer()
                                    .primary()
                                    .label("FANZAログイン画面を開く")
                                    .on_click({
                                        let handle = handle.clone();
                                        move |_, _window, cx| {
                                            handle.update(cx, |this, cx| {
                                                this.show_fanza_login = true;
                                                cx.notify();
                                            });
                                        }
                                    }),
                            )
                            .into_any_element(),
                        // BOOTH: WebView で pixiv ログイン（モーダル内で完結）
                        Some(AuthProvider::Booth) => div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        "BOOTH のログインはアプリ内ブラウザ（WebView）で行います。\nログイン画面で pixiv アカウントにログインすると、セッションが自動で保存されます。",
                                    ),
                            )
                            .child(
                                Button::new("auth-booth-open")
                                    .cursor_pointer()
                                    .primary()
                                    .label("BOOTHログイン画面を開く")
                                    .on_click({
                                        let handle = handle.clone();
                                        move |_, _window, cx| {
                                            handle.update(cx, |this, cx| {
                                                this.show_booth_login = true;
                                                cx.notify();
                                            });
                                        }
                                    }),
                            )
                            .into_any_element(),
                        // 技術書典: email/password フォーム
                        Some(AuthProvider::TechBookFest) => div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        "技術書典のログインはアプリ内ブラウザ（WebView）で行います。\nログイン画面でメールアドレスとパスワードを入力すると、セッションが自動で保存されます。",
                                    ),
                            )
                            .child(
                                Button::new("auth-tbf-open")
                                    .cursor_pointer()
                                    .primary()
                                    .label("技術書典ログイン画面を開く")
                                    .on_click({
                                        let handle = handle.clone();
                                        move |_, _window, cx| {
                                            handle.update(cx, |this, cx| {
                                                this.show_tbf_login = true;
                                                cx.notify();
                                            });
                                        }
                                    }),
                            )
                            .into_any_element(),
                    })
                    .into_any_element()
            })
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::AppContext as _;
    use gpui_kit::TestAppContext;

    use super::{AuthDialog, AuthProvider};
    use crate::app_state::AppState;

    #[gpui_kit::test]
    async fn tbf_provider_opens_webview_login_flag(cx: &mut TestAppContext) {
        cx.update(AppState::init_test);
        let dialog = cx.new(AuthDialog::new);
        dialog.update(cx, |dialog, _| {
            dialog.open_with_provider(Some(AuthProvider::TechBookFest));
        });
        // プロバイダ指定で WebView ログインモーダルを直接開く（遅延なし）。
        assert!(
            dialog.read_with(cx, |d, _| d.show_tbf_login()),
            "TechBookFest provider should open webview login directly"
        );
    }
}

#[cfg(test)]
mod dialog_render_tests {
    use gpui_kit::AppContext as _;
    use gpui_kit::TestAppContext;

    use crate::app_state::AppState;

    use super::*;

    #[gpui_kit::test]
    async fn auth_dialog_renders_without_panic(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        let dialog = cx.new(AuthDialog::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(480.0),
                height: gpui_kit::px(360.0),
            },
            |window, cx| gpui_kit::component::Root::new(dialog.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        visual.update(|window, cx| {
            let arena_clear = window.draw(cx);
            arena_clear.clear(cx);
        });
        // reaching here without panicking = render works
    }

    #[gpui_kit::test]
    async fn open_without_provider_falls_back_to_techbookfest(cx: &mut TestAppContext) {
        cx.update(AppState::init_test);
        let dialog = cx.new(AuthDialog::new);
        dialog.update(cx, |dialog, _| {
            dialog.open_with_provider(None);
        });
        // プロバイダ未指定は技術書典（主プロバイダ）で開く（選択画面は廃止）
        assert_eq!(
            dialog.read_with(cx, |d, _| d.provider()),
            Some(AuthProvider::TechBookFest)
        );
    }

    #[gpui_kit::test]
    async fn booth_provider_opens_webview_login_flag(cx: &mut TestAppContext) {
        cx.update(AppState::init_test);
        let dialog = cx.new(AuthDialog::new);
        dialog.update(cx, |dialog, _| {
            dialog.open_with_provider(Some(AuthProvider::Booth));
        });
        // プロバイダ指定で WebView ログインモーダルを直接開く（遅延なし）。
        assert!(
            dialog.read_with(cx, |d, _| d.show_booth_login()),
            "Booth provider should open webview login directly"
        );
    }

    #[gpui_kit::test]
    async fn google_provider_opens_webview_login_flag(cx: &mut TestAppContext) {
        cx.update(AppState::init_test);
        let dialog = cx.new(AuthDialog::new);
        dialog.update(cx, |dialog, _| {
            dialog.open_with_provider(Some(AuthProvider::Google));
        });
        // プロバイダ指定で WebView ログインモーダルを直接開く（遅延なし）。
        assert!(
            dialog.read_with(cx, |d, _| d.show_google_login()),
            "Google provider should open webview login directly"
        );
    }

    #[gpui_kit::test]
    async fn overlay_open_and_close_actions_toggle_workspace(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        let workspace = cx.new(crate::workspace::Workspace::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(900.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(workspace.clone(), window, cx),
        );
        let mut visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        visual.update(|window, cx| {
            let arena_clear = window.draw(cx);
            arena_clear.clear(cx);
        });
        assert!(!workspace.read_with(cx, |w, _| w.show_auth));
        let _ = &mut visual;
        visual.dispatch_action(crate::actions::OpenAuth);
        assert!(workspace.read_with(cx, |w, _| w.show_auth));
        // overlay renders inside the existing window (no new window)
        visual.update(|window, cx| {
            let arena_clear = window.draw(cx);
            arena_clear.clear(cx);
        });
        visual.dispatch_action(crate::actions::CloseAuth);
        assert!(!workspace.read_with(cx, |w, _| w.show_auth));
    }
}
