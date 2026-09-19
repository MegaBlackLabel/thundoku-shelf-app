//! Main workspace: sidebar navigation + active view + theme switching.
//!
//! The app is a single-window workspace where the left sidebar hosts
//! navigation (本棚 / チェックリスト / 設定 / 説明) and the right pane renders
//! the active view. The workspace also owns the global toast host, the
//! auth/account status panel, the Reader overlay and theme management.

use std::time::Duration;

#[cfg(windows)]
use gpui_kit::WindowControlArea;
use gpui_kit::{
    Animation, AnimationExt as _, AppContext as _, InteractiveElement as _, ReadGlobal as _,
    StatefulInteractiveElement as _, Styled as _, prelude::FluentBuilder as _,
};

use crate::components::dialog::{dialog_button, dialog_surface, fade_dialog};
use gpui_kit::component::Disableable as _;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::Dialog;
use gpui_kit::component::notification::Notification;
use gpui_kit::component::notification::NotificationType;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Theme, ThemeMode, WindowExt as _};
use gpui_kit::{
    AnyView, App, Context, Entity, FontWeight, IntoElement, Menu, MenuItem, ParentElement, Render,
    SharedString, Window, div, px,
};
#[cfg(windows)]
use raw_window_handle::HasWindowHandle;

use crate::app_state::AppState;
use crate::icons::AppIcon;
use crate::views::about::AboutView;
use crate::views::auth::{AuthDialog, AuthProvider};
use crate::views::bookshelf::BookshelfView;
use crate::views::checklist::ChecklistView;
use crate::views::history::HistoryView;
use crate::views::notes::NotesView;
use crate::views::reader::ReaderView;
use crate::views::report::ReportView;
use crate::views::settings::SettingsView;
use thundoku_core::db;
use thundoku_core::db::{books, bookshelf, progress};
use thundoku_core::tbf;

/// アクティブなナビゲーション先。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NavTarget {
    Bookshelf,
    History,
    Notes,
    Checklist,
    /// GitHub Issue を作るレポート画面（GitHub ログイン時のみ導線を出す）。
    Report,
    Settings,
    About,
}

/// 「アップロードして終了」の実行中に出す通知。
///
/// 確認ダイアログは押した時点で閉じるため、これが進行中の唯一の手がかりになる。
const EXIT_UPLOAD_NOTICE: &str =
    "バックアップをアップロード中です…（完了するとアプリが終了します）";

/// 進行中の通知の id（`Notification::id`）。
///
/// 同じ id の通知は**置き換わる**ので、進捗のたびに積み直しても増えない。
/// 進行中でない通知は id を持たない（別々に積まれる）ままにする。
struct ProgressNotice;

/// アプリメニュー（macOS のメニューバー相当。gpui の set_menus に渡す）。
///
/// `quit_enabled` が false のときは「終了」を無効化する（終了時のアップロード中など、
/// メニューから中断させたくないとき）。`checklist_enabled` が false のときは
/// 「チェックリスト」を無効化する（技術書典に未ログインのとき）。
pub fn app_menus(quit_enabled: bool, checklist_enabled: bool) -> Vec<Menu> {
    let app = Menu::new("App").items([
        MenuItem::action("Thundoku Shelf について", crate::actions::OpenAbout),
        MenuItem::separator(),
        MenuItem::action("終了", crate::actions::QuitApp).disabled(!quit_enabled),
    ]);
    let view = Menu::new("表示").items([
        MenuItem::action("サイドバーを切り替え", crate::actions::ToggleSidebar),
        MenuItem::action("テーマを切り替え", crate::actions::ToggleTheme),
        MenuItem::separator(),
        MenuItem::action("本棚", crate::actions::ShowBookshelf),
        MenuItem::action("閲覧履歴", crate::actions::ShowHistory),
        MenuItem::action("付箋", crate::actions::ShowNotes),
        MenuItem::action("チェックリスト", crate::actions::ShowChecklist)
            .disabled(!checklist_enabled),
        MenuItem::action("設定", crate::actions::ShowSettings),
        MenuItem::action("説明", crate::actions::ShowAbout),
    ]);
    vec![app, view]
}

/// アプリの現在の状態に合わせてアプリメニューを組み直す（macOS のメニューバー。
/// Windows では no-op）。
///
/// 可否は `AppState` から読む（「終了」= 終了時のアップロード中でないこと、
/// 「チェックリスト」= 技術書典にログイン済みであること）。判定を 1 箇所に集約する
/// ため、状態を変えた側は `app_menus` ではなくこれを呼ぶ。
pub fn sync_app_menus(cx: &App) {
    let state = AppState::global(cx);
    let quit_enabled = !state
        .exit_uploading
        .load(std::sync::atomic::Ordering::SeqCst);
    let checklist_enabled = *state.tbf_logged_in.lock();
    cx.set_menus(app_menus(quit_enabled, checklist_enabled));
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
    pub history: Entity<HistoryView>,
    pub notes: Entity<NotesView>,
    checklist: Entity<ChecklistView>,
    about: Entity<AboutView>,
    /// レポート画面（GitHub Issue を作る）。
    report: Entity<ReportView>,
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
    /// ログイン中（専用のダミー画面を表示中）か。
    auth_loading: bool,
    /// ウィンドウを閉じる時に、バックアップ対象の変更を確認中か。
    exit_upload_prompt: bool,
    /// 終了時のバックアップアップロード実行中か。
    exit_uploading: bool,
}

impl Workspace {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let settings = cx.new(SettingsView::new);
        let bookshelf = cx.new(BookshelfView::new);
        let history = cx.new(HistoryView::new);
        let notes = cx.new(NotesView::new);
        let checklist = cx.new(ChecklistView::new);
        let about = cx.new(AboutView::new);
        let report = cx.new(ReportView::new);

        let mut this = Self {
            active: NavTarget::Bookshelf,
            sidebar_open: false,
            sidebar_close_generation: 0,
            bookshelf_submenu_open: false,
            unread_count: 0,
            toast_host_generation: 0,
            settings,
            bookshelf,
            history,
            notes,
            checklist,
            about,
            report,
            auth_panel_open: false,
            show_auth: false,
            auth_dialog: None,
            reader: None,
            show_restore_prompt: false,
            restore_info: None,
            restoring: false,
            show_drive_prompt: false,
            auth_loading: false,
            exit_upload_prompt: false,
            exit_uploading: false,
        };
        this.register_actions(cx);
        this.refresh_unread_count(cx);
        // 保存済みモードを適用する前に、ダークの面の階層を引き直しておく
        crate::theme::apply_dark_surfaces(cx);
        crate::theme::apply_light_surfaces(cx);
        this.restore_theme_mode(cx);
        // 保存済みプロフィールが無い場合（アップグレード直後）はここで取得しておく。
        // 所有者（sub）が分からないと、終了時のバックアップがスキップされ、復元確認も
        // 出せない（[`backup_owner_ids`] を参照）。
        this.restore_google_profile(cx);
        this.check_startup_backup(cx);
        this.start_login_done_watcher(cx);
        this.start_checklist_poller(cx);
        this
    }

    /// Google / GitHub ログイン（成功・失敗）完了フラグを監視し、認証モーダルを閉じる。
    /// 技術書典のログイン状態（`tbf_logged_in`）も同じループで見張り、変わったら
    /// アプリメニューを組み直す（メニューはアプリ全体で 1 つなので、サイドバーの
    /// ように描画のたびに読むことができない）。
    /// `AuthDialog` から `Workspace` を直接 update すると RefCell 再入問題で固まるため、
    /// AppState のフラグを追ってここで状態をリセットする。
    /// GitHub（Device Flow）は view 側が完了時に `github_login_done` を立てる。
    fn start_login_done_watcher(&mut self, cx: &mut Context<Self>) {
        let handle = cx.weak_entity();
        let flag = AppState::global(cx).google_login_done.clone();
        let github_flag = AppState::global(cx).github_login_done.clone();
        let logout_flag = AppState::global(cx).google_logout_done.clone();
        let auth_open = AppState::global(cx).auth_open_requested.clone();
        let auth_provider = AppState::global(cx).auth_open_provider.clone();
        let db = AppState::global(cx).db_pool.clone();
        // 技術書典のログイン状態（メニューの「チェックリスト」の可否に効く）
        let tbf_logged_in = AppState::global(cx).tbf_logged_in.clone();
        let mut tbf_was_logged_in = *tbf_logged_in.lock();
        cx.spawn(async move |_window, cx| {
            loop {
                cx.background_executor()
                    .timer(std::time::Duration::from_millis(100))
                    .await;
                // 認証モーダルを開く要求（SettingsView → 直接 update の RefCell 再入を回避）
                if auth_open.load(std::sync::atomic::Ordering::SeqCst) {
                    auth_open.store(false, std::sync::atomic::Ordering::SeqCst);
                    let provider = auth_provider.lock().take();
                    let _ = handle.update(cx, |this, cx| {
                        this.show_auth = true;
                        let dialog = this
                            .auth_dialog
                            .get_or_insert_with(|| cx.new(AuthDialog::new))
                            .clone();
                        if let Some(provider) = provider {
                            dialog.update(cx, |d, _| {
                                d.open_with_provider(Some(provider));
                            });
                        }
                        // Workspace 自身の Context でエンティティ通知
                        // （サイドバーと同じ構造。cx.refresh より安全）
                        cx.notify();
                    });
                }
                if flag.load(std::sync::atomic::Ordering::SeqCst) {
                    flag.store(false, std::sync::atomic::Ordering::SeqCst);
                    // Drive 設定をバックグラウンドで照会（RefCell 借用中のブロッキング SQL を避ける）
                    // 「最終同期が無い（一度も同期していない）」なら同期確認を出す。
                    let last_sync = db::settings::get(&db, "drive.last_sync_at").ok().flatten();
                    let _ = handle.update(cx, |this, cx| {
                        this.show_auth = false;
                        this.auth_dialog = None;
                        // ログイン完了後は設定画面に戻す
                        this.auth_loading = false;
                        this.active = NavTarget::Settings;
                        this.sidebar_open = true;
                        if last_sync.is_none() {
                            this.show_drive_prompt = true;
                        }
                        // ログイン状態が変わったので本棚を再フィルタ（owner モデル）。
                        this.bookshelf.update(cx, |b, bx| b.reload(bx));
                    });
                    // cx.notify() は RefCell already borrowed を起こすため、
                    // AsyncApp::refresh()（&self）で再描画を要求する。
                    cx.refresh();
                }
                // ログアウトで未ログインに戻ったら本棚を再フィルタ（未所属のみ表示）。
                if logout_flag.load(std::sync::atomic::Ordering::SeqCst) {
                    logout_flag.store(false, std::sync::atomic::Ordering::SeqCst);
                    let _ = handle.update(cx, |this, cx| {
                        this.bookshelf.update(cx, |b, bx| b.reload(bx));
                    });
                    cx.refresh();
                }
                // GitHub ログイン（Device Flow）の完了。Google と同じく認証モーダルを閉じて
                // 設定画面へ戻す。GitHub は書店ではないので本棚の再フィルタは不要。
                if github_flag.load(std::sync::atomic::Ordering::SeqCst) {
                    github_flag.store(false, std::sync::atomic::Ordering::SeqCst);
                    let _ = handle.update(cx, |this, _cx| {
                        this.show_auth = false;
                        this.auth_dialog = None;
                        this.auth_loading = false;
                        this.active = NavTarget::Settings;
                        this.sidebar_open = true;
                    });
                    // cx.notify() は RefCell already borrowed を起こすため、
                    // AsyncApp::refresh()（&self）で再描画を要求する。
                    cx.refresh();
                }
                // 技術書典のログイン状態が変わったらアプリメニューを組み直す
                // （「チェックリスト」の可否。ログイン / ログアウトのどちらの経路でも
                //   ここを通るので、呼び出し側ごとに set_menus を書かなくてよい）。
                let tbf_now = *tbf_logged_in.lock();
                if tbf_now != tbf_was_logged_in {
                    tbf_was_logged_in = tbf_now;
                    let _ = handle.update(cx, |_this, cx| sync_app_menus(cx));
                }
            }
        })
        .detach();
    }

    /// チェックリストの定期取得（ポーリング）を開始する。Workspace 常駐で、
    /// アプリ起動中はいつでも取得する。各周期で `checklist.poll.interval_min` と
    /// 各イベントの ON/OFF（`tbf_events.poll_sync_enabled`）を再読みするため、
    /// 設定・トグルの変更は次の周期から反映される。tbf クライアントの Mutex が
    /// 手動同期と直列化する。Drive 同期はデータ変化時のみ（`sync_drive_now` が
    /// 内部で md5 差分を判定するので無変化ならノーコスト）。
    fn start_checklist_poller(&mut self, cx: &mut Context<Self>) {
        let db = AppState::global(cx).db_pool.clone();
        let tbf_client = AppState::global(cx).tbf.clone();
        let tbf_logged_in = AppState::global(cx).tbf_logged_in.clone();
        let settings_entity = self.settings.downgrade();
        cx.spawn(async move |_window, cx| {
            // セッション切れの OpenAuth は 1 回だけ出す（連続で出さない）
            let mut auth_dispatched = false;
            loop {
                let interval_min = db::settings::get(&db, "checklist.poll.interval_min")
                    .ok()
                    .flatten()
                    .and_then(|v| v.parse::<i64>().ok())
                    .map(|n| n.max(1))
                    .unwrap_or(5);
                // 未ログインならスキップ（次の周期へ）。tbf ログイン必須。
                if !*tbf_logged_in.lock() {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(interval_min as u64 * 60))
                        .await;
                    continue;
                }
                let enabled = db::checklist::list_enabled_slugs(&db).unwrap_or_default();
                if enabled.is_empty() {
                    cx.background_executor()
                        .timer(std::time::Duration::from_secs(interval_min as u64 * 60))
                        .await;
                    continue;
                }
                // tbf クライアントの Mutex ロックで手動 sync() と直列化する。
                // スコープを限定して await の前に必ず解放する（clippy: await_holding_lock）。
                let (any_changed, session_expired) = {
                    let mut client = tbf_client.lock();
                    let mut any_changed = false;
                    let mut session_expired = false;
                    for slug in &enabled {
                        match tbf::sync::refresh_checklist(&db, &mut client, slug) {
                            Ok(outcome) => {
                                if outcome.changed {
                                    any_changed = true;
                                }
                            }
                            Err(message) => {
                                log::error!("checklist poll failed for {slug}: {message}");
                                if message.contains("session expired") {
                                    session_expired = true;
                                }
                            }
                        }
                    }
                    (any_changed, session_expired)
                };
                if session_expired && !auth_dispatched {
                    auth_dispatched = true;
                    cx.update(|app| {
                        app.defer(|app| app.dispatch_action(&crate::actions::OpenAuth));
                    });
                }
                if any_changed {
                    let _ = settings_entity.update(cx, |settings, cx| settings.sync_drive_now(cx));
                }
                cx.background_executor()
                    .timer(std::time::Duration::from_secs(interval_min as u64 * 60))
                    .await;
            }
        })
        .detach();
    }

    /// 現在のアクティブビューを返す（エンティティの参照）。
    fn active_view(&self, _cx: &Context<Self>) -> AnyView {
        match self.active {
            NavTarget::Bookshelf => AnyView::from(self.bookshelf.clone()),
            NavTarget::History => AnyView::from(self.history.clone()),
            NavTarget::Notes => AnyView::from(self.notes.clone()),
            NavTarget::Checklist => AnyView::from(self.checklist.clone()),
            NavTarget::Report => AnyView::from(self.report.clone()),
            NavTarget::Settings => AnyView::from(self.settings.clone()),
            NavTarget::About => AnyView::from(self.about.clone()),
        }
    }
    /// ナビゲーション先を切り替える。
    fn switch_to(&mut self, target: NavTarget, cx: &mut Context<Self>) {
        self.active = target;
        let had_reader = self.reader.is_some();
        if self.reader.is_some() {
            self.reader = None;
        }
        // リーダーを開いたままナビゲーションを切り替えた場合（サイドバー等）も、
        // 読書で進んだ進捗・読了フラグを本棚キャッシュへ反映する
        if had_reader {
            self.bookshelf.update(cx, |b, cx| b.reload(cx));
            self.refresh_unread_count(cx);
        }
        // 履歴は開いた時点のデータを出す（読書直後の新しいセッションを反映する）
        if target == NavTarget::History {
            self.history.update(cx, |h, cx| h.reload(cx));
        }
        if target == NavTarget::Notes {
            self.notes.update(cx, |n, cx| n.reload(cx));
        }
        // ナビゲーションを切り替えたらログイン中のダミー画面を終了する
        // （例: ブックマークアイコンで説明画面を開いたとき）。
        self.auth_loading = false;
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
        if mode == "system" {
            // システムの明暗に追従する
            Theme::sync_system_appearance(None, cx);
        } else {
            let theme_mode = match mode {
                "dark" => ThemeMode::Dark,
                _ => ThemeMode::Light,
            };
            Theme::change(theme_mode, None, cx);
        }
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
        // - ダウンロード済み: 進捗から `ReadingState` で判定（未読 / 読書中 / 読了）
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
                                // 表示・フィルタと同じ判定（未読 = 進捗が無い / 1 ページ目も
                                // 読んでいない）。読書中は未読に数えない。
                                progress::ReadingState::from_progress(progress.as_ref())
                                    == progress::ReadingState::Unread
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
    /// 最後にアップロードした内容から Drive 側が動いていれば復元確認を表示する
    /// （ローカル側だけが進んだ場合は何も出さない）。
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
        // 差分判定は所有者ベースで揃える。所有者（ログイン中の sub）が分からないときは
        // 比較しない: Drive 側は所有者で絞られたバックアップなので、ローカル全件と
        // 突き合わせると内容が同じでも必ず差分ありになり、起動のたびに復元確認が出る。
        let Some(book_ids) = backup_owner_ids(state) else {
            log::info!(
                "startup backup check: 所有者（Google プロフィール）が未取得のため復元確認をスキップ"
            );
            return;
        };
        // 最後にアップロードしたバックアップの内容（基準値）。Drive 側がこれから
        // 変わっていないなら、差分はローカル側だけのもの＝復元する必要はない。
        let baseline = db::settings::get(&db, thundoku_core::drive::sync::BACKUP_BASELINE_KEY)
            .ok()
            .flatten();
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
            let status = thundoku_core::drive::sync::inspect_drive_backup(
                &db,
                &mut drive,
                &folder_id,
                Some(&book_ids),
                baseline.as_deref(),
            )
            .ok()??;
            // Drive 側が最後のアップロードから動いていないときは復元候補にしない。
            // （ローカル側だけが進んでいる場合に「復元しますか」を出すと、新しい
            // ローカルを古いバックアップで上書きしてしまう）
            if !status.should_offer_restore() {
                log::info!(
                    "startup backup check: skip (drive_changed={}, local_differs={})",
                    status.drive_changed,
                    status.local_differs
                );
                return None;
            }
            log::info!(
                "startup backup check: drive backup moved since the last upload (md5={:?})",
                status.info.md5
            );
            Some(status.info)
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

    /// 取り込み確認モーダルが出ている間はビューアーを開かない。
    ///
    /// モーダルはビューアーの下（本棚）に描かれるため、ビューアーを重ねると
    /// 選択肢が見えず操作できなくなる。
    fn reader_blocked_by_import_confirm(&self, cx: &App) -> bool {
        self.bookshelf.read(cx).has_pending_import()
    }

    /// リーダーを開く（本棚・チェックリストからの委譲）。
    pub fn open_reader(&mut self, cx: &mut Context<Self>, book_id: String) {
        if self.reader_blocked_by_import_confirm(cx) {
            return;
        }
        let reader = cx.new(|cx| ReaderView::for_book(cx, book_id));
        self.reader = Some(reader);
        cx.notify();
    }

    /// 付箋から本を開く（指定ページ + 見開きの左右を復元する）。
    pub fn open_reader_at(
        &mut self,
        cx: &mut Context<Self>,
        book_id: String,
        page: i64,
        content_id: String,
        side: Option<thundoku_core::db::notes::SpreadSide>,
    ) {
        if self.reader_blocked_by_import_confirm(cx) {
            return;
        }
        let reader = cx.new(|cx| ReaderView::for_book_at(cx, book_id, page, content_id, side));
        self.reader = Some(reader);
        cx.notify();
    }

    /// サンプルページ（チェックリストの試し読み）を開く。
    pub fn open_sample_reader(&mut self, cx: &mut Context<Self>, item_id: String) {
        if self.reader_blocked_by_import_confirm(cx) {
            return;
        }
        let reader = cx.new(|cx| ReaderView::for_sample(cx, item_id));
        self.reader = Some(reader);
        cx.notify();
    }

    /// リーダーを閉じる。
    pub fn close_reader(&mut self, cx: &mut Context<Self>) {
        // 読んでいた本を本棚の選択状態に戻し、フォーカスを取り直す
        // （矢印キー・hjkl で再び選択操作できるようにする）。
        let book_id = self
            .reader
            .as_ref()
            .and_then(|reader| reader.read(cx).book_id());
        if let Some(reader) = self.reader.take() {
            reader.update(cx, |r, cx| r.end_session(cx));
        }
        // 読書で進んだ進捗・読了フラグを本棚カードと未読バッジに反映する
        // （reload しないと本棚キャッシュが開く前のまま残り、
        //  読了済みなのに未読・1/XX 表示が残ってしまう）
        // リーダーで付箋を付け外しした結果を付箋画面にも反映する
        self.notes.update(cx, |n, cx| n.reload(cx));
        if let Some(book_id) = book_id {
            self.bookshelf.update(cx, |b, cx| {
                b.reload(cx);
                b.restore_selection(cx, book_id.as_ref());
            });
        }
        self.refresh_unread_count(cx);
        cx.notify();
    }

    /// 起動時の Drive 復元確認（`show_restore_prompt`）が表示中か。
    /// 終了判定（close ハンドラ）から参照するための公開アクセサ。
    pub fn restore_prompt_active(&self) -> bool {
        self.show_restore_prompt
    }

    /// ウィンドウの「閉じる」要求を処理する（true = 閉じてよい）。
    ///
    /// 終了時のアップロード中は、進行中のアップロードを中断させないため閉じない
    /// （閉じるボタンは無効にできないので、ここで要求を止める）。
    pub fn handle_window_close_request(&mut self, cx: &mut Context<Self>) -> bool {
        if self.exit_uploading {
            return false;
        }
        if AppState::global(cx)
            .exit_checked
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            // 確認ダイアログを既に表示し、キャンセルされていない → そのまま閉じる
            return true;
        }
        AppState::global(cx)
            .exit_checked
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // 起動時の Drive 復元確認が出ている間は、そのまま閉じる（Drive 側のバックアップを
        // ローカル（旧/空）で上書きしないため、アップロード確認は出さない）。
        if self.restore_prompt_active() {
            return true;
        }
        self.request_exit_upload_check(cx);
        false
    }

    /// ウィンドウを閉じる時に、バックアップ対象に変更があるかを確認する。
    pub fn request_exit_upload_check(&mut self, cx: &mut Context<Self>) {
        self.exit_upload_prompt = true;
        cx.notify();
    }

    /// メニューの「終了」から終了する（`QuitApp` アクションのハンドラ）。
    ///
    /// ウィンドウを閉じる場合と同じアップロード確認を出す。ただし起動時の Drive
    /// 復元確認（`show_restore_prompt`）が表示中のときは、ローカル（旧/空）で
    /// Drive を上書きしないよう確認を出さずに終了する（確認ダイアログ自体が
    /// `show_restore_prompt` 中は描画されないため、ここで分岐しないと「終了を
    /// 押しても何も起きない」になる）。
    pub fn quit_from_menu(&mut self, cx: &mut Context<Self>) {
        if self.restore_prompt_active() {
            AppState::global(cx)
                .exit_checked
                .store(true, std::sync::atomic::Ordering::SeqCst);
            cx.quit();
            return;
        }
        self.request_exit_upload_check(cx);
    }

    /// 指定プロバイダの認証モーダルを開く。`dispatch_action` を使わず直接 state を
    /// 更新する（RefCell 再入で固まるのを回避）。設定画面のログインボタンから呼ばれる。
    pub fn open_auth(
        &mut self,
        cx: &mut Context<Self>,
        provider: crate::views::auth::AuthProvider,
    ) {
        // SettingsView を非表示にしてからダイアログを表示する（WebView 作成時の RefCell 競合回避）。
        // 本棚ではなく専用のダミー画面を表示する。
        self.auth_loading = true;
        self.show_auth = true;
        let dialog = self
            .auth_dialog
            .get_or_insert_with(|| cx.new(AuthDialog::new))
            .clone();
        dialog.update(cx, |d, _| {
            d.open_with_provider(Some(provider));
        });
        // 本棚に切り替えた（SettingsView 非表示）ので、cx.notify で再描画しても
        // WebView 作成時の RefCell 競合は発生しない。
        cx.notify();
    }

    /// 認証モーダルを閉じる。`dispatch_action` を使わず直接 state を更新する
    /// （ウィンドウの RefCell 再入問題でアプリが固まるのを避ける）。
    pub fn close_auth(&mut self, cx: &mut Context<Self>) {
        self.show_auth = false;
        self.auth_dialog = None;
        // cx.notify() はウィンドウの RefCell 借用中だと RefCell already borrowed で
        // アプリが固まるため、次のフレームで通知する。
        let entity_id = cx.entity().entity_id();
        cx.defer(move |cx| cx.notify(entity_id));
    }

    /// Google ログイン完了時の後処理。認証モーダルを閉じ、Drive 同期が未設定なら
    /// 確認ダイアログを表示する（`PromptDriveEnable` と同じ分岐）。
    pub fn login_done(&mut self, cx: &mut Context<Self>) {
        self.show_auth = false;
        self.auth_dialog = None;
        // ブロッキング SQL（db::settings::get）を cx の RefCell 借用中に実行すると
        // RefCell already borrowed でアプリが固まるため、バックグラウンドで取得する。
        let db = AppState::global(cx).db_pool.clone();
        let handle = cx.entity();
        cx.spawn(async move |_window, cx| {
            let enabled = db::settings::get(&db, "drive.sync.enabled")
                .ok()
                .flatten()
                .is_some_and(|v| v == "true" || v == "1");
            handle.update(cx, |this, _cx| {
                if !enabled {
                    this.show_drive_prompt = true;
                }
            });
            // cx.notify() は RefCell already borrowed を起こすため、
            // AsyncApp::refresh()（&self）で再描画を要求する。
            cx.refresh();
        })
        .detach();
    }

    /// 終了時の確認ダイアログで「アップロードして終了」を押したとき。
    pub fn confirm_exit_upload(&mut self, cx: &mut Context<Self>) {
        self.exit_upload_prompt = false;
        self.exit_uploading = true;
        // 確認ダイアログは閉じるので、進行中であることを通知で知らせる
        // （見た目が何も変わらないと、終了したのか固まったのか分からない）。
        // 完了（アプリ終了）まで消えない通知にする（長いアップロードでも見失わない）
        crate::app_state::set_sticky_progress_notice(cx, EXIT_UPLOAD_NOTICE);
        AppState::global(cx)
            .exit_uploading
            .store(true, std::sync::atomic::Ordering::SeqCst);
        // アップロード中はメニューの「終了」も無効にする（macOS。Windows では no-op）
        sync_app_menus(cx);
        AppState::global(cx)
            .exit_checked
            .store(true, std::sync::atomic::Ordering::SeqCst);
        cx.notify();
        let handle = cx.entity();
        let state = AppState::global(cx);
        let google = state.google.clone();
        let db = state.db_pool.clone();
        let packs_dir = state.packs_dir.clone();
        let downloads_dir = state.downloads_dir.clone();
        let db_path = state.data_dir.join("thundoku-shelf.db");
        let google_sub = state.google_profile.lock().as_ref().map(|p| p.sub.clone());
        let db_key = state.secrets.db_key().ok();
        let task: gpui_kit::Task<Result<(), String>> = cx.background_executor().spawn(async move {
            let folder_id = db::settings::get(&db, "drive.sync.folder_id")
                .ok()
                .flatten()
                .ok_or_else(|| "Drive 同期が未設定です".to_string())?;
            let mut google_guard = google.lock();
            let client = google_guard
                .as_mut()
                .ok_or_else(|| "Google にログインしてください".to_string())?;
            let token = client.access_token().map_err(|e| e.to_string())?;
            let mut drive = thundoku_core::drive::DriveClient::new(
                Box::new(thundoku_core::tbf::UreqTransport::new()),
                token,
            );
            let _ = thundoku_core::drive::sync::sync(thundoku_core::drive::sync::SyncRequest {
                pool: &db,
                drive: &mut drive,
                packs_dir: &packs_dir,
                downloads_dir: &downloads_dir,
                identity_sub: google_sub.as_deref(),
                owner_key: db_key.as_ref(),
                folder_id: &folder_id,
                db_path: Some(&db_path),
            })
            .map_err(|e| e.to_string())?;
            Ok(())
        });
        cx.spawn(async move |_window, cx| {
            // アップロードは best-effort（失敗しても終了する）が、無言で捨てない:
            // 「アップロードしたつもり」のまま終了すると、次回起動で毎回復元確認が
            // 出る原因を後から追えない。
            if let Err(error) = task.await {
                log::error!("exit upload failed: {error}");
            }
            handle.update(cx, |this, cx| {
                this.exit_uploading = false;
                AppState::global(cx)
                    .exit_uploading
                    .store(false, std::sync::atomic::Ordering::SeqCst);
                sync_app_menus(cx);
                cx.notify();
            });
            cx.update(|cx| cx.quit());
        })
        .detach();
    }

    /// 終了時の確認ダイアログで「キャンセル」を押したとき。
    pub fn cancel_exit_upload(&mut self, cx: &mut Context<Self>) {
        self.exit_upload_prompt = false;
        // キャンセル後は再度終了時に確認ダイアログを出すため、確認済みフラグを戻す。
        AppState::global(cx)
            .exit_checked
            .store(false, std::sync::atomic::Ordering::SeqCst);
        cx.notify();
    }

    /// 終了時の確認ダイアログで「保存せずにアプリ終了」を押したとき。
    pub fn quit_without_upload(&mut self, cx: &mut Context<Self>) {
        self.exit_upload_prompt = false;
        AppState::global(cx)
            .exit_checked
            .store(true, std::sync::atomic::Ordering::SeqCst);
        cx.notify();
        cx.quit();
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
        let github_logged_in = *AppState::global(cx).github_logged_in.lock();
        // client_id が埋め込まれていないビルドでは GitHub ログインを開始できない
        let github_login_enabled = !crate::app_state::default_github_client_id().is_empty();
        let tbf_logged_in = *AppState::global(cx).tbf_logged_in.lock();
        let booth_logged_in = *AppState::global(cx).booth_logged_in.lock();
        let fanza_logged_in = *AppState::global(cx).fanza_logged_in.lock();
        let dlsite_logged_in = *AppState::global(cx).dlsite_logged_in.lock();

        let google_email = AppState::global(cx)
            .google_profile
            .lock()
            .clone()
            .map(|p| p.email);
        let github_login = AppState::global(cx)
            .github_profile
            .lock()
            .clone()
            .map(|p| p.login)
            .filter(|login| !login.is_empty());
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
            // GitHub は client_id 未設定のビルドだと開始できないので、ログインボタンを
            // グレー表示にして押しても何も起きない見た目にする。
            let login_enabled =
                login_provider != Some(AuthProvider::Github) || github_login_enabled;
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
                        })
                        // GitHub はログイン中、@login を 2 段目に表示
                        .when(github_login.is_some() && name_for_id == "GitHub", |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("@{}", github_login.clone().unwrap())),
                            )
                        })
                        // client_id 未設定のビルドでは押せない理由を出す（非活性の理由が
                        // 見えないと「壊れている」と見える）。
                        .when(!github_login_enabled && name_for_id == "GitHub", |this| {
                            this.child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child("client_id 未設定"),
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
                                .text_color(gpui_kit::rgb(0x16a34a)),
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
                            .when(login_enabled, |this| {
                                this.on_click({
                                    let provider = login_provider;
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        if let Some(provider) = provider {
                                            cx.defer(move |cx| {
                                                cx.dispatch_action(
                                                    &crate::actions::OpenAuthProvider { provider },
                                                );
                                            });
                                            handle.update(cx, |this, cx| {
                                                this.auth_panel_open = false;
                                                cx.notify();
                                            });
                                        }
                                    }
                                })
                            })
                            .rounded_md()
                            .px_2()
                            .py_1()
                            .bg(if login_enabled {
                                theme.primary
                            } else {
                                theme.secondary
                            })
                            .text_color(if login_enabled {
                                theme.primary_foreground
                            } else {
                                theme.muted_foreground
                            })
                            .text_xs()
                            .when(login_enabled, |this| this.cursor_pointer())
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
                "GitHub",
                github_logged_in,
                Some(AuthProvider::Github),
                Some(crate::views::settings::SettingsView::logout_github),
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
            .child(site_row(
                "FANZA同人",
                fanza_logged_in,
                Some(AuthProvider::Fanza),
                Some(crate::views::settings::SettingsView::logout_fanza),
            ))
            .child(site_row(
                "DLsite",
                dlsite_logged_in,
                Some(AuthProvider::Dlsite),
                Some(crate::views::settings::SettingsView::logout_dlsite),
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
        reg!(crate::actions::QuitApp, |this, cx| this.quit_from_menu(cx));
        reg!(crate::actions::ShowBookshelf, |this, cx| {
            this.switch_to(NavTarget::Bookshelf, cx);
            this.sidebar_open = true;
            cx.notify();
        });
        reg!(crate::actions::ShowHistory, |this, cx| {
            this.switch_to(NavTarget::History, cx);
            this.sidebar_open = true;
            cx.notify();
        });
        reg!(crate::actions::ShowNotes, |this, cx| {
            this.switch_to(NavTarget::Notes, cx);
            this.sidebar_open = true;
            cx.notify();
        });
        reg_with!(
            crate::actions::OpenReaderAtPage,
            |action: &crate::actions::OpenReaderAtPage,
             this: &mut Workspace,
             cx: &mut Context<Workspace>| this.open_reader_at(
                cx,
                action.book_id.to_string(),
                action.page,
                action.content_id.to_string(),
                action
                    .side
                    .as_deref()
                    .and_then(thundoku_core::db::notes::SpreadSide::parse),
            )
        );
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
        reg_with!(
            crate::actions::OpenReader,
            |action: &crate::actions::OpenReader,
             this: &mut Workspace,
             cx: &mut Context<Workspace>| {
                this.open_reader(cx, action.book_id.to_string());
            }
        );
        reg_with!(
            crate::actions::OpenSampleReader,
            |action: &crate::actions::OpenSampleReader,
             this: &mut Workspace,
             cx: &mut Context<Workspace>| {
                this.open_sample_reader(cx, action.item_id.to_string());
            }
        );
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let handle = cx.entity();

        // メッセージは gpui-kit の Notification（右上のトースト）で出す。
        // 自前のバーは廃止した（自動で消える・種別ごとに色が付く・履歴が残る）。
        let (toast, toast_kind, toast_generation, toast_autohide, toast_progress) = {
            let state = AppState::global(cx);
            (
                state.toast_message.lock().clone(),
                *state.toast_kind.lock(),
                *state.toast_generation.lock(),
                *state.toast_autohide.lock(),
                *state.toast_progress.lock(),
            )
        };
        if self.toast_host_generation != toast_generation {
            self.toast_host_generation = toast_generation;
            if let Some(message) = toast {
                let kind = match toast_kind {
                    crate::app_state::ToastKind::Info => NotificationType::Info,
                    crate::app_state::ToastKind::Success => NotificationType::Success,
                    crate::app_state::ToastKind::Error => NotificationType::Error,
                };
                let mut notification = Notification::new()
                    .with_type(kind)
                    .message(message)
                    .autohide(toast_autohide);
                if toast_progress {
                    // 進行中は Spinner を添える。面と文字だけだと「進んでいるのか
                    // 固まっているのか」が分からないため。id を固定して、進捗で
                    // 積み直しても通知が増えない（置き換わる）ようにする。
                    notification = notification.id::<ProgressNotice>().content(|_, _, cx| {
                        gpui_kit::div()
                            .flex()
                            .flex_row()
                            .debug_selector(|| "notice-progress".into())
                            .items_center()
                            .child(
                                gpui_kit::component::spinner::Spinner::new()
                                    .small()
                                    .color(cx.theme().muted_foreground),
                            )
                            .into_any_element()
                    });
                }
                window.push_notification(notification, cx);
            }
            // 表示は Notification が持つので、こちらの状態はすぐ消す（世代は残して再表示を防ぐ）
            crate::app_state::clear_toast(cx);
        }

        // ログイン中は専用のダミー画面を表示する（SettingsView を描画せず、
        // WebView 作成時の RefCell 競合を回避）。
        let active_view: gpui_kit::AnyElement = if self.auth_loading {
            div()
                .flex()
                .flex_1()
                .items_center()
                .justify_center()
                .flex_col()
                .gap_2()
                .text_color(theme.muted_foreground)
                .child(div().text_lg().child("Google ログイン中..."))
                .child(
                    div()
                        .text_sm()
                        .child("認証ウィンドウでログインを完了してください"),
                )
                .into_any_element()
        } else {
            self.active_view(cx).into_any_element()
        };
        let sidebar = self.sidebar(cx);
        div()
              .id("app-sidebar")
              .debug_selector(|| "app-sidebar".into())
              .flex()
              .flex_col()
              .w_full()
              .h_full()
             .relative()
              .bg(theme.background)
            .on_key_down({
                move |event, window, _cx| {
                    match event.keystroke.key.as_str() {
                        "f11" => toggle_maximize(window),
                        "escape" => restore_window(window),
                        _ => {}
                    }
                }
            })
             .child({
                #[cfg(windows)]
                {
                    Self::win_title_bar(window, &theme).into_any_element()
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
                            .child(active_view),
                    ),
             )
            // リーダーはサイドバー・タイトルバーの影響を受けないよう、ウィンドウ全体に
            // absolute で重ねる（Mac はタイトルバーが無くリーダーが全ウィンドウに広がるため、
            // Windows も同じ挙動にする）。これでリーダー内部のフィット計算
            // （window.bounds().size）が実際の表示領域と一致し、見開き画像が右・下にはみ出さない。
            .child(if let Some(reader) = &self.reader {
                let view: AnyView = AnyView::from(reader.clone());
                div()
                    .id("reader-overlay")
                    .absolute()
                    // ウィンドウのタイトルバー（閉じる/最小化/最大化）は
                    // リーダー表示中も使えるように残す（top = タイトルバー高さ）。
                    .top(px(36.0))
                    .right_0()
                    .bottom_0()
                    .left_0()
                    .bg(theme.background)
                    // リーダー内のクリックを下の層（本棚）へ伝えない。
                    // これが無いと、リーダーの余白をクリックしたときに
                    // 下にある本棚のカードが反応して本が開き直る。
                    .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                        cx.stop_propagation();
                    })
                    .child(view)
                    .into_any_element()
            } else {
                div().into_any_element()
            })
            // gpui-kit の Notification レイヤー（右上に出る。既定 5 秒で自動的に消える）
            .children(gpui_kit::component::Root::render_notification_layer(
                window, cx,
            ))
            .child(if self.auth_panel_open {
                let panel = self.account_panel(cx);
                gpui_kit::deferred(
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
                gpui_kit::deferred(
                    div()
                        .id("auth-backdrop")
                        .debug_selector(|| "auth-backdrop".into())
                        .absolute()
                        .top_0()
                        .right_0()
                        .bottom_0()
                        .left_0()
                        .bg(gpui_kit::rgba(0x00000066))
                        .on_mouse_down(gpui_kit::MouseButton::Left, {
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
                gpui_kit::deferred(
                    Dialog::new(cx)
                        // 面は「浮いた面」に揃える（背景と同色だとダークで同化する）
                        .bg(cx.theme().colors.popover)
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
                                    dialog_button("restore-cancel", "キャンセル")
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
                gpui_kit::deferred(
                    Dialog::new(cx)
                        // 面は「浮いた面」に揃える（背景と同色だとダークで同化する）
                        .bg(cx.theme().colors.popover)
                        .title(div().child("Google Drive と同期しますか？"))
                        .content(move |content, _window, _cx| {
                            content.child(div().text_sm().child(
                                "Google ログインできました。本棚を Google Drive に保存して、複数の端末で同期できます。今すぐ同期しますか？"
                            ))
                        })
                        .footer(
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .child(
                                    dialog_button("drive-cancel", "キャンセル")
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
                                        .label("同期する")
                                        .on_click({
                                            let handle = handle.clone();
                                            move |_, _window, cx| {
                                                handle.update(cx, |this, cx| {
                                                    this.show_drive_prompt = false;
                                                    let db = &AppState::global(cx).db_pool;
                                                    let _ = db::settings::set(db, "drive.sync.enabled", "true");
                                                    // 有効化だけでなく、実際に今すぐ同期を実行する。
                                                    this.sync_drive(cx);
                                                    crate::app_state::set_toast(cx, "Google Drive と同期しました");
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
            .child(if self.exit_upload_prompt && !self.show_restore_prompt {
                let handle = cx.entity();
                let uploading = self.exit_uploading;
                let content = dialog_surface(cx)
                    .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("バックアップのアップロード"))
                    .child(div().text_sm().child(
                        if uploading {
                            "バックアップ対象に変更があります。アップロードしています…"
                        } else {
                            "バックアップ対象に変更があります。Google Drive にアップロードして終了しますか？"
                        },
                    ))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_center()
                            .gap_2()
                            .child(
                                dialog_button("exit-upload-cancel", "キャンセル")
                                    .disabled(uploading)
                                    .on_click({
                                        let handle = handle.clone();
                                        move |_, _window, cx| {
                                            handle.update(cx, |this, cx| {
                                                this.cancel_exit_upload(cx);
                                            });
                                        }
                                    }),
                            )
                            .child(
                                dialog_button("exit-upload-quit", "保存せずにアプリ終了")
                                    .disabled(uploading)
                                    .on_click({
                                        let handle = handle.clone();
                                        move |_, _window, cx| {
                                            handle.update(cx, |this, cx| {
                                                this.quit_without_upload(cx);
                                            });
                                        }
                                    }),
                            )
                            .child(
                                Button::new("exit-upload-confirm")
                                    .cursor_pointer()
                                    .primary()
                                    .label("アップロードして終了")
                                    .disabled(uploading)
                                    .on_click({
                                        let handle = handle.clone();
                                        move |_, _window, cx| {
                                            handle.update(cx, |this, cx| {
                                                this.confirm_exit_upload(cx);
                                            });
                                        }
                                    }),
                            )
                    );
                fade_dialog(window, cx, self.exit_upload_prompt, content).into_any_element()
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
    fn win_title_bar(
        window: &mut Window,
        theme: &gpui_kit::component::Theme,
    ) -> gpui_kit::AnyElement {
        // Windows 標準のタイトルバーボタンのホバー色（テーマに応じて明暗を出す）。
        // 背景（theme.secondary）と区別できるよう、ダークは明るめ・ライトは濃いめのグレー。
        let hover_bg = crate::views::hover_bg(theme);
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
                            // ネイティブに最大化⇔復元のトグルを任せる（HTMAXBUTTON）
                            .window_control_area(WindowControlArea::Max)
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
                            .hover(move |style| style.bg(gpui_kit::rgb(0xe11d48)))
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
        // レポート（GitHub にログインしているときだけ設定の上に出す）。
        // ログインしていないと Issue を作れないため導線も出さない。
        let github_logged_in = *AppState::global(cx).github_logged_in.lock();
        let tbf_logged_in = *AppState::global(cx).tbf_logged_in.lock();
        let report_item = github_logged_in.then(|| {
            self.bottom_item(
                "sidebar-nav-report",
                Icon::new(AppIcon::Megaphone)
                    .size(px(24.0))
                    .text_color(theme.muted_foreground)
                    .into_any_element(),
                "レポート",
                open,
                |this, cx| {
                    this.switch_to(NavTarget::Report, cx);
                },
                handle.clone(),
                cx,
            )
        });
        // バッジ色分け: 100 件以上=赤 / 10〜99 件=黄 / 1〜9 件=緑
        let badge_color = if unread_count >= 100 {
            gpui_kit::rgb(0xef4444)
        } else if unread_count >= 10 {
            gpui_kit::rgb(0xeab308)
        } else {
            gpui_kit::rgb(0x10b981)
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
                .with_easing(gpui_kit::quadratic)
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
                            NavTarget::History,
                            Icon::new(AppIcon::History)
                                .size(px(24.0))
                                .into_any_element(),
                            "閲覧履歴",
                            open,
                            active,
                            handle.clone(),
                            cx,
                        ),
                    )
                    .child(
                        self.nav_row(
                            NavTarget::Notes,
                            Icon::new(AppIcon::StickyNote)
                                .size(px(24.0))
                                .into_any_element(),
                            "付箋",
                            open,
                            active,
                            handle.clone(),
                            cx,
                        ),
                    )
                    // チェックリストは技術書典専用（同期も試し読みも技術書典の
                    // セッションが要る）。未ログインでは導線を出さない。
                    .when(tbf_logged_in, |this| {
                        this.child(
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
                        )
                    }),
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
                    .when_some(report_item, |this, item| this.child(item))
                    .child(
                        self.bottom_item(
                            "sidebar-nav-settings",
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
                            "sidebar-nav-theme",
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
                            "sidebar-nav-account",
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
        badge_color: gpui_kit::Rgba,
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
                        this.switch_to(NavTarget::About, cx);
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
        icon: gpui_kit::AnyElement,
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
            NavTarget::History => "sidebar-nav-history",
            NavTarget::Notes => "sidebar-nav-notes",
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

    /// 下部のアイテム（レポート / 設定 / テーマ / アカウント 共通）。
    ///
    /// `id` は静的な文字列で渡す（`debug_selector` が `&'static str` を要求するため。
    /// テストが行の位置を検証する）。
    #[allow(clippy::too_many_arguments)]
    fn bottom_item(
        &self,
        id: &'static str,
        icon: gpui_kit::AnyElement,
        label: &str,
        open: bool,
        on_click: impl Fn(&mut Workspace, &mut Context<Workspace>) + 'static,
        handle: Entity<Workspace>,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let theme = cx.theme().clone();
        let label = label.to_string();
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

    /// 本棚サブメニューの高さ上限（px）。
    /// 固定値にするとログイン中のサイトが増えたときに下の行が切れるため、行数から求める。
    /// 1 行 = `text_xs`(12px) + `py_1p5`(上下 6px) に余裕を足した値。
    fn bookshelf_submenu_height(rows: usize) -> f32 {
        const ROW_H: f32 = 34.0;
        const GAP: f32 = 2.0; // gap_0p5
        const TOP: f32 = 4.0; // mt_1
        TOP + rows as f32 * ROW_H + rows.saturating_sub(1) as f32 * GAP
    }

    fn bookshelf_submenu(&mut self, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.bookshelf_submenu_open;
        let theme = cx.theme().clone();
        let handle = cx.entity();
        let site_filter = self.bookshelf.update(cx, |b, _| b.site_filter());
        let tbf_logged_in = *AppState::global(cx).tbf_logged_in.lock();
        let booth_logged_in = *AppState::global(cx).booth_logged_in.lock();
        let fanza_logged_in = *AppState::global(cx).fanza_logged_in.lock();
        let dlsite_logged_in = *AppState::global(cx).dlsite_logged_in.lock();
        // 「すべての本」+ ログイン中のサイトの行数ぶんの高さを確保する
        let submenu_height = Self::bookshelf_submenu_height(
            1 + usize::from(tbf_logged_in)
                + usize::from(booth_logged_in)
                + usize::from(fanza_logged_in)
                + usize::from(dlsite_logged_in),
        );

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
                    .when(fanza_logged_in, |this| {
                        this.child(
                            div()
                                .id("bookshelf-site-fanza")
                                .flex()
                                .items_center()
                                .gap_2()
                                .rounded_lg()
                                .px_2()
                                .py_1p5()
                                .text_xs()
                                .when(site_filter.as_deref() == Some("fanza"), |this| {
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
                                                b.set_site_filter(cx, Some("fanza"));
                                            });
                                            this.switch_to(NavTarget::Bookshelf, cx);
                                            this.refresh_unread_count(cx);
                                        });
                                    }
                                })
                                .child("FANZA同人"),
                        )
                    })
                    .when(dlsite_logged_in, |this| {
                        this.child(
                            div()
                                .id("bookshelf-site-dlsite")
                                .debug_selector(|| "bookshelf-site-dlsite".into())
                                .flex()
                                .items_center()
                                .gap_2()
                                .rounded_lg()
                                .px_2()
                                .py_1p5()
                                .text_xs()
                                .when(site_filter.as_deref() == Some("dlsite"), |this| {
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
                                                b.set_site_filter(cx, Some("dlsite"));
                                            });
                                            this.switch_to(NavTarget::Bookshelf, cx);
                                            this.refresh_unread_count(cx);
                                        });
                                    }
                                })
                                .child("DLsite"),
                        )
                    })
                    .with_animation(
                        SharedString::from(format!(
                            "bookshelf-submenu-anim-{}",
                            if open { "open" } else { "closed" }
                        )),
                        Animation::new(Duration::from_millis(200))
                            .with_easing(gpui_kit::ease_in_out),
                        move |this, t| {
                            let t = t.clamp(0.0, 1.0);
                            let height = if open {
                                submenu_height * t
                            } else {
                                submenu_height * (1.0 - t)
                            };
                            let opacity = if open { t } else { 1.0 - t };
                            this.max_h(px(height)).opacity(opacity.clamp(0.0, 1.0))
                        },
                    ),
            )
    }
}

/// 起動時の復元確認・終了時のアップロードで使う所有者集合（現在の Google アカウントの本）。
///
/// Drive 側の `thundoku-backup.json` は「アップロードした時点のアカウントに帰属する本」
/// だけを含むため、比較・アップロードも同じ範囲で行う必要がある。プロフィール
/// （`sub`）か暗号鍵が無ければ範囲を決められないので `None` を返す（＝所有者不明）。
fn backup_owner_ids(state: &AppState) -> Option<std::collections::HashSet<String>> {
    let sub = state
        .google_profile
        .lock()
        .as_ref()
        .map(|p| p.sub.clone())?;
    let key = state.secrets.db_key().ok()?;
    db::books::owned_book_ids(&state.db_pool, &key, Some(&sub)).ok()
}

/// F11 用: 最大化⇔復元をトグルする。
fn toggle_maximize(window: &mut Window) {
    if window.is_maximized() {
        window_restore(window);
    } else {
        window_maximize(window);
    }
}

/// ESC 用: 最大化中なら復元する。通常時は何もしない。
fn restore_window(window: &mut Window) {
    if window.is_maximized() {
        window_restore(window);
    }
}

fn window_maximize(window: &mut Window) {
    #[cfg(windows)]
    {
        if let Ok(handle) = window.window_handle()
            && let raw_window_handle::RawWindowHandle::Win32(win) = handle.as_raw()
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::{SW_MAXIMIZE, ShowWindow};
            let hwnd = win.hwnd.get() as _;
            unsafe {
                ShowWindow(hwnd, SW_MAXIMIZE);
            }
        }
    }
    #[cfg(not(windows))]
    {
        window.zoom_window();
    }
}

fn window_restore(window: &mut Window) {
    #[cfg(windows)]
    {
        if let Ok(handle) = window.window_handle()
            && let raw_window_handle::RawWindowHandle::Win32(win) = handle.as_raw()
        {
            use windows_sys::Win32::UI::WindowsAndMessaging::{SW_RESTORE, ShowWindow};
            let hwnd = win.hwnd.get() as _;
            unsafe {
                ShowWindow(hwnd, SW_RESTORE);
            }
        }
    }
    #[cfg(not(windows))]
    {
        window.zoom_window();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app_state::AppState;
    use crate::app_state::ToastKind;
    use gpui_kit::TestAppContext;

    fn setup(cx: &mut TestAppContext) -> gpui_kit::Entity<Workspace> {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        cx.new(Workspace::new)
    }

    /// メニューから名前で項目を引く（`app_menus` の可否を検証する）。
    fn find_menu_item(menus: Vec<Menu>, name: &str) -> MenuItem {
        menus
            .into_iter()
            .flat_map(|menu| menu.items)
            .find(|item| matches!(item, MenuItem::Action { name: item_name, .. } if item_name.as_ref() == name))
            .unwrap_or_else(|| panic!("メニューに「{name}」が無い"))
    }

    /// 数フレーム描画する（通知の反映まで見る）。
    fn draw_frames(visual: &mut gpui_kit::VisualTestContext) {
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
    }

    /// いま表示されている Notification の件数。
    fn notification_count(visual: &mut gpui_kit::VisualTestContext) -> usize {
        visual.update(|window, cx| {
            window
                .root::<gpui_kit::component::Root>()
                .expect("root")
                .expect("root view")
                .read(cx)
                .notification
                .read(cx)
                .notifications()
                .len()
        })
    }

    /// メニューの「終了」（`QuitApp`）で終了確認（アップロードの確認）が出ること。
    ///
    /// アクションが未登録だと何も起きず、macOS のメニューから終了できない。
    #[gpui_kit::test]
    async fn quit_app_action_opens_exit_prompt(cx: &mut TestAppContext) {
        let ws = setup(cx);
        assert!(
            !ws.read_with(cx, |w, _| w.exit_upload_prompt),
            "初期状態では確認ダイアログは出ていない"
        );
        cx.update(|cx| {
            cx.dispatch_action(&crate::actions::QuitApp);
        });
        assert!(
            ws.read_with(cx, |w, _| w.exit_upload_prompt),
            "終了でアップロード確認が表示されること"
        );
    }

    #[gpui_kit::test]
    async fn sidebar_toggle_flips_open_state(cx: &mut TestAppContext) {
        let ws = setup(cx);
        let initial = ws.read_with(cx, |w, _| w.sidebar_open);
        cx.update(|cx| {
            ws.update(cx, |w, cx| w.toggle_sidebar(cx));
        });
        let after = ws.read_with(cx, |w, _| w.sidebar_open);
        assert_ne!(initial, after, "sidebar toggle must flip the open state");
    }

    /// サイドバーの「閲覧履歴」行が出て、クリックで履歴ビューへ切り替わること。
    #[gpui_kit::test]
    async fn sidebar_history_row_switches_to_the_history_view(cx: &mut TestAppContext) {
        let ws = setup(cx);
        cx.update(|cx| {
            ws.update(cx, |w, cx| {
                w.sidebar_open = true;
                cx.notify();
            });
        });
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
            },
            |window, cx| gpui_kit::component::Root::new(ws.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        let draw = |visual: &mut gpui_kit::VisualTestContext| {
            for _ in 0..4 {
                visual.update(|window, cx| {
                    let arena_clear = window.draw(cx);
                    arena_clear.clear(cx);
                });
            }
        };
        draw(visual);
        let row = visual
            .debug_bounds("sidebar-nav-history")
            .expect("サイドバーに閲覧履歴の行が出ていない");
        visual.simulate_click(row.center(), gpui_kit::Modifiers::default());
        assert_eq!(
            ws.read_with(cx, |w, _| w.active),
            NavTarget::History,
            "閲覧履歴の行クリックで履歴ビューへ切り替わっていない"
        );
        draw(visual);
        assert!(
            visual.debug_bounds("history-root").is_some(),
            "履歴ビューが描画されていない"
        );
    }

    /// サイドバーに「付箋」の行が出て、閲覧履歴の下・チェックリストの上に並び、
    /// クリックで付箋画面に切り替わる。
    /// 通知: メッセージは gpui-kit の Notification として出す（自前のトーストバーは廃止）。
    /// 同じメッセージが世代ごとに 1 回だけ積まれることも確認する。
    #[gpui_kit::test]
    async fn toast_is_delivered_as_a_notification(cx: &mut TestAppContext) {
        let ws = setup(cx);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
            },
            |window, cx| gpui_kit::component::Root::new(ws.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        draw_frames(visual);
        assert_eq!(notification_count(visual), 0, "最初から通知が出ている");

        cx.update(|cx| {
            crate::app_state::set_toast_kind(cx, crate::app_state::ToastKind::Success, "テスト通知")
        });
        draw_frames(visual);
        assert_eq!(
            notification_count(visual),
            1,
            "メッセージが通知になっていない"
        );

        // 追加で描画しても同じメッセージが重複して積まれない（世代で 1 回だけ）
        draw_frames(visual);
        assert_eq!(
            notification_count(visual),
            1,
            "同じメッセージが重複して通知されている"
        );

        // エラーは種別つきで別の通知になる
        cx.update(|cx| {
            crate::app_state::set_toast_kind(cx, crate::app_state::ToastKind::Error, "失敗しました")
        });
        draw_frames(visual);
        assert_eq!(notification_count(visual), 2, "2 件目の通知が出ていない");
    }

    /// 進行中の通知には Spinner が出る（終了時のアップロード・取り込み中）。
    ///
    /// 通知の面だけでは「進んでいるのか固まっているのか」が分からないので、
    /// 実行中はスピナーを添える。普通の通知には出さない（常時スピナーだと
    /// 何が進行中なのか分からなくなる）。
    /// 終了時のアップロード中は「進行中・自動で消えない」通知にする。
    ///
    /// 完了するとアプリが終了するので、途中で消えると「止まった」ように見える。
    /// また、実行中であることが分かるようスピナーを出す。
    #[gpui_kit::test]
    async fn exit_upload_notice_is_progress_and_sticky(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        cx.update(|cx| crate::app_state::set_sticky_progress_notice(cx, EXIT_UPLOAD_NOTICE));

        let (progress, autohide, message) = cx.update(|cx| {
            let state = AppState::global(cx);
            (
                *state.toast_progress.lock(),
                *state.toast_autohide.lock(),
                state.toast_message.lock().clone(),
            )
        });
        assert!(progress, "進行中になっていない（スピナーが出ない）");
        assert!(!autohide, "自動で消える設定になっている");
        assert_eq!(message.as_deref(), Some(EXIT_UPLOAD_NOTICE));
    }

    #[gpui_kit::test]
    async fn progress_notice_shows_a_spinner(cx: &mut TestAppContext) {
        let ws = setup(cx);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
            },
            |window, cx| gpui_kit::component::Root::new(ws.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        draw_frames(visual);

        cx.update(|cx| {
            crate::app_state::set_toast_kind(
                cx,
                crate::app_state::ToastKind::Info,
                "ふつうのお知らせ",
            )
        });
        draw_frames(visual);
        assert_eq!(notification_count(visual), 1, "通知が出ていない");
        assert!(
            visual.debug_bounds("notice-progress").is_none(),
            "進行中でない通知に Spinner が出ている"
        );

        cx.update(|cx| crate::app_state::set_progress_notice(cx, "アップロード中です…"));
        draw_frames(visual);
        assert!(
            visual.debug_bounds("notice-progress").is_some(),
            "進行中の通知に Spinner が出ていない"
        );
    }

    /// 進行中の通知は積み上がらず、同じ 1 つが置き換わる。
    ///
    /// ダウンロードや取り込みは進捗のたびに積み直すので、置き換わらないと
    /// 通知が増え続けて（上限 10 件で）他の通知を押し出してしまう。
    #[gpui_kit::test]
    async fn progress_notice_is_replaced_instead_of_stacked(cx: &mut TestAppContext) {
        let ws = setup(cx);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
            },
            |window, cx| gpui_kit::component::Root::new(ws.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        draw_frames(visual);

        cx.update(|cx| crate::app_state::set_progress_notice(cx, "取り込み中です…（10%）"));
        draw_frames(visual);
        assert_eq!(notification_count(visual), 1, "進行中の通知が出ていない");

        cx.update(|cx| crate::app_state::set_progress_notice(cx, "取り込み中です…（40%）"));
        draw_frames(visual);
        assert_eq!(
            notification_count(visual),
            1,
            "進捗の更新で通知が積み上がっている（置き換わっていない）"
        );
    }

    #[gpui_kit::test]
    async fn sidebar_notes_row_opens_the_notes_screen(cx: &mut TestAppContext) {
        let ws = setup(cx);
        cx.update(|cx| {
            // チェックリスト行は技術書典ログイン時のみ出る（並び順の検証に必要）
            *AppState::global(cx).tbf_logged_in.lock() = true;
            ws.update(cx, |w, cx| {
                w.sidebar_open = true;
                cx.notify();
            });
        });
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
            },
            |window, cx| gpui_kit::component::Root::new(ws.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        let draw = |visual: &mut gpui_kit::VisualTestContext| {
            for _ in 0..4 {
                visual.update(|window, cx| {
                    let arena_clear = window.draw(cx);
                    arena_clear.clear(cx);
                });
            }
        };
        draw(visual);
        let history = visual
            .debug_bounds("sidebar-nav-history")
            .expect("閲覧履歴の行");
        let notes = visual
            .debug_bounds("sidebar-nav-notes")
            .expect("付箋の行が出ていない");
        let checklist = visual
            .debug_bounds("sidebar-nav-checklist")
            .expect("チェックリストの行");
        assert!(
            history.origin.y < notes.origin.y && notes.origin.y < checklist.origin.y,
            "並び順が 閲覧履歴 → 付箋 → チェックリスト になっていない"
        );
        visual.simulate_click(notes.center(), gpui_kit::Modifiers::default());
        assert_eq!(ws.read_with(cx, |w, _| w.active), NavTarget::Notes);
        draw(visual);
        assert!(
            visual.debug_bounds("notes-root").is_some(),
            "付箋画面が描画されていない"
        );
    }

    /// サイドバーのナビ行は「本棚 → 閲覧履歴 → 付箋 → チェックリスト」の順に並ぶ。
    #[gpui_kit::test]
    async fn sidebar_history_row_sits_after_the_bookshelf(cx: &mut TestAppContext) {
        let ws = setup(cx);
        cx.update(|cx| {
            // チェックリスト行は技術書典ログイン時のみ出る（並び順の検証に必要）
            *AppState::global(cx).tbf_logged_in.lock() = true;
            ws.update(cx, |w, cx| {
                w.sidebar_open = true;
                cx.notify();
            });
        });
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
            },
            |window, cx| gpui_kit::component::Root::new(ws.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        let bookshelf = visual
            .debug_bounds("sidebar-nav-bookshelf")
            .expect("本棚の行");
        let history = visual
            .debug_bounds("sidebar-nav-history")
            .expect("閲覧履歴の行");
        let checklist = visual
            .debug_bounds("sidebar-nav-checklist")
            .expect("チェックリストの行");
        assert!(
            bookshelf.origin.y < history.origin.y && history.origin.y < checklist.origin.y,
            "並び順が 本棚 → 閲覧履歴 → チェックリスト になっていない"
        );
    }

    /// サイドバーの「チェックリスト」行は技術書典にログインしているときだけ出る
    /// （ログインしていないと同期も試し読みもできないため導線も出さない）。
    #[gpui_kit::test]
    async fn checklist_row_needs_techbookfest_login(cx: &mut TestAppContext) {
        let ws = setup(cx);
        cx.update(|cx| {
            ws.update(cx, |w, cx| {
                w.sidebar_open = true;
                cx.notify();
            });
        });
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
            },
            |window, cx| gpui_kit::component::Root::new(ws.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        let draw = |visual: &mut gpui_kit::VisualTestContext| {
            for _ in 0..4 {
                visual.update(|window, cx| {
                    let arena_clear = window.draw(cx);
                    arena_clear.clear(cx);
                });
            }
        };

        draw(visual);
        assert!(
            visual.debug_bounds("sidebar-nav-checklist").is_none(),
            "技術書典に未ログインなのにチェックリスト行が出ている"
        );

        cx.update(|cx| {
            *AppState::global(cx).tbf_logged_in.lock() = true;
            ws.update(cx, |_, cx| cx.notify());
        });
        draw(visual);
        let notes = visual
            .debug_bounds("sidebar-nav-notes")
            .expect("付箋の行が出ていない");
        let checklist = visual
            .debug_bounds("sidebar-nav-checklist")
            .expect("技術書典にログインしてもチェックリスト行が出ていない");
        // 「付箋」の下に出る（並び順は 本棚 → 閲覧履歴 → 付箋 → チェックリスト）
        assert!(
            notes.origin.y < checklist.origin.y,
            "チェックリスト行が付箋より下に無い: notes_y={} checklist_y={}",
            notes.origin.y.as_f32(),
            checklist.origin.y.as_f32()
        );
        // 他のナビ行と同じ幅（アイコンと文字が縦に揃う）
        assert!(
            (checklist.size.width.as_f32() - notes.size.width.as_f32()).abs() < 1.0,
            "チェックリスト行の幅が他の行と揃っていない: checklist={:?} notes={:?}",
            checklist.size,
            notes.size
        );

        // ログアウトすると導線も消える（keyring を触る logout_tbf は他テストと
        // 競合するので、ここではログイン状態だけを落とす）
        cx.update(|cx| {
            *AppState::global(cx).tbf_logged_in.lock() = false;
            ws.update(cx, |_, cx| cx.notify());
        });
        draw(visual);
        assert!(
            visual.debug_bounds("sidebar-nav-checklist").is_none(),
            "ログアウトしてもチェックリスト行が残っている"
        );
    }

    /// サイドバーの「レポート」行は GitHub にログインしているときだけ出る
    /// （ログインしていないと Issue を作れないため導線も出さない）。
    #[gpui_kit::test]
    async fn report_row_needs_github_login(cx: &mut TestAppContext) {
        let ws = setup(cx);
        cx.update(|cx| {
            ws.update(cx, |w, cx| {
                w.sidebar_open = true;
                cx.notify();
            });
        });
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
            },
            |window, cx| gpui_kit::component::Root::new(ws.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        let draw = |visual: &mut gpui_kit::VisualTestContext| {
            for _ in 0..4 {
                visual.update(|window, cx| {
                    let arena_clear = window.draw(cx);
                    arena_clear.clear(cx);
                });
            }
        };

        draw(visual);
        assert!(
            visual.debug_bounds("sidebar-nav-report").is_none(),
            "未ログインなのにレポート行が出ている"
        );

        cx.update(|cx| {
            *AppState::global(cx).github_logged_in.lock() = true;
            ws.update(cx, |_, cx| cx.notify());
        });
        draw(visual);
        let report = visual
            .debug_bounds("sidebar-nav-report")
            .expect("GitHub にログインしてもレポート行が出ていない");
        let settings = visual
            .debug_bounds("sidebar-nav-settings")
            .expect("設定の行が出ていない");
        // 「設定」の上に出る
        assert!(
            report.origin.y < settings.origin.y,
            "レポート行が設定より上に無い: report_y={} settings_y={}",
            report.origin.y.as_f32(),
            settings.origin.y.as_f32()
        );
        // 他の行と同じ位置・同じ幅（アイコンと文字が縦に揃う）。行を余分な div で
        // 包むと `w_full()` の基準がずれて幅が変わり、アイコン位置がずれる。
        assert!(
            (report.origin.x.as_f32() - settings.origin.x.as_f32()).abs() < 1.0
                && (report.size.width.as_f32() - settings.size.width.as_f32()).abs() < 1.0,
            "レポート行の位置・幅が他の行と揃っていない: report={:?} settings={:?}",
            report.size,
            settings.size
        );

        // ログアウトすると導線も消える
        // （keyring を触る clear_github_token は他のテストと競合するので、
        //   ここではログイン状態だけを落とす）
        cx.update(|cx| {
            *AppState::global(cx).github_logged_in.lock() = false;
            ws.update(cx, |_, cx| cx.notify());
        });
        draw(visual);
        assert!(
            visual.debug_bounds("sidebar-nav-report").is_none(),
            "ログアウトしてもレポート行が残っている"
        );
    }

    #[gpui_kit::test]
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

    #[gpui_kit::test]
    async fn reader_back_click_does_not_reach_the_layer_below(cx: &mut TestAppContext) {
        // 回帰: リーダー下部の「戻る」クリックが下の層（本棚）へ抜けて
        // 同じ位置の本が開き直らないこと（オーバーレイがクリックを遮る）。
        let ws = setup(cx);
        cx.update(|cx| {
            ws.update(cx, |w, cx| w.open_reader(cx, "missing-book".into()));
        });
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(900.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(ws.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        let back = visual
            .debug_bounds("viewer-back")
            .expect("戻るボタンが描画されている");
        visual.simulate_click(back.center(), gpui_kit::Modifiers::default());
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        assert!(
            ws.read_with(cx, |w, _| w.reader.is_none()),
            "戻るで閉じたあと、クリックが下に抜けて本が開き直らないこと"
        );
    }

    #[gpui_kit::test]
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
    #[gpui_kit::test]
    async fn closing_reader_refreshes_bookshelf_read_state(cx: &mut TestAppContext) {
        use thundoku_core::db::documents;
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        // b1: 3 ページの本（ダウンロード直後は reading_progress が無く
        // 本棚は imported_documents の total_pages から "1/3" 未読と表示する）
        cx.update(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            books::insert(
                db,
                &books::Book {
                    id: "b1".into(),
                    title: "本1".into(),
                    author: String::new(),
                    circle_name: "サークルA".into(),
                    purchase_date: None,
                    file_name: "b1.pdf".into(),
                    file_size: 10,
                    opfs_path: "b1.opfspack".into(),
                    cover_thumbnail: None,
                    tbf_product_id: None,
                    site_id: None,
                    tags_fetched: 1,
                    pack_id: Some("b1".into()),
                    is_favorite: 0,
                    is_hidden: 0,
                    created_at: "2026-08-21 00:00:00".into(),
                    updated_at: "2026-08-21 00:00:00".into(),
                    media_category: None,
                    ai_type: None,
                    is_drm: 0,
                    release_date: None,
                    description: None,
                    theme: None,
                    maker_id: None,
                    page_count: None,
                    age_rating: None,
                    series_name: None,
                },
            )
            .unwrap();
            documents::insert_document(
                db,
                &documents::ImportedDocument {
                    id: "d1".into(),
                    book_id: "b1".into(),
                    source_type: "pdf".into(),
                    file_hash: "h".into(),
                    total_pages: 3,
                    metadata: None,
                    status: "done".into(),
                    created_at: "2026-08-21 00:00:00".into(),
                    updated_at: "2026-08-21 00:00:00".into(),
                },
            )
            .unwrap();
            for page in 0..3 {
                documents::insert_image(
                    db,
                    &documents::DocumentImage {
                        id: format!("img{page}"),
                        document_id: "d1".into(),
                        content_id: None,
                        format_id: None,
                        page_number: page,
                        image_type: "page".into(),
                        opfs_path: format!("b1/p{page}"),
                        width: 1,
                        height: 1,
                        mime_type: "image/webp".into(),
                        file_size: 1,
                        extracted_text: None,
                        pack_entry_path: None,
                        created_at: "2026-08-21 00:00:00".into(),
                    },
                )
                .unwrap();
            }
        });
        // 本棚は構築時点の進捗（未読 1/3）をキャッシュしている
        let ws = cx.new(Workspace::new);
        let before = ws.read_with(cx, |w, _| {
            w.bookshelf.read_with(cx, |b, _| {
                b.progress_for_book("b1").unwrap_or((0, None, false))
            })
        });
        assert_eq!(before.0, 1, "before reading the cache shows page 1");
        assert!(!before.2, "before reading the book is unread");

        // リーダーを開き、save_progress が最終ページで finished_at を立てた状態を
        // DB 書き込みで再現する（本棚キャッシュはまだ古い）
        cx.update(|cx| {
            ws.update(cx, |w, cx| w.open_reader(cx, "b1".into()));
        });
        cx.update(|cx| {
            let state = AppState::global(cx);
            progress::upsert(
                &state.db_pool,
                &progress::ReadingProgress {
                    book_id: "b1".into(),
                    content_id: String::new(),
                    current_page: 3,
                    total_pages: Some(3),
                    finished_at: Some("2026-09-01 00:00:00".into()),
                    last_read_at: "2026-09-01 00:00:00".into(),
                },
            )
            .unwrap();
        });

        // リーダーを閉じたとき、本棚の進捗キャッシュが DB と一致する
        cx.update(|cx| {
            ws.update(cx, |w, cx| w.close_reader(cx));
        });
        let after = ws.read_with(cx, |w, _| {
            w.bookshelf.read_with(cx, |b, _| {
                b.progress_for_book("b1").unwrap_or((0, None, false))
            })
        });
        assert_eq!(after.0, 3, "closing the reader must show the last page");
        assert!(after.2, "closing the reader must mark the book as read");
    }
    #[gpui_kit::test]
    async fn open_reader_action_dispatches_open_reader(cx: &mut TestAppContext) {
        let ws = setup(cx);
        // 本棚から dispatch される OpenReader アクションが register_actions の
        // バインドを通って open_reader を呼ぶ（ビューアが開く）ことを検証する。
        // 存在しない book_id でもリーダーは開ける（エラーはビューアー内で表示）。
        cx.update(|cx| {
            cx.dispatch_action(&crate::actions::OpenReader {
                book_id: "action-dispatched-book".into(),
            });
        });
        let opened = ws.read_with(cx, |w, _| w.reader.is_some());
        assert!(opened, "OpenReader action should open the reader");
    }

    #[gpui_kit::test]
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

    #[gpui_kit::test]
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

    #[gpui_kit::test]
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

    #[gpui_kit::test]
    async fn unread_count_counts_unread_books(cx: &mut TestAppContext) {
        let ws = setup(cx);
        let count = ws.read_with(cx, |w, _| w.unread_count);
        assert_eq!(count, 0, "no books in an empty DB");
    }

    #[gpui_kit::test]
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

    #[gpui_kit::test]
    async fn sidebar_defaults_to_closed(cx: &mut TestAppContext) {
        let ws = setup(cx);
        let open = ws.read_with(cx, |w, _| w.sidebar_open);
        assert!(!open, "sidebar はデフォルトで閉じた状態");
    }

    /// 本棚サブメニューは、ログイン中のサイトが増えても全行が収まる高さになること。
    /// 高さが固定 150px だったときは 5 行目（4 サイト目 = DLsite）が切れていた。
    #[gpui_kit::test]
    async fn bookshelf_submenu_fits_all_rows(cx: &mut TestAppContext) {
        let ws = setup(cx);
        cx.update(|cx| {
            let state = AppState::global(cx);
            *state.tbf_logged_in.lock() = true;
            *state.booth_logged_in.lock() = true;
            *state.fanza_logged_in.lock() = true;
            *state.dlsite_logged_in.lock() = true;
            ws.update(cx, |w, cx| {
                w.sidebar_open = true;
                w.bookshelf_submenu_open = true;
                cx.notify();
            });
        });
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
            },
            |window, cx| gpui_kit::component::Root::new(ws.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        // 開閉アニメーション（200ms）が完了するまで描画する。
        // 「高さが変わらなくなったら完了」とみなす（回数固定だと進み具合がぶれて不安定になる）。
        let mut prev_height = gpui_kit::px(-1.0);
        for _ in 0..20 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(50));
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
            let height = visual
                .debug_bounds("bookshelf-submenu")
                .map(|b| b.size.height);
            if height == Some(prev_height) {
                break;
            }
            if let Some(height) = height {
                prev_height = height;
            }
        }
        let wrap = visual
            .debug_bounds("bookshelf-submenu")
            .expect("サブメニューが描画されている");
        let last = visual
            .debug_bounds("bookshelf-site-dlsite")
            .expect("最終行 (DLsite) が描画されている");
        assert!(
            last.origin.y + last.size.height
                <= wrap.origin.y + wrap.size.height + gpui_kit::px(1.0),
            "サブメニューの高さが足りず最終行が切れている: wrap={wrap:?} last={last:?}"
        );
    }

    #[gpui_kit::test]
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

    #[gpui_kit::test]
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

    #[gpui_kit::test]
    async fn auth_panel_shows_status_and_opens_selected_provider(cx: &mut TestAppContext) {
        let ws = setup(cx);
        cx.update(|cx| {
            cx.dispatch_action(&crate::actions::OpenAuth);
        });
        assert!(ws.read_with(cx, |w, _| w.show_auth));
    }

    fn google_profile(sub: &str) -> thundoku_core::google::GoogleProfile {
        thundoku_core::google::GoogleProfile {
            sub: sub.to_string(),
            email: format!("{sub}@example.com"),
            name: "ユーザー".to_string(),
            picture: None,
        }
    }

    fn test_book(id: &str) -> books::Book {
        books::Book {
            id: id.to_string(),
            title: "t".to_string(),
            author: String::new(),
            circle_name: String::new(),
            purchase_date: None,
            file_name: "t.pdf".to_string(),
            file_size: 1,
            opfs_path: format!("{id}.opfspack"),
            cover_thumbnail: None,
            tbf_product_id: None,
            site_id: None,
            tags_fetched: 1,
            pack_id: None,
            is_favorite: 0,
            is_hidden: 0,
            created_at: "2026-09-16 00:00:00".to_string(),
            updated_at: "2026-09-16 00:00:00".to_string(),
            media_category: None,
            ai_type: None,
            is_drm: 0,
            release_date: None,
            description: None,
            theme: None,
            maker_id: None,
            page_count: None,
            age_rating: None,
            series_name: None,
        }
    }

    /// Google の sub が未取得のときは、復元の差分比較に使える集合が無いこと。
    ///
    /// 差分比較は「Drive のバックアップ（所有者で絞られている）」と「ローカルの同じ範囲」を
    /// 突き合わせる必要がある。sub を持たないままローカル全件で比較すると範囲が食い違い、
    /// 内容が同じでも毎回「差分あり」になって起動のたびに復元確認が出る。
    #[gpui_kit::test]
    async fn backup_owner_ids_are_unavailable_without_a_google_profile(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        cx.update(|cx| {
            let state = AppState::global(cx);
            assert!(
                state.google_profile.lock().is_none(),
                "前提: プロフィール未取得"
            );
            assert!(
                backup_owner_ids(state).is_none(),
                "所有者が分からないのに比較対象の集合を作っている"
            );
        });
    }

    /// ログイン中の sub に帰属する本だけを比較対象にすること
    /// （他アカウントの本・未所属の本は Drive のバックアップに含まれない）。
    #[gpui_kit::test]
    async fn backup_owner_ids_only_include_the_current_account(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        cx.update(|cx| {
            let state = AppState::global(cx);
            let key = state.secrets.db_key().expect("暗号鍵");
            for (id, owner) in [
                ("b-mine", Some("sub-1")),
                ("b-other", Some("sub-2")),
                ("b-local", None),
            ] {
                books::insert(&state.db_pool, &test_book(id)).unwrap();
                books::set_owner_sub(
                    &state.db_pool,
                    id,
                    owner.map(|sub| thundoku_core::owner::encrypt(&key, sub)),
                )
                .unwrap();
            }
            *state.google_profile.lock() = Some(google_profile("sub-1"));

            assert_eq!(
                backup_owner_ids(state),
                Some(std::collections::HashSet::from(["b-mine".to_string()])),
                "現在のアカウントに帰属する本だけを対象にする"
            );
        });
    }

    /// アップロード中は、閉じるボタンでウィンドウを閉じられないこと。
    ///
    /// アップロードはアプリ内のバックグラウンドタスクなので、閉じてしまうと中断される
    /// （バックアップが中途半端なまま終了する）。
    #[gpui_kit::test]
    async fn window_close_is_blocked_while_uploading(cx: &mut TestAppContext) {
        let ws = setup(cx);

        // 1 回目の「閉じる」→ アップロード確認を出す（この時点では閉じない）
        let allowed = cx.update(|cx| ws.update(cx, |w, cx| w.handle_window_close_request(cx)));
        assert!(!allowed, "確認ダイアログを出すときは閉じない");
        assert!(
            ws.read_with(cx, |w, _| w.exit_upload_prompt),
            "アップロード確認が出ていない"
        );

        // 「アップロードして終了」を押した後（アップロード中）
        cx.update(|cx| {
            ws.update(cx, |w, _cx| {
                w.exit_upload_prompt = false;
                w.exit_uploading = true;
            });
            AppState::global(cx)
                .exit_checked
                .store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let allowed = cx.update(|cx| ws.update(cx, |w, cx| w.handle_window_close_request(cx)));
        assert!(!allowed, "アップロード中は閉じられない");
        assert!(
            ws.read_with(cx, |w, _| w.exit_uploading),
            "前提: アップロード中のままであること"
        );

        // アップロードが終われば閉じられる
        cx.update(|cx| {
            ws.update(cx, |w, _cx| w.exit_uploading = false);
        });
        let allowed = cx.update(|cx| ws.update(cx, |w, cx| w.handle_window_close_request(cx)));
        assert!(allowed, "アップロードが終わったら閉じられる");
    }

    /// アップロード中はメニューの「終了」を無効にすること（macOS のメニューバー）。
    #[test]
    fn quit_menu_item_is_disabled_while_uploading() {
        assert!(
            !find_menu_item(app_menus(true, true), "終了").is_disabled(),
            "通常は「終了」が有効であること"
        );
        assert!(
            find_menu_item(app_menus(false, true), "終了").is_disabled(),
            "アップロード中は「終了」を無効にすること"
        );
    }

    /// 技術書典に未ログインなら、メニューの「チェックリスト」を無効にすること
    /// （サイドバーの行を隠すのと同じ条件。ログインしていないと同期も試し読みもできない）。
    #[test]
    fn checklist_menu_item_needs_techbookfest_login() {
        assert!(
            !find_menu_item(app_menus(true, true), "チェックリスト").is_disabled(),
            "ログイン済みなら「チェックリスト」が有効であること"
        );
        assert!(
            find_menu_item(app_menus(true, false), "チェックリスト").is_disabled(),
            "未ログインでは「チェックリスト」を無効にすること"
        );
    }

    /// 取り込み確認モーダルが出ている間は、ビューアーを開かないこと。
    ///
    /// モーダルは本棚の層に描かれるため、ビューアーを重ねると選択肢が見えず操作できない。
    #[gpui_kit::test]
    async fn viewer_does_not_open_while_the_import_dialog_is_shown(cx: &mut TestAppContext) {
        let ws = setup(cx);

        // 取り込み確認が出ている状態にする
        cx.update(|cx| {
            let bookshelf = ws.read_with(cx, |w, _| w.bookshelf.clone());
            bookshelf.update(cx, |b, cx| {
                let (reply, _answer) = std::sync::mpsc::channel();
                b.request_pending_import(
                    crate::views::bookshelf::PendingImport {
                        database_id: "db-1".into(),
                        title: "総集編".into(),
                        choices: Vec::new(),
                        selected: 0,
                        reply,
                    },
                    cx,
                );
            });
        });

        cx.update(|cx| {
            cx.dispatch_action(&crate::actions::OpenReader {
                book_id: "b1".into(),
            });
        });
        assert!(
            ws.read_with(cx, |w, _| w.reader.is_none()),
            "取り込み確認中はビューアーを開かないこと"
        );
    }

    /// 取り込み確認モーダルが来たら、開いていたビューアーを閉じること。
    ///
    /// モーダルはビューアーの下の層に描かれるため、重なったままだと操作できない。
    #[gpui_kit::test]
    async fn import_dialog_closes_the_viewer(cx: &mut TestAppContext) {
        let ws = setup(cx);
        cx.update(|cx| {
            cx.dispatch_action(&crate::actions::OpenReader {
                book_id: "b1".into(),
            });
        });
        assert!(
            ws.read_with(cx, |w, _| w.reader.is_some()),
            "前提: ビューアーが開いている"
        );

        cx.update(|cx| {
            let bookshelf = ws.read_with(cx, |w, _| w.bookshelf.clone());
            bookshelf.update(cx, |b, cx| {
                let (reply, _answer) = std::sync::mpsc::channel();
                b.request_pending_import(
                    crate::views::bookshelf::PendingImport {
                        database_id: "db-1".into(),
                        title: "総集編".into(),
                        choices: Vec::new(),
                        selected: 0,
                        reply,
                    },
                    cx,
                );
            });
        });

        assert!(
            ws.read_with(cx, |w, _| w.reader.is_none()),
            "取り込み確認が出たらビューアーは閉じること"
        );
    }

    /// 終了時のアップロードを始めたら、通知でアップロード中であることを知らせること。
    ///
    /// 「アップロードして終了」で確認ダイアログは閉じるため、通知が無いと
    /// 画面には何も残らない（終了するのか固まったのか分からない）。
    #[gpui_kit::test]
    async fn exit_upload_shows_an_uploading_notification(cx: &mut TestAppContext) {
        let ws = setup(cx);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
            },
            |window, cx| gpui_kit::component::Root::new(ws.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        draw_frames(visual);
        assert_eq!(notification_count(visual), 0, "最初から通知が出ている");

        cx.update(|cx| {
            ws.update(cx, |w, cx| w.confirm_exit_upload(cx));
            // アップロードのタスク（テストでは即失敗して終了要求まで進む）が片付く前に、
            // 同じ更新の中で通知が積まれたことを見る
            let state = AppState::global(cx);
            assert_eq!(
                *state.toast_kind.lock(),
                ToastKind::Info,
                "アップロード中は進捗なので Info で出すこと"
            );
            let message = state
                .toast_message
                .lock()
                .clone()
                .expect("アップロード中の通知が出ていない");
            assert!(
                message.contains("アップロード中"),
                "アップロード中であることが分かる文言になっていない: {message}"
            );
        });

        // 通知として描画される
        draw_frames(visual);
        assert_eq!(
            notification_count(visual),
            1,
            "アップロード中の通知が描画されていない"
        );
    }

    /// アップロード中の通知は自動で消えないこと（他の通知は 5 秒で消える）。
    ///
    /// 長いアップロードでは途中で通知が消えて、また「終了したのか固まったのか」が
    /// 分からなくなるため、完了（＝アプリ終了）まで出しておく。
    #[gpui_kit::test]
    async fn exit_upload_notification_stays_until_the_app_quits(cx: &mut TestAppContext) {
        let ws = setup(cx);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
            },
            |window, cx| gpui_kit::component::Root::new(ws.clone(), window, cx),
        );
        // 時計を進めるために executor を先に取る（`visual` が cx を借りるため）
        let clock = cx.background_executor.clone();
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        draw_frames(visual);

        // 対照: 通常の通知は自動で消える
        cx.update(|cx| {
            crate::app_state::set_toast_kind(cx, ToastKind::Success, "テスト通知");
        });
        draw_frames(visual);
        assert_eq!(notification_count(visual), 1, "通知が出ていない");
        clock.advance_clock(std::time::Duration::from_secs(6));
        draw_frames(visual);
        assert_eq!(
            notification_count(visual),
            0,
            "通常の通知が自動で消えていない（前提が崩れている）"
        );

        // アップロード中の通知は消えない
        cx.update(|cx| {
            ws.update(cx, |w, cx| w.confirm_exit_upload(cx));
        });
        draw_frames(visual);
        assert_eq!(
            notification_count(visual),
            1,
            "アップロード中の通知が出ていない"
        );
        clock.advance_clock(std::time::Duration::from_secs(6));
        draw_frames(visual);
        assert_eq!(
            notification_count(visual),
            1,
            "アップロード中の通知が自動で消えている（完了まで出しておくこと）"
        );
    }
}
