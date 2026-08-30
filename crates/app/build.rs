//! ビルド時リソース。
//!
//! Windows（x86_64-pc-windows-msvc, release）のときだけ、exe にアプリアイコンを
//! 埋め込む。macOS は .app バンドルの Info.plist / icon.icns で対応するため不要。
//! （debug ビルドは gpui_windows のシェーダーコンパイル（fxc.exe）が必要な関係で
//!   macOS クロスビルドでのみ作るため、ここでは release のみ有効にする。）

#[cfg(all(target_os = "windows", not(debug_assertions)))]
fn main() {
    let mut res = winres::WindowsResource::new();
    res.set_icon("assets/app-icon/icon.ico");
    res.compile()
        .expect("failed to compile Windows resources (icon.ico)");
}

#[cfg(not(all(target_os = "windows", not(debug_assertions))))]
fn main() {}
