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
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::notification::NotificationType;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Theme, ThemeMode, WindowExt as _};
use gpui_kit::{
    AnyView, App, Context, Entity, Focusable as _, FontWeight, IntoElement, Menu, MenuItem,
    ParentElement, Render, SharedString, Window, div, px,
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
    /// お気に入り（本棚と同じビューをお気に入り固定で表示する）。
    Favorites,
    History,
    Notes,
    Checklist,
    /// GitHub Issue をブラウザーで作るレポート画面。
    Report,
    Settings,
    About,
}

/// 「アップロードして終了」の実行中に出す通知。
///
/// 確認ダイアログは押した時点で閉じるため、これが進行中の唯一の手がかりになる。
const EXIT_UPLOAD_NOTICE: &str =
    "バックアップをアップロード中です…（完了するとアプリが終了します）";

/// パスフレーズ未設定の警告の本文（ログイン直後に 1 回だけ出す）。
///
/// 「いま危ない状態である」ことと、**パスフレーズがあれば端末を失っても復元できる**
/// ことを短く伝える（設定画面の「本の鍵」カードと同じ説明に揃える）。
const PASSPHRASE_NOTICE_BODY: &str = "この端末の鍵はパスフレーズで保護されていません。\
     パスフレーズを設定すると、端末を失っても Drive のバックアップから鍵を復元できます。";

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
        MenuItem::action("お気に入り", crate::actions::ShowFavorites),
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
    /// レポート画面（GitHub Issue をブラウザーで作る）。
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
    /// ログイン中(専用のダミー画面を表示中)か。
    auth_loading: bool,
    /// 現在のテーマモード（`theme.mode` のキャッシュ）。
    ///
    /// サイドバーのテーマ項目のラベルに使うため、描画のたびに DB を引かない
    /// （起動時の `restore_theme_mode` と切替時の `set_theme` だけで更新する）。
    theme_mode_name: String,
    /// ウィンドウを閉じる時に、バックアップ対象の変更を確認中か。
    exit_upload_prompt: bool,
    /// 終了時のバックアップアップロード実行中か。
    exit_uploading: bool,
    /// 解錠ダイアログで尋ねられているか。
    ///
    /// `AppState::pack_key_prompt` の要求に追随する（背景タスクが要求を積み、
    /// 監視タスクが気づいてここへ反映する）。
    pack_key_pending: bool,
    /// 解錠ダイアログの入力欄（初回表示時に生成）。
    pack_key_input: Option<Entity<InputState>>,
    /// 入力欄を空にしてフォーカスする要求（ダイアログを出した直後に 1 回だけ）。
    pack_key_input_reset: bool,
    /// 解錠ダイアログに出す案内（直前のパスフレーズが違ったとき）。
    pack_key_error: Option<String>,
    /// パスフレーズ未設定の警告を出したいか（ログイン直後の判定結果）。
    ///
    /// 「あとで」を押すまで保持する（他のモーダルが勝っている間は待たせ、
    /// それが閉じたら出す）。
    passphrase_notice: bool,
    /// このログインセッションで警告の判定を済ませたか。
    ///
    /// 1 回判定したら再判定しない（Drive を引き直さない）。
    passphrase_notice_checked: bool,
    /// このログインセッションで警告を閉じた（「あとで」「設定する」）か。
    ///
    /// 閉じた後に遅れて届いた判定結果でも出し直さない（ログアウト→再ログインで戻す）。
    passphrase_notice_dismissed: bool,
}

/// いずれかのサイト（技術書典 / BOOTH / FANZA同人 / DLsite）にログイン済みか。
///
/// Google はアカウント連携用で本の供給元（サイト）ではないため含めない。
/// 本棚のサイト行を出す判定と同じ 4 サイトを見る。
fn any_site_logged_in(cx: &App) -> bool {
    let state = AppState::global(cx);
    *state.tbf_logged_in.lock()
        || *state.booth_logged_in.lock()
        || *state.fanza_logged_in.lock()
        || *state.dlsite_logged_in.lock()
}

/// サイドバーの行（アイコン + ラベル）の見た目。
///
/// 選択状態は**色だけに頼らず**、下地とラベルの太さでも示す（アイコンだけの幅でも
/// 「いまどこに居るか」が分かるようにするため。Design Guides の「State must be
/// visible」「Do not encode meaning by color alone」）。選択の強調色は `primary`
/// （ガイドの「`primary` for the principal action or selection emphasis」）。
#[derive(Debug, Clone, Copy, PartialEq)]
struct SidebarRowStyle {
    icon_color: gpui_kit::Hsla,
    label_color: gpui_kit::Hsla,
    /// ラベルを太字にするか（選択中だけ）。
    label_emphasis: bool,
    /// 行の下地（選択中だけ敷く）。
    background: Option<gpui_kit::Hsla>,
}

/// サイドバーの行の見た目を決める（選択中 / 非選択）。
///
/// 選択中は**塗りつぶし**（`primary` の下地 + `primary_foreground` のアイコン / ラベル）
/// にする。下地だけ・文字色だけの違いは、テーマ（とくにダーク）で `primary` と
/// `muted_foreground` の差が小さいと沈んで見えなくなるため、コントラストが保証された
/// 組み合わせ（ガイドの `primary` + `primary_foreground`）で示す。
/// 非選択はアイコンもラベルも `muted_foreground` に落とす。
fn sidebar_row_style(theme: &gpui_kit::component::Theme, selected: bool) -> SidebarRowStyle {
    if selected {
        SidebarRowStyle {
            icon_color: theme.primary_foreground,
            label_color: theme.primary_foreground,
            label_emphasis: true,
            background: Some(theme.primary),
        }
    } else {
        SidebarRowStyle {
            icon_color: theme.muted_foreground,
            label_color: theme.muted_foreground,
            label_emphasis: false,
            background: None,
        }
    }
}

/// サイドバーの行のアイコン色（選択状態に応じる）。
///
/// `Icon` は親の文字色を継承せず自前の既定色で描かれるので、行に `text_color` を
/// 置くだけでは選択色にならない。アイコンを作る側でこれを使う。
fn sidebar_icon_color(
    theme: &gpui_kit::component::Theme,
    target: NavTarget,
    active: NavTarget,
) -> gpui_kit::Hsla {
    sidebar_row_style(theme, active == target).icon_color
}

/// サイドバーのロゴ（説明画面への入口）のタイル地色。
///
/// 説明画面を開いているときだけ `primary`（＝サイドバーの選択中と同じ塗りつぶし）、
/// それ以外は控えめな `muted`。ロゴは常に `primary` で塗ってあったため、
/// 説明画面を開いているかどうかがサイドバーから分からなかった。
fn sidebar_logo_background(
    theme: &gpui_kit::component::Theme,
    about_active: bool,
) -> gpui_kit::Hsla {
    if about_active {
        theme.primary
    } else {
        theme.muted
    }
}

/// ロゴのグリフ色（タイル地色に対して読める色）。
fn sidebar_logo_glyph_color(
    theme: &gpui_kit::component::Theme,
    about_active: bool,
) -> gpui_kit::Hsla {
    if about_active {
        theme.primary_foreground
    } else {
        theme.muted_foreground
    }
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

        // どのサイトにもログインしていなければ、本棚ではなく説明画面を初期表示に
        // する（本が 1 冊も入らない空の本棚より、各ストアのログイン手順が書かれた
        // 説明画面のほうが入口として機能する）。Google だけのログインは
        // サイトのログインではないため本棚を出さない。
        let active = if any_site_logged_in(cx) {
            NavTarget::Bookshelf
        } else {
            NavTarget::About
        };

        let mut this = Self {
            active,
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
            theme_mode_name: "system".to_string(),
            exit_upload_prompt: false,
            exit_uploading: false,
            pack_key_pending: false,
            pack_key_input: None,
            pack_key_input_reset: false,
            pack_key_error: None,
            passphrase_notice: false,
            passphrase_notice_checked: false,
            passphrase_notice_dismissed: false,
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

    /// Google ログイン（成功・失敗）完了フラグを監視し、認証モーダルを閉じる。
    /// 技術書典のログイン状態（`tbf_logged_in`）も同じループで見張り、変わったら
    /// アプリメニューを組み直す（メニューはアプリ全体で 1 つなので、サイドバーの
    /// ように描画のたびに読むことができない）。
    /// `AuthDialog` から `Workspace` を直接 update すると RefCell 再入問題で固まるため、
    /// AppState のフラグを追ってここで状態をリセットする。
    fn start_login_done_watcher(&mut self, cx: &mut Context<Self>) {
        let handle = cx.weak_entity();
        let flag = AppState::global(cx).google_login_done.clone();
        let logout_flag = AppState::global(cx).google_logout_done.clone();
        let auth_open = AppState::global(cx).auth_open_requested.clone();
        let auth_provider = AppState::global(cx).auth_open_provider.clone();
        let pack_key_prompt = AppState::global(cx).pack_key_prompt.clone();
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
                    // ログインできたら pack の鍵（v3 の PRK）を背景で解決する
                    // （keyring → Drive の bundle。パスフレーズがあれば解錠ダイアログ）。
                    // 鍵の解決が終わった時点で、パスフレーズ未設定なら警告を出す
                    // （ログインセッションごとに 1 回。「あとで」なら再表示しない）。
                    let _ = handle.update(cx, |this, cx| {
                        this.reset_passphrase_notice();
                        this.start_login_key_flow(cx);
                    });
                    // cx.notify() は RefCell already borrowed を起こすため、
                    // AsyncApp::refresh()（&self）で再描画を要求する。
                    cx.refresh();
                }
                // ログアウトで未ログインに戻ったら本棚を再フィルタ（未所属のみ表示）。
                if logout_flag.load(std::sync::atomic::Ordering::SeqCst) {
                    logout_flag.store(false, std::sync::atomic::Ordering::SeqCst);
                    let _ = handle.update(cx, |this, cx| {
                        // パスフレーズ未設定の警告はログインセッションの話なので一緒に片付ける
                        // （次にログインしたときに、また判定して出す）。
                        this.reset_passphrase_notice();
                        this.bookshelf.update(cx, |b, bx| b.reload(bx));
                    });
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
                // サイトのログインが完了していたら、そのサイトの同期を始める
                // （要求は `AuthDialog` が立て、ここで 1 回だけ消費する）。
                let _ = handle.update(cx, |this, cx| this.handle_login_sync_request(cx));
                // pack の鍵（v3 の PRK）の解錠要求（背景タスク → モーダル）。
                // 要求が現れたら再描画して解錠ダイアログを出す（背景側は答えを待っている）。
                let request = pack_key_prompt.pending();
                let pending = request.is_some();
                let error = request.and_then(|request| request.error);
                let _ = handle.update(cx, |this, cx| {
                    if this.pack_key_pending != pending {
                        this.pack_key_pending = pending;
                        if pending {
                            // 新しい要求なので入力欄を空にしてフォーカスし直す
                            this.pack_key_input_reset = true;
                        }
                        cx.notify();
                    }
                    this.pack_key_error = error;
                });
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
        let google_profile = AppState::global(cx).google_profile.clone();
        let secrets = AppState::global(cx).secrets.clone();
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
                    // 所有者（現在の Google アカウント）は周期ごとに読み直す
                    // （ログイン/ログアウトが次の周期から反映される）。
                    let owner = {
                        let profile = google_profile.lock().clone();
                        crate::app_state::owner_token_from(profile.as_ref(), &secrets)
                    };
                    let mut client = tbf_client.lock();
                    let mut any_changed = false;
                    let mut session_expired = false;
                    for slug in &enabled {
                        match tbf::sync::refresh_checklist(&db, &mut client, slug, owner.as_deref())
                        {
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
            // お気に入りは本棚と同じビュー（表示中フラグでお気に入り固定にする）
            NavTarget::Favorites => AnyView::from(self.bookshelf.clone()),
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
        // お気に入り画面は本棚と同じビューを共用する（表示と絞り込みだけ切り替える）
        self.bookshelf.update(cx, |bookshelf, cx| {
            bookshelf.set_favorites_only(target == NavTarget::Favorites, cx)
        });
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
        // 設定は開いた時点のデータを出す（描画のたびに読み直さない = スクロールを軽くする）
        if target == NavTarget::Settings {
            self.settings.update(cx, |s, cx| s.reload(cx));
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
        let next = match self.theme_mode_name.as_str() {
            "dark" => "system",
            "system" => "light",
            _ => "dark",
        };
        self.set_theme(next, cx);
    }

    /// テーマモードを設定して保存する。
    pub fn set_theme(&mut self, mode: &str, cx: &mut Context<Self>) {
        self.theme_mode_name = mode.to_string();
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
        let mode = db::settings::get(&AppState::global(cx).db_pool, "theme.mode")
            .ok()
            .flatten()
            .unwrap_or_else(|| "system".to_string());
        self.theme_mode_name = mode.clone();
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

    /// Google プロフィールの復元(起動時)。
    pub fn restore_google_profile(&mut self, cx: &mut Context<Self>) {
        let settings = self.settings.clone();
        settings.update(cx, |s, cx| s.refresh_google_profile(cx));
    }

    /// サイトのログイン完了で立った同期要求を消費して、そのサイトを同期する。
    ///
    /// ログイン直後の本棚は購入済みの一覧が空なので、手動で「同期」を押させない。
    /// 監視タスク（`start_login_done_watcher`）が 100ms ごとに呼ぶ。要求は 1 回で
    /// 消費するので、同じサイトを何度も同期しない。要求が無ければ何もしない。
    pub(crate) fn handle_login_sync_request(&mut self, cx: &mut Context<Self>) {
        let Some(site) = AppState::global(cx).login_sync_requested.lock().take() else {
            return;
        };
        log::info!("site login done: {site} を自動同期する");
        let bookshelf = self.bookshelf.clone();
        bookshelf.update(cx, |b, bx| b.sync_site(&site, bx));
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
        let Some(owner) = backup_owner_scope(state) else {
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
                Some(&owner.book_ids),
                Some(&owner_filter_of(&owner)),
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
                        crate::app_state::set_toast_kind(
                            cx,
                            crate::app_state::ToastKind::Success,
                            "Drive バックアップを復元しました",
                        );
                    } else {
                        crate::app_state::set_toast_kind(
                            cx,
                            crate::app_state::ToastKind::Error,
                            "バックアップの復元に失敗しました",
                        );
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

    /// pack の鍵（v3 の PRK）を解決すべきか（未ログイン・解決済みなら不要）。
    fn needs_pack_key_unlock(&self, cx: &App) -> bool {
        let state = AppState::global(cx);
        if state.pack_root_key().is_some() {
            return false;
        }
        state
            .google_profile
            .lock()
            .as_ref()
            .is_some_and(|profile| !profile.sub.trim().is_empty())
    }

    /// ログイン直後の鍵の流れ: 解決 → その結果に応じた「パスフレーズ未設定」の警告。
    ///
    /// 警告は**鍵の解決が終わってから**判定する。先に判定すると、解錠ダイアログ
    /// （背景タスクが答えを待っている）を出している最中に、まだ鍵が無いという理由で
    /// 判定してしまう（＝本当はパスフレーズがある端末でも警告が出かねない）。
    ///
    /// 解決できなかった場合はトーストで理由を知らせる（`Unavailable` = 復元の案内は
    /// core の文言に入っている）。
    fn start_login_key_flow(&mut self, cx: &mut Context<Self>) {
        let task = crate::pack_keys::unlock_task(cx, "ログイン");
        cx.spawn(async move |this, cx| {
            let result = task.await;
            let _ = this.update(cx, |this, cx| {
                if let Err(error) = result {
                    log::warn!("pack key unlock failed: {error}");
                    crate::app_state::set_toast_kind(cx, crate::app_state::ToastKind::Error, error);
                }
                this.start_passphrase_notice_check(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// パスフレーズ未設定の警告を出すか判定する（ログインセッションごとに 1 回）。
    ///
    /// 判定は Drive を引くので背景で行う。判定できないとき（オフライン等）は出さない
    /// （[`crate::pack_keys::passphrase_notice_needed`]）。
    fn start_passphrase_notice_check(&mut self, cx: &mut Context<Self>) {
        if self.passphrase_notice_checked {
            return;
        }
        self.passphrase_notice_checked = true;
        let keys = crate::pack_keys::KeyContext::from_state(AppState::global(cx));
        let task = cx
            .background_executor()
            .spawn(async move { keys.needs_passphrase_notice() });
        cx.spawn(async move |this, cx| {
            let wanted = task.await;
            let _ = this.update(cx, |this, cx| this.apply_passphrase_notice(wanted, cx));
        })
        .detach();
    }

    /// 判定の結果を反映する（調べるのは背景・反映は UI スレッド）。
    ///
    /// 「あとで」を選んだ後に遅れて届いた結果でも出し直さない（`dismissed`）。
    fn apply_passphrase_notice(&mut self, wanted: bool, cx: &mut Context<Self>) {
        if wanted && !self.passphrase_notice_dismissed {
            self.passphrase_notice = true;
            cx.notify();
        }
    }

    /// ログインセッションの始まりに警告の状態を戻す（ログインのたびに 1 回出す）。
    fn reset_passphrase_notice(&mut self) {
        self.passphrase_notice = false;
        self.passphrase_notice_checked = false;
        self.passphrase_notice_dismissed = false;
    }

    /// パスフレーズ未設定の警告で「あとで」を選んだ（このセッションでは再表示しない）。
    fn dismiss_passphrase_notice(&mut self, cx: &mut Context<Self>) {
        self.passphrase_notice = false;
        self.passphrase_notice_dismissed = true;
        cx.notify();
    }

    /// パスフレーズ未設定の警告で「設定する」を選んだ（設定画面の本の鍵カードへ）。
    fn open_passphrase_settings(&mut self, cx: &mut Context<Self>) {
        self.passphrase_notice = false;
        // 「設定する」を選んだ時点で用は済んでいるので、このセッションでは出し直さない
        self.passphrase_notice_dismissed = true;
        self.switch_to(NavTarget::Settings, cx);
        self.sidebar_open = true;
        // 本の鍵カードの入力欄にフォーカスを合わせる（すぐ入力できるように）
        self.settings
            .update(cx, |settings, cx| settings.request_passphrase_focus(cx));
        cx.notify();
    }

    /// 解錠ダイアログの入力欄を遅延生成する（初回表示のみ）。
    fn ensure_pack_key_input(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<InputState> {
        if let Some(input) = self.pack_key_input.clone() {
            return input;
        }
        // 肩越しに読まれないようマスクする（値は `value()` で取れる）。
        // 目のアイコン（`Input::mask_toggle`）で表示 ⇄ マスクを切り替えられる。
        let input = cx.new(|cx| {
            InputState::new(window, cx)
                .masked(true)
                .placeholder("パスフレーズ")
        });
        self.pack_key_input = Some(input.clone());
        input
    }

    /// 解錠ダイアログの答えを背景タスクへ返す（答えが要る側は待っている）。
    fn answer_pack_key_prompt(
        &mut self,
        answer: crate::pack_keys::PassphraseAnswer,
        cx: &mut Context<Self>,
    ) {
        AppState::global(cx).pack_key_prompt.answer(answer);
        self.pack_key_pending = false;
        self.pack_key_input_reset = true;
        cx.notify();
    }

    /// リーダーを開く（本棚・チェックリストからの委譲）。
    pub fn open_reader(&mut self, cx: &mut Context<Self>, book_id: String) {
        if self.reader_blocked_by_import_confirm(cx) {
            return;
        }
        // 鍵（v3 の PRK）が未解決なら先に解決する。鍵が無いまま開くと暗号化 pack を
        // 読めないため、解錠（必要ならパスフレーズ入力）してから開く。
        if self.needs_pack_key_unlock(cx) {
            let task = crate::pack_keys::unlock_task(cx, "本を開く");
            cx.spawn(async move |this, cx| {
                let result = task.await;
                let _ = this.update(cx, |this, cx| {
                    if let Err(error) = result {
                        log::warn!("pack key unlock failed: {error}");
                        crate::app_state::set_toast_kind(
                            cx,
                            crate::app_state::ToastKind::Error,
                            error,
                        );
                    }
                    let reader = cx.new(|cx| ReaderView::for_book(cx, book_id));
                    this.reader = Some(reader);
                    cx.notify();
                });
            })
            .detach();
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
        if self.needs_pack_key_unlock(cx) {
            let task = crate::pack_keys::unlock_task(cx, "本を開く");
            cx.spawn(async move |this, cx| {
                let result = task.await;
                let _ = this.update(cx, |this, cx| {
                    if let Err(error) = result {
                        log::warn!("pack key unlock failed: {error}");
                        crate::app_state::set_toast_kind(
                            cx,
                            crate::app_state::ToastKind::Error,
                            error,
                        );
                    }
                    let reader =
                        cx.new(|cx| ReaderView::for_book_at(cx, book_id, page, content_id, side));
                    this.reader = Some(reader);
                    cx.notify();
                });
            })
            .detach();
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

    /// いま表示中のモーダルを登録簿へ反映する（描画の先頭と、閉じる要求の判定前に呼ぶ）。
    ///
    /// 各ビューが自分の状態を独立に持っているので、**ここで全員ぶんを 1 か所に集める**
    /// （`Workspace` は本棚のモーダルも getter で読める）。登録簿は優先度で 1 つだけを
    /// 勝者にするので、同じ画面に 2 つ出ることはない。
    pub(crate) fn sync_modals(&self, cx: &App) {
        use crate::app_state::{ModalKind, set_modal};
        set_modal(cx, ModalKind::Login, self.show_auth);
        set_modal(cx, ModalKind::ExitConfirm, self.exit_upload_prompt);
        // 鍵（v3 の PRK）の解錠は背景タスクが答えを待っているので、どの画面でも出す
        // （要求は `AppState::pack_key_prompt` が持つ）。
        set_modal(
            cx,
            ModalKind::PackPassphrase,
            AppState::global(cx).pack_key_prompt.pending().is_some(),
        );
        // パスフレーズ未設定のお願いは**情報**で、答えを待っている相手がいない。
        // 優先度は最も弱く（他が終わってから出す）し、登録簿の外にある確認
        // （Drive の有効化・起動時の取り込み確認）と重なるときも出さない。
        set_modal(
            cx,
            ModalKind::PassphraseNotice,
            self.passphrase_notice && !self.show_drive_prompt && !self.show_restore_prompt,
        );
        // 本棚のモーダルは**本棚が表示されているときだけ**登録する。表示されていない
        // （他画面にいる）間は登録を外し、見えないモーダルで「閉じる」を塞がないようにする。
        let bookshelf_active = matches!(self.active, NavTarget::Bookshelf | NavTarget::Favorites);
        let bookshelf = self.bookshelf.read(cx);
        set_modal(
            cx,
            ModalKind::Import,
            bookshelf_active && bookshelf.has_pending_import(),
        );
        set_modal(
            cx,
            ModalKind::DownloadConfirm,
            bookshelf_active && bookshelf.has_pending_download_confirm(),
        );
        set_modal(
            cx,
            ModalKind::DownloadCancel,
            bookshelf_active && bookshelf.has_pending_download_cancel(),
        );
        set_modal(
            cx,
            ModalKind::SyncNotice,
            bookshelf_active && bookshelf.has_pending_sync_notice(),
        );
    }

    /// ウィンドウの「閉じる」要求を処理する（true = 閉じてよい）。
    ///
    /// 終了時のアップロード中は、進行中のアップロードを中断させないため閉じない
    /// （閉じるボタンは無効にできないので、ここで要求を止める）。ユーザーの答えを
    /// 待っているモーダル（ログイン・取り込み確認など）が出ている間も閉じない
    /// （終了確認を重ねない。ログイン中はサイト側の同意や SSO が途中で切れる）。
    pub fn handle_window_close_request(&mut self, cx: &mut Context<Self>) -> bool {
        if self.exit_uploading {
            return false;
        }
        // 起動時の Drive 復元確認が出ている間は、そのまま閉じる（Drive 側のバックアップを
        // ローカル（旧/空）で上書きしないため、アップロード確認は出さない）。
        if self.restore_prompt_active() {
            AppState::global(cx)
                .exit_checked
                .store(true, std::sync::atomic::Ordering::SeqCst);
            return true;
        }
        // モーダルが出ている間は閉じさせない（終了確認を重ねない）。
        self.sync_modals(cx);
        if let Some(kind) = crate::app_state::active_modal(cx) {
            crate::app_state::set_toast_kind(
                cx,
                crate::app_state::ToastKind::Info,
                format!("{}が終わるまで閉じられません", kind.label()),
            );
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
        // 他のモーダルが出ている間は終了確認を重ねない（閉じるボタンと同じ扱い）。
        self.sync_modals(cx);
        if let Some(kind) = crate::app_state::active_modal(cx) {
            crate::app_state::set_toast_kind(
                cx,
                crate::app_state::ToastKind::Info,
                format!("{}が終わるまで閉じられません", kind.label()),
            );
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
        // pack の鍵（v3 の PRK）の解決に要るもの（背景スレッドへ move する）
        let keys = crate::pack_keys::KeyContext::from_state(state);
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
            // 鍵（v3 の PRK）を用意する。無ければ暗号化 pack は上げられない
            // （必要なら解錠ダイアログが出る。終了処理なので失敗しても終了はする）。
            let pack_root_key = keys.unlock("終了時のバックアップ").unwrap_or_else(|error| {
                log::warn!("exit upload: 鍵を解決できない: {error}");
                None
            });
            let _ = thundoku_core::drive::sync::sync(thundoku_core::drive::sync::SyncRequest {
                pool: &db,
                drive: &mut drive,
                packs_dir: &packs_dir,
                downloads_dir: &downloads_dir,
                identity_sub: google_sub.as_deref(),
                pack_root_key: pack_root_key.as_ref(),
                owner_key: db_key.as_ref(),
                folder_id: &folder_id,
                db_path: Some(&db_path),
            })
            .map_err(|e| e.to_string())?;
            // 未アップロードの鍵 bundle があれば上げ直す（仕様 §5.1）
            match keys.retry_pending_upload() {
                Ok(true) => log::info!("exit upload: 鍵 bundle の再アップロードに成功"),
                Ok(false) => {}
                Err(error) => log::warn!("exit upload: 鍵 bundle を上げられない: {error}"),
            }
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
        let tbf_logged_in = *AppState::global(cx).tbf_logged_in.lock();
        let booth_logged_in = *AppState::global(cx).booth_logged_in.lock();
        let fanza_logged_in = *AppState::global(cx).fanza_logged_in.lock();
        let dlsite_logged_in = *AppState::global(cx).dlsite_logged_in.lock();

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
                            .on_click({
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
        reg!(crate::actions::ShowFavorites, |this, cx| {
            this.switch_to(NavTarget::Favorites, cx);
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

        // モーダルの登録簿を更新し、**優先度が最も高い 1 つだけ**を描く。
        // 負けた側は状態を保持したまま描かれないので、勝者が閉じれば自然に出る
        // （実機で「終了確認」と「同期の続き通知」が同時に出ていたのを防ぐ）。
        self.sync_modals(cx);
        let modal = crate::app_state::active_modal(cx);

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
                Self::title_bar().into_any_element()
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
            // リーダーはサイドバーの影響を受けないよう、ウィンドウ幅いっぱいに absolute で
            // 重ねる（上端はタイトルバーの下。タイトルバーは表示中も使えるように残す）。
            // フィット計算（window.bounds().size - WIN_TITLE_BAR_HEIGHT）と実際の表示領域が
            // 一致し、見開き画像が右・下にはみ出さない。
            .child(if let Some(reader) = &self.reader {
                let view: AnyView = AnyView::from(reader.clone());
                div()
                    .id("reader-overlay")
                    .debug_selector(|| "reader-overlay".into())
                    .absolute()
                    // ウィンドウのタイトルバー（閉じる/最小化/最大化）は
                    // リーダー表示中も使えるように残す（top = タイトルバー高さ）。
                    .top(px(TITLE_BAR_HEIGHT))
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
            .child(if self.show_auth && modal == Some(crate::app_state::ModalKind::Login) {
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
                                                    crate::app_state::set_toast_kind(
                                                        cx,
                                                        crate::app_state::ToastKind::Success,
                                                        "Google Drive と同期しました",
                                                    );
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
            .child(if self.exit_upload_prompt
                && !self.show_restore_prompt
                && modal == Some(crate::app_state::ModalKind::ExitConfirm)
            {
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
            // パスフレーズ未設定のお願い（ログイン直後に 1 回だけ。優先度は最も弱いので
            // 他のモーダルが終わってから出る）
            .child(if self.passphrase_notice
                && modal == Some(crate::app_state::ModalKind::PassphraseNotice)
            {
                let handle = cx.entity();
                let content = dialog_surface(cx)
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("本の鍵を保護してください"),
                    )
                    .child(div().text_sm().child(PASSPHRASE_NOTICE_BODY))
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_center()
                            .gap_2()
                            .child(
                                dialog_button("passphrase-notice-later", "あとで")
                                    .debug_selector(|| "passphrase-notice-later".into())
                                    .on_click({
                                        let handle = handle.clone();
                                        move |_, _window, cx| {
                                            handle.update(cx, |this, cx| {
                                                this.dismiss_passphrase_notice(cx)
                                            });
                                        }
                                    }),
                            )
                            .child(
                                Button::new("passphrase-notice-settings")
                                    .debug_selector(|| "passphrase-notice-settings".into())
                                    .cursor_pointer()
                                    .primary()
                                    .label("設定する")
                                    .on_click({
                                        let handle = handle.clone();
                                        move |_, _window, cx| {
                                            handle.update(cx, |this, cx| {
                                                this.open_passphrase_settings(cx)
                                            });
                                        }
                                    }),
                            ),
                    );
                fade_dialog(window, cx, self.passphrase_notice, content).into_any_element()
            } else {
                div().into_any_element()
            })
            .child(if self.pack_key_pending
                && modal == Some(crate::app_state::ModalKind::PackPassphrase)
            {
                let handle = cx.entity();
                let input = self.ensure_pack_key_input(window, cx);
                if self.pack_key_input_reset {
                    self.pack_key_input_reset = false;
                    input.update(cx, |state, cx| state.set_value("", window, cx));
                    let focus = input.read(cx).focus_handle(cx);
                    window.focus(&focus, cx);
                }
                let request = AppState::global(cx).pack_key_prompt.pending();
                let purpose = request
                    .as_ref()
                    .map(|request| request.purpose.clone())
                    .unwrap_or_default();
                let error = self.pack_key_error.clone();
                let danger = cx.theme().danger;
                let mut content = dialog_surface(cx)
                    .child(
                        div()
                            .text_lg()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("本の鍵を解錠"),
                    )
                    .child(div().text_sm().child(format!(
                        "{purpose}には本の鍵が必要ですが、この端末に鍵がありません。\
                         別端末で設定したパスフレーズを入力すると復元できます（次回からは尋ねません）。"
                    )))
                    // 入力欄の右端の目のアイコンでマスク ⇄ 表示を切り替える
                    // （既定はマスク。値とフォーカスは保たれる）
                    .child(
                        div()
                            .debug_selector(|| "pack-key-input".into())
                            .child(
                                Input::new(&input)
                                    .cursor_text()
                                    .w_full()
                                    .mask_toggle(),
                            ),
                    );
                if let Some(error) = error {
                    content = content.child(div().text_sm().text_color(danger).child(error));
                }
                let content = content.child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_center()
                        .gap_2()
                        .child(
                            // スキップ = Google ログインの鍵（sub ラップ）だけで解く。
                            // 「スキップしたときだけ」sub ラップへ落ちる（仕様 §4.1 手順 3）。
                            dialog_button("pack-key-skip", "スキップ")
                                .cursor_pointer()
                                .on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        handle.update(cx, |this, cx| {
                                            this.answer_pack_key_prompt(
                                                crate::pack_keys::PassphraseAnswer::Skipped,
                                                cx,
                                            );
                                        });
                                    }
                                }),
                        )
                        .child(
                            Button::new("pack-key-unlock")
                                .cursor_pointer()
                                .primary()
                                .label("解錠")
                                .on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        handle.update(cx, |this, cx| {
                                            let value = this
                                                .pack_key_input
                                                .as_ref()
                                                .map(|input| {
                                                    input.read(cx).value().trim().to_string()
                                                })
                                                .unwrap_or_default();
                                            if value.is_empty() {
                                                this.pack_key_error = Some(
                                                    "パスフレーズを入力してください".to_string(),
                                                );
                                                cx.notify();
                                                return;
                                            }
                                            this.answer_pack_key_prompt(
                                                crate::pack_keys::PassphraseAnswer::Passphrase(value),
                                                cx,
                                            );
                                        });
                                    }
                                }),
                        ),
                );
                fade_dialog(window, cx, true, content).into_any_element()
            } else {
                div().into_any_element()
            })
    }
}

/// ウィンドウ上部のタイトルバーの高さ（px）。
///
/// 非 Windows は gpui-kit の `TitleBar`（`TITLE_BAR_HEIGHT` = 34 px）を使うので、ここも
/// 同じ値にする。リーダーのオーバーレイ上端（＝タイトルバーを隠さない位置）と、
/// リーダー内部のフィット計算（`image_viewer::WIN_TITLE_BAR_HEIGHT`）がこの値を参照する。
#[cfg(windows)]
pub const TITLE_BAR_HEIGHT: f32 = 36.0;
#[cfg(not(windows))]
pub const TITLE_BAR_HEIGHT: f32 = 34.0;

/// 未読数バッジの地色（件数で変わる）と文字色。
///
/// 地色はテーマに依らない固定色なので、文字色も固定の濃色にする。WCAG のコントラストは
/// 赤 5.6:1 / 黄 10.9:1 / 緑 8.3:1 で、テーマ由来の `primary_foreground` を使うと
/// ライトテーマで白になり、黄の上で 1.9:1 まで落ちて読めなくなる。
pub(crate) const BADGE_RED: u32 = 0xef4444;
pub(crate) const BADGE_YELLOW: u32 = 0xeab308;
pub(crate) const BADGE_GREEN: u32 = 0x10b981;
pub(crate) const BADGE_TEXT: u32 = 0x0a0a0a;

/// サイドバーの描画（72px の閉状態 ↔ 256px の開状態を 200ms でアニメーション）。
impl Workspace {
    /// macOS / Linux のウィンドウ上部のタイトルバー（gpui-kit の `TitleBar`）。
    ///
    /// `TitleBar::window_options()` は透過タイトルバー + `app_owns_titlebar_drag` を
    /// 指定する。つまりタイトルバーはアプリが描く契約で、描かないと信号機（閉じる /
    /// 最小化 / 最大化）が下のヘッダーに重なって押せなくなり、ウィンドウをドラッグ
    /// する領域も無くなる。システムメニューバーがメニューを持つので、空いた左側は
    /// ウィンドウ名に使う（gpui-kit の story アプリと同じ流儀）。
    #[cfg(not(windows))]
    fn title_bar() -> gpui_kit::AnyElement {
        gpui_kit::component::TitleBar::new()
            .child(
                div()
                    .debug_selector(|| "app-title-bar".into())
                    .flex()
                    .h_full()
                    .items_center()
                    .child(
                        div()
                            .debug_selector(|| "app-title-bar-title".into())
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .child("Thundoku Shelf"),
                    ),
            )
            .into_any_element()
    }

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
        // 未読数は**本棚のときだけ**出す（履歴 / 付箋 / 設定などでは本棚の話ではないため）。
        // サイドバーを開いたときの「未読数 N 件」と、閉じたときのロゴのバッジで同じ扱いにする。
        let unread = (active == NavTarget::Bookshelf).then_some(unread_count);
        // テーマ名はキャッシュから（描画のたびに `theme.mode` を DB から読まない）
        let theme_mode_name = self.theme_mode_name.clone();
        // レポート（設定の上に出す）。GitHub のトークンを持たない（投稿はブラウザーで
        // 行う）ので、ログイン状態に関わらず常に出す。
        let tbf_logged_in = *AppState::global(cx).tbf_logged_in.lock();
        let report_item = self.bottom_item(
            "sidebar-nav-report",
            Icon::new(AppIcon::Megaphone)
                .size(px(24.0))
                .text_color(sidebar_icon_color(&theme, NavTarget::Report, active))
                .into_any_element(),
            "レポート",
            open,
            active == NavTarget::Report,
            |this, cx| {
                this.switch_to(NavTarget::Report, cx);
            },
            handle.clone(),
            cx,
        );
        // バッジ色分け: 100 件以上=赤 / 10〜99 件=黄 / 1〜9 件=緑
        let badge_color = if unread_count >= 100 {
            gpui_kit::rgb(BADGE_RED)
        } else if unread_count >= 10 {
            gpui_kit::rgb(BADGE_YELLOW)
        } else {
            gpui_kit::rgb(BADGE_GREEN)
        };

        div()
            .id("sidebar")
            .debug_selector(|| "sidebar".into())
            .h_full()
            .flex()
            .flex_col()
            .relative()
            .overflow_hidden()
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
                    .debug_selector(|| "sidebar-header".into())
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
                    .child(self.sidebar_logo(
                        unread,
                        badge_color,
                        open,
                        active == NavTarget::About,
                        handle.clone(),
                        cx,
                    ))
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
                                .when_some(unread, |this, count| {
                                    this.child(
                                        div()
                                            .debug_selector(|| "sidebar-unread-count".into())
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(format!("未読数 {count} 件")),
                                    )
                                }),
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
                                .text_color(sidebar_icon_color(
                                    &theme,
                                    NavTarget::Bookshelf,
                                    active,
                                ))
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
                            NavTarget::Favorites,
                            Icon::new(AppIcon::HeartFilled)
                                .size(px(24.0))
                                .text_color(sidebar_icon_color(
                                    &theme,
                                    NavTarget::Favorites,
                                    active,
                                ))
                                .into_any_element(),
                            "お気に入り",
                            open,
                            active,
                            handle.clone(),
                            cx,
                        ),
                    )
                    .child(
                        self.nav_row(
                            NavTarget::History,
                            Icon::new(AppIcon::History)
                                .size(px(24.0))
                                .text_color(sidebar_icon_color(&theme, NavTarget::History, active))
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
                                .text_color(sidebar_icon_color(&theme, NavTarget::Notes, active))
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
                                    .text_color(sidebar_icon_color(
                                        &theme,
                                        NavTarget::Checklist,
                                        active,
                                    ))
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
                    .child(report_item)
                    .child(
                        self.bottom_item(
                            "sidebar-nav-settings",
                            Icon::new(IconName::Settings)
                                .size(px(24.0))
                                .text_color(sidebar_icon_color(&theme, NavTarget::Settings, active))
                                .into_any_element(),
                            "設定",
                            open,
                            active == NavTarget::Settings,
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
                            .into_any_element(),
                            match theme_mode_name.as_str() {
                                "dark" => "ダーク",
                                "system" => "システム",
                                _ => "ライト",
                            },
                            open,
                            // テーマは画面ではなく切替操作なので選択状態を持たない
                            false,
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
                                .into_any_element(),
                            "アカウント",
                            open,
                            // アカウントはパネルを開く操作なので選択状態を持たない
                            false,
                            |this, cx| {
                                this.open_auth_panel(cx);
                            },
                            handle.clone(),
                            cx,
                        ),
                    ),
            )
    }

    /// 未読数バッジに出すラベル。3 桁で頭打ちにする。
    ///
    /// バッジはロゴのタイル（36×36）の右上に載るので、桁が増えるほどタイルを覆う
    /// （実測: 4 桁で ~34px = タイル幅の 94%、5 桁で ~40px = タイルより広い）。
    /// 「99+」は「99 より多い」を保ったまま 3 グリフに収まる最大の表現。
    fn unread_badge_label(count: usize) -> String {
        if count > 99 {
            "99+".to_string()
        } else {
            count.to_string()
        }
    }

    /// サイドバーのロゴ（ブックマーク + 未読バッジ）。
    ///
    /// `unread` は**本棚のときだけ** `Some` が渡される（呼び出し側で判定する）。
    fn sidebar_logo(
        &self,
        unread: Option<usize>,
        badge_color: gpui_kit::Rgba,
        open: bool,
        about_active: bool,
        handle: Entity<Workspace>,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        // バッジは閉じているときだけ。0 件では出さない。
        let badge = unread.filter(|count| !open && *count > 0);
        div()
            .id("sidebar-logo")
            .debug_selector(|| "sidebar-logo".into())
            .relative()
            .flex_shrink_0()
            // 閉じたときはナビ行と同じ 36x36 に揃える（ホバー / 選択で出る下地と同じ大きさ。
            // 40x40 だと下地より一回り大きく、白いブロックだけが浮いて見えていた）。
            // 開いたときはアプリ名の隣に出るので少し大きい 40x40 のまま
            .w(px(if open { 40.0 } else { 36.0 }))
            .h(px(if open { 40.0 } else { 36.0 }))
            .rounded_xl()
            // 説明画面を開いているときだけ選択中と同じ塗りつぶしにする
            .bg(sidebar_logo_background(&theme, about_active))
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
                // ナビ行のアイコンと同じ 24px（箱は 36×36 で揃っているので、中身の大きさも
                // 揃えないとロゴだけ小さく見える）
                div().debug_selector(|| "sidebar-logo-glyph".into()).child(
                    Icon::new(AppIcon::BookMarked)
                        .size(px(24.0))
                        .text_color(sidebar_logo_glyph_color(&theme, about_active)),
                ),
            )
            .child(match badge {
                Some(count) => div()
                    .debug_selector(|| "sidebar-unread-badge".into())
                    .absolute()
                    .right(px(-4.0))
                    .top(px(-4.0))
                    // タイル（36×36）の右上に載るので小さく保つ。10px フォント + 4px 余白で
                    // 桁数無制限だと、3 桁で 28×16（タイルの 35%）になり、タイルの右上
                    // 67% を覆ってロゴを隠していた（実機の実測）。9px / 3px 余白 / 14px 角で
                    // タイルの 25% 前後に収まる。
                    .min_w(px(14.0))
                    .h(px(14.0))
                    .px(px(3.0))
                    .rounded_full()
                    .bg(badge_color)
                    .flex()
                    .items_center()
                    .justify_center()
                    // 地色（赤 / 黄 / 緑）は件数で変わる固定色なので、文字色もテーマに
                    // 依らず固定の濃色にする。`primary_foreground` はライトテーマで白に
                    // なり、黄 (0xeab308) の上でコントラスト 1.9:1 まで落ちて読めない。
                    .text_color(gpui_kit::rgb(BADGE_TEXT))
                    .text_size(px(9.0))
                    .child(Self::unread_badge_label(count))
                    .into_any_element(),
                None => div().into_any_element(),
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
            NavTarget::Favorites => "sidebar-nav-favorites",
            NavTarget::History => "sidebar-nav-history",
            NavTarget::Notes => "sidebar-nav-notes",
            NavTarget::Checklist => "sidebar-nav-checklist",
            _ => "sidebar-nav-other",
        };
        let is_active = active == target;
        let row = sidebar_row_style(&theme, is_active);
        let site_menu = target == NavTarget::Bookshelf;
        div()
            .id(id)
            .debug_selector(move || id.into())
            .flex()
            .items_center()
            .gap_2()
            .py(px(6.0))
            .rounded_xl()
            // アイコンは行の色を継承する（`Icon` 側で色を指定しない）
            .text_color(row.icon_color)
            .when_some(row.background, |this, bg| this.bg(bg))
            .when(open, |this| this.ml(px(18.0)).px(px(6.0)))
            .when(open, |this| this.w_full())
            .when(!open, |this| this.w(px(36.0)).h(px(36.0)).justify_center())
            .hover(|style| style.bg(theme.muted))
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
                        .font_weight(if row.label_emphasis {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::MEDIUM
                        })
                        .text_color(row.label_color)
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
        selected: bool,
        on_click: impl Fn(&mut Workspace, &mut Context<Workspace>) + 'static,
        handle: Entity<Workspace>,
        cx: &mut Context<Self>,
    ) -> gpui_kit::AnyElement {
        let theme = cx.theme().clone();
        let label = label.to_string();
        let row = sidebar_row_style(&theme, selected);
        div()
            .id(id)
            .debug_selector(move || id.into())
            .flex()
            .items_center()
            .gap_2()
            .py(px(6.0))
            .rounded_xl()
            // アイコンは行の色を継承する（`Icon` 側で色を指定しない）
            .text_color(row.icon_color)
            .when_some(row.background, |this, bg| this.bg(bg))
            .when(open, |this| this.ml(px(18.0)).px(px(6.0)))
            .when(open, |this| this.w_full())
            .when(!open, |this| this.w(px(36.0)).h(px(36.0)).justify_center())
            .hover(|style| style.bg(theme.muted))
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
                        .font_weight(if row.label_emphasis {
                            FontWeight::SEMIBOLD
                        } else {
                            FontWeight::MEDIUM
                        })
                        .text_color(row.label_color)
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
                            .when(site_filter.is_none(), |this| {
                                this.bg(theme.secondary)
                                    .text_color(theme.primary)
                                    .font_weight(FontWeight::MEDIUM)
                            })
                            .hover(|style| style.bg(theme.muted))
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
                                        .text_color(theme.primary)
                                        .font_weight(FontWeight::MEDIUM)
                                })
                                .hover(|style| style.bg(theme.muted))
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
                                        .text_color(theme.primary)
                                        .font_weight(FontWeight::MEDIUM)
                                })
                                .hover(|style| style.bg(theme.muted))
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
                                        .text_color(theme.primary)
                                        .font_weight(FontWeight::MEDIUM)
                                })
                                .hover(|style| style.bg(theme.muted))
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
                                        .text_color(theme.primary)
                                        .font_weight(FontWeight::MEDIUM)
                                })
                                .hover(|style| style.bg(theme.muted))
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

/// 起動時の復元確認・終了時のアップロードで使う所有者スコープ（現在の Google アカウント）。
///
/// Drive 側の `thundoku-backup.json` は「アップロードした時点のアカウントに帰属する
/// データ」だけを含むため、比較・アップロードも同じ範囲で行う必要がある。
/// `book_ids` は `books`（本の id で辿れるテーブル）用、`key` / `sub` は
/// 本棚・チェックリスト・お気に入りの `owner_sub` 判定（`OwnerFilter`）用。
struct BackupOwnerScope {
    key: [u8; 32],
    sub: String,
    book_ids: std::collections::HashSet<String>,
}

/// プロフィール（`sub`）か暗号鍵が無ければ `None`（＝所有者不明）。
fn backup_owner_scope(state: &AppState) -> Option<BackupOwnerScope> {
    let sub = state
        .google_profile
        .lock()
        .as_ref()
        .map(|p| p.sub.clone())?;
    let key = state.secrets.db_key().ok()?;
    let book_ids = db::books::owned_book_ids(&state.db_pool, &key, Some(&sub)).ok()?;
    Some(BackupOwnerScope { key, sub, book_ids })
}

/// バックアップ（`export_json` / `inspect_drive_backup`）用の所有者フィルタ。
fn owner_filter_of(scope: &BackupOwnerScope) -> thundoku_core::db::backup::OwnerFilter<'_> {
    thundoku_core::db::backup::OwnerFilter {
        key: &scope.key,
        sub: Some(&scope.sub),
    }
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

    /// 起動時の初期表示を決めるテスト用の土台（指定したサイトだけログイン済みにする）。
    fn setup_with_logins(
        cx: &mut TestAppContext,
        logged_in_sites: &[&str],
        google_logged_in: bool,
    ) -> gpui_kit::Entity<Workspace> {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        cx.update(|cx| {
            let state = AppState::global(cx);
            *state.google_logged_in.lock() = google_logged_in;
            for site in logged_in_sites {
                match *site {
                    "tbf" => *state.tbf_logged_in.lock() = true,
                    "booth" => *state.booth_logged_in.lock() = true,
                    "fanza" => *state.fanza_logged_in.lock() = true,
                    "dlsite" => *state.dlsite_logged_in.lock() = true,
                    other => panic!("未知のサイト: {other}"),
                }
            }
        });
        cx.new(Workspace::new)
    }

    /// どのサイトにもログインしていなければ、起動時は本棚ではなく説明画面を出す。
    ///
    /// 本が 1 冊も入らない状態で空の本棚を出しても行き先が無く、説明画面には
    /// ストアのログイン手順が書いてあるのでそちらを入口にする。
    #[gpui_kit::test]
    async fn starts_on_the_about_view_when_no_site_is_logged_in(cx: &mut TestAppContext) {
        let ws = setup_with_logins(cx, &[], false);
        assert_eq!(
            ws.read_with(cx, |w, _| w.active),
            NavTarget::About,
            "全サイト未ログインなのに本棚が初期表示になっている"
        );
    }

    /// Google にログインしていても、サイトにログインしていなければ説明画面。
    ///
    /// Google はアカウント連携用で本の供給元（サイト）ではないため、
    /// それだけでは本棚を初期表示にしない。
    #[gpui_kit::test]
    async fn google_login_alone_does_not_start_on_the_bookshelf(cx: &mut TestAppContext) {
        let ws = setup_with_logins(cx, &[], true);
        assert_eq!(
            ws.read_with(cx, |w, _| w.active),
            NavTarget::About,
            "Google だけのログインで本棚が初期表示になっている"
        );
    }

    /// どのサイトでもよいので 1 つログイン済みなら、本棚を初期表示にする。
    #[gpui_kit::test]
    async fn starts_on_the_bookshelf_when_a_site_is_logged_in(cx: &mut TestAppContext) {
        let ws = setup_with_logins(cx, &["tbf"], false);
        assert_eq!(
            ws.read_with(cx, |w, _| w.active),
            NavTarget::Bookshelf,
            "サイトにログイン済みなのに本棚が初期表示になっていない"
        );
    }

    /// サイトのログインが完了したら、そのサイトの同期を始めること。
    ///
    /// ログイン直後の本棚は購入済みの一覧がまだ入っていないので、手動で「同期」を
    /// 押させない。要求は 1 回で消費する（毎フレーム同期しない）。
    #[gpui_kit::test]
    async fn site_login_request_starts_that_sites_sync(cx: &mut TestAppContext) {
        let ws = setup(cx);
        cx.update(|cx| {
            let state = AppState::global(cx);
            *state.booth_logged_in.lock() = true;
            *state.login_sync_requested.lock() = Some("booth".to_string());
        });
        cx.update(|cx| ws.update(cx, |w, cx| w.handle_login_sync_request(cx)));
        let toast = cx.update(|cx| AppState::global(cx).toast_message.lock().clone());
        assert_eq!(
            toast.as_deref(),
            Some("BOOTH サイトのデータを取得中です"),
            "ログイン後にそのサイトの同期が始まっていない"
        );
        assert!(
            cx.update(|cx| AppState::global(cx).login_sync_requested.lock().is_none()),
            "同期の要求が消費されていない（要求が残ると毎回同期してしまう）"
        );
    }

    /// 同期の要求が無いときは何もしないこと。
    #[gpui_kit::test]
    async fn no_site_login_request_does_not_sync(cx: &mut TestAppContext) {
        let ws = setup(cx);
        cx.update(|cx| ws.update(cx, |w, cx| w.handle_login_sync_request(cx)));
        assert!(
            cx.update(|cx| AppState::global(cx).toast_message.lock().is_none()),
            "要求が無いのに同期が走っている"
        );
    }

    /// サイドバーの選択状態は塗りつぶし（`primary` の下地 + `primary_foreground`）で示すこと。
    ///
    /// 以前は下地（`secondary`）だけだったため、アイコンだけの幅では「いまどこに居るか」が
    /// 分からなかった（ホバーの下地とも同じ色で、余計に紛らわしかった）。文字色だけを
    /// 変える案も、ダークテーマでは `primary` と `muted_foreground` の差が小さく沈む。
    #[gpui_kit::test]
    async fn sidebar_row_style_marks_the_selected_row(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        cx.update(|cx| {
            let theme = cx.theme();
            let selected = sidebar_row_style(theme, true);
            let unselected = sidebar_row_style(theme, false);

            assert_eq!(
                selected.icon_color, theme.primary_foreground,
                "選択中のアイコンが primary_foreground になっていない"
            );
            assert_eq!(
                selected.label_color, theme.primary_foreground,
                "選択中のラベルが primary_foreground になっていない"
            );
            assert_eq!(
                selected.background,
                Some(theme.primary),
                "選択中の下地が primary の塗りつぶしになっていない"
            );
            assert!(
                selected.label_emphasis,
                "選択中のラベルが強調（太字）になっていない"
            );
            assert_ne!(
                selected.icon_color, unselected.icon_color,
                "選択中と非選択のアイコン色が同じ"
            );

            assert_eq!(
                unselected.icon_color, theme.muted_foreground,
                "非選択のアイコンが控えめな色になっていない"
            );
            assert_eq!(
                unselected.label_color, theme.muted_foreground,
                "非選択のラベルが控えめな色になっていない"
            );
            assert!(!unselected.label_emphasis, "非選択のラベルが強調されている");
            assert!(
                unselected.background.is_none(),
                "非選択の行に下地が敷かれている"
            );
        });
    }

    /// ロゴのタイルは説明画面を開いているときだけ `primary`（＝選択中）で、
    /// それ以外は控えめな `muted`。グリフは地色に対して読める色にする。
    ///
    /// サイドバーの他の行は選択中が `primary` の塗りつぶしなので、ロゴも同じ色使いに
    /// そろえる（ロゴは常に `primary` で塗ってあったため、説明画面を開いているか
    /// どうかが分からなかった）。
    #[gpui_kit::test]
    async fn sidebar_logo_tile_marks_the_about_view(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        cx.update(|cx| {
            let theme = cx.theme();
            assert_eq!(
                sidebar_logo_background(theme, true),
                theme.primary,
                "説明画面を開いているときのロゴの地色が primary でない"
            );
            assert_eq!(
                sidebar_logo_background(theme, false),
                theme.muted,
                "説明画面以外のときのロゴの地色が控えめな色でない"
            );
            assert_ne!(
                sidebar_logo_background(theme, true),
                sidebar_logo_background(theme, false),
                "説明画面のときとそれ以外でロゴの地色が同じ"
            );
            assert_eq!(
                sidebar_logo_glyph_color(theme, true),
                theme.primary_foreground,
                "選択中のロゴのグリフが primary_foreground でない"
            );
            assert_eq!(
                sidebar_logo_glyph_color(theme, false),
                theme.muted_foreground,
                "非選択のロゴのグリフが控えめな色でない"
            );
        });
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

    /// ログイン（WebView）モーダルが出ている間は、ウィンドウを閉じさせない。
    ///
    /// ログイン中に閉じるとサイト側の同意・SSO の途中でセッションが中途半端になる。
    #[gpui_kit::test]
    async fn close_request_is_rejected_while_logging_in(cx: &mut TestAppContext) {
        let ws = setup(cx);
        ws.update(cx, |ws, cx| {
            ws.open_auth(cx, crate::views::auth::AuthProvider::Dlsite);
        });
        cx.run_until_parked();

        let allowed = ws.update(cx, |ws, cx| ws.handle_window_close_request(cx));
        assert!(!allowed, "ログイン中は閉じられてはいけない");
        assert!(
            !ws.read_with(cx, |ws, _| ws.exit_upload_prompt),
            "ログイン中に終了確認ダイアログを重ねてはいけない"
        );
    }

    /// 取り込み確認（分割取り込みの選択）が出ている間も閉じさせない。
    ///
    /// 終了確認を重ねると 2 つのモーダルが同時に出て、取り込みが答え待ちのまま残る。
    #[gpui_kit::test]
    async fn close_request_is_rejected_while_an_import_prompt_is_open(cx: &mut TestAppContext) {
        let ws = setup(cx);
        ws.update(cx, |ws, cx| {
            // 取り込み確認は本棚画面のモーダル（表示中のときだけ閉じる要求を止める）
            ws.active = NavTarget::Bookshelf;
            let (reply, _rx) = std::sync::mpsc::channel();
            ws.bookshelf.update(cx, |bookshelf, cx| {
                bookshelf.request_pending_import(
                    crate::views::bookshelf::PendingImport {
                        database_id: "db-1".into(),
                        title: "テスト本".into(),
                        choices: Vec::new(),
                        selected: 0,
                        reply,
                    },
                    cx,
                );
            });
        });
        cx.run_until_parked();

        let allowed = ws.update(cx, |ws, cx| ws.handle_window_close_request(cx));
        assert!(!allowed, "取り込み確認中は閉じられてはいけない");
        assert!(
            !ws.read_with(cx, |ws, _| ws.exit_upload_prompt),
            "取り込み確認に終了確認を重ねてはいけない"
        );
    }

    /// 終了確認が出ている間は、同期の続き通知（後から立つ状態）が**勝者にならない**。
    ///
    /// 実機で「終了確認」と「同期の続き通知」が同時に出ていた。通知は状態を保持したまま
    /// 描かれないので、終了確認を閉じれば自然に出る（破棄しない）。
    #[gpui_kit::test]
    async fn exit_confirm_wins_over_a_newer_sync_notice(cx: &mut TestAppContext) {
        let ws = setup(cx);
        ws.update(cx, |ws, cx| {
            ws.active = NavTarget::Bookshelf;
            ws.exit_upload_prompt = true;
            ws.bookshelf.update(cx, |bookshelf, _| {
                // 実機で重なっていた状態を再現する（98 件 / 全体 290 件 / あと 2 回）
                bookshelf.pending_sync_notice = Some(crate::views::bookshelf::SyncNotice {
                    site_id: "fanza",
                    label: "FANZA",
                    saved: 98,
                    total_items: 290,
                    remaining_runs: 2,
                });
            });
            ws.sync_modals(cx);
            assert_eq!(
                crate::app_state::active_modal(cx),
                Some(crate::app_state::ModalKind::ExitConfirm),
                "終了確認が勝ち、続き通知は待つこと"
            );
            // 終了確認を閉じたら続き通知が繰り上がる（状態は残っている）
            ws.exit_upload_prompt = false;
            ws.sync_modals(cx);
            assert_eq!(
                crate::app_state::active_modal(cx),
                Some(crate::app_state::ModalKind::SyncNotice),
                "続き通知が破棄されている"
            );
        });
    }

    /// モーダルが無ければ、これまでどおり終了確認が出る（閉じる要求は拒否）。
    #[gpui_kit::test]
    async fn close_request_opens_the_exit_prompt_when_no_modal_is_open(cx: &mut TestAppContext) {
        let ws = setup(cx);
        let allowed = ws.update(cx, |ws, cx| ws.handle_window_close_request(cx));
        assert!(!allowed, "確認を出すときは閉じない");
        assert!(
            ws.read_with(cx, |ws, _| ws.exit_upload_prompt),
            "モーダルが無いときは終了確認を出す"
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

    /// ウィンドウ上部のタイトルバーにアプリ名が出て、信号機に重ならないこと。
    ///
    /// `TitleBar::window_options()` は透過タイトルバー + `app_owns_titlebar_drag` を
    /// 指定する（＝タイトルバーはアプリが描く契約）。描かないと信号機が下のヘッダーに
    /// 重なり、ウィンドウをドラッグする領域も無くなる。
    #[cfg(target_os = "macos")]
    #[gpui_kit::test]
    async fn title_bar_shows_the_app_name_beside_the_traffic_lights(cx: &mut TestAppContext) {
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
        let title = visual
            .debug_bounds("app-title-bar-title")
            .expect("タイトルバーにアプリ名が出ていない");
        // 信号機は左上 (9, 9) から 3 つ並ぶ（閉じる / 最小化 / 最大化）。
        // 重なると押せなくなるので、その右から始まっていること。
        assert!(
            title.origin.x >= gpui_kit::px(80.0),
            "タイトルバーのアプリ名が信号機に重なっている（x = {:?}）",
            title.origin.x
        );
    }

    /// リーダーを開いてもタイトルバーが隠れないこと（リーダーの上端がタイトルバーの直下）。
    ///
    /// リーダーは `absolute` のオーバーレイだが、ウィンドウのタイトルバー（閉じる /
    /// 最小化 / 最大化）は表示中も使えるように残す。ずれるとタイトルバーが欠けるか、
    /// 本棚のヘッダーが細く覗く。
    #[cfg(target_os = "macos")]
    #[gpui_kit::test]
    async fn reader_overlay_starts_below_the_title_bar(cx: &mut TestAppContext) {
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
        let bar = visual
            .debug_bounds("app-title-bar")
            .expect("タイトルバーが出ていない");
        let bar_bottom = bar.origin.y + bar.size.height;
        cx.update(|cx| {
            ws.update(cx, |w, cx| w.open_reader(cx, "missing-book".into()));
        });
        draw_frames(visual);
        let overlay = visual
            .debug_bounds("reader-overlay")
            .expect("リーダーのオーバーレイが出ていない");
        assert!(
            overlay.origin.y >= bar_bottom && overlay.origin.y <= bar_bottom + gpui_kit::px(1.0),
            "リーダーの上端がタイトルバーの下端と合っていない（リーダー {:?} / タイトルバー下端 {:?}）",
            overlay.origin.y,
            bar_bottom
        );
    }

    /// サイドバーの先頭（ロゴ行）が上端に張り付いていること。
    ///
    /// タイトルバーが信号機のぶんを確保しているので、サイドバー側にも同じ余白を置くと
    /// 二重に下がる（macOS は 30 px が二重になっていた）。
    #[cfg(target_os = "macos")]
    #[gpui_kit::test]
    async fn sidebar_header_sits_at_the_top(cx: &mut TestAppContext) {
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
        draw_frames(visual);
        let sidebar = visual
            .debug_bounds("sidebar")
            .expect("サイドバーが出ていない");
        let header = visual
            .debug_bounds("sidebar-header")
            .expect("サイドバーのロゴ行が出ていない");
        assert_eq!(
            header.origin.y, sidebar.origin.y,
            "サイドバーのロゴ行が上端から下がっている（ロゴ行 {:?} / サイドバー上端 {:?}）",
            header.origin.y, sidebar.origin.y
        );
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
        // サイドバーの幅は開閉アニメーション（180ms）で動く。行の位置が落ち着いてから
        // 測ってクリックする（途中で測るとクリック位置がずれて、切り替わらないことがある）。
        // 「位置が変わらなくなったら完了」とみなす（回数固定だと進み具合がぶれて不安定になる）。
        let mut prev_x = gpui_kit::px(-1.0);
        for _ in 0..20 {
            cx.executor()
                .advance_clock(std::time::Duration::from_millis(50));
            draw(visual);
            let x = visual.debug_bounds("sidebar-nav-notes").map(|b| b.origin.x);
            if x == Some(prev_x) {
                break;
            }
            if let Some(x) = x {
                prev_x = x;
            }
        }
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

    /// サイドバーの「レポート」行は**いつでも出す**（GitHub のトークンを持たず、
    /// 投稿はブラウザーで行うので、ログイン状態に依存しない）。
    #[gpui_kit::test]
    async fn report_row_is_always_available(cx: &mut TestAppContext) {
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
        let report = visual
            .debug_bounds("sidebar-nav-report")
            .expect("ログインしていなくてもレポート行が出ていない");
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

    /// 未読数は**本棚のときだけ**出す。サイドバーを開いたときの「未読数 N 件」と、
    /// 閉じたときのロゴのバッジの両方で同じ扱いにする（履歴 / 付箋 / 設定などの
    /// 画面では本棚の話ではないので出さない）。
    #[gpui_kit::test]
    async fn unread_count_shows_only_on_the_bookshelf_screen(cx: &mut TestAppContext) {
        let ws = setup(cx);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
            },
            |window, cx| gpui_kit::component::Root::new(ws.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for open in [false, true] {
            for (target, expected, label) in [
                (NavTarget::Bookshelf, true, "本棚"),
                (NavTarget::Favorites, false, "お気に入り"),
                (NavTarget::History, false, "閲覧履歴"),
                (NavTarget::Notes, false, "付箋"),
                (NavTarget::Checklist, false, "チェックリスト"),
                (NavTarget::Settings, false, "設定"),
                (NavTarget::About, false, "説明"),
                (NavTarget::Report, false, "レポート"),
            ] {
                cx.update(|cx| {
                    ws.update(cx, |w, cx| {
                        w.sidebar_open = open;
                        w.switch_to(target, cx);
                        // 件数の算出は別テスト。ここは「どの画面で出すか」を見るので直接入れる
                        w.unread_count = 18;
                        cx.notify();
                    });
                });
                draw_frames(visual);
                let selector = if open {
                    "sidebar-unread-count"
                } else {
                    "sidebar-unread-badge"
                };
                assert_eq!(
                    visual.debug_bounds(selector).is_some(),
                    expected,
                    "{label}（サイドバー {}）: {selector} の表示が違う",
                    if open { "開" } else { "閉" }
                );
            }
        }
    }

    /// テーマモードは起動時と切替時にだけ読み、描画では DB を引かない（キャッシュを持つ）。
    ///
    /// サイドバーのテーマ項目のラベルは描画のたびに必要になるため、`theme.mode` を毎回
    /// 読むとスクロールのたびにクエリが走る。切替後にキャッシュが古いままだと表示が
    /// 追従しないので、保存値と一致することも見る。
    #[gpui_kit::test]
    async fn theme_mode_is_cached_and_updated_on_change(cx: &mut TestAppContext) {
        let ws = setup(cx);
        let before = ws.read_with(cx, |w, _| w.theme_mode_name.clone());
        assert!(!before.is_empty(), "テーマモードが読み込まれていない");
        cx.update(|cx| ws.update(cx, |w, cx| w.cycle_theme(cx)));
        let after = ws.read_with(cx, |w, _| w.theme_mode_name.clone());
        assert_ne!(before, after, "テーマ切替後にキャッシュが更新されていない");
        let stored = cx.read(|cx| {
            thundoku_core::db::settings::get(&AppState::global(cx).db_pool, "theme.mode").unwrap()
        });
        assert_eq!(
            Some(after),
            stored,
            "キャッシュが保存値とずれている（表示が実際の設定と食い違う）"
        );
    }

    /// 未読数バッジの文字は、どの地色の上でも読めること（WCAG 2.x の 4.5:1 以上）。
    ///
    /// 地色はテーマに依らない固定色なので、テーマ由来の色（`primary_foreground`）を
    /// 文字色に使うとライトテーマで白になり、黄の上で読めなくなる。
    #[test]
    fn unread_badge_text_is_legible_on_every_badge_color() {
        for (name, background) in [("赤", BADGE_RED), ("黄", BADGE_YELLOW), ("緑", BADGE_GREEN)]
        {
            let ratio = contrast_ratio(BADGE_TEXT, background);
            assert!(ratio >= 4.5, "{name}のバッジで文字が読めない: {ratio:.2}:1");
        }
    }

    /// WCAG 2.x のコントラスト比（1.0〜21.0）。
    fn contrast_ratio(a: u32, b: u32) -> f32 {
        fn luminance(color: u32) -> f32 {
            let channel = |shift: u32| {
                let value = ((color >> shift) & 0xff) as f32 / 255.0;
                if value <= 0.03928 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            };
            0.2126 * channel(16) + 0.7152 * channel(8) + 0.0722 * channel(0)
        }
        let (la, lb) = (luminance(a), luminance(b));
        let (hi, lo) = if la > lb { (la, lb) } else { (lb, la) };
        (hi + 0.05) / (lo + 0.05)
    }

    /// ロゴのグリフは下のナビ行のアイコンと同じ 24px（箱は 36×36 で揃っているので、
    /// 中身の大きさも揃えないとロゴだけ小さく見える）。
    #[gpui_kit::test]
    async fn sidebar_logo_glyph_is_as_large_as_nav_icons(cx: &mut TestAppContext) {
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
        let glyph = visual
            .debug_bounds("sidebar-logo-glyph")
            .expect("ロゴのグリフが無い");
        // ナビ行のアイコンはすべて 24px（`Icon::new(...).size(px(24.0))`）
        assert_eq!(
            glyph.size.width.as_f32(),
            24.0,
            "ロゴのグリフがナビのアイコンと揃っていない"
        );
        assert_eq!(glyph.size.height.as_f32(), 24.0);
    }

    /// 未読数バッジがロゴのタイル（36×36）を覆い尽くさない。
    ///
    /// バッジはタイルの右上に載るので、大きいとロゴそのものを隠す。実機（272 件）では
    /// 10px フォント + 4px 余白のピルが 28×16 = タイルの **35%** になり、タイルの右上
    /// 67% を覆っていた（ロゴのグリフの上端が隠れて見えない）。
    /// 9px フォント / 3px 余白 / 14px 角に詰めると 23×14 = **25%** になる。
    /// 面積がタイルの 30% 未満に収まっていることを見る。
    #[gpui_kit::test]
    async fn unread_badge_does_not_dominate_the_logo_tile(cx: &mut TestAppContext) {
        let ws = setup(cx);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
            },
            |window, cx| gpui_kit::component::Root::new(ws.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        cx.update(|cx| {
            ws.update(cx, |w, cx| {
                w.sidebar_open = false;
                w.switch_to(NavTarget::Bookshelf, cx);
                w.unread_count = 272;
                cx.notify();
            });
        });
        draw_frames(visual);
        let tile = visual
            .debug_bounds("sidebar-logo")
            .expect("ロゴのタイルが無い");
        let badge = visual
            .debug_bounds("sidebar-unread-badge")
            .expect("未読数バッジが無い");
        let ratio = (badge.size.width.as_f32() * badge.size.height.as_f32())
            / (tile.size.width.as_f32() * tile.size.height.as_f32());
        assert!(
            ratio < 0.30,
            "バッジがタイルを覆いすぎ: badge={:?} tile={:?} ratio={ratio:.3}",
            badge.size,
            tile.size
        );
    }

    /// 未読数バッジのラベルは 3 桁で頭打ちにする。
    ///
    /// バッジはロゴのタイル（36×36）の右上に載るので、桁が増えるほどタイルを覆う。
    /// 「99+」は「99 より多い」を保ったまま 3 グリフ（＝今の見た目と同じ幅）に収まる
    /// 最大の表現。4 桁以上は数字を出さない。
    #[test]
    fn unread_badge_label_caps_at_three_glyphs() {
        assert_eq!(Workspace::unread_badge_label(1), "1");
        assert_eq!(Workspace::unread_badge_label(9), "9");
        assert_eq!(Workspace::unread_badge_label(10), "10");
        assert_eq!(Workspace::unread_badge_label(99), "99");
        assert_eq!(Workspace::unread_badge_label(100), "99+");
        assert_eq!(Workspace::unread_badge_label(272), "99+");
        assert_eq!(Workspace::unread_badge_label(usize::MAX), "99+");
    }

    /// サイドバーの「お気に入り」: 本棚と同じビューを使い、**お気に入りの絞り込みボタンは
    /// 出さない**（画面そのものが絞り込み）。タイトルは「お気に入り」で、自動ダウンロードの
    /// 説明を出す。
    #[gpui_kit::test]
    async fn favorites_screen_hides_the_favorite_filter_and_shows_the_note(
        cx: &mut TestAppContext,
    ) {
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

        // 本棚: 絞り込みボタンは出る / 説明は出ない
        cx.update(|cx| ws.update(cx, |w, cx| w.switch_to(NavTarget::Bookshelf, cx)));
        draw_frames(visual);
        assert!(
            visual.debug_bounds("filter-favorite").is_some(),
            "本棚にお気に入りの絞り込みボタンが出ていない"
        );
        assert!(
            visual.debug_bounds("favorites-note").is_none(),
            "本棚に自動ダウンロードの説明が出ている"
        );

        // お気に入り: 本棚のビューを出しつつ、絞り込みボタンは出さず説明を出す
        cx.update(|cx| ws.update(cx, |w, cx| w.switch_to(NavTarget::Favorites, cx)));
        draw_frames(visual);
        assert!(
            visual.debug_bounds("bookshelf-root").is_some(),
            "お気に入り画面が本棚のビューを使っていない"
        );
        assert!(
            visual.debug_bounds("filter-favorite").is_none(),
            "お気に入り画面にお気に入りの絞り込みボタンが出ている"
        );
        assert!(
            visual.debug_bounds("favorites-note").is_some(),
            "お気に入り画面に自動ダウンロードの説明が出ていない"
        );
    }

    /// お気に入り画面では「タグ取得」「同期」を出さない（本棚側の操作なので）。
    #[gpui_kit::test]
    async fn favorites_screen_hides_tag_fetch_and_sync(cx: &mut TestAppContext) {
        let ws = setup(cx);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
            },
            |window, cx| gpui_kit::component::Root::new(ws.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();

        // 本棚: どちらも出る
        cx.update(|cx| ws.update(cx, |w, cx| w.switch_to(NavTarget::Bookshelf, cx)));
        draw_frames(visual);
        for selector in ["bookshelf-tag-fetch", "bookshelf-sync"] {
            assert!(
                visual.debug_bounds(selector).is_some(),
                "本棚に {selector} が出ていない"
            );
        }

        // お気に入り: どちらも出さない（お気に入り画面は本棚の操作を持たない）
        cx.update(|cx| ws.update(cx, |w, cx| w.switch_to(NavTarget::Favorites, cx)));
        draw_frames(visual);
        for selector in ["bookshelf-tag-fetch", "bookshelf-sync"] {
            assert!(
                visual.debug_bounds(selector).is_none(),
                "お気に入り画面に {selector} が出ている"
            );
        }
    }

    /// サイドバーの「お気に入り」行は本棚の下・閲覧履歴の上に出て、クリックで切り替わる。
    #[gpui_kit::test]
    async fn favorites_row_sits_below_bookshelf(cx: &mut TestAppContext) {
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
        draw_frames(visual);
        let bookshelf = visual
            .debug_bounds("sidebar-nav-bookshelf")
            .expect("本棚の行が出ていない");
        let favorites = visual
            .debug_bounds("sidebar-nav-favorites")
            .expect("お気に入りの行が出ていない");
        let history = visual
            .debug_bounds("sidebar-nav-history")
            .expect("閲覧履歴の行が出ていない");
        assert!(
            bookshelf.origin.y < favorites.origin.y && favorites.origin.y < history.origin.y,
            "並びが 本棚 → お気に入り → 閲覧履歴 になっていない: \
             bookshelf_y={} favorites_y={} history_y={}",
            bookshelf.origin.y.as_f32(),
            favorites.origin.y.as_f32(),
            history.origin.y.as_f32()
        );

        cx.update(|cx| {
            ws.update(cx, |w, cx| w.switch_to(NavTarget::Favorites, cx));
        });
        assert_eq!(
            ws.read_with(cx, |w, _| w.active),
            NavTarget::Favorites,
            "お気に入りへ切り替わっていない"
        );
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
                backup_owner_scope(state).is_none(),
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
                backup_owner_scope(state).map(|scope| scope.book_ids),
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

    /// パスフレーズ未設定の警告は、**ログインセッションごとに 1 回**だけ出す。
    ///
    /// 「あとで」を選んだ後に、遅れて届いた判定結果で出し直すと「あとで」が効かない。
    /// ログアウト→再ログインではまた出す（[`Workspace::reset_passphrase_notice`]）。
    #[gpui_kit::test]
    async fn passphrase_notice_shows_once_per_login_session(cx: &mut TestAppContext) {
        let ws = setup(cx);
        // 判定の結果「パスフレーズ未設定」と分かった
        ws.update(cx, |w, cx| w.apply_passphrase_notice(true, cx));
        assert!(
            ws.read_with(cx, |w, _| w.passphrase_notice),
            "警告を出す判定が反映されていない"
        );

        // 判定は 1 セッション 1 回（2 回目は Drive を引き直さない）
        ws.update(cx, |w, cx| w.start_passphrase_notice_check(cx));
        assert!(
            ws.read_with(cx, |w, _| w.passphrase_notice_checked),
            "判定済みの印が立っていない（毎回 Drive を引いてしまう）"
        );

        // 「あとで」で閉じる
        ws.update(cx, |w, cx| w.dismiss_passphrase_notice(cx));
        assert!(!ws.read_with(cx, |w, _| w.passphrase_notice));

        // 遅れて届いた同じ結果では出し直さない
        ws.update(cx, |w, cx| w.apply_passphrase_notice(true, cx));
        assert!(
            !ws.read_with(cx, |w, _| w.passphrase_notice),
            "「あとで」の後に警告を出し直している"
        );

        // ログアウト→再ログインではまた出す
        ws.update(cx, |w, _| w.reset_passphrase_notice());
        ws.update(cx, |w, cx| w.apply_passphrase_notice(true, cx));
        assert!(
            ws.read_with(cx, |w, _| w.passphrase_notice),
            "再ログインしてもう一度出なくなっている"
        );
    }

    /// 警告は他のモーダルと重ならない（優先度で 1 つだけ出す）。
    ///
    /// 情報なので他のモーダル（答えを待つ確認）には譲り、閉じたら繰り上がる。
    /// 登録簿の外にある確認（Drive の有効化）とも重ねない。
    #[gpui_kit::test]
    async fn passphrase_notice_yields_to_other_modals(cx: &mut TestAppContext) {
        use crate::app_state::{ModalKind, active_modal};

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

        cx.update(|cx| ws.update(cx, |w, cx| w.apply_passphrase_notice(true, cx)));
        draw_frames(visual);
        assert_eq!(
            cx.update(|cx| active_modal(cx)),
            Some(ModalKind::PassphraseNotice),
            "警告がモーダルとして登録されていない"
        );
        assert!(
            visual.debug_bounds("passphrase-notice-settings").is_some(),
            "警告ダイアログが描画されていない"
        );

        // 終了確認（答えを待つ）が勝ち、警告は待つ
        cx.update(|cx| {
            ws.update(cx, |w, cx| {
                w.exit_upload_prompt = true;
                cx.notify();
            });
        });
        draw_frames(visual);
        assert_eq!(
            cx.update(|cx| active_modal(cx)),
            Some(ModalKind::ExitConfirm),
            "答えを待つ確認より警告が前に出ている"
        );
        assert!(
            visual.debug_bounds("passphrase-notice-settings").is_none(),
            "2 つのモーダルが同時に出ている"
        );

        // 閉じたら繰り上がる（警告の状態は保持されている）
        cx.update(|cx| {
            ws.update(cx, |w, cx| {
                w.exit_upload_prompt = false;
                cx.notify();
            });
        });
        draw_frames(visual);
        assert_eq!(
            cx.update(|cx| active_modal(cx)),
            Some(ModalKind::PassphraseNotice),
            "勝者が閉じても警告が出てこない"
        );

        // Drive の有効化の確認（登録簿の外の確認）と重なるときは出さない
        cx.update(|cx| {
            ws.update(cx, |w, cx| {
                w.show_drive_prompt = true;
                cx.notify();
            });
        });
        draw_frames(visual);
        assert_eq!(
            cx.update(|cx| active_modal(cx)),
            None,
            "Drive の確認と警告を重ねて出している"
        );
        assert!(visual.debug_bounds("passphrase-notice-settings").is_none());
    }

    /// 警告の「設定する」は設定画面へ移動し、「あとで」は閉じるだけ。
    #[gpui_kit::test]
    async fn passphrase_notice_buttons_open_settings_or_dismiss(cx: &mut TestAppContext) {
        // Google ログイン済みにしておく（設定画面の本の鍵カードに入力欄が出る）
        let ws = setup_with_logins(cx, &[], true);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
            },
            |window, cx| gpui_kit::component::Root::new(ws.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        draw_frames(visual);

        // 「あとで」は閉じるだけ（画面は変わらない）
        let before = ws.read_with(cx, |w, _| w.active);
        cx.update(|cx| ws.update(cx, |w, cx| w.apply_passphrase_notice(true, cx)));
        draw_frames(visual);
        let later = visual
            .debug_bounds("passphrase-notice-later")
            .expect("「あとで」ボタンが出ていない");
        visual.simulate_click(later.center(), gpui_kit::Modifiers::default());
        assert!(
            !ws.read_with(cx, |w, _| w.passphrase_notice),
            "「あとで」で閉じていない"
        );
        assert_eq!(
            ws.read_with(cx, |w, _| w.active),
            before,
            "「あとで」で画面が変わっている"
        );

        // 「設定する」は設定画面へ移動し、本の鍵カードの入力欄にフォーカスを合わせる
        cx.update(|cx| {
            ws.update(cx, |w, cx| {
                w.reset_passphrase_notice();
                w.apply_passphrase_notice(true, cx);
            });
        });
        draw_frames(visual);
        let settings_button = visual
            .debug_bounds("passphrase-notice-settings")
            .expect("「設定する」ボタンが出ていない");
        visual.simulate_click(settings_button.center(), gpui_kit::Modifiers::default());
        draw_frames(visual);
        assert_eq!(
            ws.read_with(cx, |w, _| w.active),
            NavTarget::Settings,
            "「設定する」で設定画面へ移動していない"
        );
        assert!(
            !ws.read_with(cx, |w, _| w.passphrase_notice),
            "「設定する」で閉じていない"
        );
        // 本の鍵カードは先頭にあり、入力欄にフォーカスが入っている
        let key_card = visual
            .debug_bounds("settings-card-key")
            .expect("設定画面に本の鍵カードが出ていない");
        assert!(
            key_card.top() < gpui_kit::px(800.0) && key_card.bottom() > gpui_kit::px(0.0),
            "本の鍵カードが画面の外にある（上端から見えない）"
        );
        let input = ws
            .read_with(cx, |w, cx| w.settings.read(cx).passphrase_input())
            .expect("入力欄が作られていない");
        assert!(
            visual.update(|window, cx| input.read(cx).focus_handle(cx).is_focused(window)),
            "本の鍵カードの入力欄にフォーカスが入っていない"
        );
    }

    /// 解錠ダイアログの入力欄も、右端の目のアイコンでマスク ⇄ 表示を切り替えられる。
    #[gpui_kit::test]
    async fn pack_key_dialog_input_mask_toggles_with_the_eye_button(cx: &mut TestAppContext) {
        let ws = setup(cx);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
            },
            |window, cx| gpui_kit::component::Root::new(ws.clone(), window, cx),
        );
        // 背景タスクがパスフレーズを尋ねている状態を作る（ダイアログは要求を見て出す）
        let prompt = cx.update(|cx| AppState::global(cx).pack_key_prompt.clone());
        let asked = std::thread::spawn({
            let prompt = prompt.clone();
            move || prompt.ask("本を開く", None)
        });
        for _ in 0..200 {
            if prompt.pending().is_some() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(prompt.pending().is_some(), "解錠の要求が出ていない");

        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        cx.update(|cx| {
            ws.update(cx, |w, cx| {
                w.pack_key_pending = true;
                cx.notify();
            });
        });
        draw_frames(visual);

        let input = ws.read_with(cx, |w, _| w.pack_key_input.clone()).expect("入力欄");
        assert!(
            cx.read(|cx| input.read(cx).presentation().is_masked()),
            "既定はマスク"
        );

        let bounds = visual
            .debug_bounds("pack-key-input")
            .expect("解錠ダイアログの入力欄が出ていない");
        let eye = gpui_kit::point(
            bounds.right() - gpui_kit::px(22.0),
            bounds.center().y,
        );
        visual.update(|window, cx| {
            input.update(cx, |state, cx| state.set_value("passphrase-1", window, cx));
        });

        visual.simulate_click(eye, gpui_kit::Modifiers::default());
        assert!(
            !cx.read(|cx| input.read(cx).presentation().is_masked()),
            "目のアイコンを押しても表示に切り替わらない"
        );
        assert_eq!(
            cx.read(|cx| input.read(cx).value()),
            "passphrase-1",
            "切り替えで入力内容を失っている"
        );

        // 背景タスク（テストが起こした 1 件）を終わらせる
        cx.update(|cx| {
            AppState::global(cx)
                .pack_key_prompt
                .answer(crate::pack_keys::PassphraseAnswer::Skipped);
        });
        assert_eq!(asked.join().ok(), Some(crate::pack_keys::PassphraseAnswer::Skipped));
    }
}
