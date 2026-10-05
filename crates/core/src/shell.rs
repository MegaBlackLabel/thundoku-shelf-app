//! OS のシェルに「開く」を頼む（URL / フォルダ）。
//!
//! Windows は **`ShellExecuteW`（OS の API）**を使う。**コマンドライン経由にしない**:
//! `cmd /C start "" <target>` は `cmd` が `&` をコマンド区切り、`%XX` を環境変数として
//! 解釈するため、URL が最初の `&` で切れて渡る（実測: `cmd /C echo <URL>` の出力は
//! `?client_id=…` まで）。`explorer <URL>` も不可（URL を渡すとエクスプローラーが開くだけで
//! 既定ブラウザが開かない。実測 2026-09-23）。`ShellExecuteW` は対象をそのままシェルへ渡す。
//! **フォルダも同じ API で開く**（`"open"` でエクスプローラーが開く）。
//!
//! macOS は `open`、Linux は `xdg-open`。
//!
//! **`update`（描画）の中で呼ばない**: Windows の `ShellExecuteW` はシェルの
//! メッセージループを回し、保留中のタスクが再入して `RefCell already borrowed` で
//! アプリが落ちる（実測 2026-09-30）。背景スレッドで呼び、結果だけ UI へ返す。

use std::path::Path;

/// シェルに `target`（URL / パス）を開かせる。
pub fn open_in_shell(target: &str) -> std::io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::UI::Shell::ShellExecuteW;
        use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

        let operation = wide("open");
        let target = wide(target);
        // SAFETY: 3 つの文字列はこの関数の間だけ生きる NUL 終端 UTF-16 で、
        // ポインタ引数は ShellExecuteW が呼び出し中しか読まない。
        let result = unsafe {
            ShellExecuteW(
                std::ptr::null_mut(),
                operation.as_ptr(),
                target.as_ptr(),
                std::ptr::null(),
                std::ptr::null(),
                SW_SHOWNORMAL,
            )
        };
        // ShellExecute の戻り値は「32 以下なら失敗」と決まっている。
        if (result as isize) <= 32 {
            return Err(std::io::Error::other(format!(
                "ShellExecuteW failed ({})",
                result as isize
            )));
        }
        Ok(())
    }
    #[cfg(not(target_os = "windows"))]
    {
        let program = if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        std::process::Command::new(program)
            .arg(target)
            .spawn()
            .map(|_| ())
    }
}

/// フォルダ（またはファイル）を OS のファイルマネージャーで開く。
///
/// 開けないとき（パスが無い等）は `Err`。Windows は戻り値が 32 以下なら失敗。
pub fn open_path(path: &Path) -> std::io::Result<()> {
    open_in_shell(&path.to_string_lossy())
}

/// NUL 終端の UTF-16（Windows の `*W` API 用）。
#[cfg(target_os = "windows")]
fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}
