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
/// ダイアログの面は「浮いた面」(`popover`) に統一した。背景と同色のままだと、
/// ダークでは膜と影がほとんど効かず境界がヘアライン頼みになる（実測 1.02:1）。
/// 背景より 4〜8 明度ポイント明るくすると境界が面そのもので分かる。
///
/// モード切替（[`Theme::change`]）がこの設定を読むので、切替より前に呼ぶこと
/// （[`crate::workspace::Workspace::new`] が保存済みモードの適用前に呼ぶ）。
pub fn apply_dark_surfaces(cx: &mut App) {
    {
        let theme = Theme::global_mut(cx);
        let mut dark = (*theme.dark_theme).clone();
        dark.colors.popover = Some("neutral-900".into());
        dark.colors.border = Some("neutral-600".into());
        dark.colors.overlay = Some("#0000008C".into()); // 55%（標準モーダルの推奨帯）
        // 枠線（入力欄・選択欄・`border_1()` を付けたボタン）を面から見えるようにする
        dark.colors.input = Some("neutral-600".into());
        theme.dark_theme = Rc::new(dark);
    }
    // すでにダークなら、いま画面に出ている色にも反映する。
    // （モード切替の前に呼ばれた場合は設定を書き換えるだけでよい）
    if Theme::global(cx).mode.is_dark() {
        Theme::change(ThemeMode::Dark, None, cx);
    }
}

/// ライトの面の階層をアプリの意図に合わせる。
///
/// 既定のライトは膜（`overlay`）が 5% しかない。ダイアログの面は背景と同じ白なので、
/// 膜・縁・影のうち膜がほぼ効かず、確認ダイアログが背景に同化して気づかない
/// （面 #ffffff vs 膜後の背景 #f2f2f2 = 明度差 0.05 未満）。
///
/// ライトは「面を背景より明るくする」方向に余地が無い（面はすでに白）ので、
/// **膜を濃くする**のが効く。加えて縁も一段濃くする（既定 #e5e5e5 は白い面の上で
/// ほとんど見えない）。
pub fn apply_light_surfaces(cx: &mut App) {
    {
        let theme = Theme::global_mut(cx);
        let mut light = (*theme.light_theme).clone();
        // 膜は「確認ダイアログの背面を沈める」ためのもの。薄いと面が浮かない
        light.colors.overlay = Some("#00000080".into());
        // 縁は面（白）の上でも見える濃さに
        light.colors.border = Some("neutral-300".into());
        light.colors.input = Some("neutral-300".into());
        theme.light_theme = Rc::new(light);
    }
    if !Theme::global(cx).mode.is_dark() {
        Theme::change(ThemeMode::Light, None, cx);
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
            // ダイアログの面（`popover`）と、その上のボタン（既定 variant）の明度差
            let button = (theme.colors.button.l - theme.colors.popover.l).abs();
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
    /// ダイアログの面は「浮いた面」(`popover`)。ボタンの面（`button` /
    /// `button_secondary`）がそれと同じ明るさだと、キャンセル / 保存せずに終了が
    /// 面に溶けて読めなくなる（比較対象は背景ではなく**ダイアログの面**）。
    #[gpui_kit::test]
    async fn dark_dialog_buttons_do_not_blend_into_the_surface(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        let (_, button, _) = dark_gaps(cx);
        let secondary = cx.update(|cx| {
            apply_dark_surfaces(cx);
            Theme::change(ThemeMode::Dark, None, cx);
            let theme = cx.theme();
            (theme.colors.button_secondary.l - theme.colors.popover.l).abs()
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

    /// 膜（overlay）を重ねた背景の明度。
    fn scrimmed(background: gpui_kit::Hsla, overlay: gpui_kit::Hsla) -> f32 {
        overlay.l * overlay.a + background.l * (1.0 - overlay.a)
    }

    /// ライト: ダイアログの面が、膜を重ねた背景から浮いて見えること。
    ///
    /// 既定のライトは膜が 5% しかなく、面も背景と同じ白なので、確認ダイアログが
    /// 背景と同化して気づかない（面 #ffffff vs 膜後の背景 #f2f2f2）。暗くして浮かせる
    /// 以外に手が無いモードなので、膜の強さが効く。
    #[gpui_kit::test]
    async fn light_dialog_surface_stands_out_from_the_scrimmed_page(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        let (gap, overlay) = cx.update(|cx| {
            apply_light_surfaces(cx);
            Theme::change(ThemeMode::Light, None, cx);
            let theme = cx.theme();
            let page = theme.colors.background;
            let dialog = theme.colors.popover;
            let overlay = theme.colors.overlay;
            ((dialog.l - scrimmed(page, overlay)).abs(), overlay.a)
        });
        assert!(
            overlay >= 0.4,
            "ライト: 膜が薄すぎてダイアログが背景から浮かない（不透明度 {overlay:.2}）"
        );
        assert!(
            gap >= 0.05,
            "ライト: ダイアログの面が膜を重ねた背景と同化する（明度差 {gap:.3}）"
        );
    }

    /// ライト: 縁取りが背景・カード面・浮いた面のどこでも見えること。
    #[gpui_kit::test]
    async fn light_borders_are_visible_on_every_surface(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        let surfaces = cx.update(|cx| {
            apply_light_surfaces(cx);
            Theme::change(ThemeMode::Light, None, cx);
            let theme = cx.theme();
            let border = theme.colors.border;
            [
                ("背景", theme.colors.background, border),
                ("カード", theme.colors.muted, border),
                ("浮いた面", theme.colors.popover, border),
            ]
        });
        for (name, surface, border) in surfaces {
            let gap = border_gap(surface, border);
            assert!(
                gap >= 0.05,
                "ライト: 縁取りが{name}に溶けている（明度差 {gap:.3}）"
            );
        }
    }
}
