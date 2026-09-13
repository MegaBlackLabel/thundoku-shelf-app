pub mod about;
pub mod auth;
pub mod bookshelf;
pub mod booth_login;
pub mod checklist;
pub mod dlsite_login;
pub mod fanza_login;
pub mod google_login;
pub mod history;
pub mod reader;
pub mod settings;
pub mod tag_edit;
pub mod tbf_login;

/// ホバー中の背景色。
///
/// テーマの `muted` / `secondary` はダークで同色（どちらも 15%）なので、既に
/// `muted` を背景にしている要素ではホバーしても色が変わらない。背景と区別できる
/// よう、ダークは明るめ・ライトは濃いめの明示グレーを返す。
pub(crate) fn hover_bg(theme: &gpui_kit::component::Theme) -> gpui_kit::Rgba {
    match theme.mode {
        gpui_kit::component::ThemeMode::Dark => gpui_kit::rgb(0x3e3e3e),
        gpui_kit::component::ThemeMode::Light => gpui_kit::rgb(0xcfcfcf),
    }
}
