//! Application-wide state: data directories, DB connection, TBF/Google
//! clients and auth state. Registered as a gpui global.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use gpui_kit::ReadGlobal as _;
use gpui_kit::{App, Bounds, Global, Point, Size, Window, WindowBounds, px};
use parking_lot::Mutex;
use thundoku_core::booth::BoothSession;
use thundoku_core::db;
use thundoku_core::dlsite::client::DlsiteSession;
use thundoku_core::fanza::client::FanzaSession;
use thundoku_core::google::{GoogleClient, GoogleProfile};
use thundoku_core::secrets::SecretStore;
use thundoku_core::session_store::{PurgeMarker, SessionVault, StoreSession};
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

/// ログアウト時に削除できなかった資格情報の印に使うスロット名（Google）。
pub const GOOGLE_LOGOUT_SLOT: &str = "google";

/// 「ログアウトしたのに保存値を消せなかった」印。データディレクトリのファイルに置く
/// （DB / keyring とは別の障害領域なので、そちらが使えなくても記録できる）。
pub fn purge_marker(state: &AppState) -> PurgeMarker {
    PurgeMarker::new(&state.data_dir)
}

/// 購入一覧の分割同期の続き位置（ストアごと）。
///
/// 1 回の同期で全ページ取らず、ユーザーの操作ごとに少しずつ進める。アプリを閉じると
/// 失われるが、取り込みは upsert なので取り直しても二重登録にはならない。
/// ログイン・ログアウト（アカウントの切り替え）でそのストアの位置は捨てる。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SyncCursors {
    pub fanza: Option<thundoku_core::fanza::sync::PurchaseCursor>,
    pub dlsite: Option<thundoku_core::dlsite::sync::PurchaseCursor>,
}

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
    /// 解決済みの pack ルート鍵（v3 = PRK）。Google ログイン後に解決し、ログアウトで破棄する。
    ///
    /// pack の復号は冊ごとに `root.derive_pack_key(book_id)` で導出する鍵を使う
    /// （仕様 `docs/spec/10-pack-keys.md` §2 / §6）。解決（keyring → Drive の
    /// `thundoku-keys.json` → 必要ならパスフレーズ入力）は [`crate::pack_keys`] が行い、
    /// ここは**解決済みの値**を読む側が取るための置き場。
    pub pack_root_key: Arc<Mutex<Option<opfspack::PackRootKey>>>,
    /// パスフレーズの解錠ダイアログ（背景スレッド → UI）の受け渡し。
    ///
    /// `PackKeyStore` の prompt コールバックは同期なので、背景タスクが
    /// [`crate::pack_keys::PackKeyPrompt::ask`] で要求を積み、UI（`Workspace`）が
    /// モーダルで答える。
    pub pack_key_prompt: Arc<crate::pack_keys::PackKeyPrompt>,
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
    /// ストアのセッション Cookie の暗号化保存（keyring の鍵が使えない環境では `None`）。
    pub session_vault: Option<SessionVault>,
    /// 購入一覧の分割同期の続き位置（ストアごと。`SyncCursors` の説明を参照）。
    pub sync_cursors: Arc<Mutex<SyncCursors>>,
    /// サイトのログイン完了で立てる同期要求（サイト id。`Workspace` が消費する）。
    ///
    /// ログイン直後の本棚は購入済みの一覧が空なので、手動で「同期」を押させずに
    /// そのサイトを同期する。ログインの完了イベント（`*LoginDone`）は実際にログイン
    /// 操作をしたときだけ発火する（起動時のセッション復元では発火しない）ため、
    /// 再起動のたびに同期が走ることはない。
    pub login_sync_requested: Arc<Mutex<Option<String>>>,
    /// アプリ全体のトーストメッセージ（workspace が表示する）
    pub toast_message: Arc<Mutex<Option<String>>>,
    /// トーストの種別（gpui-kit の Notification に流すときの Info / Success / Error）
    pub toast_kind: Arc<Mutex<ToastKind>>,
    /// トーストの世代（新メッセージごとに増える。タイマー再起動用）
    pub toast_generation: Arc<Mutex<u64>>,
    /// トーストを自動で消すか（終了時のアップロード中など、完了まで出したいときは false）
    pub toast_autohide: Arc<Mutex<bool>>,
    /// 進行中のトーストか（通知に Spinner を添えて「動いている」ことを示す）
    pub toast_progress: Arc<Mutex<bool>>,
    /// 保護つき通知（タグ取得の完了など）の保護期限。この時刻まではバックグラウンドの
    /// 通知（進行中・定期同期の完了）で上書きしない（`set_protected_notice` が立てる）
    pub toast_protected_until: Arc<Mutex<Option<std::time::Instant>>>,
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
    /// 設定の「ローカルデータをすべて削除」でローカルのデータを消したことを通知する。
    /// チェックリストの「空のイベントを開いたら 1 回だけ自動同期」は削除前の試行を
    /// 引きずらないよう、この通知で履歴を捨てる。
    pub local_data_deleted: Arc<AtomicBool>,
    /// いま表示中のモーダル（ダイアログ）の登録簿。
    ///
    /// 各ビューは自分の状態を独立に持っているため、**同じ画面に 2 つ出てしまう**
    /// （実機: 終了確認と取り込み確認 / 終了確認と同期の続き通知）。描画のたびに
    /// 所有者が自分の状態を登録し、[`active_modal`] が優先度で 1 つだけを勝者にする。
    /// 負けた側は**状態を保持したまま描かない**ので、勝者が閉じれば自然に出る
    /// （取り込み確認のように worker が答えを待つものも、破棄せずに待たせられる）。
    pub modals: Arc<Mutex<std::collections::BTreeSet<ModalKind>>>,
}

/// ユーザーの答えを待つモーダル（ダイアログ）の種別。
///
/// **宣言順が優先度**（後ろほど強い）。同じ画面に 2 つ出さないため、[`active_modal`] が
/// 最も強い 1 つを選ぶ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ModalKind {
    /// パスフレーズ未設定のお願い（ログイン直後に 1 回だけ出す情報）。
    /// 答えを待っている相手がいないので**最も弱い**（他のモーダルが終わってから出す）。
    PassphraseNotice,
    /// 同期の続き通知（情報 + 「続きを取り込む」）。状態は保持されるので後から出せる。
    SyncNotice,
    /// 終了確認（アップロードの確認）。
    ExitConfirm,
    /// ダウンロード中止の確認。
    DownloadCancel,
    /// 未ダウンロード本のダウンロード確認（はい / いいえ）。
    DownloadConfirm,
    /// 取り込み（分割取り込み）の確認。worker が答えを待っているので必ず出す。
    Import,
    /// pack の鍵（パスフレーズ）の解錠。背景タスクが答えを待っているので必ず出す。
    PackPassphrase,
    /// ログイン（WebView / 同意）。途中で閉じるとサイト側のセッションが壊れる。
    Login,
    /// 設定の確認（全削除 / 保存先変更 / 同期情報のクリア）。
    SettingsConfirm,
    /// リーダーの付箋入力（リーダー画面のダイアログ）。
    NoteDialog,
}

impl ModalKind {
    /// トースト・ログ用の短い日本語（「◯◯が終わるまで閉じられません」に使う）。
    pub fn label(self) -> &'static str {
        match self {
            Self::PassphraseNotice => "パスフレーズの設定の確認",
            Self::SyncNotice => "同期の続きの確認",
            Self::ExitConfirm => "終了の確認",
            Self::DownloadCancel => "ダウンロードの中止の確認",
            Self::DownloadConfirm => "ダウンロードの確認",
            Self::Import => "取り込みの確認",
            Self::PackPassphrase => "鍵（パスフレーズ）の入力",
            Self::Login => "ログイン",
            Self::SettingsConfirm => "設定の確認",
            Self::NoteDialog => "付箋の入力",
        }
    }
}

/// 表示中のモーダルを登録する（描画のたびに所有者が呼ぶ。`false` で解除）。
pub fn set_modal(cx: &App, kind: ModalKind, showing: bool) {
    let modals = AppState::global(cx).modals.clone();
    let mut modals = modals.lock();
    if showing {
        modals.insert(kind);
    } else {
        modals.remove(&kind);
    }
}

/// いま描くべきモーダル（優先度が最も強い 1 つ）。無ければ `None`。
pub fn active_modal(cx: &App) -> Option<ModalKind> {
    AppState::global(cx).modals.lock().iter().next_back().copied()
}

/// 何かのモーダルが表示中か（ウィンドウを閉じてよいかの判定に使う）。
pub fn any_modal_active(cx: &App) -> bool {
    !AppState::global(cx).modals.lock().is_empty()
}


// WebView2（wry）の呼び出しのうち、Windows のメッセージループを回して gpui の窓更新と
// 衝突し得るのは**生成**と **`cookies_for_url`** だけで、どちらも App の借用の外から
// 呼ぶようにした（生成 = `views::create_login_webview`、Cookie 収集 =
// `views::collect_session_cookies` をタスク本文から呼ぶ）。借用中に pump する経路が
// 無くなったため、以前使っていた pump カウンタ / 待機ヘルパー / ガードは持たない。

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

/// 保存済みの Google プロフィール（`sub` 等）を keyring から復元する（起動時に呼ぶ）。
///
/// プロフィールはネットワーク（`userinfo`）でしか取れないが、`sub` は所有者
/// （`books.owner_sub`）の判定に使う。起動直後に sub が無いと、Drive バックアップの
/// 所有者フィルタを組めず、終了時のアップロードが丸ごとスキップされ、起動のたびに
/// 「復元しますか」が出る（Drive 側は所有者で絞られた古いバックアップのため）。
fn restore_google_profile(secrets: &SecretStore) -> Option<thundoku_core::google::GoogleProfile> {
    let profile = thundoku_core::google::saved_profile(secrets);
    // `sub` はログに出さない（pack の master key の導出元 = 実質の復号秘密）。
    log::info!(
        "google profile: 起動時復元 = {}",
        thundoku_core::google::profile_log_label(profile.as_ref())
    );
    profile
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
        let _ =
            db::clear_owner_model_if_first_run(&db_pool, &packs_dir, &data_dir.join("thumbnails"));
        // is_drm の旧データ（同期が「未検証」の意味で書いた 0）を「不明（2）」へ一度だけ移す。
        match db::migrate_drm_status_once(&db_pool) {
            Ok(true) => log::info!("drm status: 旧データを「不明」へ移行しました"),
            Ok(false) => {}
            Err(error) => log::warn!("drm status: 移行に失敗（次回起動で再試行）: {error}"),
        }

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
        // 前回のログアウトで keyring から削除できなかった資格情報は復元しない
        // （「ログアウトしたのに次回起動で勝手にログインし直す」のを防ぐ）。
        let purge_marker = PurgeMarker::new(&data_dir);
        if purge_marker.is_pending(GOOGLE_LOGOUT_SLOT) {
            log::warn!(
                "google: 前回のログアウトで資格情報を削除できなかったため復元しません（再ログインが必要）"
            );
            let _ = secrets.delete(thundoku_core::secrets::USER_GOOGLE);
            thundoku_core::google::delete_saved_profile(&secrets);
            purge_marker.clear(GOOGLE_LOGOUT_SLOT);
        }
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
                // `sub` は所有者判定（本棚の絞り込み・Drive バックアップの所有者フィルタ）に
                // 使うため、保存済みプロフィールを起動時に復元する。表示用の email 等が
                // 古くても実害は無く、設定画面を開けば取得し直す。
                restore_google_profile(&secrets)
            } else {
                None
            };
        // keyring に保存済みトークンがあるかでログイン状態を判定する。
        // プロフィール（email 等）の取得に失敗してもログイン状態は維持する。
        let google_logged_in = google.as_ref().is_some_and(|c| c.has_tokens());

        // BOOTH / FANZA / DLsite のセッションを DB（app_settings）から復元する。
        // セッション Cookie は Windows Credential Manager の上限（2560 UTF-16 文字）を
        // 超えることがあるため DB に置くが、**keyring の鍵で暗号化して**保存する
        // （平文の旧値・改ざん・別鍵は復号できず、未ログインとして破棄される）。
        let session_vault = match SessionVault::new(&secrets, &data_dir) {
            Ok(vault) => Some(vault),
            Err(error) => {
                // 鍵が取れない = セッションを安全に保存できない。平文で保存しない。
                log::warn!(
                    "session vault を初期化できないためストアのセッションを復元/保存しません: {error}"
                );
                None
            }
        };
        let booth_session = session_vault
            .as_ref()
            .and_then(|vault| vault.load::<BoothSession>(&db_pool, StoreSession::Booth))
            .filter(BoothSession::logged_in);
        let booth_logged_in = booth_session.is_some();
        log::info!(
            "booth session: 起動時復元 = {}",
            if booth_logged_in {
                format!(
                    "ログイン済み（cookies={}）",
                    booth_session
                        .as_ref()
                        .map(BoothSession::cookies_count)
                        .unwrap_or(0)
                )
            } else {
                "未ログイン".to_string()
            }
        );

        let fanza_session = session_vault
            .as_ref()
            .and_then(|vault| vault.load::<FanzaSession>(&db_pool, StoreSession::Fanza))
            .filter(FanzaSession::logged_in);
        let fanza_logged_in = fanza_session.is_some();

        let dlsite_session = session_vault
            .as_ref()
            .and_then(|vault| vault.load::<DlsiteSession>(&db_pool, StoreSession::Dlsite))
            .filter(DlsiteSession::logged_in);
        let dlsite_logged_in = dlsite_session.is_some();

        // 起動時に keyring の PRK（pack のルート鍵）を読む。ネットワークは触らず、
        // Drive の bundle からの復元（必要ならパスフレーズ入力）はログイン後 /
        // 必要になった時点で背景タスクが行う（[`crate::pack_keys`]）。
        let pack_root_key = google_profile
            .as_ref()
            .and_then(|profile| crate::pack_keys::keyring_root_key(&secrets, &profile.sub));

        cx.set_global(Self {
            data_dir,
            packs_dir,
            downloads_dir,
            db_pool,
            tbf: Arc::new(Mutex::new(tbf)),
            google: Arc::new(Mutex::new(google)),
            secrets,
            google_profile: Arc::new(Mutex::new(google_profile)),
            pack_root_key: Arc::new(Mutex::new(pack_root_key)),
            pack_key_prompt: Arc::new(crate::pack_keys::PackKeyPrompt::default()),
            google_logged_in: Arc::new(Mutex::new(google_logged_in)),
            google_login_error: Arc::new(Mutex::new(None)),
            tbf_logged_in: Arc::new(Mutex::new(tbf_logged_in)),
            booth_session: Arc::new(Mutex::new(booth_session)),
            booth_logged_in: Arc::new(Mutex::new(booth_logged_in)),
            fanza_session: Arc::new(Mutex::new(fanza_session)),
            fanza_logged_in: Arc::new(Mutex::new(fanza_logged_in)),
            dlsite_session: Arc::new(Mutex::new(dlsite_session)),
            dlsite_logged_in: Arc::new(Mutex::new(dlsite_logged_in)),
            session_vault,
            sync_cursors: Arc::new(Mutex::new(SyncCursors::default())),
            login_sync_requested: Arc::new(Mutex::new(None)),
            toast_message: Arc::new(Mutex::new(None)),
            toast_kind: Arc::new(Mutex::new(ToastKind::Info)),
            toast_generation: Arc::new(Mutex::new(0)),
            toast_autohide: Arc::new(Mutex::new(true)),
            toast_progress: Arc::new(Mutex::new(false)),
            toast_protected_until: Arc::new(Mutex::new(None)),
            workspace: Arc::new(Mutex::new(None)),
            bookshelf_invalidated: Arc::new(Mutex::new(false)),
            exit_checked: Arc::new(AtomicBool::new(false)),
            exit_uploading: Arc::new(AtomicBool::new(false)),
            google_login_done: Arc::new(AtomicBool::new(false)),
            google_logout_done: Arc::new(AtomicBool::new(false)),
            auth_open_requested: Arc::new(AtomicBool::new(false)),
            local_data_deleted: Arc::new(AtomicBool::new(false)),
            auth_open_provider: Arc::new(Mutex::new(None)),
            modals: Arc::new(Mutex::new(std::collections::BTreeSet::new())),
        });
        // Zenn タグを起動時に 1 回だけ取得する（取り込み時のネットワーク待ちをなくす）
        std::thread::spawn(|| {
            let _ = thundoku_core::tags::fetch_zenn_tags();
        });
    }

    /// Test-only initialization: in-memory DB, no keyring.
    pub fn init_test(cx: &mut App) {
        // テストは BookshelfView::new → 所有者フィルタ → db_key() の経路で
        // keychain に到達するため、メモリバックエンドにして触らないようにする
        // （開発機の許可ダイアログ／CI でのアイテム作成を避ける）。
        SecretStore::use_memory_backend();
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
        let secrets = SecretStore::new();
        // 本番と同じ復元経路を通す（keyring の代わりにメモリバックエンド）
        let session_vault =
            SessionVault::new(&secrets, &std::env::temp_dir().join("thundoku-shelf-test")).ok();
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
            secrets,
            // メモリバックエンドの keyring はプロセス内で共有されるため、テストでは
            // 保存済みプロフィールを復元しない（復元すると所有者フィルタが全テストに
            // 波及する）。復元経路は init_with_data_dir（本番）で検証する。
            google_profile: Arc::new(Mutex::new(None)),
            pack_root_key: Arc::new(Mutex::new(None)),
            pack_key_prompt: Arc::new(crate::pack_keys::PackKeyPrompt::default()),
            google_logged_in: Arc::new(Mutex::new(false)),
            google_login_error: Arc::new(Mutex::new(None)),
            tbf_logged_in: Arc::new(Mutex::new(false)),
            booth_session: Arc::new(Mutex::new(None)),
            booth_logged_in: Arc::new(Mutex::new(false)),
            session_vault,
            fanza_session: Arc::new(Mutex::new(None)),
            fanza_logged_in: Arc::new(Mutex::new(false)),
            dlsite_session: Arc::new(Mutex::new(None)),
            dlsite_logged_in: Arc::new(Mutex::new(false)),
            sync_cursors: Arc::new(Mutex::new(SyncCursors::default())),
            login_sync_requested: Arc::new(Mutex::new(None)),
            toast_message: Arc::new(Mutex::new(None)),
            toast_kind: Arc::new(Mutex::new(ToastKind::Info)),
            toast_generation: Arc::new(Mutex::new(0)),
            toast_autohide: Arc::new(Mutex::new(true)),
            toast_progress: Arc::new(Mutex::new(false)),
            toast_protected_until: Arc::new(Mutex::new(None)),
            workspace: Arc::new(Mutex::new(None)),
            bookshelf_invalidated: Arc::new(Mutex::new(false)),
            exit_checked: Arc::new(AtomicBool::new(false)),
            exit_uploading: Arc::new(AtomicBool::new(false)),
            google_login_done: Arc::new(AtomicBool::new(false)),
            google_logout_done: Arc::new(AtomicBool::new(false)),
            auth_open_requested: Arc::new(AtomicBool::new(false)),
            local_data_deleted: Arc::new(AtomicBool::new(false)),
            auth_open_provider: Arc::new(Mutex::new(None)),
            modals: Arc::new(Mutex::new(std::collections::BTreeSet::new())),
        });
    }

    /// 解決済みの pack ルート鍵（v3 = PRK）。未解決なら `None`。
    ///
    /// 読む側（リーダー・付箋のサムネイルなど）はここから取り、冊ごとの pack 鍵を
    /// `root.derive_pack_key(book_id)` で導出する。解決（ログイン後の復元・
    /// パスフレーズ入力）は [`crate::pack_keys`] が背景で行う。
    pub fn pack_root_key(&self) -> Option<opfspack::PackRootKey> {
        self.pack_root_key.lock().clone()
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

/// Google プロフィール（`sub` 等）を keyring に保存する（**ブロッキング**）。
///
/// `sub` は所有者（`books.owner_sub`）の判定に使う。プロフィールは `userinfo` を
/// 叩かないと取れないため、保存しておかないと次回起動で「誰の本か」が分からず、
/// Drive バックアップのアップロードが丸ごとスキップされる。
pub fn store_google_profile_secret(
    profile: &thundoku_core::google::GoogleProfile,
) -> Result<(), String> {
    thundoku_core::google::save_profile(&SecretStore::new(), profile).map_err(|error| {
        log::error!("google profile save failed: {error}");
        "プロフィールを保存できませんでした（資格情報ストアを確認してください）".to_string()
    })
}

/// `owner_token` の実体。バックグラウンドタスクから `AppState` 全体を持たずに
/// 呼べるよう、プロフィールと鍵だけを受け取る。
pub fn owner_token_from(
    profile: Option<&thundoku_core::google::GoogleProfile>,
    secrets: &SecretStore,
) -> Option<String> {
    let sub = profile.map(|profile| profile.sub.as_str())?;
    let key = secrets.db_key().ok()?;
    Some(thundoku_core::owner::encrypt(&key, sub))
}

/// 現在の Google アカウントの所有者トークン（keyring の DB 鍵で暗号化した `sub`）。
///
/// 本棚・チェックリスト・お気に入りの `owner_sub` に書く値。DB バックアップを
/// 現在のアカウントの行だけに絞るために使う。未ログイン、または暗号鍵が無いときは
/// `None`（＝未所属のまま。誰のものでもないことを表す）。
pub fn owner_token(state: &AppState) -> Option<String> {
    let profile = state.google_profile.lock().clone();
    owner_token_from(profile.as_ref(), &state.secrets)
}

/// メモリ上のプロフィールを更新する（keyring への保存が済んだ後に呼ぶ）。
///
/// ログに `sub` を出さない: pack の master key は `sub` から導出されるため、
/// `sub` は実質の復号秘密であり、ログ（Windows は `%TEMP%/thundoku-shelf/thundoku.log`
/// に既定 debug レベルで残る）へ書くと pack を取得した第三者に復号材料を渡すことになる。
pub fn set_google_profile(cx: &App, profile: &thundoku_core::google::GoogleProfile) {
    let state = AppState::global(cx);
    // アカウントが変わったら解決済みの pack ルート鍵は捨てる（鍵はアカウントごと — 仕様 §2）。
    // 別アカウントの pack を前のアカウントの鍵で復号しようとしない。
    let changed = state
        .google_profile
        .lock()
        .as_ref()
        .is_some_and(|current| current.sub != profile.sub);
    if changed {
        *state.pack_root_key.lock() = None;
    }
    *state.google_profile.lock() = Some(profile.clone());
    *state.google_logged_in.lock() = true;
    *state.google_login_error.lock() = None;
    log::info!("google profile: 状態を更新");
}

/// Google プロフィールを keyring に保存し、メモリ上の状態を更新する（同期版）。
///
/// keyring の応答待ちで固まりうる（許可ダイアログ等）。保存に失敗してもメモリ上の
/// 状態は更新する: ログイン自体は成功しており、保存できなかったのは「次回起動で
/// 所有者を特定するための控え」だけなので、今のセッションを未ログイン扱いにはしない。
pub fn save_google_profile(cx: &App, profile: &thundoku_core::google::GoogleProfile) {
    if let Err(error) = store_google_profile_secret(profile) {
        log::warn!("google profile: 保存に失敗（次回起動で sub を復元できない）: {error}");
    }
    set_google_profile(cx, profile);
}

/// Google からログアウトした状態にする（keyring の削除は呼び出し側で行う）。
pub fn clear_google_profile(cx: &App) {
    let state = AppState::global(cx);
    *state.google_profile.lock() = None;
    // 解決済みの pack ルート鍵も捨てる（アカウントごとの鍵。別アカウントの pack を
    // 前のアカウントの鍵で読もうとしない — 仕様 §2）。
    *state.pack_root_key.lock() = None;
    *state.google_logged_in.lock() = false;
    log::info!("google profile: 状態をクリア");
}

/// アプリ全体の通知を出す（Workspace が gpui-kit の Notification に流す。既定 5 秒で自動消滅）。
/// トーストホスト（workspace）を notify して再レンダリングを促す。
/// 通知の種別（gpui-kit の `NotificationType` に対応）。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ToastKind {
    #[default]
    Info,
    Success,
    Error,
}

/// 種別つきでメッセージを積む（表示は `Workspace` が Notification に流す。既定 5 秒で消える）。
pub fn set_toast_kind(cx: &mut App, kind: ToastKind, message: impl Into<String>) {
    set_toast_kind_with(cx, kind, message, true);
}

/// **進行中**のメッセージを積む（通知に Spinner が付き、同じ id で置き換わる）。
///
/// ダウンロードや取り込みのように「終わるまで動き続ける」処理で使う。同じ文言を
/// 何度積んでも通知は増えず、1 つが更新される。既定では 5 秒で消えるので、
/// 進行中は進捗が変わったときに積み直す（終われば自然に消える）。
pub fn set_progress_notice(cx: &mut App, message: impl Into<String>) {
    // 保護つきの通知（タグ取得の完了など）が表示中なら、進行中の通知で上書きしない
    // （進行中は進捗のたびに積み直すため、放っておくと読む前に消してしまう）
    if !background_notice_allowed(cx, ToastKind::Info) {
        return;
    }
    // `set_toast_kind_with` が進行中フラグを false に戻すので、あとから立てる
    set_toast_kind_with(cx, ToastKind::Info, message, true);
    *AppState::global(cx).toast_progress.lock() = true;
}

/// 保護つき通知（`set_protected_notice`）を、バックグラウンドの通知で
/// 上書きしない時間。自動消滅（5 秒）と同じにして、読み終わる前に消えないようにする。
pub const NOTICE_PROTECT: std::time::Duration = std::time::Duration::from_secs(5);

/// 保護つきの通知を出す（`NOTICE_PROTECT` のあいだは、進行中・定期同期の完了などの
/// バックグラウンド通知に上書きされない）。タグ取得の完了のように「読んでほしい」
/// 通知で使う。
pub fn set_protected_notice(cx: &mut App, kind: ToastKind, message: impl Into<String>) {
    *AppState::global(cx).toast_protected_until.lock() =
        Some(std::time::Instant::now() + NOTICE_PROTECT);
    set_toast_kind(cx, kind, message);
}

/// 保護つき通知がまだ表示中か（`until` が保護期限、`now` が現在時刻）。
fn notice_is_protected(until: Option<std::time::Instant>, now: std::time::Instant) -> bool {
    until.is_some_and(|until| now < until)
}

/// バックグラウンドの通知（進行中・定期同期の完了）を出してよいか。
/// 保護つき通知が表示中のあいだは見送る（エラーは見送らない = 必ず出す）。
pub fn background_notice_allowed(cx: &App, kind: ToastKind) -> bool {
    if kind == ToastKind::Error {
        return true;
    }
    let until = *AppState::global(cx).toast_protected_until.lock();
    !notice_is_protected(until, std::time::Instant::now())
}

/// **進行中のまま消えない**メッセージを積む（終了時のアップロードなど）。
///
/// 完了するとアプリが終了する処理で使う。自動では消えないので、見失わない。
pub fn set_sticky_progress_notice(cx: &mut App, message: impl Into<String>) {
    // `set_toast_kind_with` が進行中フラグを false に戻すので、あとから立てる
    set_toast_kind_with(cx, ToastKind::Info, message, false);
    *AppState::global(cx).toast_progress.lock() = true;
}

/// 自動消滅するかどうかも指定して積む。
///
/// `autohide = false` は「終わるまで出しておきたい」とき（終了時のアップロード中など）に使う。
/// 消えるのはアプリ終了のときだけになる。
pub fn set_toast_kind_with(
    cx: &mut App,
    kind: ToastKind,
    message: impl Into<String>,
    autohide: bool,
) {
    let state = AppState::global(cx);
    *state.toast_kind.lock() = kind;
    *state.toast_autohide.lock() = autohide;
    // 進行中フラグは既定 false。`set_progress_notice` 系が呼び出し後に立て直す
    *state.toast_progress.lock() = false;
    set_toast(cx, message);
}

/// 生のトースト差し替え。**`set_toast_kind*` からだけ呼ぶ**（直接呼ぶと `autohide` /
/// 種別 / 進行中フラグが前の値のまま残り、「消えない通知」がそのまま残る不具合になる）。
fn set_toast(cx: &mut App, message: impl Into<String>) {
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

/// BOOTH のセッションを DB（app_settings）に暗号化して永続化し、グローバル状態を更新する。
/// Windows Credential Manager の上限（2560 UTF-16 文字）を Cookie が超えることが
/// あるため DB を使い、**値は keyring の鍵で暗号化する**（平文で置かない）。
pub fn save_booth_session(cx: &App, session: &BoothSession) {
    let state = AppState::global(cx);
    let logged_in = session.logged_in();
    save_store_session(state, StoreSession::Booth, session);
    *state.booth_session.lock() = Some(session.clone());
    *state.booth_logged_in.lock() = logged_in;
}

/// BOOTH のセッションを破棄する（ログアウト）。
///
/// メモリ上は即座に未ログインへ落とす。**永続値の削除に失敗したら `Err`** を返すので、
/// 呼び出し側は「消えた」と誤って表示しないこと（次回起動で復元され得る）。
pub fn clear_booth_session(cx: &App) -> Result<(), String> {
    let state = AppState::global(cx);
    *state.booth_session.lock() = None;
    *state.booth_logged_in.lock() = false;
    clear_store_session(state, StoreSession::Booth)
}

/// ストアのセッションを暗号化して DB に保存する。
///
/// 暗号鍵が取れない環境では**保存しない**（平文へのフォールバックは禁止）。
/// その場合、そのセッションは次回起動で復元されない（＝再ログインが必要）。
fn save_store_session<T: serde::Serialize>(state: &AppState, store: StoreSession, value: &T) {
    let label = store.label();
    match &state.session_vault {
        Some(vault) => match vault.save(&state.db_pool, store, value) {
            Ok(()) => log::info!("{label} session: DB 保存成功（暗号化）"),
            Err(error) => log::error!("{label} session: DB 保存失敗: {error}"),
        },
        None => log::warn!(
            "{label} session: 暗号鍵が無いため保存しません（次回起動では再ログインが必要）"
        ),
    }
}

/// ストアのセッションの永続値を消す。失敗したら「次回起動で復元しない」印を残す。
fn clear_store_session(state: &AppState, store: StoreSession) -> Result<(), String> {
    let label = store.label();
    match &state.session_vault {
        Some(vault) => vault.clear(&state.db_pool, store).map_err(|error| {
            log::error!("{label} session: 永続値の削除に失敗: {error}");
            error.to_string()
        }),
        None => {
            // 鍵が無い＝暗号化して保存していない。旧平文が残っていれば消しておく。
            let _ = db::settings::delete(&state.db_pool, store.settings_key());
            Ok(())
        }
    }
}

/// FANZA同人 のセッションを DB（app_settings）に暗号化して永続化し、グローバル状態を更新する。
pub fn save_fanza_session(cx: &App, session: &FanzaSession) {
    let state = AppState::global(cx);
    let logged_in = session.logged_in();
    save_store_session(state, StoreSession::Fanza, session);
    *state.fanza_session.lock() = Some(session.clone());
    *state.fanza_logged_in.lock() = logged_in;
    // アカウントが変わると続き位置は無意味（別アカウントの一覧を途中から取らない）
    state.sync_cursors.lock().fanza = None;
}

/// FANZA同人 のセッションを破棄する（ログアウト）。削除に失敗したら `Err`。
pub fn clear_fanza_session(cx: &App) -> Result<(), String> {
    let state = AppState::global(cx);
    *state.fanza_session.lock() = None;
    *state.fanza_logged_in.lock() = false;
    state.sync_cursors.lock().fanza = None;
    clear_store_session(state, StoreSession::Fanza)
}

/// DLsite のセッションを DB（app_settings）に暗号化して永続化し、グローバル状態を更新する。
pub fn save_dlsite_session(cx: &App, session: &DlsiteSession) {
    let state = AppState::global(cx);
    let logged_in = session.logged_in();
    save_store_session(state, StoreSession::Dlsite, session);
    *state.dlsite_session.lock() = Some(session.clone());
    *state.dlsite_logged_in.lock() = logged_in;
    // アカウントが変わると続き位置は無意味（別アカウントの一覧を途中から取らない）
    state.sync_cursors.lock().dlsite = None;
}

/// DLsite のセッションを破棄する（ログアウト）。削除に失敗したら `Err`。
pub fn clear_dlsite_session(cx: &App) -> Result<(), String> {
    let state = AppState::global(cx);
    *state.dlsite_session.lock() = None;
    *state.dlsite_logged_in.lock() = false;
    state.sync_cursors.lock().dlsite = None;
    clear_store_session(state, StoreSession::Dlsite)
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

#[cfg(test)]
mod tests {
    use super::*;

    /// モーダルは**優先度で 1 つだけ**が選ばれる（同じ画面に 2 つ出さない）。
    ///
    /// 負けた側は登録を残したまま描かれないので、勝者が閉じれば自然に繰り上がる
    /// （取り込み確認のように worker が答えを待つものも、破棄せずに待たせられる）。
    #[gpui_kit::test]
    async fn only_the_highest_priority_modal_is_active(cx: &mut gpui_kit::TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        cx.update(|cx| {
            assert_eq!(active_modal(cx), None, "初期状態はモーダル無し");
            // パスフレーズ未設定のお願いは情報なので最も弱い（他が終わってから出る）
            set_modal(cx, ModalKind::PassphraseNotice, true);
            assert_eq!(active_modal(cx), Some(ModalKind::PassphraseNotice));
            set_modal(cx, ModalKind::SyncNotice, true);
            assert_eq!(
                active_modal(cx),
                Some(ModalKind::SyncNotice),
                "同期の続き通知が勝ち、パスフレーズのお願いは待つ"
            );
            set_modal(cx, ModalKind::PassphraseNotice, false);

            set_modal(cx, ModalKind::ExitConfirm, true);
            assert_eq!(
                active_modal(cx),
                Some(ModalKind::ExitConfirm),
                "終了確認が勝ち、同期の続き通知は待つ（実機で重なっていた組み合わせ）"
            );

            set_modal(cx, ModalKind::Login, true);
            assert_eq!(
                active_modal(cx),
                Some(ModalKind::Login),
                "ログイン中は他を出さない"
            );
            assert!(any_modal_active(cx));

            // 鍵（v3 の PRK）の解錠は背景タスクが答えを待っているので、
            // 取り込み確認などより強い（待たせたまま隠さない）
            set_modal(cx, ModalKind::Import, true);
            assert_eq!(active_modal(cx), Some(ModalKind::Login));
            set_modal(cx, ModalKind::Login, false);
            set_modal(cx, ModalKind::PackPassphrase, true);
            assert_eq!(
                active_modal(cx),
                Some(ModalKind::PackPassphrase),
                "解錠は取り込み確認より先に出す（答えを待っている）"
            );
            set_modal(cx, ModalKind::PackPassphrase, false);
            set_modal(cx, ModalKind::Import, false);
            assert_eq!(
                active_modal(cx),
                Some(ModalKind::ExitConfirm),
                "消えたら次が繰り上がる"
            );
            set_modal(cx, ModalKind::ExitConfirm, false);
            set_modal(cx, ModalKind::SyncNotice, false);
            assert_eq!(active_modal(cx), None);
            assert!(!any_modal_active(cx));
        });
    }

    fn google_profile() -> thundoku_core::google::GoogleProfile {
        thundoku_core::google::GoogleProfile {
            sub: "sub-1".to_string(),
            email: "user@example.com".to_string(),
            name: "ユーザー".to_string(),
            picture: None,
        }
    }

    /// keyring に保存したプロフィール（sub）が起動時に復元されること。
    ///
    /// プロフィールは設定画面を開くまで取得されない（`userinfo` はネットワーク）ため、
    /// 復元しないと起動直後の所有者判定（本棚の絞り込み・Drive バックアップの所有者
    /// フィルタ）に sub が無い。Drive 側のバックアップは所有者で絞られているので、
    /// 終了時のアップロードが丸ごとスキップされ、起動のたびに復元確認が出る。
    #[gpui_kit::test]
    async fn restores_google_profile_on_startup(cx: &mut gpui_kit::TestAppContext) {
        // keyring には触らない（メモリバックエンドはプロセス内で共有されるが、
        // プロフィールを読むのは本番の起動経路＝このテストだけ）
        thundoku_core::secrets::SecretStore::use_memory_backend();
        cx.update(gpui_kit::component::init);
        let data_dir = std::env::temp_dir().join("thundoku-shelf-test/google-profile-startup");
        let _ = std::fs::remove_dir_all(&data_dir);
        cx.update(|cx| AppState::init_with_data_dir(cx, data_dir.clone()));
        cx.update(|cx| {
            let state = AppState::global(cx);
            thundoku_core::google::delete_saved_profile(&state.secrets);
            assert!(
                state.google_profile.lock().is_none(),
                "保存前はプロフィールを持たない"
            );
            // プロフィール取得時（ログイン完了・設定画面を開いたとき）に保存される
            thundoku_core::google::save_profile(&state.secrets, &google_profile())
                .expect("保存できる");
        });

        // 次回起動相当（keyring から読み直す）
        cx.update(|cx| AppState::init_with_data_dir(cx, data_dir.clone()));

        cx.update(|cx| {
            let state = AppState::global(cx);
            let restored = state
                .google_profile
                .lock()
                .clone()
                .expect("プロフィールが復元されていない");
            assert_eq!(
                restored.sub, "sub-1",
                "所有者判定に使う sub が復元されていない"
            );
        });

        // メモリバックエンドはプロセス内で共有されるため後始末する
        cx.update(|cx| {
            thundoku_core::google::delete_saved_profile(&AppState::global(cx).secrets);
        });
        let _ = std::fs::remove_dir_all(&data_dir);
    }

    /// 保護期限は 5 秒（自動消滅と同じ）。
    #[test]
    fn notice_is_protected_only_within_the_window() {
        let now = std::time::Instant::now();
        assert!(!notice_is_protected(None, now), "保護していなければ素通し");
        assert!(
            notice_is_protected(Some(now + std::time::Duration::from_millis(1)), now),
            "期限内は保護する"
        );
        assert!(
            !notice_is_protected(Some(now), now),
            "期限ちょうどは保護しない（自動消滅と同じ）"
        );
        assert!(
            !notice_is_protected(Some(now), now + NOTICE_PROTECT),
            "5 秒経ったら上書きを許す"
        );
    }

    /// 保護つきの通知（タグ取得の完了など）は、進行中の通知に上書きされない。
    #[gpui_kit::test]
    async fn progress_notice_does_not_overwrite_a_protected_notice(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        cx.update(|cx| {
            set_protected_notice(
                cx,
                ToastKind::Success,
                "FANZA のタグ情報を 20 件取得しました",
            );
            set_progress_notice(cx, "ダウンロード中です…");
            let state = AppState::global(cx);
            assert_eq!(
                state.toast_message.lock().as_deref(),
                Some("FANZA のタグ情報を 20 件取得しました"),
                "進行中の通知が保護つき通知を消している"
            );
            assert!(
                !*state.toast_progress.lock(),
                "進行中のままになっている（スピナーが付く）"
            );
        });
    }

    /// 保護つき通知が無いときは、進行中の通知は今までどおり出る。
    #[gpui_kit::test]
    async fn progress_notice_is_shown_without_a_protected_notice(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        cx.update(|cx| {
            set_toast_kind(cx, ToastKind::Info, "本棚を更新しました");
            set_progress_notice(cx, "ダウンロード中です…");
            let state = AppState::global(cx);
            assert_eq!(
                state.toast_message.lock().as_deref(),
                Some("ダウンロード中です…"),
                "保護つきでないときは進行中の通知を止めてはいけない"
            );
            assert!(*state.toast_progress.lock(), "進行中の印が立っていない");
        });
    }

    /// 「消えない通知」（終了時アップロード）のあとでも、通常の通知は自動消滅に戻る。
    ///
    /// 生の `set_toast` を直に呼んでいた経路（Drive 同期の「同期完了」など）は
    /// `autohide = false` を引き継いでしまい、**通知が消えずに残り続けていた**。
    #[gpui_kit::test]
    async fn notice_after_a_sticky_one_autohides_again(cx: &mut gpui_kit::TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        cx.update(|cx| {
            set_sticky_progress_notice(cx, "アップロード中です…");
            assert!(!*AppState::global(cx).toast_autohide.lock());
            set_toast_kind(
                cx,
                ToastKind::Info,
                "同期完了（DL 1 / UL 0 / スキップ 0 / 競合 0）",
            );
            let state = AppState::global(cx);
            assert!(
                *state.toast_autohide.lock(),
                "消えない通知の設定が残っている（他と同じ時間で消えない）"
            );
            assert!(!*state.toast_progress.lock(), "進行中フラグが残っている");
        });
    }
}
