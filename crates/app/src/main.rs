//! Thundoku Shelf desktop binary.

// Windows ではコンソール（黒い cmd 窓）を出さない（GUI サブシステム）。
#![cfg_attr(windows, windows_subsystem = "windows")]

use gpui_kit::component::*;
use gpui_kit::*;

use thundoku_core::single_instance::{InstanceError, InstanceGuard};
use thundoku_shelf::app_state::AppState;
use thundoku_shelf::icons::AppAssets;
use thundoku_shelf::workspace::Workspace;

fn main() {
    // Windows で WebView（gpui-wry）を描画させるには DirectComposition を
    // 無効化する必要がある（gpui-component examples/webview に準拠）。
    // 無効化しないと WebView2 子ウィンドウが GPUI ウィンドウに正しく表示されず、
    // 技術書典・BOOTH・Google のログイン画面が「空」になる。
    #[cfg(windows)]
    unsafe {
        std::env::set_var("GPUI_DISABLE_DIRECT_COMPOSITION", "true");
    }

    // サンプル（デモ）モード（`--demo` / `THUNDOKU_DEMO=1`）。メモリ内 DB と一時
    // ディレクトリだけで動き、ネットワークへは出ない。単一インスタンスのロック
    // （下）とデータの保存先（`AppState::init_demo`）を本物の起動と混ぜないよう、
    // **何よりも先に**決める。
    let demo = thundoku_shelf::demo::enabled(
        &std::env::args().collect::<Vec<_>>(),
        std::env::var("THUNDOKU_DEMO").ok().as_deref(),
    );

    // 2 重起動を防ぐ（macOS / Windows / Linux 共通。実体は OS のファイルロック）。
    // ログの初期化より先に判定する: 下のログ初期化は File::create で切り詰めるため、
    // 2 個目のプロセスが起動中インスタンスのログを壊してしまう。ロックは main の
    // 間ずっと保持する（プロセスが終了すれば OS が解放する）。
    let _instance_guard: Option<InstanceGuard> = match instance_lock_path(demo) {
        Some(lock_path) => match InstanceGuard::acquire(&lock_path) {
            Ok(guard) => Some(guard),
            Err(InstanceError::AlreadyRunning(_)) => {
                // 既に起動している。2 個目は何もせず静かに終了する。
                note_second_launch(&lock_path, demo);
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

    // ログは**データディレクトリ配下**（`<data_dir>/logs/thundoku.log`）に出す。
    // レポート画面が同じ場所を表示し、「ログの格納先を開く」で開く（issue #8）。
    // 初期化は単一インスタンスの判定より**後**（ログ初期化が大きすぎるファイルを
    // 切り詰めるため、2 個目の起動が起動中インスタンスのログを壊すのを防ぐ）。
    thundoku_shelf::logging::init(&log_data_dir(demo));

    // 調査用の UA 差し替え（`thundoku_core::ua`）が効いている状態で起動したかを残す。
    // ストアの同期が失敗したとき、「差し替えた状態で走らせたのか」をログだけで確定できる
    // ようにする（実機確認のときに最初に見る行）。
    for key in [
        thundoku_core::ua::ENV_DLSITE,
        thundoku_core::ua::ENV_FANZA,
        thundoku_core::ua::ENV_BOOTH,
        thundoku_core::ua::ENV_COVER,
    ] {
        if let Ok(value) = std::env::var(key)
            && !value.trim().is_empty()
        {
            log::info!("UA override: {key}={value}");
        }
    }

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
            cx.activate(true);

            // サンプル（デモ）モードはメモリ内 DB + 一時ディレクトリで起動する
            // （本物のデータディレクトリと keyring には触らない）。
            if demo {
                AppState::init_demo(cx);
            } else {
                AppState::init(cx);
            }
            // 起動時の状態をメニューに反映する（技術書典にログイン済みなら
            // 「チェックリスト」を有効にする）。
            thundoku_shelf::workspace::sync_app_menus(cx);

            cx.spawn(async move |cx| {
                // 前回のウィンドウ配置・サイズがあれば復元する（macOS / Linux / Windows 共通）
                let saved_bounds =
                    cx.update(|cx| thundoku_shelf::app_state::load_window_bounds(cx));
                let mut options = TitleBar::window_options();
                match saved_bounds {
                    Some(bounds) => options.window_bounds = Some(bounds),
                    // サンプル（デモ）モードは撮影用に広めの既定サイズで開く
                    // （保存済みがあればそちらを優先する）。
                    None if demo => {
                        let bounds = cx.update(|cx| {
                            gpui_kit::WindowBounds::Windowed(gpui_kit::Bounds::centered(
                                None,
                                gpui_kit::Size {
                                    width: gpui_kit::px(1600.0),
                                    height: gpui_kit::px(1000.0),
                                },
                                cx,
                            ))
                        });
                        options.window_bounds = Some(bounds);
                    }
                    None => {}
                }
                // 本の上のツールバー（メニュー）が隠れる幅まで狭められないようにする。
                // ディスプレイがその幅より狭いときは画面幅で止める（画面外へはみ出すと
                // 何も操作できなくなるため）。高さは制限しない。
                let min_width = cx.update(|cx| {
                    let needed = thundoku_shelf::views::bookshelf::MIN_WINDOW_WIDTH;
                    cx.displays()
                        .first()
                        .map(|display| {
                            gpui_kit::px(display.visible_bounds().size.width.as_f32().min(needed))
                        })
                        .unwrap_or(gpui_kit::px(needed))
                });
                options.window_min_size = Some(gpui_kit::Size {
                    width: min_width,
                    height: gpui_kit::px(0.0),
                });
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
///
/// サンプル（デモ）モードはファイル名を分ける: 本物の起動中インスタンスと同じ
/// ロックを使うと、デモを見ようとしただけで「既に起動している」と判定されて
/// 何も出ずに終了してしまう（データも別なので、同時に動いて問題ない）。
fn instance_lock_path(demo: bool) -> Option<std::path::PathBuf> {
    let name = if demo {
        "instance-demo.lock"
    } else {
        "instance.lock"
    };
    dirs::config_dir().map(|dir| dir.join("thundoku-shelf").join(name))
}

/// ログの出力先になるデータディレクトリ。
///
/// サンプル（デモ）モードは一時領域（[`thundoku_shelf::demo::data_dir`]）に出す:
/// 本物のデータディレクトリに書くと、サンプルを見ただけで実ログを切り詰めてしまう
/// （`logging::init` は大きすぎるファイルを `File::create` で作り直す）。
fn log_data_dir(demo: bool) -> std::path::PathBuf {
    if demo {
        thundoku_shelf::demo::data_dir()
    } else {
        thundoku_shelf::app_state::resolve_data_dir()
    }
}

/// 2 重起動を検知したことをログに残す。
///
/// 起動中インスタンスが使っているログを切り詰めないよう、**追記**で書く
/// （`logging::init` はここでは呼ばない: 2 個目はロガーを初期化せずに終了する）。
/// ファイルに書けなければ標準エラーへ出す（開発時に見えるように）。
fn note_second_launch(lock_path: &std::path::Path, demo: bool) {
    let log_path = thundoku_shelf::logging::log_file(&log_data_dir(demo));
    let message = format!(
        "既に別のインスタンスが起動しているため終了します（lock: {}）",
        lock_path.display()
    );
    // 追記できないときは標準エラーへ出す（開発時に見えるように）
    if !thundoku_shelf::logging::append_line(&log_path, &message) {
        eprintln!("{message}");
    }
}

#[cfg(test)]
mod tests {
    // `use super::*` は使わない: `main.rs` は `gpui_kit::*` を glob 輸入していて、
    // その中の `test` マクロが std の `#[test]` を覆い隠す（展開が再帰してコンパイルできない）。
    // 対象は 2 つだけなので、`super::` で明示して呼ぶ。

    /// サンプル（デモ）モードのログとロックは本物の起動と分ける。
    ///
    /// ここを間違えると、デモを見ただけで本物のログが切り詰められたり
    /// 「既に起動している」と判定されたりする（`main.rs` の起動判定はテストが無いため、
    /// この分岐だけは直接検証しておく）。
    #[test]
    fn demo_logs_and_locks_do_not_collide_with_the_real_run() {
        assert!(
            super::log_data_dir(true).starts_with(thundoku_shelf::demo::data_dir()),
            "サンプルのログが一時領域の外に出ている"
        );
        assert_ne!(
            super::log_data_dir(true),
            super::log_data_dir(false),
            "サンプルのログが本物のデータディレクトリに混ざっている"
        );
        assert_ne!(
            super::instance_lock_path(true),
            super::instance_lock_path(false),
            "サンプルのロックが本物の起動と共有されている"
        );
    }
}
