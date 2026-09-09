//! Application-wide state: data directories, DB connection, TBF/Google
//! clients and auth state. Registered as a gpui global.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gpui_kit::ReadGlobal as _;
use gpui_kit::{App, Bounds, Global, Point, Size, Window, WindowBounds, px};
use parking_lot::Mutex;
use thundoku_core::booth::BoothSession;
use thundoku_core::dlsite::client::DlsiteSession;
use thundoku_core::fanza::client::FanzaSession;
use thundoku_core::db;
use thundoku_core::google::{GoogleClient, GoogleProfile};
use thundoku_core::secrets::SecretStore;
use thundoku_core::tbf::{TbfClient, TbfSession};
/// The Google OAuth client id (public info; embedded at build time from
/// `THUNDOKU_GOOGLE_CLIENT_ID`). Web 版（thundoku-web）と同じクライアントを
/// デフォルトとして使い、未設定でも Google ログインが動くようにする。
pub const DEFAULT_GOOGLE_CLIENT_ID: &str = match option_env!("THUNDOKU_GOOGLE_CLIENT_ID") {
    Some(value) if !value.is_empty() => value,
    _ => "1054619943130-2kaqpgnm719bp8l8rslm8rkuvdhb945s.apps.googleusercontent.com",
};

/// Google OAuth client secret. デスクトップでは公開情報扱いで許容
/// （Google が client_secret を要求する場合のため）。`THUNDOKU_GOOGLE_CLIENT_SECRET`
/// 環境変数で上書き可能。デフォルトはクライアント
/// `1054619943130-2kaqpgnm...` の secret（Downloads の client_secret JSON より）。
pub const DEFAULT_GOOGLE_CLIENT_SECRET: Option<&str> =
    match option_env!("THUNDOKU_GOOGLE_CLIENT_SECRET") {
        Some(value) if !value.is_empty() => Some(value),
        _ => Some("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"),
    };

pub struct AppState {
    pub data_dir: PathBuf,
    pub packs_dir: PathBuf,
    pub downloads_dir: PathBuf,
    /// sqlx の接続プール（migrations 適用済み）
    pub db_pool: thundoku_core::db::SqlitePool,
    pub tbf: Arc<Mutex<TbfClient>>,
    pub google: Arc<Mutex<Option<GoogleClient>>>,
    pub secrets: SecretStore,
    pub google_profile: Arc<Mutex<Option<GoogleProfile>>>,
    /// Google ログイン状態（keyring にトークンがあるか）。プロフィール未取得でも維持する。
    pub google_logged_in: Arc<Mutex<bool>>,
    /// モーダルなし Google ログインの直近のエラー（ログイン状態パネルに表示）
    pub google_login_error: Arc<Mutex<Option<String>>>,
    pub tbf_logged_in: Arc<Mutex<bool>>,
    /// BOOTH（booth.pm）のセッション Cookie
    pub booth_session: Arc<Mutex<Option<BoothSession>>>,
    pub booth_logged_in: Arc<Mutex<bool>>,
    /// FANZA同人（www.dmm.co.jp/dc/doujin）のセッション Cookie
    pub fanza_session: Arc<Mutex<Option<FanzaSession>>>,
    pub fanza_logged_in: Arc<Mutex<bool>>,
    /// DLsite（www.dlsite.com）のセッション Cookie
    pub dlsite_session: Arc<Mutex<Option<DlsiteSession>>>,
    pub dlsite_logged_in: Arc<Mutex<bool>>,
    /// アプリ全体のトーストメッセージ（workspace が表示する）
    pub toast_message: Arc<Mutex<Option<String>>>,
    /// トーストの世代（新メッセージごとに増える。タイマー再起動用）
    pub toast_generation: Arc<Mutex<u64>>,
    /// トーストホストを持つ workspace（トースト表示時の notify 用）
    pub workspace: Arc<Mutex<Option<gpui_kit::WeakEntity<crate::workspace::Workspace>>>>,
    /// 本棚の再読込が必要（設定画面の非表示解除等）。render で確認して reload する
    pub bookshelf_invalidated: Arc<Mutex<bool>>,
    /// 終了確認ダイアログ（バックアップ対象の確認）を表示済みかどうか。
    /// キャンセル時は false に戻し、再度終了時に確認を出す。
    pub exit_checked: Arc<AtomicBool>,
    /// 終了時の「アップロードして終了」実行中か（本棚のダウンロード・ビューアー起動を
    /// ブロックするために Workspace と本棚で共有する）。
    pub exit_uploading: Arc<AtomicBool>,
    /// Google ログイン（成功・失敗）が完了したことを Workspace 監視タスクへ通知する。
    /// Workspace 側で show_auth をリセットする（RefCell 再入問題を回避するため）。
    pub google_login_done: Arc<AtomicBool>,
    /// Google ログアウトが完了したことを Workspace 監視タスクへ通知する。
    /// 本棚を未ログイン（未所属のみ表示）に再フィルタするため。
    pub google_logout_done: Arc<AtomicBool>,
    /// 認証モーダルを開く要求。Workspace 監視タスクが検知して show_auth を
    /// 設定する（SettingsView → Workspace の直接 update による RefCell 再入を回避）。
    pub auth_open_requested: Arc<AtomicBool>,
    /// auth_open_requested 時の認証プロバイダ。
    pub auth_open_provider: Arc<Mutex<Option<crate::views::auth::AuthProvider>>>,
}

impl Global for AppState {}

/// データ保存先の設定ファイルの場所。データディレクトリとは独立し、
/// 保存先を変更しても常に読めるように config ディレクトリに置く。
fn data_settings_path() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("thundoku-shelf")
        .join("settings.json")
}

/// 変更された保存先（データディレクトリ）を読み込む。未設定は None。
fn loaded_data_path() -> Option<PathBuf> {
    let path = data_settings_path();
    let json = std::fs::read_to_string(&path).ok()?;
    let value: serde_json::Value = serde_json::from_str(&json).ok()?;
    value
        .get("data_path")
        .and_then(|v| v.as_str())
        .map(PathBuf::from)
}

/// データ保存先を設定ファイルに保存する（保存先変更時に呼ぶ）。
pub fn save_data_path(path: &std::path::Path) {
    let file = data_settings_path();
    if let Some(parent) = file.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let json = serde_json::json!({ "data_path": path.to_string_lossy() });
    let _ = std::fs::write(
        &file,
        serde_json::to_string_pretty(&json).unwrap_or_default(),
    );
}

impl AppState {
    /// Initialize from the real data directory and the OS keyring.
    pub fn init(cx: &mut App) {
        // 保存先が変更されている場合はそのパスを使う（設定ファイルは
        // データディレクトリとは独立した場所に保存し、必ず読めるようにする）。
        let data_dir = loaded_data_path().unwrap_or_else(|| {
            dirs::data_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join("thundoku-shelf")
        });
        Self::init_with_data_dir(cx, data_dir);
    }

    /// Initialize with a specific data directory (keyring-backed secrets).
    pub fn init_with_data_dir(cx: &mut App, data_dir: PathBuf) {
        let packs_dir = data_dir.join("packs");
        let downloads_dir = data_dir.join("downloads");
        let db_path = data_dir.join("thundoku-shelf.db");
        std::fs::create_dir_all(&packs_dir).expect("create packs dir");
        std::fs::create_dir_all(&downloads_dir).expect("create downloads dir");

        // sqlx で接続 + マイグレーション適用（migrations/ ディレクトリ管理）
        let db_pool = db::connect(&db_path).expect("open database");
        // P4: 初回起動（新旧モデル移行）で既存データをクリアして新モデルで開始する。
        let _ = db::clear_owner_model_if_first_run(
            &db_pool,
            &packs_dir,
            &data_dir.join("thumbnails"),
        );

        let secrets = SecretStore::new();
        let mut tbf = TbfClient::new();
        let tbf_logged_in =
            if let Ok(Some(json)) = secrets.load(thundoku_core::secrets::USER_TECHBOOKFEST) {
                match serde_json::from_str(&json) {
                    Ok(session) => {
                        tbf.restore_session(session);
                        tbf.is_authenticated()
                    }
                    Err(_) => false,
                }
            } else {
                false
            };

        let client_id = default_client_id();
        let mut google: Option<GoogleClient> = if client_id.is_empty() {
            None
        } else {
            Some(GoogleClient::new(
                client_id,
                DEFAULT_GOOGLE_CLIENT_SECRET.map(String::from),
            ))
        };
        let google_profile: Option<thundoku_core::google::GoogleProfile> =
            if let Some(client) = google.as_mut() {
                if let Ok(Some(json)) = secrets.load(thundoku_core::secrets::USER_GOOGLE)
                    && let Ok(tokens) = serde_json::from_str(&json)
                {
                    client.restore_tokens(tokens);
                }
                None // profile is fetched on demand; presence is tracked in UI
            } else {
                None
            };
        // keyring に保存済みトークンがあるかでログイン状態を判定する。
        // プロフィール（email 等）の取得に失敗してもログイン状態は維持する。
        let google_logged_in = google.as_ref().is_some_and(|c| c.has_tokens());

        // BOOTH セッションを DB（app_settings）から復元する。
        // セッション Cookie は Windows Credential Manager の上限（2560 UTF-16 文字）を
        // 超えることがあるため、keyring ではなく DB に保存する。
        let booth_session = db::settings::get(&db_pool, "booth.session")
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
            .filter(|session: &BoothSession| session.logged_in());
        let booth_logged_in = booth_session.is_some();
        log::info!(
            "booth session: 起動時復元 = {}",
            if booth_logged_in {
                format!(
                    "ログイン済み（cookies={}）",
                    booth_session.as_ref().map(|s| s.cookies.len()).unwrap_or(0)
                )
            } else {
                "未ログイン".to_string()
            }
        );

        // FANZA同人セッションも DB（app_settings）から復元する（BOOTH と同様、
        // Cookie が巨大で keyring 上限を超えるため DB 保存）。
        let fanza_session = db::settings::get(&db_pool, "fanza.session")
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
            .filter(|session: &FanzaSession| session.logged_in());
        let fanza_logged_in = fanza_session.is_some();

        // DLsite セッションも DB（app_settings）から復元する（FANZA と同様、
        // Cookie が巨大で keyring 上限を超えるため DB 保存）。
        let dlsite_session = db::settings::get(&db_pool, "dlsite.session")
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
            .filter(|session: &DlsiteSession| session.logged_in());
        let dlsite_logged_in = dlsite_session.is_some();

        cx.set_global(Self {
            data_dir,
            packs_dir,
            downloads_dir,
            db_pool,
            tbf: Arc::new(Mutex::new(tbf)),
            google: Arc::new(Mutex::new(google)),
            secrets,
            google_profile: Arc::new(Mutex::new(google_profile)),
            google_logged_in: Arc::new(Mutex::new(google_logged_in)),
            google_login_error: Arc::new(Mutex::new(None)),
            tbf_logged_in: Arc::new(Mutex::new(tbf_logged_in)),
            booth_session: Arc::new(Mutex::new(booth_session)),
            booth_logged_in: Arc::new(Mutex::new(booth_logged_in)),
            fanza_session: Arc::new(Mutex::new(fanza_session)),
            fanza_logged_in: Arc::new(Mutex::new(fanza_logged_in)),
            dlsite_session: Arc::new(Mutex::new(dlsite_session)),
            dlsite_logged_in: Arc::new(Mutex::new(dlsite_logged_in)),
            toast_message: Arc::new(Mutex::new(None)),
            toast_generation: Arc::new(Mutex::new(0)),
            workspace: Arc::new(Mutex::new(None)),
            bookshelf_invalidated: Arc::new(Mutex::new(false)),
            exit_checked: Arc::new(AtomicBool::new(false)),
            exit_uploading: Arc::new(AtomicBool::new(false)),
            google_login_done: Arc::new(AtomicBool::new(false)),
            google_logout_done: Arc::new(AtomicBool::new(false)),
            auth_open_requested: Arc::new(AtomicBool::new(false)),
            auth_open_provider: Arc::new(Mutex::new(None)),
        });
        // Zenn タグを起動時に 1 回だけ取得する（取り込み時のネットワーク待ちをなくす）
        std::thread::spawn(|| {
            let _ = thundoku_core::tags::fetch_zenn_tags();
        });
    }

    /// Test-only initialization: in-memory DB, no keyring.
    pub fn init_test(cx: &mut App) {
        // sqlx のメモリ DB プール（1 接続固定で同一メモリを共有）＋マイグレーション適用
        let db_pool = thundoku_core::db::block_on(async {
            let options = sqlx::sqlite::SqliteConnectOptions::new()
                .filename(":memory:")
                .foreign_keys(true)
                .create_if_missing(true);
            sqlx::sqlite::SqlitePoolOptions::new()
                .max_connections(1)
                .connect_with(options)
                .await
        })
        .expect("in-memory sqlx pool");
        thundoku_core::db::migrate(&db_pool).expect("migrate");
        let client_id = default_client_id();
        cx.set_global(Self {
            data_dir: std::env::temp_dir().join("thundoku-shelf-test"),
            packs_dir: std::env::temp_dir().join("thundoku-shelf-test/packs"),
            downloads_dir: std::env::temp_dir().join("thundoku-shelf-test/downloads"),
            db_pool,
            tbf: Arc::new(Mutex::new(TbfClient::new())),
            google: Arc::new(Mutex::new(if client_id.is_empty() {
                None
            } else {
                Some(GoogleClient::new(
                    client_id,
                    DEFAULT_GOOGLE_CLIENT_SECRET.map(String::from),
                ))
            })),
            secrets: SecretStore::new(),
            google_profile: Arc::new(Mutex::new(None)),
            google_logged_in: Arc::new(Mutex::new(false)),
            google_login_error: Arc::new(Mutex::new(None)),
            tbf_logged_in: Arc::new(Mutex::new(false)),
            booth_session: Arc::new(Mutex::new(None)),
            booth_logged_in: Arc::new(Mutex::new(false)),
            fanza_session: Arc::new(Mutex::new(None)),
            fanza_logged_in: Arc::new(Mutex::new(false)),
            dlsite_session: Arc::new(Mutex::new(None)),
            dlsite_logged_in: Arc::new(Mutex::new(false)),
            toast_message: Arc::new(Mutex::new(None)),
            toast_generation: Arc::new(Mutex::new(0)),
            workspace: Arc::new(Mutex::new(None)),
            bookshelf_invalidated: Arc::new(Mutex::new(false)),
            exit_checked: Arc::new(AtomicBool::new(false)),
            exit_uploading: Arc::new(AtomicBool::new(false)),
            google_login_done: Arc::new(AtomicBool::new(false)),
            google_logout_done: Arc::new(AtomicBool::new(false)),
            auth_open_requested: Arc::new(AtomicBool::new(false)),
            auth_open_provider: Arc::new(Mutex::new(None)),
        });
    }
}

/// Build-time client id; empty when not configured (settings view can set it).
pub fn default_client_id() -> String {
    if DEFAULT_GOOGLE_CLIENT_ID.is_empty() {
        String::new()
    } else {
        DEFAULT_GOOGLE_CLIENT_ID.to_string()
    }
}

/// アプリ全体のトーストを表示する（workspace のトーストホストが 3 秒で消す）。
/// トーストホスト（workspace）を notify して再レンダリングを促す。
pub fn set_toast(cx: &mut App, message: impl Into<String>) {
    // workspace（トーストホスト）は notify 用に先に取り出しておく
    let ws = {
        let state = AppState::global(cx);
        *state.toast_message.lock() = Some(message.into());
        *state.toast_generation.lock() += 1;
        state.workspace.lock().clone()
    };
    log::info!("set_toast: workspace host={}", ws.is_some());
    if let Some(ws) = ws {
        // レンダリング中の再入パニックを避けるため defer で次フレームに回す
        cx.defer(move |cx| {
            let _ = ws.update(cx, |_, cx| cx.notify());
        });
    }
}

/// トーストをクリアする。
pub fn clear_toast(cx: &App) {
    let state = AppState::global(cx);
    *state.toast_message.lock() = None;
}

/// 技術書典のセッションを keyring に永続化し、クライアントと状態を更新する。
/// WebView ログイン（Cookie 取得）の完了時に呼ぶ。
pub fn save_tbf_session(cx: &App, session: &TbfSession) {
    let state = AppState::global(cx);
    if let Ok(json) = serde_json::to_string(session) {
        let _ = state
            .secrets
            .save(thundoku_core::secrets::USER_TECHBOOKFEST, &json);
    }
    state.tbf.lock().restore_session(session.clone());
    *state.tbf_logged_in.lock() = true;
    log::info!("tbf session saved -> tbf_logged_in = true");
}

/// BOOTH のセッションを DB（app_settings）に永続化し、グローバル状態を更新する。
/// Windows Credential Manager の上限（2560 UTF-16 文字）を Cookie が超えることが
/// あるため、keyring ではなく DB を使う。
pub fn save_booth_session(cx: &App, session: &BoothSession) {
    let state = AppState::global(cx);
    let logged_in = session.logged_in();
    if let Ok(json) = serde_json::to_string(session) {
        match db::settings::set(&state.db_pool, "booth.session", &json) {
            Ok(()) => log::info!(
                "booth session: DB 保存成功（cookies={}）",
                session.cookies.len()
            ),
            Err(e) => log::error!("booth session: DB 保存失敗: {e}"),
        }
    }
    *state.booth_session.lock() = Some(session.clone());
    *state.booth_logged_in.lock() = logged_in;
}

/// BOOTH のセッションを破棄する（ログアウト）。
pub fn clear_booth_session(cx: &App) {
    let state = AppState::global(cx);
    let _ = db::settings::delete(&state.db_pool, "booth.session");
    *state.booth_session.lock() = None;
    *state.booth_logged_in.lock() = false;
}

/// FANZA同人 のセッションを DB（app_settings）に永続化し、グローバル状態を更新する。
/// BOOTH と同様、セッション Cookie は keyring 上限を超えるため DB に保存する。
pub fn save_fanza_session(cx: &App, session: &FanzaSession) {
    let state = AppState::global(cx);
    let logged_in = session.logged_in();
    if let Ok(json) = serde_json::to_string(session) {
        let _ = db::settings::set(&state.db_pool, "fanza.session", &json);
    }
    *state.fanza_session.lock() = Some(session.clone());
    *state.fanza_logged_in.lock() = logged_in;
}

/// FANZA同人 のセッションを破棄する（ログアウト）。
pub fn clear_fanza_session(cx: &App) {
    let state = AppState::global(cx);
    let _ = db::settings::delete(&state.db_pool, "fanza.session");
    *state.fanza_session.lock() = None;
    *state.fanza_logged_in.lock() = false;
}

/// DLsite のセッションを DB（app_settings）に永続化し、グローバル状態を更新する。
/// BOOTH/FANZA と同様、セッション Cookie は keyring 上限を超えるため DB に保存する。
pub fn save_dlsite_session(cx: &App, session: &DlsiteSession) {
    let state = AppState::global(cx);
    let logged_in = session.logged_in();
    if let Ok(json) = serde_json::to_string(session) {
        let _ = db::settings::set(&state.db_pool, "dlsite.session", &json);
    }
    *state.dlsite_session.lock() = Some(session.clone());
    *state.dlsite_logged_in.lock() = logged_in;
}

/// DLsite のセッションを破棄する（ログアウト）。
pub fn clear_dlsite_session(cx: &App) {
    let state = AppState::global(cx);
    let _ = db::settings::delete(&state.db_pool, "dlsite.session");
    *state.dlsite_session.lock() = None;
    *state.dlsite_logged_in.lock() = false;
}

/// 保存済みのウィンドウ状態（最大化/通常/フルスクリーン + 復元 size）の DB キー。
const WINDOW_BOUNDS_KEY: &str = "window.bounds";

///  を JSON 文字列に変換する（ に保存するため）。
fn encode_window_bounds(bounds: WindowBounds) -> String {
    let (state, b) = match bounds {
        WindowBounds::Windowed(b) => ("windowed", b),
        WindowBounds::Maximized(b) => ("maximized", b),
        WindowBounds::Fullscreen(b) => ("fullscreen", b),
    };
    serde_json::json!({
        "state": state,
        "x": b.origin.x.as_f32(),
        "y": b.origin.y.as_f32(),
        "w": b.size.width.as_f32(),
        "h": b.size.height.as_f32(),
    })
    .to_string()
}

/// 保存済みのウィンドウ状態 JSON を  に復元する。
fn decode_window_bounds(json: &str) -> Option<WindowBounds> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let b = Bounds::new(
        Point::new(px(v["x"].as_f64()? as f32), px(v["y"].as_f64()? as f32)),
        Size::new(px(v["w"].as_f64()? as f32), px(v["h"].as_f64()? as f32)),
    );
    let state = v["state"].as_str()?;
    Some(match state {
        "maximized" => WindowBounds::Maximized(b),
        "fullscreen" => WindowBounds::Fullscreen(b),
        _ => WindowBounds::Windowed(b),
    })
}

/// 現在のウィンドウ状態を  に保存する（ウィンドウを閉じる時に呼ぶ）。
pub fn save_window_bounds(window: &Window, cx: &App) {
    let state = AppState::global(cx);
    let bounds = window.window_bounds();
    let json = encode_window_bounds(bounds);
    let _ = db::settings::set(&state.db_pool, WINDOW_BOUNDS_KEY, &json);
}

/// 保存済みのウィンドウ状態を復元する（起動時に呼ぶ）。
pub fn load_window_bounds(cx: &App) -> Option<WindowBounds> {
    let state = AppState::global(cx);
    let json = db::settings::get(&state.db_pool, WINDOW_BOUNDS_KEY)
        .ok()
        .flatten()?;
    decode_window_bounds(&json)
}
