//! 設定ビュー: アカウント・Google Drive・外観・データ管理。

use std::path::PathBuf;

use crate::components::dialog::{dialog_button, dialog_surface, fade_dialog};
use gpui_kit::StyledImage as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::dialog::Dialog;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::{
    App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement, Render, SharedString, StatefulInteractiveElement as _, Subscription, Window,
    div, img, px,
};
use gpui_kit::{ReadGlobal as _, Styled as _};

use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName};
use thundoku_core::db;
use thundoku_core::drive::sync;
use thundoku_core::drive::{DriveApi, DriveClient};
use thundoku_core::secrets;
use thundoku_core::tbf::UreqTransport;

use crate::actions::OpenAuthProvider;
use crate::app_state::AppState;
use crate::views::bookshelf::{
    cover_cache_path, fetch_cover_bytes, remove_legacy_cover_cache, write_cover_cache,
};

/// 本棚の表示状態（非表示フラグ等）が変わったことを通知するイベント
#[derive(Clone, Copy)]
pub struct BookshelfDataChanged;

impl gpui_kit::EventEmitter<BookshelfDataChanged> for SettingsView {}
use crate::icons::AppIcon;
use crate::views::auth::AuthProvider;

pub struct SettingsView {
    busy: bool,
    error: Option<String>,
    /// 非表示にした本の一覧（設定画面で表示解除できる）
    hidden_items: Vec<thundoku_core::db::bookshelf::BookshelfItem>,
    /// 非表示リストの表紙取得の進行中フラグ（二重実行防止）
    fetch_covers_in_progress: bool,
    /// 表紙を再取得中のサイト（設定の「表紙更新」。二重実行防止）
    cover_refresh_site: Option<&'static str>,
    confirm_delete: bool,
    /// 保存先変更で選択された新しいデータディレクトリ（移動確認用）
    pending_data_dir: Option<std::path::PathBuf>,
    /// 保存先変更の移動確認ダイアログの表示フラグ
    confirm_data_dir: bool,
    /// 「同期情報をクリア」の確認ダイアログ表示フラグ
    confirm_clear_sync: bool,
    storage_bytes: u64,
    /// 技術書典サイトのビューアー表示モード（viewer.mode.techbookfest と同期）
    tbf_viewer_mode: String,
    /// 技術書典サイトのページめくり方向
    tbf_page_turn: String,
    /// BOOTH サイトのビューアー表示モード（viewer.mode.booth と同期）
    booth_viewer_mode: String,
    /// BOOTH サイトのページめくり方向
    booth_page_turn: String,
    /// FANZA同人サイトのビューアー表示モード（viewer.mode.fanza と同期）
    fanza_viewer_mode: String,
    /// FANZA同人サイトのページめくり方向
    fanza_page_turn: String,
    /// DLsite サイトのビューアー表示モード（viewer.mode.dlsite と同期）
    dlsite_viewer_mode: String,
    /// DLsite サイトのページめくり方向
    dlsite_page_turn: String,
    /// ビューアのホイール方向（`down-to-next` = 下スクロールで次へ / `up-to-next`）。
    /// サイトに依らない設定なので共通カードに出す。
    viewer_wheel_direction: String,
    /// データベース情報: 未読/読んでいる途中/読了 の冊数
    status_counts: (usize, usize, usize),
    /// プロフィール再取得中フラグ（設定画面表示時のログイン状態チェック）
    #[cfg_attr(test, allow(dead_code))]
    profile_fetching: bool,
    /// 総本数（render での毎回の DB 読みを避けるためのキャッシュ）
    book_count: usize,
    /// Drive 同期有効フラグ（render での毎回の DB 読みを避けるためのキャッシュ）
    drive_enabled: bool,
    /// Drive の最終同期時刻 / 件数 / 合計バイト数（同じくキャッシュ。`reload` で更新）
    drive_last_sync: Option<String>,
    drive_file_count: usize,
    drive_total_bytes: u64,
    /// API の最終同期時刻（同じくキャッシュ。`reload` で更新）
    api_last_sync_at: Option<String>,
    /// チェックリスト定期取得間隔の編集入力（render で遅延生成）。
    poll_interval_input: Option<Entity<InputState>>,
    /// 入力の変更（Blur / Enter）を購読して確定するための Subscription。
    poll_interval_subscription: Option<Subscription>,
}

/// ビューアのホイール方向の設定キーと値。
///
/// Windows / macOS とも「ユーザーにとっての下（＝文書の先へ進む方向）で次へ」が自然:
/// Windows はホイールを前方へ回す（`WM_MOUSEWHEEL` の +120）が下・次へ、
/// macOS は AppKit がナチュラルスクロール設定を反映した delta を渡すので、
/// 同じ規則でユーザーの「下」がそのまま次へになる。
pub(crate) const WHEEL_DIRECTION_KEY: &str = "viewer.wheel_direction";
pub(crate) const WHEEL_DIRECTION_DEFAULT: &str = "down-to-next";
pub(crate) const WHEEL_DIRECTION_UP: &str = "up-to-next";

/// Google の再ログインが必要なときの案内（生の `invalid_grant` JSON は出さない）。
const GOOGLE_AUTH_EXPIRED_NOTICE: &str = "Google のログインが無効になりました（トークンが失効または取り消されています）。\
     もう一度ログインしてください";

/// Google の認証が失効したことを示すメッセージか。
///
/// 同期のエラーは `String` に畳まれて渡ってくるため、core の `GoogleError` の文言で判定する
/// （`RefreshTokenRevoked` / `NoRefreshToken` はどちらも再ログインが必要）。core 側の文言が
/// 変わったらテストが落ちるようにしてある。
fn is_google_auth_expired(message: &str) -> bool {
    message.contains("re-authorize required")
}

impl SettingsView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let storage_bytes = {
            let state = AppState::global(cx);
            dir_size(&state.data_dir)
        };
        Self {
            busy: false,
            error: None,
            hidden_items: Vec::new(),
            fetch_covers_in_progress: false,
            cover_refresh_site: None,
            confirm_delete: false,
            pending_data_dir: None,
            confirm_data_dir: false,
            confirm_clear_sync: false,
            storage_bytes,
            // サイト別キー → 既存のグローバル設定（viewer.mode 等）にフォールバック
            tbf_viewer_mode: Self::read_setting(cx, "viewer.mode.techbookfest")
                .or_else(|| Self::read_setting(cx, "viewer.mode"))
                .unwrap_or_else(|| "single".into()),
            tbf_page_turn: Self::read_setting(cx, "viewer.page_turn.techbookfest")
                .or_else(|| Self::read_setting(cx, "viewer.page_turn"))
                .unwrap_or_else(|| "left-to-right".into()),
            booth_viewer_mode: Self::read_setting(cx, "viewer.mode.booth")
                .unwrap_or_else(|| "single".into()),
            booth_page_turn: Self::read_setting(cx, "viewer.page_turn.booth")
                .unwrap_or_else(|| "left-to-right".into()),
            // FANZA は既定で見開き + 右綴じ（サイト設定が無ければ）。
            fanza_viewer_mode: Self::read_setting(cx, "viewer.mode.fanza")
                .or_else(|| Self::read_setting(cx, "viewer.mode"))
                .unwrap_or_else(|| "spread".into()),
            fanza_page_turn: Self::read_setting(cx, "viewer.page_turn.fanza")
                .or_else(|| Self::read_setting(cx, "viewer.page_turn"))
                .unwrap_or_else(|| "right-to-left".into()),
            // DLsite も同人漫画のため既定で見開き + 右綴じ。
            dlsite_viewer_mode: Self::read_setting(cx, "viewer.mode.dlsite")
                .or_else(|| Self::read_setting(cx, "viewer.mode"))
                .unwrap_or_else(|| "spread".into()),
            dlsite_page_turn: Self::read_setting(cx, "viewer.page_turn.dlsite")
                .or_else(|| Self::read_setting(cx, "viewer.page_turn"))
                .unwrap_or_else(|| "right-to-left".into()),
            // ホイール方向は画面に入ったとき（`reload`）に読む。ここは既定値。
            // Windows / macOS とも「ユーザーにとっての下（文書の先へ進む）」が次へ
            // （macOS は AppKit がナチュラルスクロール設定を反映した値を渡す）。
            viewer_wheel_direction: WHEEL_DIRECTION_DEFAULT.to_string(),
            status_counts: (0, 0, 0),
            profile_fetching: false,
            book_count: 0,
            drive_enabled: Self::read_setting(cx, "drive.sync.enabled")
                .is_some_and(|v| v == "true"),
            drive_last_sync: None,
            drive_file_count: 0,
            drive_total_bytes: 0,
            api_last_sync_at: None,
            poll_interval_input: None,
            poll_interval_subscription: None,
        }
    }

    fn read_setting(cx: &App, key: &str) -> Option<String> {
        let state = AppState::global(cx);
        let db = &state.db_pool;
        db::settings::get(db, key).ok().flatten()
    }

    fn write_setting(cx: &mut Context<Self>, key: &str, value: &str) {
        let state = AppState::global(cx);
        let db = &state.db_pool;
        let _ = db::settings::set(db, key, value);
    }

    /// チェックリスト定期取得間隔の編集入力を遅延生成する（初回のみ）。
    /// Enter / Blur で確定して `checklist.poll.interval_min` に保存する。
    fn ensure_poll_interval_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.poll_interval_input.is_some() {
            return;
        }
        let state = cx.new(|cx| InputState::new(window, cx).placeholder("5"));
        let interval = Self::read_setting(cx, "checklist.poll.interval_min")
            .unwrap_or_else(|| "5".to_string());
        state.update(cx, |s, cx| s.set_value(interval, window, cx));
        let sub = cx.subscribe(
            &state,
            |this: &mut Self, _: Entity<InputState>, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                    this.commit_poll_interval(cx);
                }
            },
        );
        self.poll_interval_input = Some(state);
        self.poll_interval_subscription = Some(sub);
    }

    /// 入力値（分）を 1 以上にクランプして `checklist.poll.interval_min` に保存する。
    fn commit_poll_interval(&mut self, cx: &mut Context<Self>) {
        let Some(state) = self.poll_interval_input.as_ref() else {
            return;
        };
        let text = state.read(cx).value().to_string();
        let n: i64 = text.trim().parse().unwrap_or(5).max(1);
        Self::write_setting(cx, "checklist.poll.interval_min", &n.to_string());
        cx.notify();
    }

    /// 技術書典サイトの表示モードを保存（ビューアー起動時に
    /// viewer.mode.techbookfest を読む。既存のグローバルキーも後方互換で維持）。
    pub fn set_viewer_mode(&mut self, cx: &mut Context<Self>, mode: &str) {
        self.tbf_viewer_mode = mode.to_string();
        Self::write_setting(cx, "viewer.mode.techbookfest", mode);
        Self::write_setting(cx, "viewer.mode", mode);
        cx.notify();
    }

    /// 技術書典サイトのページめくり方向を保存。
    pub fn set_page_turn(&mut self, cx: &mut Context<Self>, direction: &str) {
        self.tbf_page_turn = direction.to_string();
        Self::write_setting(cx, "viewer.page_turn.techbookfest", direction);
        Self::write_setting(cx, "viewer.page_turn", direction);
        cx.notify();
    }

    /// BOOTH サイトの表示モードを保存。
    pub fn set_booth_viewer_mode(&mut self, cx: &mut Context<Self>, mode: &str) {
        self.booth_viewer_mode = mode.to_string();
        Self::write_setting(cx, "viewer.mode.booth", mode);
        cx.notify();
    }

    /// BOOTH サイトのページめくり方向を保存。
    pub fn set_booth_page_turn(&mut self, cx: &mut Context<Self>, direction: &str) {
        self.booth_page_turn = direction.to_string();
        Self::write_setting(cx, "viewer.page_turn.booth", direction);
        cx.notify();
    }

    /// FANZA同人サイトの表示モードを保存。
    pub fn set_fanza_viewer_mode(&mut self, cx: &mut Context<Self>, mode: &str) {
        self.fanza_viewer_mode = mode.to_string();
        Self::write_setting(cx, "viewer.mode.fanza", mode);
        cx.notify();
    }

    /// FANZA同人サイトのページめくり方向を保存。
    pub fn set_fanza_page_turn(&mut self, cx: &mut Context<Self>, direction: &str) {
        self.fanza_page_turn = direction.to_string();
        Self::write_setting(cx, "viewer.page_turn.fanza", direction);
        cx.notify();
    }

    /// DLsite サイトの表示モードを保存。
    pub fn set_dlsite_viewer_mode(&mut self, cx: &mut Context<Self>, mode: &str) {
        self.dlsite_viewer_mode = mode.to_string();
        Self::write_setting(cx, "viewer.mode.dlsite", mode);
        cx.notify();
    }

    /// DLsite サイトのページめくり方向を保存。
    pub fn set_dlsite_page_turn(&mut self, cx: &mut Context<Self>, direction: &str) {
        self.dlsite_page_turn = direction.to_string();
        Self::write_setting(cx, "viewer.page_turn.dlsite", direction);
        cx.notify();
    }

    /// ビューアのホイール方向を設定して保存する（サイトに依らない共通設定）。
    pub fn set_wheel_direction(&mut self, cx: &mut Context<Self>, direction: &str) {
        self.viewer_wheel_direction = direction.to_string();
        Self::write_setting(cx, WHEEL_DIRECTION_KEY, direction);
        cx.notify();
    }

    /// 画面に入ったとき / データが変わったときに、表示に使う状態をまとめて読み込む。
    ///
    /// 呼び出し元は `Workspace::switch_to`（設定画面への切り替え時）と、この画面の
    /// 操作で DB を書き換えた直後。**`render` からは呼ばない**（描画のたびに走ると、
    /// スクロールのたびに全書籍ぶんの進捗クエリが引かれて引っかかる）。
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        // 同期中は DB の Mutex を長時間握るため、UI 側の DB 読みをスキップして
        // フリーズを避ける（完了後は自動で再開される）
        if self.busy {
            return;
        }
        self.refresh_status_counts(cx);
        self.drive_enabled =
            Self::read_setting(cx, "drive.sync.enabled").is_some_and(|v| v == "true");
        self.drive_last_sync = Self::read_setting(cx, "drive.last_sync_at");
        self.drive_file_count = Self::read_setting(cx, "drive.file_count")
            .and_then(|v| v.parse::<usize>().ok())
            .unwrap_or(0);
        self.drive_total_bytes = Self::read_setting(cx, "drive.total_bytes")
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(0);
        self.api_last_sync_at = Self::read_setting(cx, "api.last_sync_at");
        // ビューアのホイール方向（未知の値は既定に倒す）
        self.viewer_wheel_direction = Self::read_setting(cx, WHEEL_DIRECTION_KEY)
            .filter(|value| value == WHEEL_DIRECTION_UP || value == WHEEL_DIRECTION_DEFAULT)
            .unwrap_or_else(|| WHEEL_DIRECTION_DEFAULT.to_string());
        // 非表示リスト（設定画面から表示解除できるようにする）
        let state = AppState::global(cx);
        self.hidden_items = db::bookshelf::list_hidden(&state.db_pool).unwrap_or_default();
        // 表紙キャッシュが無いものはバックグラウンドで取得する
        self.fetch_missing_covers(cx);
    }

    /// 未読/読んでいる途中/読了 の冊数を集計（Web の getReadingStatusCounts 相当）。
    fn refresh_status_counts(&mut self, cx: &mut Context<Self>) {
        let state = AppState::global(cx);
        let db = &state.db_pool;
        let books = db::books::list(db).unwrap_or_default();
        let (mut unread, mut reading, mut read) = (0usize, 0usize, 0usize);
        for book in &books {
            let progress = db::progress::get(db, &book.id).ok().flatten();
            // 判定は core の `ReadingState` に集約する（3 状態の定義を 2 か所に持たない）。
            // 以前は `current_page + 1 >= total` で判定しており、1 ページ早く読了になっていた。
            match db::progress::ReadingState::from_progress(progress.as_ref()) {
                db::progress::ReadingState::Read => read += 1,
                db::progress::ReadingState::Reading => reading += 1,
                db::progress::ReadingState::Unread => unread += 1,
            }
        }
        self.status_counts = (unread, reading, read);
        self.book_count = books.len();
    }

    /// そのサイトの表紙キャッシュを捨てて取り直す（設定の「表紙更新」）。
    ///
    /// 本棚の表紙取得と同じ URL 規則（`fetch_cover_bytes`）とキャッシュ形式を使う。
    /// 取得は 4 並列の専用スレッドで行い、完了したら本棚を読み直して反映する。
    fn refresh_site_covers(&mut self, site_id: &'static str, cx: &mut Context<Self>) {
        if self.cover_refresh_site.is_some() {
            self.show_toast("表紙を取得中です（完了後にもう一度）", cx);
            return;
        }
        let state = AppState::global(cx);
        let db = state.db_pool.clone();
        let thumbnails_dir = state.data_dir.join("thumbnails");
        let booth_session = state.booth_session.lock().clone();
        let tbf_client = state.tbf.clone();
        let targets: Vec<(String, String)> = db::bookshelf::list_all(&db)
            .unwrap_or_default()
            .into_iter()
            .filter(|item| {
                item.site_id == site_id
                    && item
                        .thumbnail_url
                        .as_deref()
                        .is_some_and(|url| !url.is_empty())
            })
            .filter_map(|item| item.thumbnail_url.map(|url| (item.database_id, url)))
            .collect();
        if targets.is_empty() {
            self.show_toast("表紙を持つ本がありません", cx);
            return;
        }
        // このサイトのキャッシュを捨てる（次の取得で作り直す）
        for (database_id, _) in &targets {
            let _ = std::fs::remove_file(cover_cache_path(&thumbnails_dir, site_id, database_id));
            remove_legacy_cover_cache(&thumbnails_dir, site_id, database_id);
        }
        self.cover_refresh_site = Some(site_id);
        self.show_toast(
            format!("表紙を再取得しています（{} 件）", targets.len()),
            cx,
        );

        // 完了後に本棚を読み直すための弱参照（spawn 内では AppState を引けないため先に取る）
        let workspace = AppState::global(cx).workspace.lock().clone();
        let (done_tx, done_rx) = std::sync::mpsc::channel::<(usize, usize)>();
        std::thread::spawn(move || {
            let agent = ureq::AgentBuilder::new()
                .timeout_connect(std::time::Duration::from_secs(5))
                .timeout_read(std::time::Duration::from_secs(15))
                .build();
            let next = std::sync::atomic::AtomicUsize::new(0);
            let ok = std::sync::atomic::AtomicUsize::new(0);
            let failed = std::sync::atomic::AtomicUsize::new(0);
            std::thread::scope(|scope| {
                for _ in 0..4 {
                    let next = &next;
                    let agent = &agent;
                    let targets = &targets;
                    let ok = &ok;
                    let failed = &failed;
                    let thumbnails_dir = &thumbnails_dir;
                    let booth_session = booth_session.as_ref();
                    let tbf_client = &tbf_client;
                    scope.spawn(move || {
                        loop {
                            let index = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            let Some((database_id, url)) = targets.get(index) else {
                                break;
                            };
                            let bytes = fetch_cover_bytes(
                                agent,
                                site_id,
                                database_id,
                                url,
                                booth_session,
                                tbf_client,
                            );
                            match bytes {
                                Some(bytes) => {
                                    write_cover_cache(thumbnails_dir, site_id, database_id, &bytes);
                                    ok.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                }
                                None => {
                                    failed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                }
                            }
                        }
                    });
                }
            });
            let _ = done_tx.send((
                ok.load(std::sync::atomic::Ordering::SeqCst),
                failed.load(std::sync::atomic::Ordering::SeqCst),
            ));
        });

        cx.spawn(async move |this, cx| {
            // 完了通知を待つ（UI スレッドはブロックしない）
            loop {
                match done_rx.try_recv() {
                    Ok((ok, failed)) => {
                        let message = if ok == 0 {
                            format!("表紙を取得できませんでした（{failed} 件）")
                        } else {
                            format!("表紙を更新しました（{ok} 件 / 失敗 {failed} 件）")
                        };
                        // 本棚を読み直して新しいキャッシュを反映する
                        if let Some(ws) = workspace.clone().and_then(|weak| weak.upgrade()) {
                            ws.update(cx, |ws, cx| {
                                ws.bookshelf.update(cx, |view, cx| view.reload(cx));
                            });
                        }
                        let _ = this.update(cx, |view, cx| {
                            view.cover_refresh_site = None;
                            view.show_toast(message, cx);
                        });
                        break;
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(100))
                            .await;
                    }
                    // スレッドが落ちた場合もフラグを戻す
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        let _ = this.update(cx, |view, cx| {
                            view.cover_refresh_site = None;
                            cx.notify();
                        });
                        break;
                    }
                }
            }
        })
        .detach();
    }

    /// 表紙キャッシュが無い本の表紙を thumbnail_url からバックグラウンドで取得する。
    fn fetch_missing_covers(&mut self, cx: &mut Context<Self>) {
        if self.fetch_covers_in_progress {
            return;
        }
        let state = AppState::global(cx);
        let data_dir = state.data_dir.clone();
        // キャッシュが無く、thumbnail_url がある項目を取得対象にする
        let targets: Vec<(String, String, String)> = self
            .hidden_items
            .iter()
            .filter(|item| {
                item.is_hidden == 1
                    && item.thumbnail_url.is_some()
                    && !data_dir
                        .join("thumbnails")
                        .join(format!("{}_{}.png", item.site_id, item.database_id))
                        .exists()
            })
            .map(|item| {
                (
                    item.site_id.clone(),
                    item.database_id.clone(),
                    item.thumbnail_url.clone().unwrap(),
                )
            })
            .collect();
        if targets.is_empty() {
            return;
        }
        self.fetch_covers_in_progress = true;
        let booth_session = state.booth_session.lock().clone();
        let tbf_client = state.tbf.clone();
        let handle = cx.weak_entity();
        cx.spawn(async move |_window, cx| {
            let agent = ureq::Agent::new();
            let thumbnails_dir = data_dir.join("thumbnails");
            let _ = std::fs::create_dir_all(&thumbnails_dir);
            for (site_id, database_id, url) in &targets {
                // 本棚と同じ URL 規則・キャッシュ形式で取得する
                let bytes = fetch_cover_bytes(
                    &agent,
                    site_id,
                    database_id,
                    url,
                    booth_session.as_ref(),
                    &tbf_client,
                );
                if let Some(bytes) = bytes {
                    write_cover_cache(&thumbnails_dir, site_id, database_id, &bytes);
                }
            }
            let _ = handle.update(cx, |this, cx| {
                this.fetch_covers_in_progress = false;
                let state = AppState::global(cx);
                this.hidden_items = db::bookshelf::list_hidden(&state.db_pool).unwrap_or_default();
                cx.notify();
            });
        })
        .detach();
    }

    /// 表紙キャッシュ（thumbnails/{site}_{db}.png）から縮小表示用の画像を読み込む。
    fn load_thumbnail(
        data_dir: &std::path::Path,
        site_id: &str,
        database_id: &str,
    ) -> Option<std::sync::Arc<gpui_kit::RenderImage>> {
        // png 優先（縮小キャッシュ）、なければ元の拡張子を試す
        let base = data_dir
            .join("thumbnails")
            .join(format!("{site_id}_{database_id}"));
        let bytes = ["png", "jpg", "jpeg", "webp"]
            .iter()
            .find_map(|ext| std::fs::read(format!("{}.{ext}", base.display())).ok())?;
        let decoded = image::load_from_memory(&bytes).ok()?;
        let mut rgba = decoded.to_rgba8();
        // GPUI は BGRA を期待する
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        Some(std::sync::Arc::new(gpui_kit::RenderImage::new([
            image::Frame::new(rgba),
        ])))
    }

    /// Web の SettingsGroupCard 相当: 丸角・ボーダー・ヘッダー
    /// （アイコン + タイトル + 説明）+ コンテンツ。
    fn settings_card(
        &self,
        cx: &Context<Self>,
        title: &str,
        description: Option<&str>,
        icon: impl Into<gpui_kit::AnyElement>,
        content: impl IntoElement,
    ) -> gpui_kit::AnyElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted;
        let muted_fg = cx.theme().muted_foreground;
        let card_bg = cx.theme().background;
        let title = title.to_string();
        let description = description.map(String::from);
        let icon: gpui_kit::AnyElement = icon.into();
        div()
            .rounded_xl()
            .border_1()
            .border_color(border)
            .bg(card_bg)
            .shadow_sm()
            .overflow_hidden()
            .child(
                div()
                    .px_5()
                    .py_4()
                    .border_b_1()
                    .border_color(border)
                    .bg(muted.opacity(0.3))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(icon)
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            ),
                    )
                    .child(if let Some(desc) = description {
                        div()
                            .text_xs()
                            .text_color(muted_fg)
                            .child(desc)
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    }),
            )
            .child(content)
            .into_any_element()
    }

    /// 設定画面表示時にログイン状態をチェックする。保存済みトークンがあり、
    /// まだプロフィールが無ければ userinfo から取得して表示に反映する。
    /// テストビルドでは呼ばない（バックグラウンドタスクの完了待ちで
    /// テストのメインループが進まずハングするため）。
    #[cfg(test)]
    pub fn refresh_google_profile(&mut self, _cx: &mut Context<Self>) {}

    #[cfg(not(test))]
    pub fn refresh_google_profile(&mut self, cx: &mut Context<Self>) {
        if self.profile_fetching {
            return;
        }
        let state = AppState::global(cx);
        if state.google_profile.lock().is_some() {
            return;
        }
        let google = state.google.clone();
        // 保存済みトークンが無ければ何もしない（ネットワーク不要）
        {
            let client = google.lock();
            let has = client.as_ref().map(|c| c.has_tokens()).unwrap_or(false);
            // トークンがある時点で「ログイン済み」として扱う（プロフィール取得失敗でも維持）
            *state.google_logged_in.lock() = has;
            if !has {
                return;
            }
        }
        self.profile_fetching = true;
        let handle = cx.entity();
        let task = cx.background_executor().spawn(async move {
            let mut client = google.lock();
            let client = client.as_mut()?;
            // access_token() が期限切れを自動 refresh し、トークンが無ければ
            // NotAuthorized で None になる（未ログイン表示のまま）
            client.profile().ok()
        });
        cx.spawn(async move |_window, cx| {
            let profile = task.await;
            handle.update(cx, |this, cx| {
                this.profile_fetching = false;
                if let Some(profile) = profile {
                    // keyring への保存はブロッキングなので背景で行う（UI を固めない）
                    let to_store = profile.clone();
                    cx.background_spawn(async move {
                        let _ = crate::app_state::store_google_profile_secret(&to_store);
                    })
                    .detach();
                    crate::app_state::set_google_profile(cx, &profile);
                    log::info!("google profile restored from stored tokens");
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// アプリ全体の通知を出す（Workspace が gpui-kit の Notification に流す。既定 5 秒で自動消滅）
    fn show_toast(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        crate::app_state::set_toast(cx, message);
        cx.notify();
    }

    pub fn logout_tbf(&mut self, cx: &mut Context<Self>) {
        let t = std::time::Instant::now();
        let server_ok = {
            let state = AppState::global(cx);
            state.tbf.lock().logout()
        };
        log::info!(
            "logout_tbf: server logout {} ({:?})",
            if server_ok.is_ok() { "ok" } else { "failed" },
            t.elapsed()
        );
        {
            let state = AppState::global(cx);
            state
                .tbf
                .lock()
                .restore_session(thundoku_core::tbf::TbfSession {
                    cookies: Vec::new(),
                    xsrf_raw: String::new(),
                    xsrf_token: String::new(),
                });
            log::info!("logout_tbf: session cleared ({:?})", t.elapsed());
            *state.tbf_logged_in.lock() = false;
            // keyring の削除は数秒かかることがあるためバックグラウンドで行う
            let store = state.secrets.clone();
            cx.background_spawn(async move {
                let _ = store.delete(secrets::USER_TECHBOOKFEST);
            })
            .detach();
        }
        if server_ok.is_ok() {
            self.show_toast(
                "技術書典からログアウトしました（サイト側のセッションも破棄しました）",
                cx,
            );
        } else {
            self.show_toast(
                "技術書典からログアウトしました（サイト側のセッションは残っています）",
                cx,
            );
        }
        log::info!("logout_tbf: done ({:?})", t.elapsed());
        cx.notify();
    }

    pub fn logout_google(&mut self, cx: &mut Context<Self>) {
        let t = std::time::Instant::now();
        {
            let state = AppState::global(cx);
            if let Some(client) = state.google.lock().as_mut() {
                client.logout();
                log::info!("logout_google: client logout ({:?})", t.elapsed());
                crate::app_state::clear_google_profile(cx);
                // ログアウトで未ログインに戻るため、本棚を再フィルタするフラグを立てる。
                AppState::global(cx)
                    .google_logout_done
                    .store(true, std::sync::atomic::Ordering::SeqCst);
                // keyring の削除はバックグラウンドで行う（プロフィールの控えも消す）
                let store = state.secrets.clone();
                cx.background_spawn(async move {
                    let _ = store.delete(secrets::USER_GOOGLE);
                    thundoku_core::google::delete_saved_profile(&store);
                })
                .detach();
            }
        }
        self.show_toast("Google からログアウトしました", cx);
        log::info!("logout_google: done ({:?})", t.elapsed());
        cx.notify();
    }

    pub fn logout_github(&mut self, cx: &mut Context<Self>) {
        let t = std::time::Instant::now();
        // keyring の削除は OS の応答待ち（許可ダイアログ等）で止まり得るので背景で行う。
        // 削除に成功したときだけログイン状態を落とす（失敗時に「ログアウト済み」と見せると、
        // 次回起動で勝手にログインし直って見える）。GitHub は本棚の絞り込みに関係しないので
        // 再取得は不要。
        let handle = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { crate::app_state::delete_github_token_secret() })
                .await;
            let _ = handle.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        crate::app_state::clear_github_session(cx);
                        log::info!("logout_github: token cleared ({:?})", t.elapsed());
                        this.show_toast("GitHub からログアウトしました", cx);
                    }
                    Err(message) => {
                        log::error!("logout_github: {message}");
                        // サイドバーのアカウント欄から呼ばれると設定画面は表示されていないので、
                        // 画面内の赤字だけでは見えない。トーストでも出す。
                        crate::app_state::set_toast_kind(
                            cx,
                            crate::app_state::ToastKind::Error,
                            message.clone(),
                        );
                        this.error = Some(message);
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    pub fn logout_booth(&mut self, cx: &mut Context<Self>) {
        let t = std::time::Instant::now();
        let server_ok = {
            let state = AppState::global(cx);
            let session = state.booth_session.lock().clone();
            if let Some(ref session) = session {
                thundoku_core::booth::BoothClient::new(session).logout()
            } else {
                Ok(())
            }
        };
        log::info!(
            "logout_booth: server logout {} ({:?})",
            if server_ok.is_ok() { "ok" } else { "failed" },
            t.elapsed()
        );
        {
            let state = AppState::global(cx);
            *state.booth_session.lock() = None;
            *state.booth_logged_in.lock() = false;
            // DB（app_settings）からの削除はバックグラウンドで行う
            let db = state.db_pool.clone();
            cx.background_spawn(async move {
                let _ = db::settings::delete(&db, "booth.session");
            })
            .detach();
        }
        log::info!("logout_booth: cleared ({:?})", t.elapsed());
        if server_ok.is_ok() {
            self.show_toast(
                "BOOTH からログアウトしました（サイト側のセッションも破棄しました）",
                cx,
            );
        } else {
            self.show_toast(
                "BOOTH からログアウトしました（サイト側のセッションは残っています）",
                cx,
            );
        }
        cx.notify();
    }

    pub fn logout_fanza(&mut self, cx: &mut Context<Self>) {
        let t = std::time::Instant::now();
        {
            let state = AppState::global(cx);
            *state.fanza_session.lock() = None;
            *state.fanza_logged_in.lock() = false;
            let db = state.db_pool.clone();
            cx.background_spawn(async move {
                let _ = db::settings::delete(&db, "fanza.session");
            })
            .detach();
        }
        log::info!("logout_fanza: cleared ({:?})", t.elapsed());
        self.show_toast("FANZA からログアウトしました", cx);
        cx.notify();
    }

    pub fn logout_dlsite(&mut self, cx: &mut Context<Self>) {
        let t = std::time::Instant::now();
        {
            let state = AppState::global(cx);
            *state.dlsite_session.lock() = None;
            *state.dlsite_logged_in.lock() = false;
            let db = state.db_pool.clone();
            cx.background_spawn(async move {
                let _ = db::settings::delete(&db, "dlsite.session");
            })
            .detach();
        }
        log::info!("logout_dlsite: cleared ({:?})", t.elapsed());
        self.show_toast("DLsite からログアウトしました", cx);
        cx.notify();
    }

    pub fn toggle_drive_sync(&mut self, cx: &mut Context<Self>, enabled: bool) {
        {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            let _ = db::settings::set(
                db,
                "drive.sync.enabled",
                if enabled { "true" } else { "false" },
            );
        }
        self.drive_enabled = enabled;
        cx.notify();
    }

    /// Google の認証が失効していたとき（リフレッシュトークンの失効・取り消し）の後始末。
    ///
    /// 保存済みトークンを破棄し、再ログインの導線を自動で出す。画面に出す日本語の文言を返す
    /// （失効していなければ `None`）。`invalid_grant` の生 JSON は画面に出さない。
    fn handle_google_auth_expiry(
        &mut self,
        message: &str,
        cx: &mut Context<Self>,
    ) -> Option<String> {
        // core の `RefreshTokenRevoked`（失効・取り消し）と `NoRefreshToken` は
        // どちらも再ログインが必要。
        if !is_google_auth_expired(message) {
            return None;
        }
        log::warn!("google auth expired: {message}");
        {
            let state = AppState::global(cx);
            if let Some(client) = state.google.lock().as_mut() {
                client.logout();
            }
            crate::app_state::clear_google_profile(cx);
            state
                .google_logout_done
                .store(true, std::sync::atomic::Ordering::SeqCst);
            // 失効したトークンを keyring に残さない（削除は背景で、UI を固めない）。
            // プロフィールの控えも消す（古い sub で所有者判定を続けない）。
            let store = state.secrets.clone();
            cx.background_spawn(async move {
                let _ = store.delete(secrets::USER_GOOGLE);
                thundoku_core::google::delete_saved_profile(&store);
            })
            .detach();
        }
        // 再ログインの導線を自動で出す。
        //
        // `dispatch_action` で認証モーダルを開くと、WebView 作成時にウィンドウの RefCell を
        // 再入してアプリが固まる（設定画面のログインボタンと同じ理由）。本棚に切り替えて
        // から `Workspace::open_auth` を呼ぶ経路にする。
        cx.defer(move |cx| {
            let ws_weak = AppState::global(cx).workspace.lock().clone();
            if let Some(ws) = ws_weak.and_then(|w| w.upgrade()) {
                ws.update(cx, |ws, cx| {
                    ws.open_auth(cx, AuthProvider::Google);
                });
            }
        });
        Some(GOOGLE_AUTH_EXPIRED_NOTICE.to_string())
    }

    /// Drive 同期（接続済み前提）。エンジンは `drive::sync::sync`。
    pub fn sync_drive_now(&mut self, cx: &mut Context<Self>) {
        self.busy = true;
        self.error = None;
        // busy 状態を即座に UI へ反映（スピナー表示のため）
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
        log::info!("sync_drive_now: start");
        let task: gpui_kit::Task<Result<sync::SyncOutcome, String>> =
            cx.background_executor().spawn(async move {
                let folder_id = {
                    db::settings::get(&db, "drive.sync.folder_id")
                        .ok()
                        .flatten()
                };
                log::info!("sync_drive_now: folder_id={folder_id:?}");
                let mut client = google.lock();
                let client = client
                    .as_mut()
                    .ok_or_else(|| "Google にログインしてください".to_string())?;
                let token = client.access_token().map_err(|e| e.to_string())?;
                let mut drive = DriveClient::new(Box::new(UreqTransport::new()), token);
                log::info!("sync_drive_now: token ok, drive client ready");
                let folder_id = match folder_id {
                    Some(id) => id,
                    None => {
                        log::info!("sync_drive_now: creating folder");
                        let id = drive
                            .create_folder("thundoku-shelf")
                            .map_err(|e| e.to_string())?;
                        let _ = db::settings::set(&db, "drive.sync.folder_id", &id);
                        id
                    }
                };
                log::info!("sync_drive_now: running sync engine");
                // メインの DB プールをそのまま使う（WAL により同期タスクと並行可能）
                let outcome = sync::sync(sync::SyncRequest {
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
                log::info!(
                    "sync_drive_now: done dl={} ul={} skip={} conflicts={} db_backup={}",
                    outcome.downloaded.len(),
                    outcome.uploaded.len(),
                    outcome.skipped.len(),
                    outcome.conflicts.len(),
                    outcome.database_backed_up
                );
                Ok(outcome)
            });
        cx.spawn(async move |_window, cx| {
            let result = task.await;
            // 同期が高速に終わってもスピナーが見えるよう最短表示時間を確保する
            cx.background_executor()
                .timer(std::time::Duration::from_millis(600))
                .await;
            handle.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(outcome) => {
                        // 最終同期日時・ファイル数・容量を保存（設定画面に表示する）
                        {
                            let state = AppState::global(cx);
                            let db = &state.db_pool;
                            let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
                            let _ = db::settings::set(db, "drive.last_sync_at", &now);
                            let _ = db::settings::set(
                                db,
                                "drive.file_count",
                                &outcome.file_count.to_string(),
                            );
                            let _ = db::settings::set(
                                db,
                                "drive.total_bytes",
                                &outcome.total_bytes.to_string(),
                            );
                        }
                        // 表示に使う値を読み直す（`render` では DB を引かない）
                        this.reload(cx);
                        this.show_toast(
                            format!(
                                "同期完了（DL {} / UL {} / スキップ {} / 競合 {}）{}",
                                outcome.downloaded.len(),
                                outcome.uploaded.len(),
                                outcome.skipped.len(),
                                outcome.conflicts.len(),
                                if outcome.database_backed_up {
                                    " / DB バックアップ"
                                } else {
                                    ""
                                }
                            ),
                            cx,
                        );
                    }
                    Err(message) => {
                        log::error!("sync_drive_now failed: {message}");
                        if let Some(notice) = this.handle_google_auth_expiry(&message, cx) {
                            // トークン失効。生の応答ではなく日本語の案内を出す
                            this.error = Some(notice);
                        } else {
                            this.error = Some(message.clone());
                            if message.contains("identity required") {
                                // 暗号化 pack の復号には Google ログインが必要。
                                // 認証モーダルは `dispatch_action` ではなく `Workspace::open_auth`
                                // 経由で開く（WebView 作成時の RefCell 再入で固まるのを避ける）。
                                cx.defer(move |cx| {
                                    let ws_weak = AppState::global(cx).workspace.lock().clone();
                                    if let Some(ws) = ws_weak.and_then(|w| w.upgrade()) {
                                        ws.update(cx, |ws, cx| {
                                            ws.open_auth(cx, AuthProvider::Google);
                                        });
                                    }
                                });
                            }
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// フォルダ選択ダイアログで新しいデータ保存先を選ぶ。
    pub fn pick_data_dir(&mut self, cx: &mut Context<Self>) {
        let current = AppState::global(cx).data_dir.clone();
        if let Some(path) = rfd::FileDialog::new()
            .set_title("データ保存先を選択")
            .set_directory(&current)
            .pick_folder()
        {
            // 現在のデータ保存先と同じなら何もしない
            if path == current {
                return;
            }
            self.pending_data_dir = Some(path);
            self.confirm_data_dir = true;
            cx.notify();
        }
    }

    /// 移動確認で「OK」: 既存ファイルを新しい保存先に移動し、保存先を更新する。
    pub fn confirm_data_dir_change(&mut self, cx: &mut Context<Self>) {
        let Some(new_dir) = self.pending_data_dir.take() else {
            return;
        };
        self.confirm_data_dir = false;
        cx.notify();
        let handle = cx.entity();
        let state = AppState::global(cx);
        let current_dir = state.data_dir.clone();
        // 新しい保存先に移動する（packs / thumbnails / downloads / DB）
        let (db_path, packs, thumbnails, downloads) = (
            current_dir.join("thundoku-shelf.db"),
            current_dir.join("packs"),
            current_dir.join("thumbnails"),
            current_dir.join("downloads"),
        );
        let (ndb, npacks, nthumbs, ndl) = (
            new_dir.join("thundoku-shelf.db"),
            new_dir.join("packs"),
            new_dir.join("thumbnails"),
            new_dir.join("downloads"),
        );
        let new_dir2 = new_dir.clone();
        let task: gpui_kit::Task<(PathBuf, Vec<String>)> =
            cx.background_executor().spawn(async move {
                // 新規ディレクトリを作成
                for d in [&ndb, &npacks, &nthumbs, &ndl] {
                    if let Some(parent) = d.parent() {
                        std::fs::create_dir_all(parent).ok();
                    }
                }
                // 既存ファイル/ディレクトリを移動。DB（+WAL/SHM）と各サブディレクトリ。
                let mut moved = Vec::new();
                // 移動（rename）は同一ボリューム間でしか動かないため、
                // 別ドライブ（C: → D: など）では コピー → 削除 でフォールバックする。
                let move_item =
                    |from: &std::path::Path, to: &std::path::Path, moved: &mut Vec<String>| {
                        if from.exists() && from != to && !to.exists() {
                            let ok = std::fs::rename(from, to).is_ok() || {
                                // rename 失敗（別ボリューム等）は コピー → 削除 で試す
                                let copy_ok = if from.is_dir() {
                                    copy_dir_recursive(from, to).is_ok()
                                } else {
                                    std::fs::copy(from, to).map(|_| ()).is_ok()
                                };
                                if copy_ok {
                                    std::fs::remove_dir_all(from)
                                        .or_else(|_| std::fs::remove_file(from))
                                        .ok();
                                }
                                copy_ok
                            };
                            if ok && let Some(name) = from.file_name() {
                                moved.push(name.to_string_lossy().to_string());
                            }
                        }
                    };
                for (from, to) in [
                    (&db_path, &ndb),
                    (
                        &db_path.with_extension("db-shm"),
                        &ndb.with_extension("db-shm"),
                    ),
                    (
                        &db_path.with_extension("db-wal"),
                        &ndb.with_extension("db-wal"),
                    ),
                    (&packs, &npacks),
                    (&thumbnails, &nthumbs),
                    (&downloads, &ndl),
                ] {
                    move_item(from, to, &mut moved);
                }
                (new_dir2, moved)
            });
        cx.spawn(async move |_window, cx| {
            let (new_dir, moved) = task.await;
            handle.update(cx, |this, cx| {
                if moved.is_empty() {
                    this.show_toast(
                        "ファイルを移動できませんでした。保存先は変更されていません。".to_string(),
                        cx,
                    );
                } else {
                    crate::app_state::save_data_path(&new_dir);
                    this.show_toast(
                        format!(
                            "保存先を変更しました（移動 {} 件）。反映のためアプリを再起動してください。\n{}",
                            moved.len(),
                            new_dir.display()
                        ),
                        cx,
                    );
                }
            });
        })
        .detach();
    }

    /// 移動確認で「キャンセル」: 変更を破棄する。
    pub fn cancel_data_dir_change(&mut self, cx: &mut Context<Self>) {
        self.pending_data_dir = None;
        self.confirm_data_dir = false;
        cx.notify();
    }

    pub fn delete_all_data(&mut self, cx: &mut Context<Self>) {
        {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            for table in [
                "product_sample_pages",
                "token_analysis",
                "document_text",
                "document_images",
                "imported_documents",
                "book_tags",
                "reading_progress",
                "books",
                "bookshelf_items",
                "book_first_events",
                "checked_items",
                "tbf_events",
                "drive_sync_state",
                "favorite_tags",
                "zenn_tag_metadata",
            ] {
                let pool = db.clone();
                let _ = thundoku_core::db::block_on(async {
                    sqlx::query(&format!("DELETE FROM {table}"))
                        .execute(&pool)
                        .await
                });
            }
        }
        {
            let state = AppState::global(cx);
            for dir in ["packs", "thumbnails", "downloads"] {
                let path = state.data_dir.join(dir);
                let _ = std::fs::remove_dir_all(&path);
                let _ = std::fs::create_dir_all(&path);
            }
        }
        self.confirm_delete = false;
        self.storage_bytes = {
            let state = AppState::global(cx);
            dir_size(&state.data_dir)
        };
        // 冊数・非表示リストも消えているので読み直す（`render` では DB を引かない）
        self.reload(cx);
        self.show_toast("ローカルデータをすべて削除しました", cx);
        cx.notify();
    }
    /// ビューアの共通設定カード（サイトに依らない入力の割り当て）。
    ///
    /// ホイール方向の既定は「下スクロールで次へ」。Windows はホイールを前方へ回す
    /// （`WM_MOUSEWHEEL` の +120）が下・次へ、macOS は AppKit がナチュラルスクロール設定を
    /// 反映した delta を渡すので、どちらも「ユーザーにとっての下」がそのまま次へになる。
    fn global_viewer_settings_card(
        &self,
        cx: &Context<Self>,
        wheel_direction: String,
    ) -> impl IntoElement + 'static {
        let handle = cx.weak_entity();
        let primary = cx.theme().primary;
        let border = cx.theme().border;
        let muted_fg = cx.theme().muted_foreground;
        self.settings_card(
            cx,
            "ビューア共通設定",
            Some("サイトに依らないビューアの操作"),
            Icon::new(IconName::BookOpen)
                .size(px(16.0))
                .text_color(muted_fg),
            div().p_5().flex().flex_col().gap_4().child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child("ホイール 1 ノッチで 1 ページ送る向き"),
                    )
                    .child(
                        div().flex().flex_row().gap_4().children(
                            [
                                (WHEEL_DIRECTION_DEFAULT, "下スクロールで次へ"),
                                (WHEEL_DIRECTION_UP, "上スクロールで次へ"),
                            ]
                            .into_iter()
                            .map(|(value, label)| {
                                let selected = wheel_direction == value;
                                let handle = handle.clone();
                                let value = value.to_string();
                                div()
                                    .id(SharedString::from(format!("wheel-direction-{value}")))
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_1p5()
                                    .text_sm()
                                    .cursor_pointer()
                                    .on_click(move |_, _window, cx| {
                                        handle
                                            .update(cx, |this, cx| {
                                                this.set_wheel_direction(cx, &value);
                                            })
                                            .ok();
                                    })
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .w(px(16.0))
                                            .h(px(16.0))
                                            .rounded_full()
                                            .border_1()
                                            .border_color(if selected { primary } else { border })
                                            .child(if selected {
                                                div()
                                                    .w(px(8.0))
                                                    .h(px(8.0))
                                                    .rounded_full()
                                                    .bg(primary)
                                            } else {
                                                div()
                                            }),
                                    )
                                    .child(label)
                            }),
                        ),
                    ),
            ),
        )
    }

    /// サイト別のビューアー設定カード（表示モード + ページめくり）。
    fn site_viewer_settings_card(
        &self,
        cx: &Context<Self>,
        site_id: &'static str,
        title: &str,
        description: &str,
        viewer_mode: String,
        page_turn: String,
    ) -> impl IntoElement + 'static {
        let handle = cx.weak_entity();
        let primary = cx.theme().primary;
        let border = cx.theme().border;
        let muted_fg = cx.theme().muted_foreground;
        self.settings_card(
            cx,
            title,
            Some(description),
            Icon::new(IconName::BookOpen)
                .size(px(16.0))
                .text_color(muted_fg),
            div()
                .p_5()
                .flex()
                .flex_col()
                .gap_4()
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child("表示モード"),
                        )
                        .child(
                            div().flex().flex_row().gap_4().children(
                                [
                                    ("single", "単ページ"),
                                    ("spread", "見開き"),
                                    ("scroll", "スクロール"),
                                ]
                                .into_iter()
                                .map(|(value, label)| {
                                    let selected = viewer_mode == value;
                                    let handle = handle.clone();
                                    let value = value.to_string();
                                    div()
                                        .id(SharedString::from(format!("viewer-mode-{value}")))
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_1p5()
                                        .text_sm()
                                        .cursor_pointer()
                                        .on_click(move |_, _window, cx| {
                                            handle
                                                .update(cx, |this, cx| {
                                                    if site_id == "booth" {
                                                        this.set_booth_viewer_mode(cx, &value);
                                                    } else if site_id == "fanza" {
                                                        this.set_fanza_viewer_mode(cx, &value);
                                                    } else if site_id == "dlsite" {
                                                        this.set_dlsite_viewer_mode(cx, &value);
                                                    } else {
                                                        this.set_viewer_mode(cx, &value);
                                                    }
                                                })
                                                .ok();
                                        })
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .w(px(16.0))
                                                .h(px(16.0))
                                                .rounded_full()
                                                .border_1()
                                                .border_color(if selected {
                                                    primary
                                                } else {
                                                    border
                                                })
                                                .child(if selected {
                                                    div()
                                                        .w(px(8.0))
                                                        .h(px(8.0))
                                                        .rounded_full()
                                                        .bg(primary)
                                                } else {
                                                    div()
                                                }),
                                        )
                                        .child(label)
                                }),
                            ),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child("ページめくり"),
                        )
                        .child(
                            div().flex().flex_row().gap_4().children(
                                [("right-to-left", "右→左"), ("left-to-right", "左→右")]
                                    .into_iter()
                                    .map(|(value, label)| {
                                        let selected = page_turn == value;
                                        let handle = handle.clone();
                                        let value = value.to_string();
                                        div()
                                            .id(SharedString::from(format!("page-turn-{value}")))
                                            .flex()
                                            .flex_row()
                                            .items_center()
                                            .gap_1p5()
                                            .text_sm()
                                            .cursor_pointer()
                                            .on_click(move |_, _window, cx| {
                                                handle
                                                    .update(cx, |this, cx| {
                                                        if site_id == "booth" {
                                                            this.set_booth_page_turn(cx, &value);
                                                        } else if site_id == "fanza" {
                                                            this.set_fanza_page_turn(cx, &value);
                                                        } else if site_id == "dlsite" {
                                                            this.set_dlsite_page_turn(cx, &value);
                                                        } else {
                                                            this.set_page_turn(cx, &value);
                                                        }
                                                    })
                                                    .ok();
                                            })
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .w(px(16.0))
                                                    .h(px(16.0))
                                                    .rounded_full()
                                                    .border_1()
                                                    .border_color(if selected {
                                                        primary
                                                    } else {
                                                        border
                                                    })
                                                    .child(if selected {
                                                        div()
                                                            .w(px(8.0))
                                                            .h(px(8.0))
                                                            .rounded_full()
                                                            .bg(primary)
                                                    } else {
                                                        div()
                                                    }),
                                            )
                                            .child(label)
                                    }),
                            ),
                        ),
                )
                // 表紙更新（このサイトの表紙キャッシュを捨てて取り直す）
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(FontWeight::MEDIUM)
                                        .child("表紙"),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(muted_fg)
                                        .child("このサイトの表紙を取得し直します"),
                                ),
                        )
                        .child({
                            let handle = handle.clone();
                            Button::new(SharedString::from(format!(
                                "site-refresh-covers-{site_id}"
                            )))
                            .debug_selector(move || format!("site-refresh-covers-{site_id}"))
                            .cursor_pointer()
                            .label("表紙更新")
                            .outline()
                            .cursor_pointer()
                            .on_click(move |_, _window, cx| {
                                handle
                                    .update(cx, |this, cx| {
                                        this.refresh_site_covers(site_id, cx);
                                    })
                                    .ok();
                            })
                        }),
                ),
        )
    }
    /// アカウント欄（Google / GitHub / 技術書典 / BOOTH / FANZA / DLsite のログイン状態）。
    ///
    /// `render` のスタックフレームを抑えるため別メソッドに切り出している。
    #[allow(clippy::too_many_arguments)]
    fn account_settings_card(
        &self,
        cx: &Context<Self>,
        google_profile: Option<thundoku_core::google::GoogleProfile>,
        google_logged_in: bool,
        github_profile: Option<thundoku_core::github::GithubUser>,
        github_logged_in: bool,
        tbf_logged_in: bool,
        booth_logged_in: bool,
        fanza_logged_in: bool,
        dlsite_logged_in: bool,
    ) -> gpui_kit::AnyElement {
        let handle = cx.weak_entity();
        let muted_fg = cx.theme().muted_foreground;
        // client_id が埋め込まれていないビルドでは GitHub ログインを開始できない
        let github_login_enabled = !crate::app_state::default_github_client_id().is_empty();
        self.settings_card(
            cx,
            "アカウント",
            Some("技術書典・Google・GitHub・BOOTH・FANZA・DLsite のログイン状態"),
            Icon::new(IconName::CircleUser)
                .size(px(16.0))
                .text_color(muted_fg),
            div()
                .p_5()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .text_sm()
                                .child(div().font_weight(FontWeight::MEDIUM).child("Google"))
                                .child(div().text_xs().text_color(muted_fg).child(
                                    match &google_profile {
                                        Some(profile) if !profile.email.is_empty() => {
                                            profile.email.clone()
                                        }
                                        Some(profile) => profile.name.clone(),
                                        None if google_logged_in => "ログイン済み".to_string(),
                                        None => "未ログイン".to_string(),
                                    },
                                )),
                        )
                        .child(if google_logged_in {
                            Button::new("logout-google")
                                .cursor_pointer()
                                .label("ログアウト")
                                .cursor_pointer()
                                .on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        handle.update(cx, |this, cx| this.logout_google(cx)).ok();
                                    }
                                })
                                .into_any_element()
                        } else {
                            Button::new("login-google")
                                .cursor_pointer()
                                .label("ログイン")
                                .cursor_pointer()
                                .on_click(|_, _window, cx| {
                                    // 本棚に切り替えてからダイアログ表示（SettingsView を非表示にして
                                    // WebView 作成時の RefCell 競合を回避する）。
                                    cx.defer(move |cx| {
                                        let ws_weak = AppState::global(cx).workspace.lock().clone();
                                        if let Some(ws) = ws_weak.and_then(|w| w.upgrade()) {
                                            ws.update(cx, |ws, cx| {
                                                ws.open_auth(cx, AuthProvider::Google);
                                            });
                                        }
                                    });
                                })
                                .into_any_element()
                        }),
                )
                // GitHub 行（Device Flow。完了は Workspace の監視タスクが拾ってモーダルを閉じる）
                .child(
                    div()
                        .debug_selector(|| "account-row-github".into())
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .text_sm()
                                .child(div().font_weight(FontWeight::MEDIUM).child("GitHub"))
                                .child(div().text_xs().text_color(muted_fg).child(
                                    if !github_login_enabled {
                                        // 未設定のビルドでは Device Flow を開始できない。押せない
                                        // 理由と直し方が見えないと「壊れている」と見える。
                                        "client_id 未設定（ビルド時に THUNDOKU_GITHUB_CLIENT_ID）"
                                            .to_string()
                                    } else {
                                        match &github_profile {
                                            Some(profile) if !profile.login.is_empty() => {
                                                format!("@{}", profile.login)
                                            }
                                            Some(_) => "ログイン済み".to_string(),
                                            None if github_logged_in => "ログイン済み".to_string(),
                                            None => "未ログイン".to_string(),
                                        }
                                    },
                                )),
                        )
                        .child(if github_logged_in {
                            Button::new("logout-github")
                                .cursor_pointer()
                                .label("ログアウト")
                                .cursor_pointer()
                                .on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        handle.update(cx, |this, cx| this.logout_github(cx)).ok();
                                    }
                                })
                                .into_any_element()
                        } else {
                            Button::new("login-github")
                                .cursor_pointer()
                                .label("ログイン")
                                .cursor_pointer()
                                .disabled(!github_login_enabled)
                                .on_click(|_, _window, cx| {
                                    // 認証モーダル（GitHub は Device Flow）を開く。SettingsView を
                                    // 非表示にしてから開くのは Google と同じ理由（RefCell 競合回避）。
                                    cx.defer(move |cx| {
                                        let ws_weak = AppState::global(cx).workspace.lock().clone();
                                        if let Some(ws) = ws_weak.and_then(|w| w.upgrade()) {
                                            ws.update(cx, |ws, cx| {
                                                ws.open_auth(cx, AuthProvider::Github);
                                            });
                                        }
                                    });
                                })
                                .into_any_element()
                        }),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .text_sm()
                                .child(div().font_weight(FontWeight::MEDIUM).child("技術書典"))
                                .child(div().text_xs().text_color(muted_fg).child(
                                    if tbf_logged_in {
                                        "ログイン済み"
                                    } else {
                                        "未ログイン"
                                    },
                                )),
                        )
                        .child(if tbf_logged_in {
                            Button::new("logout-tbf")
                                .cursor_pointer()
                                .label("ログアウト")
                                .cursor_pointer()
                                .on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        handle.update(cx, |this, cx| this.logout_tbf(cx)).ok();
                                    }
                                })
                                .into_any_element()
                        } else {
                            Button::new("login-tbf")
                                .cursor_pointer()
                                .label("ログイン")
                                .cursor_pointer()
                                .on_click(|_, _window, cx| {
                                    // プロバイダ選択画面を経由せず技術書典のログインへ直接進む
                                    cx.defer(move |cx| {
                                        cx.dispatch_action(&OpenAuthProvider {
                                            provider: AuthProvider::TechBookFest,
                                        })
                                    });
                                })
                                .into_any_element()
                        }),
                )
                // BOOTH 行
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .text_sm()
                                .child(div().font_weight(FontWeight::MEDIUM).child("BOOTH"))
                                .child(div().text_xs().text_color(muted_fg).child(
                                    if booth_logged_in {
                                        "ログイン済み"
                                    } else {
                                        "未ログイン"
                                    },
                                )),
                        )
                        .child(if booth_logged_in {
                            Button::new("logout-booth")
                                .cursor_pointer()
                                .label("ログアウト")
                                .cursor_pointer()
                                .on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        handle.update(cx, |this, cx| this.logout_booth(cx)).ok();
                                    }
                                })
                                .into_any_element()
                        } else {
                            Button::new("login-booth")
                                .cursor_pointer()
                                .label("ログイン")
                                .cursor_pointer()
                                .on_click(|_, _window, cx| {
                                    // アプリ内 WebView で pixiv ログイン（BOOTH）へ直接進む
                                    cx.defer(move |cx| {
                                        cx.dispatch_action(&OpenAuthProvider {
                                            provider: AuthProvider::Booth,
                                        })
                                    });
                                })
                                .into_any_element()
                        }),
                )
                // FANZA 行
                .child(
                    div()
                        .debug_selector(|| "account-row-fanza".into())
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .text_sm()
                                .child(div().font_weight(FontWeight::MEDIUM).child("FANZA"))
                                .child(div().text_xs().text_color(muted_fg).child(
                                    if fanza_logged_in {
                                        "ログイン済み"
                                    } else {
                                        "未ログイン"
                                    },
                                )),
                        )
                        .child(if fanza_logged_in {
                            Button::new("logout-fanza")
                                .cursor_pointer()
                                .label("ログアウト")
                                .cursor_pointer()
                                .on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        handle.update(cx, |this, cx| this.logout_fanza(cx)).ok();
                                    }
                                })
                                .into_any_element()
                        } else {
                            Button::new("login-fanza")
                                .cursor_pointer()
                                .label("ログイン")
                                .cursor_pointer()
                                .on_click(|_, _window, cx| {
                                    // アプリ内 WebView で FANZA のログインへ直接進む
                                    cx.defer(move |cx| {
                                        cx.dispatch_action(&OpenAuthProvider {
                                            provider: AuthProvider::Fanza,
                                        })
                                    });
                                })
                                .into_any_element()
                        }),
                )
                // DLsite 行
                .child(
                    div()
                        .debug_selector(|| "account-row-dlsite".into())
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .text_sm()
                                .child(div().font_weight(FontWeight::MEDIUM).child("DLsite"))
                                .child(div().text_xs().text_color(muted_fg).child(
                                    if dlsite_logged_in {
                                        "ログイン済み"
                                    } else {
                                        "未ログイン"
                                    },
                                )),
                        )
                        .child(if dlsite_logged_in {
                            Button::new("logout-dlsite")
                                .cursor_pointer()
                                .label("ログアウト")
                                .cursor_pointer()
                                .on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        handle.update(cx, |this, cx| this.logout_dlsite(cx)).ok();
                                    }
                                })
                                .into_any_element()
                        } else {
                            Button::new("login-dlsite")
                                .cursor_pointer()
                                .label("ログイン")
                                .cursor_pointer()
                                .on_click(|_, _window, cx| {
                                    // アプリ内 WebView で DLsite のログインへ直接進む
                                    cx.defer(move |cx| {
                                        cx.dispatch_action(&OpenAuthProvider {
                                            provider: AuthProvider::Dlsite,
                                        })
                                    });
                                })
                                .into_any_element()
                        }),
                ),
        )
        .into_any_element()
    }
}

/// ディレクトリを再帰的にコピーする（別ボリュームへの移動フォールバック用）。
fn copy_dir_recursive(from: &std::path::Path, to: &std::path::Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let src = entry.path();
        let dst = to.join(entry.file_name());
        if src.is_dir() {
            copy_dir_recursive(&src, &dst)?;
        } else {
            std::fs::copy(&src, &dst)?;
        }
    }
    Ok(())
}

fn dir_size(path: &std::path::Path) -> u64 {
    let mut total = 0u64;
    if let Ok(entries) = std::fs::read_dir(path) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                total += dir_size(&path);
            } else if let Ok(metadata) = entry.metadata() {
                total += metadata.len();
            }
        }
    }
    total
}

impl Render for SettingsView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // チェックリスト定期取得間隔の編集入力を確保（初回のみ生成・購読）
        self.ensure_poll_interval_input(_window, cx);
        // 表示に使う状態（冊数・非表示リスト・同期情報）は `reload`（画面に入ったとき /
        // データが変わったとき）で読み込んでおく。ここで DB を引くと、スクロールのたびに
        // 描画が走るたびに全書籍ぶんの進捗クエリが走って引っかかる（実測: ホイール 1 ノッチで
        // 約 600 クエリ → 修正後は 0）。
        // 保存済みトークンからログイン状態を復元（設定画面を開いたとき）
        #[cfg(not(test))]
        self.refresh_google_profile(cx);
        let busy = self.busy;
        let error = self.error.clone();
        let tbf_logged_in = *AppState::global(cx).tbf_logged_in.lock();
        let google_profile = AppState::global(cx).google_profile.lock().clone();
        let google_logged_in = *AppState::global(cx).google_logged_in.lock();
        let github_profile = AppState::global(cx).github_profile.lock().clone();
        let github_logged_in = *AppState::global(cx).github_logged_in.lock();
        let booth_logged_in = *AppState::global(cx).booth_logged_in.lock();
        let fanza_logged_in = *AppState::global(cx).fanza_logged_in.lock();
        let dlsite_logged_in = *AppState::global(cx).dlsite_logged_in.lock();
        let data_dir = AppState::global(cx).data_dir.clone();
        let confirm_data_dir = self.confirm_data_dir;
        let confirm_clear_sync = self.confirm_clear_sync;
        let pending_data_dir = self.pending_data_dir.clone();
        let drive_enabled = self.drive_enabled;
        let drive_last_sync = self
            .drive_last_sync
            .clone()
            // 保存は UTC なのでローカル時間（JST 等）で表示する
            .and_then(|v| {
                chrono::NaiveDateTime::parse_from_str(&v, "%Y-%m-%d %H:%M:%S")
                    .map(|naive| {
                        naive
                            .and_utc()
                            .with_timezone(&chrono::Local)
                            .format("%Y-%m-%d %H:%M:%S")
                            .to_string()
                    })
                    .ok()
            });
        let drive_file_count = self.drive_file_count;
        let drive_total_bytes = self.drive_total_bytes;
        let storage_bytes = self.storage_bytes;
        let confirm_delete = self.confirm_delete;
        let tbf_viewer_mode = self.tbf_viewer_mode.clone();
        let tbf_page_turn = self.tbf_page_turn.clone();
        let booth_viewer_mode = self.booth_viewer_mode.clone();
        let booth_page_turn = self.booth_page_turn.clone();
        let fanza_viewer_mode = self.fanza_viewer_mode.clone();
        let fanza_page_turn = self.fanza_page_turn.clone();
        let dlsite_viewer_mode = self.dlsite_viewer_mode.clone();
        let dlsite_page_turn = self.dlsite_page_turn.clone();
        let status_counts = self.status_counts;
        let last_synced_at = self.api_last_sync_at.clone();
        let handle = cx.entity();
        let border = cx.theme().border;
        let muted_fg = cx.theme().muted_foreground;

        // Web の SettingsGroupCard 相当のカード
        let global_viewer_settings =
            self.global_viewer_settings_card(cx, self.viewer_wheel_direction.clone());
        let viewer_settings = self.site_viewer_settings_card(
            cx,
            "techbookfest",
            "技術書典サイト設定",
            "技術書典サイトの設定",
            tbf_viewer_mode,
            tbf_page_turn,
        );
        let booth_viewer_settings = self.site_viewer_settings_card(
            cx,
            "booth",
            "BOOTH サイト設定",
            "BOOTH サイトの設定",
            booth_viewer_mode,
            booth_page_turn,
        );
        let fanza_viewer_settings = self.site_viewer_settings_card(
            cx,
            "fanza",
            "FANZA同人サイト設定",
            "FANZA同人サイトの設定",
            fanza_viewer_mode,
            fanza_page_turn,
        );
        let dlsite_viewer_settings = self.site_viewer_settings_card(
            cx,
            "dlsite",
            "DLsite サイト設定",
            "DLsite サイトの設定",
            dlsite_viewer_mode,
            dlsite_page_turn,
        );

        let storage_settings = self.settings_card(
            cx,
            "ストレージ管理",
            Some("ダウンロード済みの本とストレージ使用状況"),
            Icon::new(AppIcon::HardDrive)
                .size(px(16.0))
                .text_color(muted_fg),
            div()
                .p_5()
                .flex()
                .flex_col()
                .gap_3()
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .items_center()
                        .justify_between()
                        .gap_3()
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .text_sm()
                                .font_weight(FontWeight::MEDIUM)
                                .child("ストレージ使用状況"),
                        )
                        .child(
                            Button::new("delete-all-data")
                                .cursor_pointer()
                                .label("ローカルデータを削除")
                                .danger()
                                .disabled(busy)
                                .cursor_pointer()
                                .on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        handle.update(cx, |this, cx| {
                                            this.confirm_delete = true;
                                            cx.notify();
                                        });
                                    }
                                }),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .flex_row()
                        .justify_between()
                        .text_sm()
                        .child(div().text_color(muted_fg).child("使用済み"))
                        .child(
                            div()
                                .font_weight(FontWeight::MEDIUM)
                                .child(format!("{} MB", storage_bytes / 1024 / 1024)),
                        ),
                )
                .child(
                    div()
                        .border_t_1()
                        .border_color(border)
                        .pt_2()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(
                            div()
                                .text_sm()
                                .text_color(muted_fg)
                                .child(format!("総本数: {}冊", self.book_count)),
                        ),
                ),
        );

        let drive_settings =
            self.settings_card(
                cx,
                "Google Drive バックアップ",
                Some("クラウドバックアップ設定"),
                Icon::new(AppIcon::Cloud)
                    .size(px(16.0))
                    .text_color(muted_fg),
                div()
                    .p_5()
                    .flex()
                    .flex_col()
                            .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .gap_3()
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child("Google バックアップ"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(muted_fg)
                                            .child("ON にすると、本棚・進捗のデータを Google Drive に自動バックアップします。"),
                                    ),
                            )
                             .child(
                                 Switch::new("drive-sync-toggle")
                                    .checked(drive_enabled)
                                    .cursor_pointer()
                                    .on_click({
                                        let handle = handle.clone();
                                        move |checked, _window, cx| {
                                            handle.update(cx, |this, cx| {
                                                this.toggle_drive_sync(cx, *checked)
                                            });
                                        }
                                    }),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(
                                Button::new("drive-sync-now")
                                    .cursor_pointer()
                                    .icon(Icon::new(AppIcon::RefreshCw).size(px(14.0)))
                                    .loading_icon(Icon::new(IconName::Loader).size(px(14.0)))
                                    .label(if !google_logged_in {
                                        "Google未ログイン"
                                    } else if busy {
                                        "同期中..."
                                    } else {
                                        "今すぐ同期"
                                    })
                                    .loading(busy)
                                    .disabled(!google_logged_in || busy)
                                    .cursor_pointer()
                                    .on_click({
                                        let handle = handle.clone();
                                        move |_, _window, cx| {
                                            handle.update(cx, |this, cx| this.sync_drive_now(cx));
                                        }
                                    }),
                            )
                            .child(
                                // データ保存先の変更
                                Button::new("change-data-dir")
                                    .cursor_pointer()
                                    .icon(Icon::new(AppIcon::HardDrive).size(px(14.0)))
                                    .label("保存先変更")
                                    .cursor_pointer()
                                    .on_click({
                                        let handle = handle.clone();
                                        move |_, _window, cx| {
                                            handle.update(cx, |this, cx| {
                                                this.pick_data_dir(cx);
                                            });
                                        }
                                    }),
                            )
                    )
                    // 現在の保存先（ローカルパス）
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_between()
                            .text_xs()
                            .child(div().text_color(muted_fg).child("保存先"))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(muted_fg)
                                    .child(data_dir.display().to_string()),
                            ),
                    )

                    // 最終同期日時
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_between()
                            .text_sm()
                            .child(div().text_color(muted_fg).child("最終同期"))
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(drive_last_sync.unwrap_or_else(|| "未同期".to_string())),
                            ),
                    )
                    // ファイル数・容量
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_between()
                            .text_sm()
                            .child(div().text_color(muted_fg).child("バックアップ"))
                            .child(div().font_weight(FontWeight::MEDIUM).child(
                                if drive_file_count == 0 && drive_total_bytes == 0 {
                                    "なし".to_string()
                                } else {
                                    format!(
                                        "{} ファイル / {} MB",
                                        drive_file_count,
                                        drive_total_bytes / 1024 / 1024
                                    )
                                },
                            )),
                    )
                    // 同期情報をクリア
                    .child(
                        div().border_t_1().border_color(border).pt_2()
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(muted_fg)
                                        .mb_1()
                                        .child("クリックすると、Google Drive との同期状態（ファイル数・容量・最終同期日時）をリセットします。本棚の本データや進捗は削除されません。"),
                                )
                                .child(
                            Button::new("drive-clear-sync")
                                .cursor_pointer()
                                .label("同期情報をクリア")
                                .danger()
                                .cursor_pointer()
                                .on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        handle.update(cx, |this, cx| {
                                            this.confirm_clear_sync = true;
                                            cx.notify();
                                        });
                                    }
                                }),
                        ),
                    ),
            );

        let poll_state = self
            .poll_interval_input
            .clone()
            .expect("poll input ensured");
        let checklist_poll_settings =
            self.settings_card(
                cx,
                "チェックリストの定期取得",
                Some("イベント画面で、技術書典サイトからのサークルチェック情報を取得する間隔を設定します"),
                Icon::new(AppIcon::RefreshCw)
                    .size(px(16.0))
                    .text_color(muted_fg),
                div()
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child("定期取得間隔（分）"),
                                    )
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(muted_fg)
                                            .child("1, 5, 10, 15, 30, 60 のいずれか。ON にしたイベントをこの間隔で自動同期します。"),
                                    ),
                            )
                            .child(
                                div()
                                    .w(px(120.0))
                                    .child(Input::new(&poll_state).cursor_text()),
                            ),
                    ),
            );

        let db_settings =
            self.settings_card(
                cx,
                "データベース情報",
                None,
                Icon::new(AppIcon::Database)
                    .size(px(16.0))
                    .text_color(muted_fg),
                div()
                    .p_5()
                    .flex()
                    .flex_col()
                    .gap_3()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .justify_between()
                            .text_sm()
                            .child(div().text_color(muted_fg).child("最終同期日時"))
                            .child(
                                div()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(last_synced_at.unwrap_or_else(|| "未同期".to_string())),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .text_sm()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .justify_between()
                                    .child(div().text_color(muted_fg).child("未読"))
                                    .child(
                                        div()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(format!("{}冊", status_counts.0)),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .justify_between()
                                    .child(div().text_color(muted_fg).child("読んでいる途中"))
                                    .child(
                                        div()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(format!("{}冊", status_counts.1)),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .justify_between()
                                    .child(div().text_color(muted_fg).child("読了"))
                                    .child(
                                        div()
                                            .font_weight(FontWeight::MEDIUM)
                                            .child(format!("{}冊", status_counts.2)),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .border_t_1()
                            .border_color(border)
                            .pt_3()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_between()
                            .gap_4()
                            .child(div().text_sm().text_color(muted_fg).child(
                                "ローカルに保存されたすべてのデータを削除します（復元不可）",
                            ))
                            .child(
                                Button::new("reset-local-data")
                                    .cursor_pointer()
                                    .label("データ初期化")
                                    .outline()
                                    .cursor_pointer()
                                    .on_click({
                                        let handle = handle.clone();
                                        move |_, _window, cx| {
                                            handle.update(cx, |this, cx| {
                                                this.confirm_delete = true;
                                                cx.notify();
                                            });
                                        }
                                    }),
                            ),
                    ),
            );

        let account_settings = self.account_settings_card(
            cx,
            google_profile,
            google_logged_in,
            github_profile,
            github_logged_in,
            tbf_logged_in,
            booth_logged_in,
            fanza_logged_in,
            dlsite_logged_in,
        );
        div()
            .size_full()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(
                div()
                    .mx_auto()
                    .w(px(840.0))
                    .py_8()
                    .px_4()
                    .flex()
                    .flex_col()
                    .gap_6()

                    // ヘッダー（Web の h1 + サブテキスト相当）
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_2xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("設定"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(muted_fg)
                                    .child("アプリケーション設定"),
                            ),
                    )
                    .child(account_settings)
                    .child(global_viewer_settings)
                    .child(viewer_settings)
                    .child(booth_viewer_settings)
                    .child(fanza_viewer_settings)
                    .child(dlsite_viewer_settings)
                    // データ設定（Web のセクション見出し）
                    .child(
                        div()
                            .px_1()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .text_color(muted_fg)
                            .child("データ設定"),
                    )
                    .child(storage_settings)
                    .child(drive_settings)
                    .child(checklist_poll_settings)
                    .child(db_settings)
                    // 外観
                    .child(
                        self.settings_card(
                            cx,
                            "外観",
                            Some("テーマ設定"),
                            Icon::new(IconName::Palette)
                                .size(px(16.0))
                                .text_color(muted_fg),
                            div()
                                .p_5()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_2()
                                .child(
                                    Button::new("theme-light")
                                        .cursor_pointer()
                                        .label("ライト")
                                        .on_click(|_, _window, cx| {
                                            cx.defer(move |cx| {
                                                let ws_weak = AppState::global(cx).workspace.lock().clone();
                                                if let Some(ws) = ws_weak.and_then(|w| w.upgrade()) {
                                                    ws.update(cx, |ws, cx| ws.set_theme("light", cx));
                                                }
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("theme-dark")
                                        .cursor_pointer()
                                        .label("ダーク")
                                        .on_click(|_, _window, cx| {
                                            cx.defer(move |cx| {
                                                let ws_weak = AppState::global(cx).workspace.lock().clone();
                                                if let Some(ws) = ws_weak.and_then(|w| w.upgrade()) {
                                                    ws.update(cx, |ws, cx| ws.set_theme("dark", cx));
                                                }
                                            });
                                        }),
                                )
                                .child(
                                    Button::new("theme-system")
                                        .cursor_pointer()
                                        .label("システム")
                                        .on_click(|_, _window, cx| {
                                            cx.defer(move |cx| {
                                                let ws_weak = AppState::global(cx).workspace.lock().clone();
                                                if let Some(ws) = ws_weak.and_then(|w| w.upgrade()) {
                                                    ws.update(cx, |ws, cx| ws.set_theme("system", cx));
                                                }
                                            });
                                        }),
                                ),
                        ),
                    )
                    // 非表示にした本（表示解除）
                    .child(
                        self.settings_card(
                            cx,
                            "非表示にした本",
                            Some("非表示にした本を復元できます"),
                            Icon::new(IconName::EyeOff)
                                .size(px(16.0))
                                .text_color(muted_fg),
                            div()
                                .p_5()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        // 5 件分の高さ + 超過分はスクロール
                                        .h(px(250.0))
                                        .overflow_y_scrollbar()
                                        .flex()
                                        .flex_col()
                                        .gap_1()
                                        .children(
                                            self.hidden_items.iter().map(|item| {
                                                let handle = handle.clone();
                                                let site_id = item.site_id.clone();
                                                let database_id = item.database_id.clone();
                                                let title = item.title.clone();
                                                let hidden_at =
                                                    item.hidden_at.clone().unwrap_or_default();
                                                // 表紙（キャッシュ）を読み込む
                                                let thumb = {
                                                    let state = AppState::global(cx);
                                                    Self::load_thumbnail(
                                                        &state.data_dir,
                                                        &site_id,
                                                        &database_id,
                                                    )
                                                };
                                                div()
                                                    .flex()
                                                    .flex_row()
                                                    .items_center()
                                                    .gap_2()
                                                    .px_2()
                                                    .py_1()
                                                    .rounded_md()
                                                    .hover(|style| {
                                                        style.bg(cx.theme().secondary)
                                                    })
                                                    .child(if let Some(thumb) = thumb {
                                                        img(thumb)
                                                            .w(px(56.0))
                                                            .h(px(78.0))
                                                            .object_fit(gpui_kit::ObjectFit::Cover)
                                                            .into_any_element()
                                                    } else {
                                                        div()
                                                            .w(px(56.0))
                                                            .h(px(78.0))
                                                            .bg(cx.theme().muted)
                                                            .flex()
                                                            .items_center()
                                                            .justify_center()
                                                            .child(
                                                                Icon::new(crate::icons::AppIcon::BookMarked)
                                                                    .size(px(28.0))
                                                                    .text_color(muted_fg),
                                                            )
                                                            .into_any_element()
                                                    })
                                                    .child(
                                                        div()
                                                            .flex_1()
                                                            .flex()
                                                            .flex_col()
                                                            .gap_0p5()
                                                            .child(
                                                                div()
                                                                    .text_sm()
                                                                    .font_weight(FontWeight::MEDIUM)
                                                                    .truncate()
                                                                    .child(title),
                                                            )
                                                            .child(
                                                                div()
                                                                    .text_xs()
                                                                    .text_color(muted_fg)
                                                                    .child(hidden_at),
                                                            ),
                                                    )
                                                    .child(
                                                        Button::new(format!(
                                                            "unhide-{site_id}-{database_id}"
                                                        ))
                                                        .cursor_pointer()
                                                        .label("非表示解除")
                                                        .on_click({
                                                            let handle = handle.clone();
                                                            move |_, _window, cx| {
                                                                handle.update(cx, |this, cx| {
                                                                    let state =
                                                                        AppState::global(cx);
                                                                    let _ = db::bookshelf::set_hidden(
                                                                        &state.db_pool,
                                                                        &site_id,
                                                                        &database_id,
                                                                        false,
                                                                    );
                                                                    this.hidden_items = db::bookshelf::list_hidden(
                                                                        &state.db_pool,
                                                                    )
                                                                    .unwrap_or_default();
                                                                    this.show_toast("非表示を解除しました", cx);
                                                                    // 本棚に戻った時に反映されるようフラグを立てる
                                                                    *AppState::global(cx)
                                                                        .bookshelf_invalidated
                                                                        .lock() = true;
                                                                });
                                                            }
                                                        }),
                                                    )
                                                    .into_any_element()
                                            }),
                                        ),
                                ),
                        ),
                    )
                    .child(if let Some(message) = error {
                        div()
                            .text_sm()
                            .text_color(gpui_kit::red())
                            .child(message)
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    })
                    .child(if confirm_delete {
                        let content = dialog_surface(cx)
                            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("ローカルデータの全削除"))
                            .child(div().text_sm().child(
                                "削除されるもの（この操作は取り消せません）:",
                            ))
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .child(div().text_sm().child("・本棚の本（書籍データ・一覧）"))
                                    .child(div().text_sm().child("・読書進捗・閲覧履歴"))
                                    .child(div().text_sm().child("・タグ・お気に入りタグ"))
                                    .child(div().text_sm().child("・表紙サムネイル・ダウンロード済みデータ"))
                                    .child(div().text_sm().child("・チェックリスト・イベント・試し読み"))
                                    .child(div().text_sm().child("・Google Drive 同期状態"))
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .justify_center()
                                    .gap_2()
                                    .child(
                                        dialog_button("delete-cancel", "キャンセル").cursor_pointer().on_click({
                                            let handle = handle.clone();
                                            move |_, _window, cx| {
                                                handle.update(cx, |this, cx| {
                                                    this.confirm_delete = false;
                                                    cx.notify();
                                                });
                                            }
                                        }),
                                    )
                                    .child(
                                        Button::new("delete-confirm").cursor_pointer()
                                            .danger()
                                            .label("削除").cursor_pointer().on_click({
                                            let handle = handle.clone();
                                            move |_, _window, cx| {
                                                handle.update(cx, |this, cx| {
                                                    this.delete_all_data(cx)
                                                });
                                            }
                                        }),
                                    ),
                            );
                        fade_dialog(_window, cx, confirm_delete, content).into_any_element()
                    } else {
                        div().into_any_element()
                    	                    })
                    .child(if confirm_data_dir {
                        Dialog::new(cx)
                            .title(div().child("データ保存先の変更"))
                            .content(move |content, _window, _cx| {
                                let msg = pending_data_dir
                                    .as_ref()
                                    .map(|p| format!("保存先を {} に変更します。現在のファイルを移動しますか？", p.display()))
                                    .unwrap_or_else(|| "保存先を変更します。".to_string());
                                content.child(div().text_sm().child(msg))
                            })
                            .footer(
                                div()
                                    .flex()
                                    .flex_row()
                                    .gap_2()
                                    .child(
                                        dialog_button("datadir-cancel", "キャンセル").cursor_pointer().on_click({
                                            let handle = handle.clone();
                                            move |_, _window, cx| {
                                                handle.update(cx, |this, cx| {
                                                    this.cancel_data_dir_change(cx);
                                                });
                                            }
                                        }),
                                    )
                                    .child(
                                        Button::new("datadir-confirm").cursor_pointer()
                                            .primary()
                                            .label("移動して変更").cursor_pointer().on_click({
                                            let handle = handle.clone();
                                            move |_, _window, cx| {
                                                handle.update(cx, |this, cx| {
                                                    this.confirm_data_dir_change(cx);
                                                });
                                            }
                                        }),
                                    ),
                            )
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    })
                    .child(if confirm_clear_sync {
                        let handle = cx.entity();
                        let content = dialog_surface(cx)
                            .child(div().text_lg().font_weight(FontWeight::SEMIBOLD).child("同期情報をクリア"))
                            .child(div().text_sm().child(
                                "Google Drive との同期状態（ファイル数・容量・最終同期日時）をリセットします。本棚の本データや進捗は削除されません。よろしいですか？",
                            ))
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .justify_center()
                                    .gap_2()
                                    .child(
                                        dialog_button("clear-sync-cancel", "キャンセル").cursor_pointer().on_click({
                                            let handle = handle.clone();
                                            move |_, _window, cx| {
                                                handle.update(cx, |this, cx| {
                                                    this.confirm_clear_sync = false;
                                                    cx.notify();
                                                });
                                            }
                                        }),
                                    )
                                    .child(
                                        Button::new("clear-sync-confirm").cursor_pointer()
                                            .primary()
                                            .label("クリアする").cursor_pointer().on_click({
                                            let handle = handle.clone();
                                            move |_, _window, cx| {
                                                handle.update(cx, |this, cx| {
                                                    {
                                                        let state = AppState::global(cx);
                                                        let db = &state.db_pool;
                                                        let _ = sync::clear_sync_state(db);
                                                    }
                                                    this.confirm_clear_sync = false;
                                                    this.show_toast("同期情報をクリアしました", cx);
                                                });
                                            }
                                        }),
                                    )
                            );
                        fade_dialog(_window, cx, confirm_clear_sync, content).into_any_element()
                    } else {
                        div().into_any_element()
                    }),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::AppContext as _;
    use gpui_kit::TestAppContext;

    use thundoku_core::db;

    use super::*;

    /// Google のトークン失効（`invalid_grant`）を見逃さないこと。
    ///
    /// 同期のエラーは `String` に畳まれて渡ってくるため core の文言で判定している。
    /// core 側の文言が変わったらこのテストが落ちる（＝気づける）ようにしておく。
    #[test]
    fn detects_google_token_expiry_from_core_messages() {
        use thundoku_core::google::GoogleError;

        assert!(is_google_auth_expired(
            &GoogleError::RefreshTokenRevoked.to_string()
        ));
        assert!(is_google_auth_expired(
            &GoogleError::NoRefreshToken.to_string()
        ));
        assert!(!is_google_auth_expired(
            &GoogleError::NotAuthorized.to_string()
        ));
        assert!(!is_google_auth_expired("network error: timed out"));

        // 案内は日本語で、生の応答（JSON）を含まない
        assert!(
            GOOGLE_AUTH_EXPIRED_NOTICE.contains("ログイン"),
            "案内が日本語でない: {GOOGLE_AUTH_EXPIRED_NOTICE}"
        );
        assert!(
            !GOOGLE_AUTH_EXPIRED_NOTICE.contains('{'),
            "案内に生の応答が混ざっている: {GOOGLE_AUTH_EXPIRED_NOTICE}"
        );
    }

    /// 非表示の本を 1 冊 seed する（設定画面の「非表示にした本」一覧に出る）。
    fn seed_hidden_item(cx: &mut TestAppContext, site_id: &str, database_id: &str) {
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            db::bookshelf::upsert(
                db,
                &db::bookshelf::BookshelfItem {
                    site_id: site_id.into(),
                    database_id: database_id.into(),
                    title: "非表示の本".into(),
                    circle_name: "サークル".into(),
                    author: String::new(),
                    thumbnail_url: None,
                    format: "PDF".into(),
                    caused_at: None,
                    event_name: None,
                    event_slug: None,
                    event_id: None,
                    file_name: None,
                    download_url: None,
                    is_downloadable: 1,
                    is_checked: 0,
                    is_purchased: 1,
                    is_new: 0,
                    is_active: 1,
                    is_favorite: 0,
                    is_hidden: 1,
                    hidden_at: Some("2026-08-21 00:00:00".into()),
                    tags_json: None,
                    synced_at: "2026-08-21 00:00:00".into(),
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
        });
    }

    /// 設定画面は、読み込んだ時点（`reload`）の状態をそのまま表示する。
    ///
    /// 以前はこれを `render` の中で毎回やっていた（`list_hidden` + 全書籍ぶんの進捗クエリ +
    /// 表紙キャッシュの有無チェック）ため、**スクロールのたびに DB を引き直していた**
    /// （実機の実測: ホイール 1 ノッチあたり約 600 クエリ）。描画はキャッシュを出すだけにする。
    #[gpui_kit::test]
    async fn render_uses_loaded_state_without_requerying(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_hidden_item(cx, "booth", "hidden-1");
        let view = cx.new(SettingsView::new);
        // 画面に入ったときに読み込む
        cx.update(|cx| view.update(cx, |this, cx| this.reload(cx)));
        assert_eq!(
            cx.read(|cx| view.read(cx).hidden_items.len()),
            1,
            "reload で非表示の一覧が読み込まれていない"
        );
        // 画面を見ていない間にデータが変わる（本棚側で非表示が解除された、相当）
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            db::bookshelf::set_hidden(db, "booth", "hidden-1", false).unwrap();
        });
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1000.0),
                height: gpui_kit::px(700.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..3 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        assert_eq!(
            cx.read(|cx| view.read(cx).hidden_items.len()),
            1,
            "描画で状態を読み直している（スクロールのたびに DB を引く原因）"
        );
    }

    /// ビューアのホイール方向設定が保存される（既定は「下スクロールで次へ」）。
    #[gpui_kit::test]
    async fn wheel_direction_setting_persists(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        let view = cx.new(SettingsView::new);
        assert_eq!(
            cx.read(|cx| view.read(cx).viewer_wheel_direction.clone()),
            "down-to-next",
            "ホイール方向の既定値が「下スクロールで次へ」になっていない"
        );
        cx.update(|cx| view.update(cx, |this, cx| this.set_wheel_direction(cx, "up-to-next")));
        let stored = cx.read(|cx| {
            let db = &AppState::global(cx).db_pool;
            db::settings::get(db, "viewer.wheel_direction").unwrap()
        });
        assert_eq!(stored.as_deref(), Some("up-to-next"));
    }

    #[gpui_kit::test]
    async fn viewer_mode_setting_persists(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        let view = cx.new(SettingsView::new);
        cx.update(|cx| view.update(cx, |this, cx| this.set_viewer_mode(cx, "scroll")));
        let stored = cx.read(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            db::settings::get(db, "viewer.mode").unwrap()
        });
        assert_eq!(stored.as_deref(), Some("scroll"));
    }

    #[gpui_kit::test]
    async fn page_turn_setting_persists(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        let view = cx.new(SettingsView::new);
        cx.update(|cx| view.update(cx, |this, cx| this.set_page_turn(cx, "left-to-right")));
        let stored = cx.read(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            db::settings::get(db, "viewer.page_turn").unwrap()
        });
        assert_eq!(stored.as_deref(), Some("left-to-right"));
    }

    #[gpui_kit::test]
    async fn settings_view_renders_without_panic(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        let view = cx.new(SettingsView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1000.0),
                height: gpui_kit::px(700.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        visual.update(|window, cx| {
            let arena_clear = window.draw(cx);
            arena_clear.clear(cx);
        });
        // パニックせず描画できれば OK（Web 構成のカード群が描画される）
    }

    #[gpui_kit::test]
    async fn account_lists_fanza_and_dlsite_and_each_site_has_cover_refresh(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        let view = cx.new(SettingsView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1000.0),
                height: gpui_kit::px(700.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        visual.update(|window, cx| {
            let arena_clear = window.draw(cx);
            arena_clear.clear(cx);
        });
        // アカウント欄に FANZA / DLsite が並ぶ
        assert!(
            visual.debug_bounds("account-row-fanza").is_some(),
            "アカウントに FANZA 行があること"
        );
        assert!(
            visual.debug_bounds("account-row-dlsite").is_some(),
            "アカウントに DLsite 行があること"
        );
        // 各サイトの設定カードに「表紙更新」がある
        let sites: [(&str, &'static str); 4] = [
            ("技術書典", "site-refresh-covers-techbookfest"),
            ("BOOTH", "site-refresh-covers-booth"),
            ("FANZA", "site-refresh-covers-fanza"),
            ("DLsite", "site-refresh-covers-dlsite"),
        ];
        for (site, selector) in sites {
            assert!(
                visual.debug_bounds(selector).is_some(),
                "{site} の表紙更新ボタンがあること"
            );
        }
    }

    #[gpui_kit::test]
    async fn account_lists_github_row(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        let view = cx.new(SettingsView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1000.0),
                height: gpui_kit::px(700.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        visual.update(|window, cx| {
            let arena_clear = window.draw(cx);
            arena_clear.clear(cx);
        });
        // アカウント欄に GitHub 行がある（未ログインでも常に出す）
        assert!(
            visual.debug_bounds("account-row-github").is_some(),
            "アカウントに GitHub 行があること"
        );
        // ログイン中（プロフィールあり）でも同じ行が描ける（@login を出す分岐）
        view.update(cx, |_, cx| {
            let state = AppState::global(cx);
            *state.github_logged_in.lock() = true;
            *state.github_profile.lock() = Some(thundoku_core::github::GithubUser {
                login: "octocat".to_string(),
                name: Some("The Octocat".to_string()),
            });
            cx.notify();
        });
        visual.update(|window, cx| {
            let arena_clear = window.draw(cx);
            arena_clear.clear(cx);
        });
        assert!(
            visual.debug_bounds("account-row-github").is_some(),
            "ログイン中も GitHub 行があること"
        );
    }

    #[gpui_kit::test]
    async fn drive_sync_toggle_persists(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        let view = cx.new(SettingsView::new);
        cx.update(|cx| view.update(cx, |this, cx| this.toggle_drive_sync(cx, true)));
        let enabled = cx.read(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            db::settings::get(db, "drive.sync.enabled").unwrap()
        });
        assert_eq!(enabled.as_deref(), Some("true"));
    }

    #[gpui_kit::test]
    async fn delete_all_data_clears_books_and_settings(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        cx.update(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            db::books::insert(
                db,
                &db::books::Book {
                    id: "b1".into(),
                    title: "本".into(),
                    author: String::new(),
                    circle_name: String::new(),
                    purchase_date: None,
                    file_name: "b1.pdf".into(),
                    file_size: 1,
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
            let _ = db::settings::set(db, "drive.sync.enabled", "true");
        });
        let view = cx.new(SettingsView::new);
        cx.update(|cx| view.update(cx, |this, cx| this.delete_all_data(cx)));
        let count = cx.read(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            db::books::list(db).unwrap().len()
        });
        assert_eq!(count, 0);
        let sync_enabled = cx.read(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            db::settings::get(db, "drive.sync.enabled").unwrap()
        });
        assert_eq!(sync_enabled.as_deref(), Some("true")); // app_settings preserved
    }
}
