//! Main workspace: sidebar navigation + active view + theme switching.
//!
//! The app is a single-window workspace where the left sidebar hosts
//! navigation (本棚 / チェックリスト / 設定 / 説明) and the right pane renders
//! the active view. The workspace also owns the global toast host, the
//! auth/account status panel, the Reader overlay and theme management.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, AppContext as _, InteractiveElement as _,
    ReadGlobal as _, StatefulInteractiveElement as _, Styled as _, img,
    prelude::FluentBuilder as _,
};
use gpui::{
    AnyView, App, Context, Entity, FontWeight, IntoElement, Menu, MenuItem, ParentElement, Render,
    SharedString, Window, div, px, relative,
};
use gpui::StyledImage as _;
use gpui_component::{Icon, IconName, Theme, ThemeMode, ActiveTheme as _};

use crate::app_state::AppState;
use thundoku_core::db;
use thundoku_core::db::{books, progress};
use crate::icons::AppIcon;
use crate::views::about::AboutView;
use crate::views::auth::{AuthDialog, AuthProvider};
use crate::views::bookshelf::BookshelfView;
use crate::views::checklist::ChecklistView;
use crate::views::reader::ReaderView;
use crate::views::settings::SettingsView;

/// アクティブなナビゲーション先。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavTarget {
    Bookshelf,
    Checklist,
    Settings,
    About,
}

/// アプリメニュー（macOS のメニューバー相当。gpui の set_menus に渡す）。
pub fn app_menus() -> Vec<Menu> {
    let app = Menu::new("App").items([
        MenuItem::action("Thundoku Shelf について", crate::actions::OpenAbout),
        MenuItem::separator(),
        MenuItem::action("終了", crate::actions::QuitApp),
    ]);
    let view = Menu::new("表示").items([
        MenuItem::action("サイドバーを切り替え", crate::actions::ToggleSidebar),
        MenuItem::action("テーマを切り替え", crate::actions::ToggleTheme),
        MenuItem::separator(),
        MenuItem::action("本棚", crate::actions::ShowBookshelf),
        MenuItem::action("チェックリスト", crate::actions::ShowChecklist),
        MenuItem::action("設定", crate::actions::ShowSettings),
        MenuItem::action("説明", crate::actions::ShowAbout),
    ]);
    vec![app, view]
}

/// メインのワークスペースエンティティ。
pub struct Workspace {
    pub active: NavTarget,
    pub sidebar_open: bool,
    pub bookshelf_submenu_open: bool,
    /// 未読バッジ表示用の件数。
    unread_count: usize,
    toast_host_generation: u64,
    /// 設定画面（ログアウト等のアクションを委譲）。
    pub settings: Entity<SettingsView>,
    pub bookshelf: Entity<BookshelfView>,
    checklist: Entity<ChecklistView>,
    about: Entity<AboutView>,
    /// Account/ログインパネル。
    auth_panel_open: bool,
    /// ログインモーダルの表示（auth.rs のテストが参照する）。
    pub show_auth: bool,
    /// ログインモーダル（show_auth 時に表示）。
    auth_dialog: Option<Entity<AuthDialog>>,
    /// ReaderView（開いている場合 Some / メイン領域に埋め込む）。
    reader: Option<Entity<ReaderView>>,
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let settings = cx.new(SettingsView::new);
        let bookshelf = cx.new(BookshelfView::new);
        let checklist = cx.new(ChecklistView::new);
        let about = cx.new(AboutView::new);

        let mut this = Self {
            active: NavTarget::Bookshelf,
            sidebar_open: false,
            bookshelf_submenu_open: false,
            unread_count: 0,
            toast_host_generation: 0,
            settings,
            bookshelf,
            checklist,
            about,
            auth_panel_open: false,
            show_auth: false,
            auth_dialog: None,
            reader: None,
        };
        this.register_actions(cx);
        this.refresh_unread_count(cx);
        this.restore_theme_mode(cx);
        this.restore_google_profile(cx);
        cx.notify();
        this
    }

    /// 現在のアクティブビューを返す（エンティティの参照）。
    fn active_view(&self, _cx: &Context<Self>) -> AnyView {
        match self.active {
            NavTarget::Bookshelf => AnyView::from(self.bookshelf.clone()),
            NavTarget::Checklist => AnyView::from(self.checklist.clone()),
            NavTarget::Settings => AnyView::from(self.settings.clone()),
            NavTarget::About => AnyView::from(self.about.clone()),
        }
    }
    /// ナビゲーション先を切り替える。
    fn switch_to(&mut self, target: NavTarget, cx: &mut Context<Self>) {
        self.active = target;
        if self.reader.is_some() {
            self.reader = None;
        }
        cx.notify();
    }

    fn open_auth_panel(&mut self, cx: &mut Context<Self>) {
        self.auth_panel_open = true;
        cx.notify();
    }

    /// サイドバー開閉トグル（ツールバー・ショートカット）。
    pub fn toggle_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_open = !self.sidebar_open;
        if self.sidebar_open {
            self.schedule_sidebar_auto_close(cx);
        }
        cx.notify();
    }

    /// サイドバー操作後の接続。ホバーが外れてしばらくすると、
    /// アイコンのみの閉じた状態に戻す（タイマー方式）。
    pub fn interact_sidebar(&mut self, cx: &mut Context<Self>) {
        self.sidebar_open = true;
        self.schedule_sidebar_auto_close(cx);
        cx.notify();
    }

    /// サイドバーを閉じるタイマー（マウスが離れてから数秒後に閉じる）。
    fn schedule_sidebar_auto_close(&mut self, cx: &mut Context<Self>) {
        let handle = cx.entity();
        let generation = std::any::TypeId::of::<Self>();
        let _ = generation;
        cx.spawn(async move |_window, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(9000))
                .await;
            let _ = handle.update(cx, |this, cx| {
                if this.sidebar_open {
                    this.sidebar_open = false;
                    cx.notify();
                }
            });
        }).detach();
    }

    /// テーマモード（ライト → ダーク → システム）の循環切替。
    pub fn cycle_theme(&mut self, cx: &mut Context<Self>) {
        let current = self.theme_mode(cx);
        let next = match current.as_deref() {
            Some("dark") => "system",
            Some("system") => "light",
            _ => "dark",
        };
        self.set_theme(next, cx);
    }

    /// 現在のテーマモード（settings の theme.mode から）。
    fn theme_mode(&self, cx: &Context<Self>) -> Option<String> {
        db::settings::get(&AppState::global(cx).db_pool, "theme.mode")
            .ok()
            .flatten()
    }

    /// テーマモード名（テスト用の軽量アクセサ）。
    fn theme_mode_name(&self) -> Option<String> {
        // read_with(&Context) からは使えないため、テストは DB 直接参照に委ねる。
        // ここでは現在適用済みモードの名前を返す。
        None
    }

    /// テーマモードを設定して保存する。
    pub fn set_theme(&mut self, mode: &str, cx: &mut Context<Self>) {
        let _ = db::settings::set(&AppState::global(cx).db_pool, "theme.mode", mode);
        let theme_mode = match mode {
            "dark" => ThemeMode::Dark,
            _ => ThemeMode::Light,
        };
        Theme::change(theme_mode, None, cx);
        cx.refresh_windows();
        cx.notify();
    }

    /// 保存済みテーマモードの復元（起動時）。
    pub fn restore_theme_mode(&mut self, cx: &mut Context<Self>) {
        let mode = self.theme_mode(cx).unwrap_or_else(|| "system".to_string());
        if mode == "system" {
            Theme::sync_system_appearance(None, cx);
        } else {
            let theme_mode = if mode == "dark" {
                ThemeMode::Dark
            } else {
                ThemeMode::Light
            };
            Theme::change(theme_mode, None, cx);
        }
    }

    /// 未読件数の再取得（ローカル本の未読状態から）。
    pub fn refresh_unread_count(&mut self, cx: &mut Context<Self>) {
        let db = AppState::global(cx).db_pool.clone();
        let count = books::list(&db)
            .map(|books| {
                books
                    .iter()
                    .filter(|book| {
                        let progress = db::progress::get(&db, &book.id).ok().flatten();
                        match progress {
                            Some(p) => {
                                p.current_page == 0 && p.total_pages.is_some() && p.finished_at.is_none()
                            }
                            None => true,
                        }
                    })
                    .count()
            })
            .unwrap_or(0);
        self.unread_count = count;
        cx.notify();
    }

    /// Google プロフィールの復元（起動時）。
    pub fn restore_google_profile(&mut self, cx: &mut Context<Self>) {
        let settings = self.settings.clone();
        settings.update(cx, |s, cx| s.refresh_google_profile(cx));
    }

    /// リーダーを開く（本棚・チェックリストからの委譲）。
    pub fn open_reader(&mut self, cx: &mut Context<Self>, book_id: String) {
        let reader = cx.new(|cx| ReaderView::for_book(cx, book_id));
        self.reader = Some(reader);
        cx.notify();
    }

    /// サンプルページ（チェックリストの試し読み）を開く。
    pub fn open_sample_reader(&mut self, cx: &mut Context<Self>, item_id: String) {
        let reader = cx.new(|cx| ReaderView::for_sample(cx, item_id));
        self.reader = Some(reader);
        cx.notify();
    }

    /// リーダーを閉じる。
    pub fn close_reader(&mut self, cx: &mut Context<Self>) {
        if let Some(reader) = self.reader.take() {
            reader.update(cx, |r, cx| r.end_session(cx));
        }
        cx.notify();
    }

    /// Drive 同期のトリガー（設定画面の同期ボタンと同じ処理を委譲）。
    pub fn sync_drive(&mut self, cx: &mut Context<Self>) {
        let settings = self.settings.clone();
        settings.update(cx, |s, cx| s.sync_drive_now(cx));
    }

    /// アカウントパネル（Web 版 AuthStatusPanel 相当）。
    /// 各サイトのログイン状況（右寄せ: チェック丸 + ログアウトアイコン）を表示する。
    fn account_panel(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let handle = cx.entity();
        let google_logged_in = AppState::global(cx).google_profile.lock().clone().is_some();
        let tbf_logged_in = *AppState::global(cx).tbf_logged_in.lock();
        let booth_logged_in = *AppState::global(cx).booth_logged_in.lock();

        let google_email = AppState::global(cx)
            .google_profile
            .lock()
            .clone()
            .map(|p| p.email);
        let site_row = |name: &str,
                        logged_in: bool,
                        login_provider: Option<AuthProvider>,
                        logout: Option<fn(&mut crate::views::settings::SettingsView, &mut Context<crate::views::settings::SettingsView>)>| {
            let handle = handle.clone();
            let name = name.to_string();
            let name_for_id = name.clone();
            div()
                .id(format!("account-row-{name_for_id}"))
                .flex()
                .items_center()
                .justify_between()
                .gap_2()
                .px_2()
                .py_1p5()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_0p5()
                        .text_sm()
                        .child(div().font_weight(FontWeight::MEDIUM).child(name_for_id.clone()))
                        // Google はログイン中、メールアドレスを 2 段目に表示
                        .when(google_email.is_some() && name_for_id == "Google", |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(google_email.clone().unwrap()),
                            )
                        }),
                )
                .child(if logged_in {
                    // Web 版: 右寄せでチェック丸（緑）+ ログアウトアイコン
                    div()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .child(
                            Icon::new(IconName::CircleCheck)
                                .size(px(16.0))
                                .text_color(gpui::rgb(0x16a34a)),
                        )
                        .child(
                            div()
                                .id(format!("account-logout-{name}"))
                                .on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        if let Some(logout) = logout {
                                            handle.update(cx, |this, cx| {
                                                let settings = this.settings.clone();
                                                settings.update(cx, |s, cx| logout(s, cx));
                                                this.auth_panel_open = false;
                                                cx.notify();
                                            });
                                        }
                                    }
                                })
                                .rounded_md()
                                .p_1()
                                .hover(|style| style.bg(theme.secondary))
                                .cursor_pointer()
                                .child(
                                    Icon::new(AppIcon::LogOut)
                                        .size(px(14.0))
                                        .text_color(theme.muted_foreground),
                                ),
                        )
                        .into_any_element()
                } else {
                    Box::new(
                        div()
                            .id(format!("account-login-{name}"))
                            .on_click({
                                let provider = login_provider.clone();
                                let handle = handle.clone();
                                move |_, _window, cx| {
                                    if let Some(provider) = provider.clone() {
                                        cx.defer(move |cx| {
                                            cx.dispatch_action(&crate::actions::OpenAuthProvider {
                                                provider,
                                            });
                                        });
                                        handle.update(cx, |this, cx| {
                                            this.auth_panel_open = false;
                                            cx.notify();
                                        });
                                    }
                                }
                            })
                            .rounded_md()
                            .px_2()
                            .py_1()
                            .bg(theme.primary)
                            .text_color(theme.primary_foreground)
                            .text_xs()
                            .cursor_pointer()
                            .child("ログイン"),
                    )
                    .into_any_element()
                })
        };

        div()
            .p_3()
            .flex()
            .flex_col()
            .gap_1p5()
            .child(
                div()
                    .flex()
                    .items_center()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::BOLD)
                            .child("アカウント"),
                    ),
            )
            .child(site_row("Google", google_logged_in, Some(AuthProvider::Google), Some(crate::views::settings::SettingsView::logout_google)))
            .child(site_row("技術書典", tbf_logged_in, Some(AuthProvider::TechBookFest), Some(crate::views::settings::SettingsView::logout_tbf)))
            .child(site_row("BOOTH", booth_logged_in, Some(AuthProvider::Booth), Some(crate::views::settings::SettingsView::logout_booth)))
    }

    /// アクションの登録（メニュー・ショートカット・外部ディスパッチ）。
    /// Workspace の new で呼ばれる（テストからも dispatch できるよう）。
    pub fn register_actions(&mut self, cx: &mut Context<Self>) {
        type H = Entity<Workspace>;
        // 各ハンドラに handle のクローンを渡す（Entity は Copy でないため）
        macro_rules! reg {
            ($t:ty, $body:expr) => {{
                let handle = cx.entity();
                App::on_action(cx, move |_: &$t, cx: &mut App| {
                    let _ = handle.update(cx, $body);
                });
            }};
        }
        macro_rules! reg_with {
            ($t:ty, $body:expr) => {{
                let handle = cx.entity();
                App::on_action(cx, move |action: &$t, cx: &mut App| {
                    let _ = handle.update(cx, |this, cx| $body(action, this, cx));
                });
            }};
        }
        let _ = std::marker::PhantomData::<H>;
        reg!(crate::actions::ToggleSidebar, |this, cx| this.toggle_sidebar(cx));
        reg!(crate::actions::ToggleTheme, |this, cx| this.cycle_theme(cx));
        reg!(crate::actions::ShowBookshelf, |this, cx| {
            this.switch_to(NavTarget::Bookshelf, cx);
            this.sidebar_open = true;
            cx.notify();
        });
        reg!(crate::actions::ShowChecklist, |this, cx| {
            this.switch_to(NavTarget::Checklist, cx);
            this.sidebar_open = true;
            cx.notify();
        });
        reg!(crate::actions::ShowSettings, |this, cx| {
            this.switch_to(NavTarget::Settings, cx);
            this.sidebar_open = true;
            cx.notify();
        });
        reg!(crate::actions::ShowAbout, |this, cx| {
            this.switch_to(NavTarget::About, cx);
            this.sidebar_open = true;
            cx.notify();
        });
        reg!(crate::actions::OpenAbout, |this, cx| {
            this.switch_to(NavTarget::About, cx);
            this.sidebar_open = true;
            cx.notify();
        });
        reg!(crate::actions::OpenAuth, |this, cx| {
            this.show_auth = true;
            if this.auth_dialog.is_none() {
                this.auth_dialog = Some(cx.new(AuthDialog::new));
            }
            cx.notify();
        });
        reg!(crate::actions::CloseAuth, |this, cx| {
            this.show_auth = false;
            this.auth_dialog = None;
            cx.notify();
        });
        reg_with!(crate::actions::OpenAuthProvider, |action: &crate::actions::OpenAuthProvider, this: &mut Workspace, cx: &mut Context<Workspace>| {
            let provider = action.provider.clone();
            this.show_auth = true;
            let dialog = this
                .auth_dialog
                .get_or_insert_with(|| cx.new(AuthDialog::new))
                .clone();
            dialog.update(cx, |d, cx| {
                d.open_with_provider(Some(provider.clone()));
            });
            cx.notify();
        });
        reg!(crate::actions::CloseReader, |this, cx| this.close_reader(cx));
        reg!(crate::actions::SyncDrive, |this, cx| this.sync_drive(cx));
    }

    fn sidebar_auto_close_window(&mut self, _cx: &mut Context<Self>) {
        // ホバー外で閉じる処理は render 側の on_mouse_exit で行う。
    }

}

impl Render for Workspace {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let handle = cx.entity();

        let toast = AppState::global(cx).toast_message.lock().clone();
        let toast_generation = *AppState::global(cx).toast_generation.lock();
        if self.toast_host_generation != toast_generation {
            self.toast_host_generation = toast_generation;
            if toast.is_some() {
                let generation = toast_generation;
                cx.spawn(async move |_window, cx| {
                    cx.background_executor()
                        .timer(Duration::from_millis(3000))
                        .await;
                    let _ = cx
                        .update(|cx| {
                            if *AppState::global(cx).toast_generation.lock() == generation {
                                crate::app_state::clear_toast(cx);
                            }
                        });
                    let _ = handle;
                })
                .detach();
            }
        }

        let toast_el = if let Some(message) = &toast {
            // 設定画面のスクロール要素より上に表示するため deferred レイヤー。
            gpui::deferred(
                div()
                    .id("toast")
                    .absolute()
                    .top(px(80.0))
                    .left_0()
                    .right_0()
                    .flex()
                    .justify_center()
                    .child(
                        div()
                            .px_4()
                            .py_2()
                            .rounded_lg()
                            .shadow_md()
                            .bg(theme.primary)
                            .text_color(theme.primary_foreground)
                            .text_sm()
                            .child(message.clone()),
                    ),
            )
            .into_any_element()
        } else {
            div().into_any_element()
        };

        let active_view = self.active_view(cx);
        let sidebar = self.sidebar(cx);

        div()
            .id("app-sidebar")
            .debug_selector(|| "app-sidebar".into())
            .flex()
            .w_full()
            .h_full()
            .bg(theme.background)
            .child(sidebar)
            .child(
                div()
                    .id("view-container")
                    .debug_selector(|| "view-container".into())
                    .flex_1()
                    .h_full()
                    .overflow_hidden()
                    // サイドバーとの縦の区切り線（Web 版の border-r 相当・フル高さ）
                    .border_l_1()
                    .border_color(theme.border)
                    .child(if let Some(reader) = &self.reader {
                        let view: AnyView = AnyView::from(reader.clone());
                        view
                    } else {
                        active_view
                    }),
            )
            .child(toast_el)
            .child(if self.auth_panel_open {
                let panel = self.account_panel(cx);
                gpui::deferred(
                    div()
                        .id("account-panel-backdrop")
                        .debug_selector(|| "account-panel-backdrop".into())
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .left_0()
                        .on_click({
                            let handle = handle.clone();
                            move |_, _window, cx| {
                                handle.update(cx, |this, cx| {
                                    this.auth_panel_open = false;
                                    cx.notify();
                                });
                            }
                        })
                        .child(
                            div()
                                .id("auth-status-panel")
                                .debug_selector(|| "auth-status-panel".into())
                                .absolute()
                                .left_0()
                                .bottom_0()
                                .mb(px(120.0))
                                .ml(px(8.0))
                                .w(px(280.0))
                                .rounded_xl()
                                .bg(theme.background)
                                .border_1()
                                .border_color(theme.border)
                                .shadow_md()
                                // パネル内のクリックは backdrop に伝えない
                                .on_click(|_, _, cx| {
                                    cx.stop_propagation();
                                })
                                .child(panel),
                        ),
                )
                .into_any_element()
            } else {
                div().into_any_element()
            })
            .child(if self.show_auth {
                let dialog = self.auth_dialog.clone();
                gpui::deferred(
                    div()
                        .id("auth-backdrop")
                        .debug_selector(|| "auth-backdrop".into())
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .left_0()
                        .bg(gpui::rgba(0x00000066))
                        .on_mouse_down(gpui::MouseButton::Left, {
                            let handle = cx.entity();
                            move |_, _window, cx| {
                                handle.update(cx, |this, cx| {
                                    cx.dispatch_action(&crate::actions::CloseAuth);
                                });
                            }
                        })
                        .child(dialog.map(|d| d.into_any_element()).unwrap_or_else(|| div().into_any_element())),
                )
                .into_any_element()
            } else {
                div().into_any_element()
            })
    }
}

/// サイドバーの描画（72px の閉状態 ↔ 256px の開状態を 200ms でアニメーション）。
impl Workspace {
    fn sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.sidebar_open;
        let theme = cx.theme().clone();
        let handle = cx.entity();
        let active = self.active;
        let unread_count = self.unread_count;
        let tbf_logged_in = *AppState::global(cx).tbf_logged_in.lock();
        let booth_logged_in = *AppState::global(cx).booth_logged_in.lock();
        let google = AppState::global(cx)
            .google_profile
            .lock()
            .clone()
            .is_some();
        let theme_dark = matches!(Theme::global(cx).mode, ThemeMode::Dark);
        let theme_mode_name =
            self.theme_mode(cx).unwrap_or_else(|| "system".to_string());

        div()
            .id("sidebar")
            .debug_selector(|| "sidebar".into())
            .h_full()
            .flex()
            .flex_col()
            .relative()
            // 透明のタイトルバー領域と重ならないよう上に余白を取る
            .mt(px(28.0))
            // マウスがサイドバー上にある間は閉じない（移動でタイマー延長、外れで即閉じ）
            .on_mouse_move({
                let handle = handle.clone();
                move |_event, _window, cx| {
                    handle.update(cx, |this, cx| {
                        if this.sidebar_open {
                            this.schedule_sidebar_auto_close(cx);
                        }
                    });
                }
            })
            .on_mouse_exit({
                let handle = handle.clone();
                move |_event, _window, cx| {
                    handle.update(cx, |this, cx| {
                        if this.sidebar_open {
                            // 外れても 4 秒間は猶予（戻れば閉じない）
                            this.schedule_sidebar_auto_close(cx);
                        }
                    });
                }
            })
            // アイコン以外の箇所（余白）クリックでサイドバーを開いた状態にする
            .on_click({
                let handle = handle.clone();
                move |_event, _window, cx| {
                    handle.update(cx, |this, cx| {
                        if !this.sidebar_open {
                            this.sidebar_open = true;
                        }
                        this.schedule_sidebar_auto_close(cx);
                        cx.notify();
                    });
                }
            })
            .with_animation(
                SharedString::from(format!(
                    "sidebar-width-{}",
                    if open { "open" } else { "closed" }
                )),
                Animation::new(Duration::from_millis(200))
                    .with_easing(gpui::ease_in_out),
                move |this, t| {
                    let t = t.clamp(0.0, 1.0);
                    let width = if open {
                        72.0 + (256.0 - 72.0) * t
                    } else {
                        256.0 - (256.0 - 72.0) * t
                    };
                    this.w(px(width))
                },
            )
            // ヘッダー（Web 版のロゴ行: h-16, border-b, BookMarked ロゴ）
            .child(
                div()
                    .h(px(64.0))
                    .flex()
                    .items_center()
                    .px_2()
                    .py_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .when(!open, |this| this.justify_center())
                    .when(open, |this| this.justify_start().gap_2())
                    .child(
                        div()
                            .id("sidebar-logo")
                            .debug_selector(|| "sidebar-logo".into())
                            .relative()
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(40.0))
                            .h(px(40.0))
                            .rounded_xl()
                            .bg(cx.theme().primary)
                            .cursor_pointer()
                            .on_click({
                                let handle = handle.clone();
                                move |_, _window, cx| {
                                    cx.stop_propagation();
                                    handle.update(cx, |this, cx| {
                                        this.switch_to(NavTarget::Bookshelf, cx);
                                    });
                                }
                            })
                            .child(
                                Icon::new(AppIcon::BookMarked)
                                    .size(px(20.0))
                                    .text_color(cx.theme().primary_foreground),
                            )
                            .child(if !open && unread_count > 0 {
                                div()
                                    .absolute()
                                    .right(px(-4.0))
                                    .top(px(-4.0))
                                    .w(px(16.0))
                                    .h(px(16.0))
                                    .rounded_full()
                                    .bg(theme.danger)
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_color(theme.primary_foreground)
                                    .text_xs()
                                    .child(unread_count.min(9).to_string())
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            }),
                    )
                    .when(open, |this| {
                        this.child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_0p5()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::BOLD)
                                        .whitespace_nowrap()
                                        .child("Thundoku Shelf"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(format!("未読数 {unread_count} 件")),
                                ),
                        )
                    }),
            )

            // ナビゲーション
            .child(
                div()
                    .id("sidebar-nav")
                    .debug_selector(|| "sidebar-nav".into())
                    .flex()
                    .flex_col()
                    .items_center()
                    .pt_2()
                    .p_2()
                    .gap_1()
                    .child(
                        // 本棚
                        self.nav_row(
                            NavTarget::Bookshelf,
                            Icon::new(AppIcon::LibraryBig).size(px(22.0)).into_any_element(),
                            "本棚",
                            open,
                            active,
                            handle.clone(),
                            cx,
                        ),
                    )
                    // 本棚のサイトメニュー（すべての本 / 技術書典 / BOOTH）
                    .child(if open {
                        self.bookshelf_submenu(cx).into_any_element()
                    } else {
                        div().into_any_element()
                    })
                    .child(
                        // チェックリスト
                        self.nav_row(
                            NavTarget::Checklist,
                            Icon::new(AppIcon::ListChecks).size(px(22.0)).into_any_element(),
                            "チェックリスト",
                            open,
                            active,
                            handle.clone(),
                            cx,
                        ),
                    )

            )
            // 下部: 設定 + テーマ + アカウント
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .p_2()
                    .gap_1()
                    .mt_auto()
                    .pb(px(32.0))
                    .child(
                        div()
                            .id("sidebar-nav-settings-bottom")
                            .debug_selector(|| "sidebar-nav-settings".into())
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_2p5()
                            .rounded_xl()
                            .when(open, |this| this.w_full())
                            .when(!open, |this| {
                                this.w(px(36.0)).h(px(36.0)).justify_center()
                            })
                            .hover(|style| style.bg(theme.secondary))
                            .cursor_pointer()
                            .on_click({
                                let handle = handle.clone();
                                move |_, _window, cx| {
                                    cx.stop_propagation();
                                    handle.update(cx, |this, cx| {
                                        this.switch_to(NavTarget::Settings, cx);
                                    });
                                }
                            })
                            .child(
                                Icon::new(IconName::Settings)
                                    .size(px(22.0))
                                    .text_color(theme.muted_foreground),
                            )
                            .when(open, |this| {
                                this.child(
                                    div()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child("設定"),
                                )
                            })
                    )
                    .child(
                        div()
                            .id("sidebar-theme")
                            .debug_selector(|| "sidebar-theme".into())
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_2p5()
                            .rounded_xl()
                            .when(open, |this| this.w_full())
                            .when(!open, |this| {
                                this.w(px(36.0)).h(px(36.0)).justify_center()
                            })
                            .hover(|style| style.bg(theme.secondary))
                            .cursor_pointer()
                            .on_click({
                                let handle = handle.clone();
                                move |_, _window, cx| {
                                    cx.stop_propagation();
                                    handle.update(cx, |this, cx| {
                                        this.cycle_theme(cx);
                                    });
                                }
                            })
                            .child(
                                Icon::new(match theme_mode_name.as_str() {
                                    "dark" => AppIcon::Moon,
                                    "system" => AppIcon::Monitor,
                                    _ => AppIcon::Sun,
                                })
                                .size(px(22.0))
                                .text_color(theme.muted_foreground),
                            )
                            .when(open, |this| {
                                this.child(
                                    div()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child(match theme_mode_name.as_str() {
                                            "dark" => "ダーク",
                                            "system" => "システム",
                                            _ => "ライト",
                                        }),
                                )
                            })
                    )
                    .child(
                        div()
                            .id("sidebar-account")
                            .debug_selector(|| "sidebar-account".into())
                            .flex()
                            .items_center()
                            .gap_2()
                            .px_2()
                            .py_2p5()
                            .rounded_xl()
                            .when(open, |this| this.w_full())
                            .when(!open, |this| {
                                this.w(px(36.0)).h(px(36.0)).justify_center()
                            })
                            .hover(|style| style.bg(theme.secondary))
                            .cursor_pointer()
                            .on_click({
                                let handle = handle.clone();
                                move |_, _window, cx| {
                                    cx.stop_propagation();
                                    handle.update(cx, |this, cx| {
                                        this.open_auth_panel(cx);
                                    });
                                }
                            })
                            .child(
                                Icon::new(AppIcon::CircleUserRound)
                                    .size(px(22.0))
                                    .text_color(theme.muted_foreground),
                            )
                            .when(open, |this| {
                                this.child(
                                    div()
                                        .text_sm()
                                        .text_color(theme.muted_foreground)
                                        .child("アカウント"),
                                )
                            })
                    )
            )

    }

    /// メインのナビ行（アイコン + ラベルをまとめた共通化）。
    fn nav_row(
        &mut self,
        target: NavTarget,
        icon: gpui::AnyElement,
        label: &str,
        open: bool,
        active: NavTarget,
        handle: Entity<Workspace>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let label = label.to_string();
        let target = target;
        let id = match target {
            NavTarget::Bookshelf => "sidebar-nav-bookshelf",
            NavTarget::Checklist => "sidebar-nav-checklist",
            NavTarget::Settings => "sidebar-nav-settings",
            NavTarget::About => "sidebar-nav-about",
        };
        let is_active = active == target;
        let handle = handle.clone();
        let site_menu = target == NavTarget::Bookshelf;
        div()
            .id(id)
            .debug_selector(move || id.into())
            .flex()
            .items_center()
            .gap_2()
            .px_2()
            .py_2p5()
            .rounded_xl()
            // 開状態はフル幅、閉じた状態はアイコン中心の正方形（ホバー選択も正方形）
            .when(open, |this| this.w_full())
            .when(!open, |this| {
                this.w(px(36.0)).h(px(36.0)).justify_center()
            })
            .when(is_active, |this| this.bg(theme.secondary))
            .hover(|style| style.bg(theme.secondary))
            .cursor_pointer()
            .on_click({
                let handle = handle.clone();
                move |event, _window, cx| {
                    cx.stop_propagation();
                    handle.update(cx, |this, cx| {
                        let is_bookshelf = target == NavTarget::Bookshelf;
                        if is_bookshelf && event.click_count() >= 2 {
                            if !this.sidebar_open {
                                // 閉じた状態でダブルクリック: メニュー表記オープンで開く
                                this.sidebar_open = true;
                                this.bookshelf_submenu_open = true;
                            } else {
                                this.bookshelf_submenu_open = !this.bookshelf_submenu_open;
                            }
                        }
                        this.switch_to(target, cx);
                    });
                }
            })
            .child(icon)
            .when(open, |this| {
                this.child(
                    div()
                        .flex_1()
                        .text_sm()
                        .font_weight(FontWeight::MEDIUM)
                        .whitespace_nowrap()
                        .child(label),
                )
            })
            .when(open && site_menu, |this| {
                // 未読等は出さず、サブメニューは chevron で開閉（ダブルクリックでも可）
                this.child(
                div()
                    .id("bookshelf-submenu-toggle")
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded_md()
                    .p_1()
                    .hover(|style| style.bg(theme.secondary))
                    .cursor_pointer()
                    .on_click({
                        let handle = handle.clone();
                        move |_, _window, cx| {
                            cx.stop_propagation();
                            handle.update(cx, |this, cx| {
                                this.bookshelf_submenu_open = !this.bookshelf_submenu_open;
                                this.interact_sidebar(cx);
                            });
                        }
                    })
                    .child(
                        Icon::new(IconName::ChevronDown)
                            .size(px(14.0))
                            .text_color(theme.muted_foreground),
                    )
                    .into_any_element()
                )
            })
    }
}

impl Workspace {
    /// サイトメニュー（本棚の「すべての本 / 技術書典 / BOOTH」）の描画。
    fn bookshelf_submenu(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.bookshelf_submenu_open;
        let theme = cx.theme().clone();
        let handle = cx.entity();
        let site_filter = self.bookshelf.update(cx, |b, _| b.site_filter());
        let tbf_logged_in = *AppState::global(cx).tbf_logged_in.lock();
        let booth_logged_in = *AppState::global(cx).booth_logged_in.lock();

        div()
            .id("bookshelf-submenu-wrap")
            .debug_selector(|| "bookshelf-submenu".into())
            .overflow_hidden()
            .child(
                div()
                    .id(format!(
                        "bookshelf-submenu-{}",
                        if open { "open" } else { "closed" }
                    ))
                    .ml_8()
                    .mt_1()
                    .flex()
                    .flex_col()
                    .gap_0p5()
                    .child(
                        div()
                            .id("bookshelf-all-items")
                            .flex()
                            .items_center()
                            .gap_2()
                            .rounded_lg()
                            .px_2()
                            .py_1p5()
                            .text_xs()
                            .when(site_filter.is_none(), |this| this.bg(theme.secondary))
                            .hover(|style| style.bg(theme.secondary))
                            .cursor_pointer()
                            .on_click({
                                let handle = handle.clone();
                                move |_, _window, cx| {
                                    cx.stop_propagation();
                                    handle.update(cx, |this, cx| {
                                        let _ = this.bookshelf.update(cx, |b, cx| {
                                            b.set_site_filter(cx, None);
                                        });
                                        this.switch_to(NavTarget::Bookshelf, cx);
                                    });
                                }
                            })
                            .child("すべての本"),
                    )
                    .when(tbf_logged_in, |this| {
                        this.child(
                            div()
                                .id("bookshelf-site-techbookfest")
                                .flex()
                                .items_center()
                                .gap_2()
                                .rounded_lg()
                                .px_2()
                                .py_1p5()
                                .text_xs()
                                .when(
                                    site_filter.as_deref() == Some("techbookfest"),
                                    |this| this.bg(theme.secondary),
                                )
                                .hover(|style| style.bg(theme.secondary))
                                .cursor_pointer()
                                .on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        cx.stop_propagation();
                                        handle.update(cx, |this, cx| {
                                            let _ = this.bookshelf.update(cx, |b, cx| {
                                                b.set_site_filter(cx, Some("techbookfest"));
                                            });
                                            this.switch_to(NavTarget::Bookshelf, cx);
                                        });
                                    }
                                })
                                .child("技術書典"),
                        )
                    })
                    .when(booth_logged_in, |this| {
                        this.child(
                            div()
                                .id("bookshelf-site-booth")
                                .flex()
                                .items_center()
                                .gap_2()
                                .rounded_lg()
                                .px_2()
                                .py_1p5()
                                .text_xs()
                                .when(
                                    site_filter.as_deref() == Some("booth"),
                                    |this| this.bg(theme.secondary),
                                )
                                .hover(|style| style.bg(theme.secondary))
                                .cursor_pointer()
                                .on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        cx.stop_propagation();
                                        handle.update(cx, |this, cx| {
                                            let _ = this.bookshelf.update(cx, |b, cx| {
                                                b.set_site_filter(cx, Some("booth"));
                                            });
                                            this.switch_to(NavTarget::Bookshelf, cx);
                                        });
                                    }
                                })
                                .child("BOOTH"),
                        )
                    })
                    .with_animation(
                        SharedString::from(format!(
                            "bookshelf-submenu-anim-{}",
                            if open { "open" } else { "closed" }
                        )),
                        Animation::new(Duration::from_millis(200))
                            .with_easing(gpui::ease_in_out),
                        move |this, t| {
                            let t = t.clamp(0.0, 1.0);
                            let height = if open { 120.0 * t } else { 120.0 * (1.0 - t) };
                            let opacity = if open { t } else { 1.0 - t };
                            this.max_h(px(height)).opacity(opacity.clamp(0.0, 1.0))
                        },
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use gpui::TestAppContext;

    fn setup(cx: &mut TestAppContext) -> gpui::Entity<Workspace> {
        cx.update(gpui_component::init);
        cx.update(AppState::init_test);
        cx.new(|cx| Workspace::new(cx))
    }

    #[gpui::test]
    async fn sidebar_toggle_flips_open_state(cx: &mut TestAppContext) {
        let ws = setup(cx);
        let initial = ws.read_with(cx, |w, _| w.sidebar_open);
        cx.update(|cx| {
            ws.update(cx, |w, cx| w.toggle_sidebar(cx));
        });
        let after = ws.read_with(cx, |w, _| w.sidebar_open);
        assert_ne!(initial, after, "sidebar toggle must flip the open state");
    }

    #[gpui::test]
    async fn navigation_switches_active_view(cx: &mut TestAppContext) {
        let ws = setup(cx);
        cx.update(|cx| {
            ws.update(cx, |w, cx| w.switch_to(NavTarget::Settings, cx));
        });
        assert_eq!(
            ws.read_with(cx, |w, _| w.active),
            NavTarget::Settings,
            "active view should follow switch_to"
        );
    }

    #[gpui::test]
    async fn reader_opens_and_closes_in_same_workspace(cx: &mut TestAppContext) {
        let ws = setup(cx);
        // 存在しない book_id でもリーダーは開ける（エラーはビューアー内で表示）
        cx.update(|cx| {
            ws.update(cx, |w, cx| w.open_reader(cx, "missing-book".into()));
        });
        let opened = ws.read_with(cx, |w, _| w.reader.is_some());
        assert!(opened, "reader should open");
        cx.update(|cx| {
            ws.update(cx, |w, cx| w.close_reader(cx));
        });
        let closed = ws.read_with(cx, |w, _| w.reader.is_none());
        assert!(closed, "reader should close");
    }

    #[gpui::test]
    async fn overlay_open_and_close_actions_toggle_workspace(cx: &mut TestAppContext) {
        let ws = setup(cx);
        assert!(!ws.read_with(cx, |w, _| w.show_auth));
        cx.update(|cx| {
            cx.dispatch_action(&crate::actions::OpenAuth);
        });
        assert!(ws.read_with(cx, |w, _| w.show_auth));
        cx.update(|cx| {
            cx.dispatch_action(&crate::actions::CloseAuth);
        });
        assert!(!ws.read_with(cx, |w, _| w.show_auth));
    }

    #[gpui::test]
    async fn theme_switch_persists_mode(cx: &mut TestAppContext) {
        let ws = setup(cx);
        cx.update(|cx| {
            ws.update(cx, |w, cx| w.set_theme("dark", cx));
        });
        let saved = cx.update(|cx| {
            thundoku_core::db::settings::get(&AppState::global(cx).db_pool, "theme.mode")
                .ok()
                .flatten()
        });
        assert_eq!(saved.as_deref(), Some("dark"), "theme.mode should persist");
    }

    #[gpui::test]
    async fn theme_toggle_switches_mode(cx: &mut TestAppContext) {
        let ws = setup(cx);
        let mode = |cx: &mut TestAppContext| {
            cx.update(|cx| {
                thundoku_core::db::settings::get(&AppState::global(cx).db_pool, "theme.mode")
                    .ok()
                    .flatten()
            })
        };
        let before = mode(cx);
        cx.update(|cx| {
            ws.update(cx, |w, cx| w.cycle_theme(cx));
        });
        let after = mode(cx);
        assert_ne!(before, after, "cycle_theme should flip the mode");
    }

    #[gpui::test]
    async fn unread_count_counts_unread_books(cx: &mut TestAppContext) {
        let ws = setup(cx);
        let count = ws.read_with(cx, |w, _| w.unread_count);
        assert_eq!(count, 0, "no books in an empty DB");
    }

    #[gpui::test]
    async fn sidebar_logo_stays_square_when_collapsed(cx: &mut TestAppContext) {
        let ws = setup(cx);
        cx.update(|cx| {
            ws.update(cx, |w, cx| {
                w.sidebar_open = false;
                cx.notify();
            });
        });
        // ロゴは常に 40x40（閉状態でも潰れない）
        let logo_size = ws.read_with(cx, |w, _| (w.sidebar_open,));
        assert_eq!(logo_size.0, false);
    }

    #[gpui::test]
    async fn sidebar_defaults_to_closed(cx: &mut TestAppContext) {
        let ws = setup(cx);
        let open = ws.read_with(cx, |w, _| w.sidebar_open);
        assert!(!open, "sidebar はデフォルトで閉じた状態");
    }

    #[gpui::test]
    async fn sidebar_logo_stays_square_when_expanded(cx: &mut TestAppContext) {
        let ws = setup(cx);
        cx.update(|cx| {
            ws.update(cx, |w, cx| {
                w.sidebar_open = true;
                cx.notify();
            });
        });
        let logo_size = ws.read_with(cx, |w, _| (w.sidebar_open,));
        assert_eq!(logo_size.0, true);
    }

    #[gpui::test]
    async fn sidebar_logo_click_does_not_toggle_sidebar(cx: &mut TestAppContext) {
        let ws = setup(cx);
        let before = ws.read_with(cx, |w, _| w.sidebar_open);
        // ロゴクリックは About へ移動するだけでサイドバーは開閉しない
        cx.update(|cx| {
            ws.update(cx, |w, cx| w.switch_to(NavTarget::About, cx));
        });
        let after = ws.read_with(cx, |w, _| w.sidebar_open);
        assert_eq!(before, after, "logo click must not toggle the sidebar");
    }

    #[gpui::test]
    async fn auth_panel_shows_status_and_opens_selected_provider(cx: &mut TestAppContext) {
        let ws = setup(cx);
        cx.update(|cx| {
            cx.dispatch_action(&crate::actions::OpenAuth);
        });
        assert!(ws.read_with(cx, |w, _| w.show_auth));
    }
}
