//! アプリ側のテーマ補正。
//!
//! ライブラリ既定のダークパレットは「面」の階層が潰れていて、ダイアログ・通知が
//! 背景に溶ける。ここでアプリの意図（背景 → カード / 入力 → 浮いた面 → 縁）に
//! 引き直す。ライトは既定のままで問題ない。

use std::rc::Rc;

use gpui_kit::App;
use gpui_kit::component::{Theme, ThemeMode};

/// ダークの面の階層をアプリの意図に合わせる。
///
/// ライブラリ既定のダークは面の階層が潰れていて、ダイアログや通知が背景に溶ける:
///
/// | トークン | 既定 | 起きること |
/// |---|---|---|
/// | `popover` | neutral-950（背景と同色） | 通知・ポップオーバー・ダイアログが背景と見分け付かない |
/// | `border` | neutral-800 | カードやダイアログの面（同じ neutral-800）の上で縁が見えない |
/// | `overlay` | 黒 20% | ほぼ黒の背景を暗くしても面が浮かない |
/// | `input` | 背景とほぼ同色 | 入力欄・選択欄・縁付きボタンの枠線が見えない |
///
/// 背景（neutral-950）→ カード・入力（neutral-800）→ 浮いた面（neutral-900）→
/// 縁（neutral-700）の階層に引き直す。ライトは既定のままで問題ない。
///
/// ダイアログの面は背景色のまま（ライトと同じ流儀）で、縁と膜と影で浮かせる。
/// 面を明るくしすぎると、その上のボタン（既定 neutral-800 相当）が沈むため。
///
/// モード切替（[`Theme::change`]）がこの設定を読むので、切替より前に呼ぶこと
/// （[`crate::workspace::Workspace::new`] が保存済みモードの適用前に呼ぶ）。
pub fn apply_dark_surfaces(cx: &mut App) {
    {
        let theme = Theme::global_mut(cx);
        let mut dark = (*theme.dark_theme).clone();
        dark.colors.popover = Some("neutral-900".into());
        dark.colors.border = Some("neutral-700".into());
        dark.colors.overlay = Some("#00000073".into());
        // 枠線（入力欄・選択欄・`border_1()` を付けたボタン）を面から見えるようにする
        dark.colors.input = Some("neutral-700".into());
        theme.dark_theme = Rc::new(dark);
    }
    // すでにダークなら、いま画面に出ている色にも反映する。
    // （モード切替の前に呼ばれた場合は設定を書き換えるだけでよい）
    if Theme::global(cx).mode.is_dark() {
        Theme::change(ThemeMode::Dark, None, cx);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::TestAppContext;
    use gpui_kit::component::ActiveTheme as _;
    use gpui_kit::component::{Theme, ThemeMode};

    /// 面に縁を重ねたときの、面からの明度差（縁が見えるか）。
    fn border_gap(surface: gpui_kit::Hsla, border: gpui_kit::Hsla) -> f32 {
        let over = border.l * border.a + surface.l * (1.0 - border.a);
        (over - surface.l).abs()
    }

    /// ダーク: 浮いた面（ダイアログ・通知・ポップオーバー）と背景の明度差。
    fn dark_gaps(cx: &mut TestAppContext) -> (f32, f32, f32) {
        cx.update(|cx| {
            apply_dark_surfaces(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            let theme = cx.theme();
            let raised = theme.colors.popover.l - theme.colors.background.l;
            // ダイアログの面（背景色）と、その上のボタン（既定 variant）の明度差
            let button = (theme.colors.button.l - theme.colors.background.l).abs();
            (raised, button, theme.colors.overlay.a)
        })
    }

    /// ダーク: 浮いた面と背景の明度差が足りていること。
    ///
    /// 既定のダークは `popover` が背景と同じ neutral-950 で、通知もポップオーバーも
    /// 背景に溶ける。
    #[gpui_kit::test]
    async fn dark_raised_surfaces_stand_out_from_the_page(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        let (raised, _, overlay) = dark_gaps(cx);
        assert!(
            raised >= 0.05,
            "ダーク: 浮いた面が背景と見分け付かない（明度差 {raised:.3}）"
        );
        assert!(
            overlay >= 0.4,
            "ダーク: 膜が薄すぎて面が浮かない（不透明度 {overlay:.2}）"
        );
    }

    /// ダーク: ダイアログの面と、その上のボタン（キャンセル等）が同化しないこと。
    ///
    /// ダイアログの面は背景色。ボタンの面（`button` / `button_secondary`）が背景と
    /// 同じ明るさだと、キャンセル / 保存せずに終了が背景に溶けて読めなくなる。
    #[gpui_kit::test]
    async fn dark_dialog_buttons_do_not_blend_into_the_surface(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        let (_, button, _) = dark_gaps(cx);
        let secondary = cx.update(|cx| {
            apply_dark_surfaces(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            let theme = cx.theme();
            (theme.colors.button_secondary.l - theme.colors.background.l).abs()
        });
        for (name, gap) in [("button", button), ("button_secondary", secondary)] {
            assert!(
                gap >= 0.05,
                "ダーク: ダイアログの{name}が面に溶けている（明度差 {gap:.3}）"
            );
        }
    }

    /// ダーク: 縁取りが背景・カード面・浮いた面のどこでも見えること。
    ///
    /// 既定の `border` は neutral-800 で、カードやダイアログの面（同じ neutral-800）の
    /// 上では見えない。
    #[gpui_kit::test]
    async fn dark_borders_are_visible_on_every_surface(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        let surfaces = cx.update(|cx| {
            apply_dark_surfaces(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            let theme = cx.theme();
            let border = theme.colors.border;
            [
                ("背景", theme.colors.background, border),
                ("カード", theme.colors.muted, border),
                ("浮いた面", theme.colors.popover, border),
                ("入力欄の枠", theme.colors.background, theme.colors.input),
            ]
        });

        for (name, surface, border) in surfaces {
            let gap = border_gap(surface, border);
            assert!(
                gap >= 0.05,
                "ダーク: 縁取りが{name}に溶けている（明度差 {gap:.3}）"
            );
        }
    }
}
