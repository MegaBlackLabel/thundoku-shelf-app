//! Thundoku Shelf desktop binary.

// Windows ではコンソール（黒い cmd 窓）を出さない（GUI サブシステム）。
#![cfg_attr(windows, windows_subsystem = "windows")]

use gpui_kit::component::*;
use gpui_kit::*;

use thundoku_core::single_instance::{InstanceError, InstanceGuard};
use thundoku_shelf::app_state::AppState;
use thundoku_shelf::icons::AppAssets;
use thundoku_shelf::workspace::{Workspace, app_menus};

fn main() {
    // Windows で WebView（gpui-wry）を描画させるには DirectComposition を
    // 無効化する必要がある（gpui-component examples/webview に準拠）。
    // 無効化しないと WebView2 子ウィンドウが GPUI ウィンドウに正しく表示されず、
    // 技術書典・BOOTH・Google のログイン画面が「空」になる。
    #[cfg(windows)]
    unsafe {
        std::env::set_var("GPUI_DISABLE_DIRECT_COMPOSITION", "true");
    }

    // 2 重起動を防ぐ（macOS / Windows / Linux 共通。実体は OS のファイルロック）。
    // ログの初期化より先に判定する: 下のログ初期化は File::create で切り詰めるため、
    // 2 個目のプロセスが起動中インスタンスのログを壊してしまう。ロックは main の
    // 間ずっと保持する（プロセスが終了すれば OS が解放する）。
    let _instance_guard: Option<InstanceGuard> = match instance_lock_path() {
        Some(lock_path) => match InstanceGuard::acquire(&lock_path) {
            Ok(guard) => Some(guard),
            Err(InstanceError::AlreadyRunning(_)) => {
                // 既に起動している。2 個目は何もせず静かに終了する。
                note_second_launch(&lock_path);
                return;
            }
            // ロックファイルが開けないだけで起動を止めるのは避ける
            Err(err) => {
                eprintln!("単一インスタンスガードを取得できないため続行します: {err}");
                None
            }
        },
        None => {
            eprintln!("単一インスタンスガードを無効化します: config ディレクトリを解決できない");
            None
        }
    };

    // Windows（Wine/CrossOver）ではコンソール出力が抑制されるためファイルにも出す
    #[cfg(windows)]
    {
        // C:\ 直下は一般ユーザーが書き込めず `expect` でパニックし、
        // 起動直後にクラッシュする（Windows で起動しない原因）。
        // 書き込み可能な既知ディレクトリ（%TEMP%\thundoku-shelf）にログを出す。
        let log_dir = std::env::temp_dir().join("thundoku-shelf");
        let _ = std::fs::create_dir_all(&log_dir);
        let log_path = log_dir.join("thundoku.log");
        let hook_log = log_path.clone();
        std::panic::set_hook(Box::new(move |info| {
            use std::io::Write;
            if let Ok(mut f) = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&hook_log)
            {
                let _ = writeln!(f, "PANIC: {info}");
            }
        }));
        // ログファイルが開けなくてもアプリは起動を続ける（best-effort）。
        //
        // 追記（append）で開く: `File::create` は既存ログを切り詰めるため、
        // 2 個目の起動が 1 個目（起動中）のログを消してしまう。また非 append の
        // ハンドルは自分のオフセットに書き込むので、追記された行を後から
        // 上書きしてしまう。追記なら両プロセスの行が残る。
        // 増え続けないよう、大きくなったら起動時に捨てる。
        const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;
        if let Ok(meta) = std::fs::metadata(&log_path)
            && meta.len() > MAX_LOG_BYTES
        {
            let _ = std::fs::File::create(&log_path);
        }
        if let Ok(f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug"))
                .target(env_logger::Target::Pipe(Box::new(f)))
                .init();
        } else {
            env_logger::init();
        }
    }
    #[cfg(not(windows))]
    env_logger::init();

    gpui_kit::application()
        .with_assets(AppAssets)
        .run(move |cx| {
            // Must be called before using any GPUI Component features.
            gpui_kit::init(cx);
            cx.set_app_identity("com.megablacklabel.thundoku-shelf", "Thundoku Shelf");
            Theme::sync_system_appearance(None, cx);
            // カルーセルのスナップはテーマの `motion.spring_move`（既定 280ms / 減衰 0.85）で
            // 動くため、1 コマ送りのたびに鈍く感じる。移動系スプリングを短くして切れよくする
            // （カルーセル側に個別の設定は無く、テーマの値が唯一のつまみ）。
            Theme::global_mut(cx).motion.spring_move =
                gpui_kit::base::Spring::new(std::time::Duration::from_millis(120))
                    .with_damping(0.9);
            cx.set_menus(app_menus(true));
            cx.activate(true);

            AppState::init(cx);

            cx.spawn(async move |cx| {
                // 前回のウィンドウ配置・サイズがあれば復元する（macOS / Linux / Windows 共通）
                let saved_bounds =
                    cx.update(|cx| thundoku_shelf::app_state::load_window_bounds(cx));
                let mut options = TitleBar::window_options();
                if let Some(bounds) = saved_bounds {
                    options.window_bounds = Some(bounds);
                }
                cx.open_window(options, |window, cx| {
                    // ウィンドウを閉じる時に現在の配置・サイズを保存する。
                    // 終了時確認ダイアログを一度だけ出す（request_exit_upload_check）。
                    // 「キャンセル」後は再度確認を出すため、確認済みフラグは AppState で管理し、
                    // キャンセル時（cancel_exit_upload）に false へ戻す。
                    window.on_window_should_close(cx, move |window, cx| {
                        thundoku_shelf::app_state::save_window_bounds(window, cx);
                        // 閉じてよいかの判断は Workspace 側（アップロード中は閉じない等）
                        let workspace = thundoku_shelf::app_state::AppState::global(cx)
                            .workspace
                            .lock()
                            .clone();
                        match workspace.and_then(|weak| weak.upgrade()) {
                            Some(ws) => ws.update(cx, |ws, cx| ws.handle_window_close_request(cx)),
                            None => false,
                        }
                    });
                    let workspace = cx.new(Workspace::new);
                    // 終了時確認（on_window_should_close）から workspace にアクセスできるよう、
                    // AppState.workspace に弱参照を設定する。
                    *thundoku_shelf::app_state::AppState::global(cx)
                        .workspace
                        .lock() = Some(workspace.downgrade());
                    cx.new(|cx| Root::new(workspace, window, cx).bg(cx.theme().background))
                })
                .unwrap_or_else(|e| panic!("failed to open window: {e:?}"));
            })
            .detach();
        });
}

/// 単一インスタンスガードのロックファイル。
///
/// データ保存先（変更可能）ではなく config ディレクトリに置く。保存先を変更しても
/// 「同時に動くアプリは 1 つ」を保つため。
fn instance_lock_path() -> Option<std::path::PathBuf> {
    dirs::config_dir().map(|dir| dir.join("thundoku-shelf").join("instance.lock"))
}

/// 2 重起動を検知したことをログに残す。
///
/// Windows はコンソールを持たない（`windows_subsystem = "windows"`）ため stderr は
/// 見えない。起動中インスタンスが使っているログを切り詰めないよう、追記で書く。
fn note_second_launch(lock_path: &std::path::Path) {
    #[cfg(windows)]
    {
        use std::io::Write as _;
        let log_path = std::env::temp_dir()
            .join("thundoku-shelf")
            .join("thundoku.log");
        if let Ok(mut file) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
        {
            let _ = writeln!(
                file,
                "既に別のインスタンスが起動しているため終了します（lock: {}）",
                lock_path.display()
            );
        }
    }
    #[cfg(not(windows))]
    eprintln!(
        "既に別のインスタンスが起動しているため終了します（lock: {}）",
        lock_path.display()
    );
}
