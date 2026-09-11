//! 独自のフェード付き中央ダイアログ（gpui_component の Dialog の代替）。
//!
//! Web の中央モーダルに合わせ、フェードイン・中央配置・バツボタンなしで表示する。
//! gpui_component の `Dialog` は `slide-down`（上からスライド）アニメーションを
//! ライブラリ内部でハードコードしているため、ライブラリに依存せず
//! アプリ側でフェードアニメーションを実現する。

use std::time::Duration;

use gpui_kit::base::{Transition, transition};
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::animation::ease_out_cubic;
use gpui_kit::{
    App, BoxShadow, InteractiveElement as _, IntoElement, ParentElement, Styled as _, anchored,
    div, hsla, point, prelude::FluentBuilder as _, px,
};

/// ダイアログを表示する。`open` が `true` の間、中央にフェードイン表示する。
/// `content` にはダイアログ本体（`dialog_surface` で構築したサーフェス）を渡す。
pub fn fade_dialog(
    window: &mut gpui_kit::Window,
    cx: &mut App,
    open: bool,
    content: impl IntoElement,
) -> impl IntoElement {
    let progress = transition(
        ("app-fade-dialog", "fade"),
        if open { 1.0 } else { 0.0 },
        Transition::new(Duration::from_millis(200)).ease(ease_out_cubic),
        window,
        cx,
    );
    let overlay = cx.theme().colors.overlay;
    let view_size = window.viewport_size();
    gpui_kit::deferred(
        anchored().snap_to_window().child(
            div()
                .w(view_size.width)
                .h(view_size.height)
                .flex()
                .items_center()
                .justify_center()
                .bg(overlay)
                // ダイアログの外側（背景）のクリックを下の層へ伝えない。
                // これが無いと、下にある本棚のカードなどが同時に反応してしまう。
                .on_mouse_down(gpui_kit::MouseButton::Left, |_, _, cx| {
                    cx.stop_propagation();
                })
                .opacity(progress)
                .when(progress < 0.01, |this| this.invisible())
                .child(content),
        ),
    )
}

/// ダイアログ本体のサーフェス（タイトル・本文・フッターを載せる枠）。
/// テーマに追従するため `cx` から背景色・文字色を取得する。
///
/// 呼び出し側では `let mut surface = dialog_surface(cx);` と先に評価し、
/// `fade_dialog` に渡す（`cx` の借用を分離するため）。
pub fn dialog_surface(cx: &App) -> gpui_kit::Div {
    // ダークモードでは黒い影が背景（暗い）に沈んで見えないため、
    // Web のダークモード標準に合わせて白の薄い縁（ハイライト）で区別する。
    let is_dark = cx.theme().mode.is_dark();
    div()
        .flex()
        .flex_col()
        .gap_3()
        .p_5()
        .rounded_xl()
        .bg(cx.theme().colors.popover)
        .text_color(cx.theme().colors.popover_foreground)
        .border_1()
        .border_color(if is_dark {
            hsla(0., 0., 1., 0.18)
        } else {
            cx.theme().colors.border
        })
        .shadow(vec![
            // ダークモードでは黒影が背景に沈むため、白のハイライト縁を先頭に置く。
            // `0 0 0 1px rgba(255,255,255,…)` 相当で、背景との境界を明るく縁取る。
            BoxShadow {
                color: hsla(0., 0., 1., 0.14),
                offset: point(px(0.), px(0.)),
                blur_radius: px(1.),
                spread_radius: px(0.),
                inset: false,
            },
            // Web のモーダルで広く使われる shadow-2xl 相当の強いレイヤー影。
            // 背景（オーバーレイ）との区別を強化するため、大きめのオフセットと強い不透明度を使う。
            BoxShadow {
                color: hsla(0., 0., 0., 0.35),
                offset: point(px(0.), px(28.)),
                blur_radius: px(56.),
                spread_radius: px(0.),
                inset: false,
            },
            BoxShadow {
                color: hsla(0., 0., 0., 0.25),
                offset: point(px(0.), px(10.)),
                blur_radius: px(24.),
                spread_radius: px(0.),
                inset: false,
            },
            BoxShadow {
                color: hsla(0., 0., 0., 0.2),
                offset: point(px(0.), px(3.)),
                blur_radius: px(8.),
                spread_radius: px(0.),
                inset: false,
            },
        ])
        .w(px(560.0))
}
