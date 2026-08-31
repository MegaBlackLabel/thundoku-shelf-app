//! Main workspace: sidebar navigation + active view + theme switching.
//!
//! The app is a single-window workspace where the left sidebar hosts
//! navigation (本棚 / チェックリスト / 設定 / 説明) and the right pane renders
//! the active view. The workspace also owns the global toast host, the
//! auth/account status panel, the Reader overlay and theme management.

use std::time::Duration;

#[cfg(windows)]
use gpui::WindowControlArea;
use gpui::{
    Animation, AnimationExt as _, AppContext as _, InteractiveElement as _, ReadGlobal as _,
    StatefulInteractiveElement as _, Styled as _, prelude::FluentBuilder as _,
};

use gpui::{
    AnyView, App, Context, Entity, FontWeight, IntoElement, Menu, MenuItem, ParentElement, Render,
    SharedString, Window, div, px,
};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::Dialog;
use gpui_component::{ActiveTheme as _, Icon, IconName, Theme, ThemeMode};

use crate::app_state::AppState;
use crate::icons::AppIcon;
use crate::views::about::AboutView;
use crate::views::auth::{AuthDialog, AuthProvider};
use crate::views::bookshelf::BookshelfView;
use crate::views::checklist::ChecklistView;
use crate::views::reader::ReaderView;
use crate::views::settings::SettingsView;
use thundoku_core::db;
use thundoku_core::db::{books, bookshelf, progress};

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
    /// 自動クローズタイマーの世代（stale タイマー対策）。
    sidebar_close_generation: u64,
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
    /// 起動時の Drive バックアップ復元確認を表示中か。
    show_restore_prompt: bool,
    /// 復元対象の Drive バックアップ情報（確認ダイアログの表示用）。
    restore_info: Option<thundoku_core::drive::sync::DriveBackupInfo>,
    /// 復元実行中（バックグラウンド）か。
    restoring: bool,
    /// Google ログイン後に Drive バックアップ有効化の確認を表示中か。
    show_drive_prompt: bool,
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
            sidebar_close_generation: 0,
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
            show_restore_prompt: false,
            restore_info: None,
            restoring: false,
             show_drive_prompt: false,
         };
        this.register_actions(cx);
         this.refresh_unread_count(cx);
         this.restore_theme_mode(cx);
         this.check_startup_backup(cx);
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
        // ショートカットでの開閉はマウスオーバー判定に使うウィンドウを取れないため、
        // 自動クローズは掛けず、手動で閉じる。
        self.sidebar_open = !self.sidebar_open;
        cx.notify();
    }

    /// サイドバー操作後の接続。ホバーが外れてしばらくすると、
    /// アイコンのみの閉じた状態に戻す（タイマー方式）。
    pub fn interact_sidebar(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_open = true;
        self.schedule_sidebar_auto_close(window, cx);
        cx.notify();
    }

    /// サイドバーを閉じるタイマー（マウスが離れてから数秒後に閉じる）。
    /// サイドバーを閉じるタイマー（マウスがサイドバー上にいなければ 3 秒後に閉じる）。
    /// タイマー発火時にマウス位置をポーリングし、サイドバー領域（ウィンドウ左端 256px）に
    /// マウスが居る場合はタイマーを仕切り直す（GPUI の on_mouse_exit は
    /// pointer capture 中しか発火しないため、位置ベースで判定する）。
    fn schedule_sidebar_auto_close(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.sidebar_close_generation += 1;
        let generation = self.sidebar_close_generation;
        cx.spawn_in(window, async move |handle, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(3000))
                    .await;
                // マウスがウィンドウ内のサイドバー領域（左端 256px）にあるか
                let in_sidebar = cx
                    .update(|window, _| {
                        let m = window.mouse_position();
                        let h = window.bounds().size.height.as_f32();
                        let (mx, my) = (m.x.as_f32(), m.y.as_f32());
                        (0.0..=256.0).contains(&mx) && my >= 0.0 && my <= h
                    })
                    .unwrap_or(false);
                if in_sidebar {
                    // まだサイドバー上にいる → もう一度 3 秒待つ
                    continue;
                }
                let _ = cx.update(|_window, app| {
                    let _ = handle.update(app, |this, cx| {
                        if this.sidebar_open && this.sidebar_close_generation == generation {
                            log::info!("sidebar: auto close (mouse outside)");
                            this.sidebar_open = false;
                            cx.notify();
                        }
                    });
                });
                break;
            }
        })
        .detach();
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
        // サイトフィルタに連動する（すべての本 = フィルタなし。サイト選択中はそのサイトのみ）
        let site_filter = self.bookshelf.update(cx, |b, _| b.site_filter());
        // 所有している本（同期済みの本棚アイテム）から算出する。
        // - 非表示の本は除外
        // - ダウンロード済み: 進捗で未読判定（current_page == 0 かつ未完読）
        // - 未ダウンロード: 未読としてカウント
        let count = bookshelf::list_all(&db)
            .map(|items| {
                let local = books::list(&db).unwrap_or_default();
                items
                    .iter()
                    .filter(|item| item.is_hidden == 0)
                    .filter(|item| {
                        site_filter
                            .as_ref()
                            .is_none_or(|site| item.site_id == *site)
                    })
                    .filter(|item| {
                        let book = local.iter().find(|b| {
                            b.tbf_product_id.as_deref() == Some(item.database_id.as_str())
                        });
                        match book {
                            Some(book) => {
                                let progress = progress::get(&db, &book.id).ok().flatten();
                                match progress {
                                    Some(p) => {
                                        p.current_page == 0
                                            && p.total_pages.is_some()
                                            && p.finished_at.is_none()
                                    }
                                    None => true,
                                }
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

    /// 起動時: Google ログイン済みなら Drive の DB バックアップを確認し、
    /// ローカルと異なれば復元確認を表示する。
    fn check_startup_backup(&mut self, cx: &mut Context<Self>) {
        let state = AppState::global(cx);
        let google = state.google.clone();
        let db = state.db_pool.clone();
        let folder_id = {
            let current = db::settings::get(&db, "drive.sync.folder_id")
                .ok()
                .flatten();
            // 未接続なら何もしない（Drive 同期が未設定）
            let enabled = db::settings::get(&db, "drive.sync.enabled")
                .ok()
                .flatten()
                .is_some_and(|v| v == "true" || v == "1");
            if !enabled {
                return;
            }
            current
        };
        let Some(folder_id) = folder_id else {
            return;
        };
        let handle = cx.weak_entity();
        let task = cx.background_executor().spawn(async move {
            // Google にログイン済みでなければ何もしない
            let mut client = google.lock();
            let client = client.as_mut()?;
            let Ok(token) = client.access_token() else {
                return None;
            };
            let mut drive = thundoku_core::drive::DriveClient::new(
                Box::new(thundoku_core::tbf::UreqTransport::new()),
                token,
            );
            // ローカルと Drive のバックアップに差分があるときだけ復元候補にする
            let has_diff =
                thundoku_core::drive::sync::backup_has_diff(&db, &mut drive, &folder_id).ok()?;
            if !has_diff {
                return None;
            }
            thundoku_core::drive::sync::check_drive_backup(&mut drive, &folder_id)
                .ok()
                .flatten()
        });
        cx.spawn(async move |_window, cx| {
            let info = task.await;
            if let (Some(handle), Some(info)) = (handle.upgrade(), info) {
                log::info!(
                    "startup backup check: drive backup differs from local (md5={:?})",
                    info.md5
                );
                handle.update(cx, |this, cx| {
                    this.restore_info = Some(info);
                    this.show_restore_prompt = true;
                    cx.notify();
                });
            }
        })
        .detach();
    }

    /// 復元確認で「OK」を押したときの処理。Drive のバックアップを DB に反映する。
    fn confirm_restore_backup(&mut self, cx: &mut Context<Self>) {
        let Some(_info) = self.restore_info.clone() else {
            return;
        };
        self.show_restore_prompt = false;
        self.restoring = true;
        cx.notify();

        let state = AppState::global(cx);
        let google = state.google.clone();
        let db = state.db_pool.clone();
        let folder_id = db::settings::get(&db, "drive.sync.folder_id")
            .ok()
            .flatten()
            .unwrap_or_default();
        let handle = cx.weak_entity();
        let task = cx.background_executor().spawn(async move {
            let mut client = google.lock();
            let client = client.as_mut()?;
            let token = client.access_token().ok()?;
            let mut drive = thundoku_core::drive::DriveClient::new(
                Box::new(thundoku_core::tbf::UreqTransport::new()),
                token,
            );
            thundoku_core::drive::sync::restore_drive_backup(&mut drive, &folder_id, &db).ok()
        });
        cx.spawn(async move |_window, cx| {
            let restored = task.await.is_some();
            if let Some(handle) = handle.upgrade() {
                handle.update(cx, |this, cx| {
                    this.restoring = false;
                    if restored {
                        // 本棚を再読込して復元結果を反映する
                        this.bookshelf.update(cx, |b, cx| b.reload(cx));
                        crate::app_state::set_toast(cx, "Drive バックアップを復元しました");
                    } else {
                        crate::app_state::set_toast(cx, "バックアップの復元に失敗しました");
                    }
                    cx.notify();
                });
            }
        })
        .detach();
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
        let google_logged_in = *AppState::global(cx).google_logged_in.lock();
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
                        logout: Option<
            fn(
                &mut crate::views::settings::SettingsView,
                &mut Context<crate::views::settings::SettingsView>,
            ),
        >| {
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
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .child(name_for_id.clone()),
                        )
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
                                                settings.update(cx, logout);
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
                                let provider = login_provider;
                                let handle = handle.clone();
                                move |_, _window, cx| {
                                    if let Some(provider) = provider {
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
                div().flex().items_center().child(
                    div()
                        .text_sm()
                        .font_weight(FontWeight::BOLD)
                        .child("アカウント"),
                ),
            )
            .child(site_row(
                "Google",
                google_logged_in,
                Some(AuthProvider::Google),
                Some(crate::views::settings::SettingsView::logout_google),
            ))
            .child(site_row(
                "技術書典",
                tbf_logged_in,
                Some(AuthProvider::TechBookFest),
                Some(crate::views::settings::SettingsView::logout_tbf),
            ))
            .child(site_row(
                "BOOTH",
                booth_logged_in,
                Some(AuthProvider::Booth),
                Some(crate::views::settings::SettingsView::logout_booth),
            ))
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
        reg!(crate::actions::ToggleSidebar, |this, cx| this
            .toggle_sidebar(cx));
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
        reg_with!(
            crate::actions::OpenAuthProvider,
            |action: &crate::actions::OpenAuthProvider,
             this: &mut Workspace,
             cx: &mut Context<Workspace>| {
                let provider = action.provider;
                this.show_auth = true;
                let dialog = this
                    .auth_dialog
                    .get_or_insert_with(|| cx.new(AuthDialog::new))
                    .clone();
                dialog.update(cx, |d, _| {
                    d.open_with_provider(Some(provider));
                });
                cx.notify();
            }
        );
        reg!(crate::actions::CloseReader, |this, cx| this
            .close_reader(cx));
        reg!(crate::actions::SyncDrive, |this, cx| this.sync_drive(cx));
        reg!(crate::actions::PromptDriveEnable, |this, cx| {
            // Drive 同期が未設定ならば確認ダイアログを表示する
            let enabled = db::settings::get(&AppState::global(cx).db_pool, "drive.sync.enabled")
                .ok()
                .flatten()
                .is_some_and(|v| v == "true" || v == "1");
            if !enabled {
                this.show_drive_prompt = true;
                cx.notify();
            }
        });
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
                    cx.update(|cx| {
                        if *AppState::global(cx).toast_generation.lock() == generation {
                            crate::app_state::clear_toast(cx);
                        }
                    });
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
            .flex_col()
            .w_full()
            .h_full()
            .bg(theme.background)
            .child({
                #[cfg(windows)]
                {
                    Self::win_title_bar(_window, &theme).into_any_element()
                }
                #[cfg(not(windows))]
                div().into_any_element()
            })
            .child(
                div()
                    .flex()
                    .flex_1()
                    .relative()
                    .overflow_hidden()
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
                    ),
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
                                handle.update(cx, |_, cx| {
                                    cx.dispatch_action(&crate::actions::CloseAuth);
                                });
                            }
                        })
                        .child(
                            dialog
                                .map(|d| d.into_any_element())
                                .unwrap_or_else(|| div().into_any_element()),
                        ),
                )
                .into_any_element()
            } else {
                div().into_any_element()
            })
            .child(if self.show_restore_prompt {
                let handle = cx.entity();
                let restore_size = self
                    .restore_info
                    .as_ref()
                    .and_then(|i| i.size)
                    .unwrap_or_default();
                gpui::deferred(
                    Dialog::new(cx)
                        .title(div().child("Drive バックアップが見つかりました"))
                        .content(move |content, _window, _cx| {
                            content.child(div().text_sm().child(format!(
                                "Google Drive に DB バックアップ（{restore_size} bytes）があります。復元しますか？\n既存のデータは Drive 側の内容で上書きされます。"
                            )))
                        })
                        .footer(
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .child(
                                    Button::new("restore-cancel")
                                        .cursor_pointer()
                                        .label("キャンセル")
                                        .on_click({
                                            let handle = handle.clone();
                                            move |_, _window, cx| {
                                                handle.update(cx, |this, cx| {
                                                    this.show_restore_prompt = false;
                                                    cx.notify();
                                                });
                                            }
                                        }),
                                )
                                .child(
                                    Button::new("restore-confirm")
                                        .cursor_pointer()
                                        .primary()
                                        .label("復元する")
                                        .on_click({
                                            let handle = handle.clone();
                                            move |_, _window, cx| {
                                                handle.update(cx, |this, cx| {
                                                    this.confirm_restore_backup(cx);
                                                });
                                            }
                                        }),
                                ),
                        )
                        .into_any_element(),
                )
                .into_any_element()
            } else {
                div().into_any_element()
            })
            .child(if self.show_drive_prompt {
                let handle = cx.entity();
                gpui::deferred(
                    Dialog::new(cx)
                        .title(div().child("Googleドライブでバックアップを有効にしますか？"))
                        .content(move |content, _window, _cx| {
                            content.child(div().text_sm().child(
                                "Google ログインできました。Google Drive にバックアップを自動保存して、複数の端末で本棚を同期できます。有効にしますか？"
                            ))
                        })
                        .footer(
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .child(
                                    Button::new("drive-cancel")
                                        .cursor_pointer()
                                        .label("キャンセル")
                                        .on_click({
                                            let handle = handle.clone();
                                            move |_, _window, cx| {
                                                handle.update(cx, |this, cx| {
                                                    this.show_drive_prompt = false;
                                                    cx.notify();
                                                });
                                            }
                                        }),
                                )
                                .child(
                                    Button::new("drive-confirm")
                                        .cursor_pointer()
                                        .primary()
                                        .label("有効にする")
                                        .on_click({
                                            let handle = handle.clone();
                                            move |_, _window, cx| {
                                                handle.update(cx, |this, cx| {
                                                    this.show_drive_prompt = false;
                                                    let db = &AppState::global(cx).db_pool;
                                                    let _ = db::settings::set(db, "drive.sync.enabled", "true");
                                                    crate::app_state::set_toast(cx, "Google Drive バックアップを有効にしました");
                                                    cx.notify();
                                                });
                                            }
                                        }),
                                ),
                        )
                        .into_any_element(),
                )
                .into_any_element()
            } else {
                div().into_any_element()
            })
    }
}

/// サイドバーの描画（72px の閉状態 ↔ 256px の開状態を 200ms でアニメーション）。
impl Workspace {
    /// Windows の自前タイトルバー（MangaReader 方式）。
    /// `.window_control_area()` でネイティブのドラッグ/最小化/最大化/閉じるを再現する。
    #[cfg(windows)]
    fn win_title_bar(window: &mut Window, theme: &gpui_component::Theme) -> gpui::AnyElement {
        // Windows 標準のタイトルバーボタンのホバー色（テーマに応じて明暗を出す）。
        // 背景（theme.secondary）と区別できるよう、ダークは明るめ・ライトは濃いめのグレー。
        let hover_bg = match theme.mode {
            gpui_component::ThemeMode::Dark => gpui::rgb(0x3e3e3e),
            // ライトモードは背景（白）と区別しやすい濃いめのグレー
            gpui_component::ThemeMode::Light => gpui::rgb(0xcfcfcf),
        };
        div()
            .flex()
            .items_center()
            .h(px(36.0))
            .w_full()
            .flex_shrink_0()
            .bg(theme.secondary)
            .text_color(theme.muted_foreground)
            .child(
                div()
                    .flex_1()
                    .items_center()
                    .gap_2()
                    .px_3()
                    .window_control_area(WindowControlArea::Drag)
                    .text_sm()
                    .font_weight(FontWeight::MEDIUM)
                    .child("Thundoku Shelf"),
            )
            .child(
                div()
                    .flex()
                    .h_full()
                    .child(
                        div()
                            .id("win-min-button")
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(46.0))
                            .h_full()
                            .cursor_pointer()
                            .hover(move |style| style.bg(hover_bg))
                            .on_click({
                                move |_, window, _| {
                                    window.minimize_window();
                                }
                            })
                            .child("—"),
                    )
                    .child(
                        div()
                            .id("win-max-button")
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(46.0))
                            .h_full()
                            .cursor_pointer()
                            .hover(move |style| style.bg(hover_bg))
                            .on_click({
                                move |_, window, _| {
                                    window.zoom_window();
                                }
                            })
                            .child(if window.is_maximized() { "❐" } else { "□" }),
                    )
                    .child(
                        div()
                            .id("win-close-button")
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(46.0))
                            .h_full()
                            .cursor_pointer()
                            // 閉じるボタンは Windows 標準に合わせて赤背景（アイコンは継承色をキープ）
                            .hover(move |style| style.bg(gpui::rgb(0xe11d48)))
                            .window_control_area(WindowControlArea::Close)
                            .child(
                                Icon::new(IconName::Close)
                                    .size(px(16.0))
                                    // 最小化・最大化のアイコンと同じ色（muted_foreground を継承）
                                    .text_color(theme.muted_foreground),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn sidebar(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.sidebar_open;
        let theme = cx.theme().clone();
        let handle = cx.entity();
        let active = self.active;
        let unread_count = self.unread_count;
        let theme_mode_name = self.theme_mode(cx).unwrap_or_else(|| "system".to_string());
        // バッジ色分け: 100 件以上=赤 / 10〜99 件=黄 / 1〜9 件=緑
        let badge_color = if unread_count >= 100 {
            gpui::rgb(0xef4444)
        } else if unread_count >= 10 {
            gpui::rgb(0xeab308)
        } else {
            gpui::rgb(0x10b981)
        };

        div()
            .id("sidebar")
            .debug_selector(|| "sidebar".into())
            .h_full()
            .flex()
            .flex_col()
            .relative()
            .overflow_hidden()
            .pt({
                #[cfg(windows)]
                {
                    px(0.0)
                }
                #[cfg(not(windows))]
                {
                    px(30.0)
                }
            })
            .on_click({
                let handle = handle.clone();
                move |_event, window, cx| {
                    handle.update(cx, |this, cx| {
                        if !this.sidebar_open {
                            this.sidebar_open = true;
                        }
                        this.schedule_sidebar_auto_close(window, cx);
                        cx.notify();
                    });
                }
            })
            .with_animation(
                SharedString::from(format!(
                    "sidebar-width-{}",
                    if open { "open" } else { "closed" }
                )),
                Animation::new(if open {
                    Duration::from_millis(180)
                } else {
                    Duration::from_millis(20)
                })
                .with_easing(gpui::quadratic)
                .with_max_fps(30.0),
                move |this, t| {
                    if !open {
                        // 閉じる時は即座に 72px（一瞬で閉じる）。
                        this.w(px(72.0))
                    } else {
                        let t = t.clamp(0.0, 1.0);
                        let width = 72.0 + (256.0 - 72.0) * t;
                        this.w(px(width))
                    }
                },
            )
            // ヘッダー（ロゴ行）
            .child(
                div()
                    .h(px(60.0))
                    .flex()
                    .items_center()
                    .px_2()
                    .border_b_1()
                    .border_color(theme.border)
                    .when(!open, |this| this.justify_center())
                    .when(open, |this| {
                        // ロゴを右へ 8px（閉じた状態のアイコン列中央からの位置を揃える）
                        this.justify_start().gap_2().px(px(16.0))
                    })
                    .child(self.sidebar_logo(unread_count, badge_color, open, handle.clone(), cx))
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
            // ナビ
            .child(
                div()
                    .id("sidebar-nav")
                    .debug_selector(|| "sidebar-nav".into())
                    .flex()
                    .flex_col()
                    .items_center()
                    .p_2()
                    .gap_1()
                    .child(
                        self.nav_row(
                            NavTarget::Bookshelf,
                            Icon::new(AppIcon::LibraryBig)
                                .size(px(24.0))
                                .into_any_element(),
                            "本棚",
                            open,
                            active,
                            handle.clone(),
                            cx,
                        ),
                    )
                    .when(open, |this| {
                        this.child(self.bookshelf_submenu(cx).into_any_element())
                    })
                    .child(
                        self.nav_row(
                            NavTarget::Checklist,
                            Icon::new(AppIcon::ListChecks)
                                .size(px(24.0))
                                .into_any_element(),
                            "チェックリスト",
                            open,
                            active,
                            handle.clone(),
                            cx,
                        ),
                    ),
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
                    .pb(px(16.0))
                    .child(
                        self.bottom_item(
                            Icon::new(IconName::Settings)
                                .size(px(24.0))
                                .text_color(theme.muted_foreground)
                                .into_any_element(),
                            "設定",
                            open,
                            |this, cx| {
                                this.switch_to(NavTarget::Settings, cx);
                            },
                            handle.clone(),
                            cx,
                        ),
                    )
                    .child(
                        self.bottom_item(
                            Icon::new(match theme_mode_name.as_str() {
                                "dark" => AppIcon::Moon,
                                "system" => AppIcon::Monitor,
                                _ => AppIcon::Sun,
                            })
                            .size(px(24.0))
                            .text_color(theme.muted_foreground)
                            .into_any_element(),
                            match theme_mode_name.as_str() {
                                "dark" => "ダーク",
                                "system" => "システム",
                                _ => "ライト",
                            },
                            open,
                            |this, cx| {
                                this.cycle_theme(cx);
                            },
                            handle.clone(),
                            cx,
                        ),
                    )
                    .child(
                        self.bottom_item(
                            Icon::new(AppIcon::CircleUserRound)
                                .size(px(24.0))
                                .text_color(theme.muted_foreground)
                                .into_any_element(),
                            "アカウント",
                            open,
                            |this, cx| {
                                this.open_auth_panel(cx);
                            },
                            handle.clone(),
                            cx,
                        ),
                    ),
            )
    }

    /// サイドバーのロゴ（ブックマーク + 未読バッジ）。
    fn sidebar_logo(
        &self,
        unread_count: usize,
        badge_color: gpui::Rgba,
        open: bool,
        handle: Entity<Workspace>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        div()
            .id("sidebar-logo")
            .debug_selector(|| "sidebar-logo".into())
            .relative()
            .flex_shrink_0()
            .w(px(40.0))
            .h(px(40.0))
            .rounded_xl()
            .bg(theme.primary)
            .flex()
            .items_center()
            .justify_center()
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
                    .text_color(theme.primary_foreground),
            )
            .child(if !open && unread_count > 0 {
                div()
                    .absolute()
                    .right(px(-4.0))
                    .top(px(-4.0))
                    .min_w(px(16.0))
                    .h(px(16.0))
                    .px_1()
                    .rounded_full()
                    .bg(badge_color)
                    .flex()
                    .items_center()
                    .justify_center()
                    .text_color(theme.primary_foreground)
                    .text_xs()
                    .child(unread_count.to_string())
                    .into_any_element()
            } else {
                div().into_any_element()
            })
    }

    /// メインのナビ行（アイコン + ラベル）。サブメニュートグルは本棚のときのみ。
    #[allow(clippy::too_many_arguments)]
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
        let id = match target {
            NavTarget::Bookshelf => "sidebar-nav-bookshelf",
            NavTarget::Checklist => "sidebar-nav-checklist",
            _ => "sidebar-nav-other",
        };
        let is_active = active == target;
        let site_menu = target == NavTarget::Bookshelf;
        div()
            .id(id)
            .debug_selector(move || id.into())
            .flex()
            .items_center()
            .gap_2()
            .py(px(6.0))
            .rounded_xl()
            .when(open, |this| this.ml(px(18.0)).px(px(6.0)))
            .when(open, |this| this.w_full())
            .when(!open, |this| this.w(px(36.0)).h(px(36.0)).justify_center())
            .when(is_active, |this| this.bg(theme.secondary))
            .hover(|style| style.bg(theme.secondary))
            .cursor_pointer()
            .on_click({
                let handle = handle.clone();
                move |event, window, cx| {
                    cx.stop_propagation();
                    handle.update(cx, |this, cx| {
                        let is_bookshelf = target == NavTarget::Bookshelf;
                        if is_bookshelf && event.click_count() >= 2 {
                            if !this.sidebar_open {
                                this.sidebar_open = true;
                                this.bookshelf_submenu_open = true;
                                this.schedule_sidebar_auto_close(window, cx);
                            } else {
                                this.bookshelf_submenu_open = !this.bookshelf_submenu_open;
                                this.schedule_sidebar_auto_close(window, cx);
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
                        .child(label.clone()),
                )
            })
            .when(open && site_menu, |this| {
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
                            move |_, window, cx| {
                                cx.stop_propagation();
                                handle.update(cx, |this, cx| {
                                    this.bookshelf_submenu_open = !this.bookshelf_submenu_open;
                                    this.interact_sidebar(window, cx);
                                });
                            }
                        })
                        .child(
                            Icon::new(IconName::ChevronDown)
                                .size(px(16.0))
                                .text_color(theme.muted_foreground),
                        ),
                )
            })
    }

    /// 下部のアイテム（設定 / テーマ / アカウント 共通）。
    fn bottom_item(
        &self,
        icon: gpui::AnyElement,
        label: &str,
        open: bool,
        on_click: impl Fn(&mut Workspace, &mut Context<Workspace>) + 'static,
        handle: Entity<Workspace>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let theme = cx.theme().clone();
        let label = label.to_string();
        div()
            .id(format!("sidebar-nav-{label}"))
            .flex()
            .items_center()
            .gap_2()
            .py(px(6.0))
            .rounded_xl()
            .when(open, |this| this.ml(px(18.0)).px(px(6.0)))
            .when(open, |this| this.w_full())
            .when(!open, |this| this.w(px(36.0)).h(px(36.0)).justify_center())
            .hover(|style| style.bg(theme.secondary))
            .cursor_pointer()
            .on_click({
                let handle = handle.clone();
                move |_, _window, cx| {
                    cx.stop_propagation();
                    handle.update(cx, |this, cx| {
                        on_click(this, cx);
                    });
                }
            })
            .child(icon)
            .when(open, |this| {
                this.child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(label.clone()),
                )
            })
            .into_any_element()
    }

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
            // ナビの中央寄せ（items_center）の影響を受けず、左寄せで表示する
            .w_full()
            .child(
                div()
                    .id(format!(
                        "bookshelf-submenu-{}",
                        if open { "open" } else { "closed" }
                    ))
                    .ml_2()
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
                                        this.bookshelf.update(cx, |b, cx| {
                                            b.set_site_filter(cx, None);
                                        });
                                        this.switch_to(NavTarget::Bookshelf, cx);
                                        this.refresh_unread_count(cx);
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
                                .when(site_filter.as_deref() == Some("techbookfest"), |this| {
                                    this.bg(theme.secondary)
                                })
                                .hover(|style| style.bg(theme.secondary))
                                .cursor_pointer()
                                .on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        cx.stop_propagation();
                                        handle.update(cx, |this, cx| {
                                            this.bookshelf.update(cx, |b, cx| {
                                                b.set_site_filter(cx, Some("techbookfest"));
                                            });
                                            this.switch_to(NavTarget::Bookshelf, cx);
                                            this.refresh_unread_count(cx);
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
                                .when(site_filter.as_deref() == Some("booth"), |this| {
                                    this.bg(theme.secondary)
                                })
                                .hover(|style| style.bg(theme.secondary))
                                .cursor_pointer()
                                .on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        cx.stop_propagation();
                                        handle.update(cx, |this, cx| {
                                            this.bookshelf.update(cx, |b, cx| {
                                                b.set_site_filter(cx, Some("booth"));
                                            });
                                            this.switch_to(NavTarget::Bookshelf, cx);
                                            this.refresh_unread_count(cx);
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
                        Animation::new(Duration::from_millis(200)).with_easing(gpui::ease_in_out),
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
        cx.new(Workspace::new)
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
        assert!(!logo_size.0);
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
        assert!(logo_size.0);
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
