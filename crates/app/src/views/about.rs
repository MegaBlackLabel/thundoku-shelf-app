//! アプリの説明ページ（Web 版トップページ https://…/ のデスクトップ版）。
//!
//! Web 版の記載（ブラウザアプリ・OPFS・PWA インストール・Cookie セッション）
//! はネイティブアプリの実態に合わせて修正してある。

use gpui_kit::Styled as _;
use gpui_kit::StyledImage as _;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Icon;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::{
    Context, FontWeight, IntoElement, ParentElement, Render, SharedString, Window, div, px,
    relative,
};

use crate::icons::AppIcon;

pub struct AboutView;

impl AboutView {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self
    }
}

impl Render for AboutView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let _ = window;
        let card_bg = cx.theme().muted;
        let card_border = cx.theme().border;
        let muted_fg = cx.theme().muted_foreground;
        let primary = cx.theme().primary;
        let primary_fg = cx.theme().primary_foreground;

        div()
            .size_full()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(
                div()
                    .mx_auto()
                    .w(px(720.0))
                    .py_8()
                    .flex()
                    .flex_col()
                    .gap_8()
                    // ヘッダー（Web のロゴ + タイトル + サブタイトル）
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_3()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .w(px(64.0))
                                    .h(px(64.0))
                                    .rounded_xl()
                                    .overflow_hidden()
                                    .bg(primary)
                                    .child(
                                        if let Some(logo) = crate::views::bookshelf::app_logo_image()
                                        {
                                            gpui_kit::img(logo)
                                                .w(px(64.0))
                                                .h(px(64.0))
                                                .object_fit(gpui_kit::ObjectFit::Contain)
                                                .into_any_element()
                                        } else {
                                            Icon::new(AppIcon::BookMarked)
                                                .size(px(30.0))
                                                .text_color(primary_fg)
                                                .into_any_element()
                                        },
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_col()
                                    .child(
                                        div()
                                            .text_2xl()
                                            .font_weight(FontWeight::BOLD)
                                            .child("Thundoku Shelf"),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(muted_fg)
                                            .child(
                                                "ネットで購入した同人誌を管理・閲覧するためのデスクトップアプリ",
                                            ),
                                    ),
                            ),
                    )
                    // このアプリについて
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("このアプリについて"),
                            )
                            .child(
                                div()
                                    .rounded_xl()
                                    .border_1()
                                    .border_color(card_border)
                                    .bg(card_bg)
                                    .p_6()
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(muted_fg)
                                            .line_height(relative(1.7))
                                            .child(
                                                "Thundoku Shelf は、技術書典（TechBookFest）などのオンラインストアで購入した同人誌・技術書をデスクトップで管理・閲覧できるアプリです。購入履歴や書籍ファイルなどのデータはお使いの PC のローカルフォルダに保存されるため、外部サーバーに残ることはありません。",
                                            ),
                                    ),
                            ),
                    )
                    // ご利用前の注意
                    .child(
                        div()
                            .rounded_xl()
                            .border_1()
                            .border_color(gpui_kit::rgb(0xfcd34d))
                            .bg(gpui_kit::rgb(0xfefce8))
                            .p_6()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .px_2()
                                    .py_1()
                                    .rounded_md()
                                    .bg(gpui_kit::rgb(0xfef3c7))
                                    .child(
                                        div()
                                            .text_xs()
                                            .font_weight(FontWeight::MEDIUM)
                                            .text_color(gpui_kit::rgb(0x92400e))
                                            .child("ご利用前の注意"),
                                    ),
                            )
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(gpui_kit::rgb(0x78350f))
                                    								.child("Google ログインは Google ドライブでのバックアップに使用します"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(gpui_kit::rgb(0x92400e))
                                    .line_height(relative(1.7))
                                    								.child(
									"書籍を読むのに Google ログインは必要ありません。Google ログインを行うと、本棚の DB や画像を Google Drive にバックアップ・同期できます。サイドバーのアカウントアイコンからログインしてください。",
								),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(gpui_kit::rgb(0x92400e))
                                    .line_height(relative(1.7))
                                    .child(
                                        "技術書典で購入済みの書籍を同期する場合は、サイドバーまたは本棚の同期ボタンから技術書典にログイン（メールアドレスとパスワード）してください。",
                                    ),
                            ),
                    )
                    // 主な機能
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("主な機能"),
                            )
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .flex_wrap()
                                    .gap_4()
                                    .children(
                                        [
                                            (
                                                "本棚",
                                                AppIcon::LibraryBig,
                                                "購入済み書籍の一覧管理。未読・読書中・読了のステータス管理、タグ付けによる分類、一括検索ができます。",
                                            ),
                                            (
                                                "PDF リーダー",
                                                AppIcon::BookMarked,
                                                "アプリ内で PDF を閲覧。ページめくり、拡大縮小、見開き表示、読書進捗の自動保存・管理ができます。",
                                            ),
                                            (
                                                "チェックリスト",
                                                AppIcon::ListChecks,
                                                "イベントごとの購入管理。Google ログイン後に同期ボタンから技術書典と同期、お気に入り登録、試し読み機能付き。",
                                            ),
                                            (
                                                "ダウンロード管理",
                                                AppIcon::Download,
                                                "技術書典からのダウンロード状況を一元管理。完了・未完了・エラーの可視化と再試行ができます。",
                                            ),
                                            (
                                                "Google Drive 同期",
                                                AppIcon::HardDrive,
                                                "書籍ファイルを Google Drive と双方向同期。別の PC や Web 版と本棚を共有できます。",
                                            ),
                                            (
                                                "自動タグ生成",
                                                AppIcon::Tag,
                                                "ダウンロード時に自動でタグを生成したり、手動でタグを追加・編集したりできます。形態素解析による名詞抽出で、書籍の分類・検索を強力にサポートします。",
                                            ),
                                        ]
                                        .into_iter()
                                        .map(|(title, icon, desc)| {
                                            let title: SharedString = title.into();
                                            let desc: SharedString = desc.into();
                                            div()
                                                .flex()
                                                .flex_col()
                                                .gap_2()
                                                .w(px(344.0))
                                                .rounded_xl()
                                                .border_1()
                                                .border_color(card_border)
                                                .bg(card_bg)
                                                .p_4()
                                                                                                            .child(
                                                                div()
                                                                    .flex()
                                                                    .flex_row()
                                                                    .items_center()
                                                                    .gap_2()
                                                                    .child(
                                                                        div()
                                                                            .flex()
                                                                            .items_center()
                                                                            .justify_center()
                                                                            .w(px(32.0))
                                                                            .h(px(32.0))
                                                                            .rounded_lg()
                                                                            .bg(gpui_kit::white())
                                                                            .child(
                                                                                Icon::new(icon)
                                                                                    .size(px(18.0))
                                                                                    .text_color(gpui_kit::rgb(0x1d4ed8)),
                                                                            ),
                                                                    )
                                                                    .child(
                                                                        div()
                                                                            .text_sm()
                                                                            .font_weight(FontWeight::MEDIUM)
                                                                            .child(title),
                                                                    ),
                                                            )
                                                .child(
                                                    div()
                                                        .text_sm()
                                                        .text_color(muted_fg)
                                                        .line_height(relative(1.6))
                                                        .child(desc),
                                                )
                                        }),
                            ),
                    )
                    // Google OAuth の利用方法
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Google OAuth の利用方法"),
                            )
                            .child(
                                div()
                                    .rounded_xl()
                                    .border_1()
                                    .border_color(card_border)
                                    .bg(card_bg)
                                    .p_6()
                                    .flex()
                                    .flex_col()
                                    .gap_4()
                                    .child(
                                        div()
                                            .flex()
                                            .flex_row()
                                            .gap_3()
                                            .child(
                                                div()
                                                    .flex()
                                                    .items_center()
                                                    .justify_center()
                                                    .w(px(32.0))
                                                    .h(px(32.0))
                                                    .rounded_lg()
                                                    .bg(primary.opacity(0.1))
                                                    .child(
                                                        Icon::new(AppIcon::BookMarked)
                                                            .size(px(16.0))
                                                            .text_color(primary),
                                                    ),
                                            )
                                            .child(
                                                div()
                                                    .flex()
                                                    .flex_col()
                                                    .gap_2()
                                                    .child(
                                                        div()
                                                            .text_sm()
                                                            .font_weight(FontWeight::MEDIUM)
                                                            .child("ログイン手順"),
                                                    )
                                                    .child(
                                                        div()
                                                            .flex()
                                                            .flex_col()
                                                            .gap_2()
                                                            .children(
                                                                [
                                                                    "1. サイドバー下部のアカウントアイコンをクリック",
                                                                    "2. 「Google でログイン」を選択",
                                                                    "3. システムブラウザで Google アカウントを認証",
                                                                    "4. 認証が完了すると自動的にログイン状態が保存されます",
                                                                ]
                                                                .into_iter()
                                                                .map(|step| {
                                                                    div()
                                                                        .text_sm()
                                                                        .text_color(muted_fg)
                                                                        .child(step)
                                                                }),
                                                            ),
                                                    ),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .border_t_1()
                                            .border_color(card_border)
                                            .pt_4()
                                            .flex()
                                            .flex_col()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .child("データ同期について"),
                                            )
                                            .children(
                                                [
                                                    "Google ログイン後、本棚とチェックリストで技術書典のデータを同期できます",
                                                    "複数デバイス間で購入履歴を共有できます（Google Drive 同期）",
                                                    "同期は各画面の同期ボタンから開始します",
                                                ]
                                                .into_iter()
                                                .map(|item| {
                                                    div()
                                                        .text_sm()
                                                        .text_color(muted_fg)
                                                        .child(format!("・{item}"))
                                                }),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .border_t_1()
                                            .border_color(card_border)
                                            .pt_4()
                                            .flex()
                                            .flex_col()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .child("取得される情報"),
                                            )
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .text_color(muted_fg)
                                                    .child(
                                                        "Google OAuth 認証により、以下の情報が取得・保存されます：",
                                                    ),
                                            )
                                            .children(
                                                [
                                                    "メールアドレス（アカウント識別用）",
                                                    "表示名（プロフィール表示用）",
                                                    "プロフィール画像（オプション）",
                                                ]
                                                .into_iter()
                                                .map(|item| {
                                                    div()
                                                        .text_sm()
                                                        .text_color(muted_fg)
                                                        .child(format!("・{item}"))
                                                }),
                                            )
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .text_color(muted_fg)
                                                    .child(
                                                        "これらの情報は認証目的のみに使用し、第三者に提供することはありません。",
                                                    ),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .border_t_1()
                                            .border_color(card_border)
                                            .pt_4()
                                            .flex()
                                            .flex_col()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .child("ログアウト"),
                                            )
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .text_color(muted_fg)
                                                    .child(
                                                        "アカウントメニューから「ログアウト」を選択すると、ローカルの認証情報が削除されます。再ログインするまで同期機能は無効になりますが、ローカルのデータは残ります。",
                                                    ),
                                            ),
                                    ),
                            ),
                    )
                    // 技術書典ログインについて（Web の Cookie 手順の置き換え）
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_4()
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("技術書典ログインの手順"),
                            )
                            .child(
                                div()
                                    .rounded_xl()
                                    .border_1()
                                    .border_color(card_border)
                                    .bg(card_bg)
                                    .p_6()
                                    .flex()
                                    .flex_col()
                                    .gap_3()
                                    .children(
                                        [
                                            "1. PC 版技術書典サイトで、同期したいアカウントでログイン済みであることを確認する",
                                            "2. サイドバー下部のアカウントアイコン、または本棚のログインダイアログで「技術書典でログイン」を選ぶ",
                                            "3. 技術書典のメールアドレスとパスワードを入力してログインする",
                                            "4. 送信するとセッション登録が完了します",
                                        ]
                                        .into_iter()
                                        .map(|step| {
                                            div()
                                                .text_sm()
                                                .text_color(muted_fg)
                                                .child(step)
                                        }),
                                    )
                                    .child(
                                        div()
                                            .border_t_1()
                                            .border_color(card_border)
                                            .pt_3()
                                            .child(
                                                div()
                                                    .text_sm()
                                                    .text_color(muted_fg)
                                                    .child(
                                                        "※ 一度ログインすると、セッションは自動的に保存され次回からは不要です。",
                                                    ),
                                            ),
                                    ),
                            ),
                    )
                    // フッター
                    .child(
                        div()
                            .border_t_1()
                            .border_color(card_border)
                            .pt_6()
                            .text_sm()
                            .text_color(muted_fg)
                            .child(
                                "Thundoku Shelf — データはすべてお使いの PC のローカルフォルダに保存されます。",
                            ),
                    ),
            ),
            )
    }
}
