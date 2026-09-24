//! 認証ダイアログ: 技術書典・Google・BOOTH をアプリ内 WebView（gpui-wry）でログインする。
//! 各プロバイダは WebView でログイン画面を開き、Cookie / 認可フローを自動処理する。

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName};
use gpui_kit::{AppContext as _, ReadGlobal as _, Styled as _};
use gpui_kit::{
    Context, Entity, FontWeight, InteractiveElement as _, IntoElement, ParentElement, Render,
    StatefulInteractiveElement as _, Window, deferred, div, px,
};
use thundoku_core::google::GoogleProfile;

use crate::app_state::AppState;
use crate::views::booth_login::{BoothLoginCancelled, BoothLoginDone, BoothLoginView};
use crate::views::dlsite_login::{DlsiteLoginCancelled, DlsiteLoginDone, DlsiteLoginView};
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
    Dlsite,
}

impl AuthProvider {
    /// 本の供給元（サイト）なら、本棚の絞り込みと同じサイト id を返す。
    ///
    /// Google はアカウント連携用で本の供給元ではないため `None`
    /// （本棚の同期は無い）。
    fn site_sync_id(self) -> Option<&'static str> {
        match self {
            AuthProvider::TechBookFest => Some("techbookfest"),
            AuthProvider::Booth => Some("booth"),
            AuthProvider::Fanza => Some("fanza"),
            AuthProvider::Dlsite => Some("dlsite"),
            AuthProvider::Google => None,
        }
    }
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
    /// BoothLoginView のイベント購読（Done / Cancelled の 2 本）。束縛せずに捨てるとイベントが届かなくなる（✕ や完了でモーダルが閉じなくなる）
    booth_subscriptions: Vec<gpui_kit::Subscription>,
    /// FANZA の WebView ログインモーダルを表示中か
    show_fanza_login: bool,
    fanza_login: Option<Entity<FanzaLoginView>>,
    /// FanzaLoginView のイベント購読（Done / Cancelled の 2 本）。束縛せずに捨てるとイベントが届かなくなる（✕ や完了でモーダルが閉じなくなる）
    fanza_subscriptions: Vec<gpui_kit::Subscription>,
    /// DLsite の WebView ログインモーダルを表示中か
    show_dlsite_login: bool,
    dlsite_login: Option<Entity<DlsiteLoginView>>,
    /// DlsiteLoginView のイベント購読（Done / Cancelled の 2 本）。束縛せずに捨てるとイベントが届かなくなる（✕ や完了でモーダルが閉じなくなる）
    dlsite_subscriptions: Vec<gpui_kit::Subscription>,
    /// Google の WebView ログインモーダルを表示中か
    show_google_login: bool,
    google_login: Option<Entity<GoogleLoginView>>,
    /// Google ログインの購読（Done / Failed / Cancelled の 3 本）。束縛せずに捨てると
    /// イベントが届かなくなる（✕ やエラーでモーダルが閉じなくなる）。
    google_subscriptions: Vec<gpui_kit::Subscription>,
    /// 技術書典の WebView ログインモーダルを表示中か
    show_tbf_login: bool,
    tbf_login: Option<Entity<TbfLoginView>>,
    /// TbfLoginView のイベント購読（Done / Cancelled の 2 本）。束縛せずに捨てるとイベントが届かなくなる（✕ や完了でモーダルが閉じなくなる）。
    tbf_subscriptions: Vec<gpui_kit::Subscription>,
}

impl AuthDialog {
    /// サイトのログインが完了したときに、そのサイトの同期を `Workspace` へ要求する。
    ///
    /// ここでは同期そのものは実行しない（本棚の状態を持つのは `BookshelfView` なので、
    /// 要求を `AppState` に置いて `Workspace` の監視タスクが拾う。`AuthDialog` から
    /// `Workspace` を直接 update すると RefCell の再入で固まる）。
    /// Google のログインでは何もしない。
    fn request_site_sync_after_login(&self, provider: AuthProvider, cx: &mut Context<Self>) {
        let Some(site) = provider.site_sync_id() else {
            return;
        };
        log::info!("site login done: {site} の同期を要求する");
        *AppState::global(cx).login_sync_requested.lock() = Some(site.to_string());
    }

    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            provider: None,
            error: None,
            tbf_logged_in: false,
            google_profile: None,
            show_booth_login: false,
            booth_login: None,
            booth_subscriptions: Vec::new(),
            show_fanza_login: false,
            fanza_login: None,
            fanza_subscriptions: Vec::new(),
            show_dlsite_login: false,
            dlsite_login: None,
            dlsite_subscriptions: Vec::new(),
            show_google_login: false,
            google_login: None,
            google_subscriptions: Vec::new(),
            show_tbf_login: false,
            tbf_login: None,
            tbf_subscriptions: Vec::new(),
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
            AuthProvider::Dlsite => self.show_dlsite_login = true,
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

    /// DLsite の WebView ログインモーダルを表示中か（テスト用）。
    #[cfg(test)]
    pub(crate) fn show_dlsite_login(&self) -> bool {
        self.show_dlsite_login
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
                        this.booth_subscriptions.clear();
                        this.request_site_sync_after_login(AuthProvider::Booth, cx);
                        cx.defer(|cx| cx.dispatch_action(&crate::actions::CloseAuth));
                    },
                );
                // キャンセル: WebView を破棄してログイン画面に戻る（次回は新規フロー）
                let _cancelled = cx.subscribe(
                    &booth_login,
                    |this: &mut Self, _: Entity<BoothLoginView>, _: &BoothLoginCancelled, _| {
                        this.show_booth_login = false;
                        this.booth_login = None;
                        this.booth_subscriptions.clear();
                    },
                );
                this.booth_subscriptions = vec![_done, _cancelled];
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
                        this.fanza_subscriptions.clear();
                        this.request_site_sync_after_login(AuthProvider::Fanza, cx);
                        cx.defer(|cx| cx.dispatch_action(&crate::actions::CloseAuth));
                    },
                );
                // キャンセル: WebView を破棄してログイン画面に戻る
                let _cancelled = cx.subscribe(
                    &fanza_login,
                    |this: &mut Self, _: Entity<FanzaLoginView>, _: &FanzaLoginCancelled, _| {
                        this.show_fanza_login = false;
                        this.fanza_login = None;
                        this.fanza_subscriptions.clear();
                    },
                );
                this.fanza_subscriptions = vec![_done, _cancelled];
                this.fanza_login = Some(fanza_login);
                cx.notify();
            });
        }
        // DLsite: WebView ログイン（テスト環境では WebView を作らない）
        if self.show_dlsite_login && self.dlsite_login.is_none() {
            cx.defer_in(window, |this, window, cx| {
                let dlsite_login = cx.new(|cx| DlsiteLoginView::new(window, cx));
                // 成功: WebView を閉じてモーダル全体も閉じる
                let _done = cx.subscribe(
                    &dlsite_login,
                    |this: &mut Self, _: Entity<DlsiteLoginView>, _: &DlsiteLoginDone, cx| {
                        this.show_dlsite_login = false;
                        this.dlsite_login = None;
                        this.dlsite_subscriptions.clear();
                        this.request_site_sync_after_login(AuthProvider::Dlsite, cx);
                        cx.defer(|cx| cx.dispatch_action(&crate::actions::CloseAuth));
                    },
                );
                // キャンセル: WebView を破棄してログイン画面に戻る
                let _cancelled = cx.subscribe(
                    &dlsite_login,
                    |this: &mut Self, _: Entity<DlsiteLoginView>, _: &DlsiteLoginCancelled, _| {
                        this.show_dlsite_login = false;
                        this.dlsite_login = None;
                        this.dlsite_subscriptions.clear();
                    },
                );
                this.dlsite_subscriptions = vec![_done, _cancelled];
                this.dlsite_login = Some(dlsite_login);
                cx.notify();
            });
        }
        // Google: 認可はシステムブラウザで行う（RFC 8252 / Google の埋め込み UA 非推奨）。
        // WebView（wry）を作らないため実ウィンドウは不要だが、他のプロバイダと同じ
        // defer_in の経路に揃えておく（生成タイミングを変えない）。
        if self.show_google_login && self.google_login.is_none() {
            cx.defer_in(window, |this, _window, cx| {
                let google_login = cx.new(GoogleLoginView::new);
                // 成功: プロフィールを保存してモーダル全体を閉じる
                let _done = cx.subscribe(
                    &google_login,
                    |this: &mut Self, _: Entity<GoogleLoginView>, event: &GoogleLoginDone, cx| {
                        let profile = event.0.clone();
                        this.google_profile = Some(profile.clone());
                        // プロフィール（sub）は keyring に保存する（次回起動の所有者判定用）
                        crate::app_state::save_google_profile(cx, &profile);
                        // WebView は完了処理で隠される。次回は新しい認可フローを開始する
                        this.google_login = None;
                        this.google_subscriptions.clear();
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
                        this.google_subscriptions.clear();
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
                        this.google_subscriptions.clear();
                    },
                );
                this.google_subscriptions = vec![_done, _failed, _cancelled];
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
                        this.tbf_subscriptions.clear();
                        this.request_site_sync_after_login(AuthProvider::TechBookFest, cx);
                        cx.defer(|cx| cx.dispatch_action(&crate::actions::CloseAuth));
                    },
                );
                // キャンセル: WebView を破棄してログイン画面に戻る（次回は新規フロー）
                let _cancelled = cx.subscribe(
                    &tbf_login,
                    |this: &mut Self, _: Entity<TbfLoginView>, _: &TbfLoginCancelled, _| {
                        this.show_tbf_login = false;
                        this.tbf_login = None;
                        this.tbf_subscriptions.clear();
                    },
                );
                this.tbf_subscriptions = vec![_done, _cancelled];
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
        // Google: 認可はシステムブラウザで行う（WebView を持たない）ため、
        // 他プロバイダのような「WebView を可視化する」処理は無い。
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
        if self.show_dlsite_login
            && let Some(dlsite) = &self.dlsite_login
        {
            dlsite.update(cx, |view, cx| view.show(cx));
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
                // Google は WebView を使わない（システムブラウザ）ため deferred にしない。
                match &self.google_login {
                    Some(google) => google.clone().into_any_element(),
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
            } else if self.show_dlsite_login {
                match &self.dlsite_login {
                    Some(dlsite) => deferred(dlsite.clone()).into_any_element(),
                    None => div().into_any_element(),
                }
            } else {
                div().into_any_element()
            })
            .child(if self.show_booth_login || self.show_google_login || self.show_tbf_login || self.show_fanza_login || self.show_dlsite_login {
                // WebView のログインモーダル表示中は auth-modal（中央モーダル）を
                // 重ねない。前面に出るログインビューがあるため不要な見た目になる。
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
                                    Some(AuthProvider::Dlsite) => "DLsiteでログイン",
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
                        // DLsite: WebView でログイン（モーダル内で完結）
                        Some(AuthProvider::Dlsite) => div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(
                                        "DLsite のログインはアプリ内ブラウザ（WebView）で行います。\nログイン画面で viviON ID にログインすると、セッションが自動で保存されます。",
                                    ),
                            )
                            .child(
                                Button::new("auth-dlsite-open")
                                    .cursor_pointer()
                                    .primary()
                                    .label("DLsiteログイン画面を開く")
                                    .on_click({
                                        let handle = handle.clone();
                                        move |_, _window, cx| {
                                            handle.update(cx, |this, cx| {
                                                this.show_dlsite_login = true;
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

    /// サイトのログインが完了したら、そのサイトの同期を要求すること。
    ///
    /// Google はアカウント連携用で本の供給元（サイト）ではないため要求しない。
    #[gpui_kit::test]
    async fn site_login_completion_requests_that_sites_sync(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        let dialog = cx.new(AuthDialog::new);
        for (provider, expected) in [
            (AuthProvider::TechBookFest, Some("techbookfest")),
            (AuthProvider::Booth, Some("booth")),
            (AuthProvider::Fanza, Some("fanza")),
            (AuthProvider::Dlsite, Some("dlsite")),
            (AuthProvider::Google, None),
        ] {
            cx.update(|cx| {
                *AppState::global(cx).login_sync_requested.lock() = None;
            });
            cx.update(|cx| {
                dialog.update(cx, |d, cx| d.request_site_sync_after_login(provider, cx));
            });
            let requested =
                cx.update(|cx| AppState::global(cx).login_sync_requested.lock().clone());
            assert_eq!(
                requested.as_deref(),
                expected,
                "{provider:?} のログイン完了で同期の要求が変わっている"
            );
        }
    }

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
    async fn dlsite_provider_opens_webview_login_flag(cx: &mut TestAppContext) {
        cx.update(AppState::init_test);
        let dialog = cx.new(AuthDialog::new);
        dialog.update(cx, |dialog, _| {
            dialog.open_with_provider(Some(AuthProvider::Dlsite));
        });
        // プロバイダ指定で WebView ログインモーダルを直接開く（遅延なし）。
        assert!(
            dialog.read_with(cx, |d, _| d.show_dlsite_login()),
            "DLsite provider should open webview login directly"
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
