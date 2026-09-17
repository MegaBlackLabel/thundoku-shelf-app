//! Thundoku Shelf desktop application library. The binary entry point is
//! `main.rs`; everything else lives here so `#[gpui_kit::test]` can exercise it.

pub mod actions;
pub mod app_state;
pub mod components;
pub mod icons;
pub mod theme;
pub mod views;
pub mod workspace;

pub use workspace::Workspace;

#[cfg(test)]
mod icon_assets_tests {
    use std::sync::Arc;

    use gpui_kit::AssetSource as _;
    use gpui_kit::SvgRenderer;

    use crate::icons::AppAssets;

    /// サイドバー・ツールバーで使うアイコン SVG がバイナリに埋め込まれていて、
    /// 実際に描画できることを確認する（main.rs の with_assets 未設定だと
    /// ここが失敗する）。
    ///
    /// Web 版（lucide-react）から生成したカスタムアイコン + gpui-component
    /// 標準アイコンの両方を検証する。
    #[test]
    fn sidebar_icons_are_embedded_and_rendered() {
        let renderer = SvgRenderer::new(Arc::new(AppAssets));
        for path in [
            // Web 版と同じ lucide アイコン（カスタム埋め込み）
            "icons/book-marked.svg",
            "icons/library-big.svg",
            "icons/list-checks.svg",
            "icons/megaphone.svg",
            "icons/refresh-cw.svg",
            "icons/layout-grid.svg",
            "icons/list.svg",
            "icons/circle-user-round.svg",
            "icons/log-in.svg",
            "icons/log-out.svg",
            // gpui-component 標準アイコン（AppAssets 経由で解決される）
            "icons/settings.svg",
            "icons/search.svg",
            "icons/close.svg",
            "icons/circle-check.svg",
        ] {
            let bytes = AppAssets
                .load(path)
                .unwrap_or_else(|e| panic!("{path}: {e}"))
                .unwrap_or_else(|| panic!("{path} is not embedded"));
            assert!(!bytes.is_empty(), "{path} is empty");

            // SVG がパースできて、非透明ピクセルが生成される = 画面に描画される
            let image = renderer
                .render_single_frame(&bytes, 2.0)
                .unwrap_or_else(|e| panic!("{path} failed to render: {e}"));
            let bytes = image.as_bytes(0).expect("at least one frame");
            assert!(
                bytes.chunks_exact(4).any(|px| px[3] != 0),
                "{path} produced no visible pixels"
            );
        }
    }
}
