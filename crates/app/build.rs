//! ビルド時リソース。
//!
//! Windows（x86_64-pc-windows-msvc, release）のときだけ、exe にアプリアイコンを
//! 埋め込む。macOS は .app バンドルの Info.plist / icon.icns で対応するため不要。
//! （debug ビルドは gpui_windows のシェーダーコンパイル（fxc.exe）が必要な関係で
//!   macOS クロスビルドでのみ作るため、ここでは release のみ有効にする。）
//!
//! また Windows ではメインスレッドのスタックがデフォルト 1 MB しかなく、GPUI の
//! 深い描画再帰（ウィンドウ・ビュー構築）で STATUS_STACK_OVERFLOW になり起動即落ち
//! する。リンカの /STACK でスタック予約量を拡張して回避する（アプリは必ずメイン
//! スレッドで動かす必要があるため、スレッド起動ではなくここで対処する）。

#[cfg(target_os = "windows")]
fn main() {
    println!("cargo:rustc-link-arg-bins=/STACK:16777216,1048576");

    #[cfg(not(debug_assertions))]
    {
        let mut res = winres::WindowsResource::new();
        res.set_icon("assets/app-icon/icon.ico");
        res.compile()
            .expect("failed to compile Windows resources (icon.ico)");
    }
}

#[cfg(not(target_os = "windows"))]
fn main() {}
