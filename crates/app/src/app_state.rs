//! Application-wide state: data directories, DB connection, TBF/Google
//! clients and auth state. Registered as a gpui global.

use std::path::PathBuf;
use std::sync::Arc;

use gpui::ReadGlobal as _;
use gpui::{App, Global};
use parking_lot::Mutex;
use thundoku_core::booth::BoothSession;
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
    /// モーダルなし Google ログインの直近のエラー（ログイン状態パネルに表示）
    pub google_login_error: Arc<Mutex<Option<String>>>,
    pub tbf_logged_in: Arc<Mutex<bool>>,
    /// BOOTH（booth.pm）のセッション Cookie
    pub booth_session: Arc<Mutex<Option<BoothSession>>>,
    pub booth_logged_in: Arc<Mutex<bool>>,
    /// アプリ全体のトーストメッセージ（workspace が表示する）
    pub toast_message: Arc<Mutex<Option<String>>>,
    /// トーストの世代（新メッセージごとに増える。タイマー再起動用）
    pub toast_generation: Arc<Mutex<u64>>,
    /// トーストホストを持つ workspace（トースト表示時の notify 用）
    pub workspace: Arc<Mutex<Option<gpui::WeakEntity<crate::workspace::Workspace>>>>,
    /// 本棚の再読込が必要（設定画面の非表示解除等）。render で確認して reload する
    pub bookshelf_invalidated: Arc<Mutex<bool>>,
}

impl Global for AppState {}

impl AppState {
    /// Initialize from the real data directory and the OS keyring.
    pub fn init(cx: &mut App) {
        let data_dir = dirs::data_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("thundoku-shelf");
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
        let google_profile = if let Some(client) = google.as_mut() {
            if let Ok(Some(json)) = secrets.load(thundoku_core::secrets::USER_GOOGLE)
                && let Ok(tokens) = serde_json::from_str(&json)
            {
                client.restore_tokens(tokens);
            }
            None // profile is fetched on demand; presence is tracked in UI
        } else {
            None
        };

        // BOOTH セッションを keyring から復元する
        let booth_session = secrets
            .load(thundoku_core::secrets::USER_BOOTH)
            .ok()
            .flatten()
            .and_then(|json| serde_json::from_str(&json).ok())
            .filter(|session: &BoothSession| session.logged_in());
        let booth_logged_in = booth_session.is_some();

        cx.set_global(Self {
            data_dir,
            packs_dir,
            downloads_dir,
            db_pool,
            tbf: Arc::new(Mutex::new(tbf)),
            google: Arc::new(Mutex::new(google)),
            secrets,
            google_profile: Arc::new(Mutex::new(google_profile)),
            google_login_error: Arc::new(Mutex::new(None)),
            tbf_logged_in: Arc::new(Mutex::new(tbf_logged_in)),
            booth_session: Arc::new(Mutex::new(booth_session)),
            booth_logged_in: Arc::new(Mutex::new(booth_logged_in)),
            toast_message: Arc::new(Mutex::new(None)),
            toast_generation: Arc::new(Mutex::new(0)),
            workspace: Arc::new(Mutex::new(None)),
            bookshelf_invalidated: Arc::new(Mutex::new(false)),
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
            google_login_error: Arc::new(Mutex::new(None)),
            tbf_logged_in: Arc::new(Mutex::new(false)),
            booth_session: Arc::new(Mutex::new(None)),
            booth_logged_in: Arc::new(Mutex::new(false)),
            toast_message: Arc::new(Mutex::new(None)),
            toast_generation: Arc::new(Mutex::new(0)),
            workspace: Arc::new(Mutex::new(None)),
            bookshelf_invalidated: Arc::new(Mutex::new(false)),
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

/// BOOTH のセッションを keyring に永続化し、グローバル状態を更新する。
pub fn save_booth_session(cx: &App, session: &BoothSession) {
    let state = AppState::global(cx);
    let logged_in = session.logged_in();
    if let Ok(json) = serde_json::to_string(session) {
        let _ = state
            .secrets
            .save(thundoku_core::secrets::USER_BOOTH, &json);
    }
    *state.booth_session.lock() = Some(session.clone());
    *state.booth_logged_in.lock() = logged_in;
}

/// BOOTH のセッションを破棄する（ログアウト）。
pub fn clear_booth_session(cx: &App) {
    let state = AppState::global(cx);
    let _ = state.secrets.delete(thundoku_core::secrets::USER_BOOTH);
    *state.booth_session.lock() = None;
    *state.booth_logged_in.lock() = false;
}
