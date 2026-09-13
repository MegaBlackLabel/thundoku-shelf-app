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

/// 対象コンテンツの注意。**最も誤解されやすい点**なので説明画面の上部で強調して出す。
const TARGET_NOTICE: &str = "本アプリが対象にするのは、購入済み・DRM の無い（非DRM）同人誌のみです。\
                             購入していないコンテンツや、DRM で保護されたコンテンツは取り込めません。";

/// ログイン手順の見出し（技術書典だけでなく各ストア共通の手順）。
const LOGIN_SECTION_TITLE: &str = "ストアのログイン手順";

/// 説明画面の「主な機能」に出す項目（タイトル / アイコン / 説明）。
/// **実装済みの機能をここに並べる**（テスト `about_lists_the_implemented_features` が
/// 主要機能の記載漏れを防ぐ）。
const FEATURES: [(&str, AppIcon, &str); 13] = [
    (
        "本棚",
        AppIcon::LibraryBig,
        "購入済み書籍をカードとリストの 2 表示で一覧管理。未読・読書中・読了のステータス、\
         タグ、サイト・イベント・お気に入りでの絞り込み、並び替え、キーワード検索に対応。\
         操作の結果は右上の通知で確認できます。",
    ),
    (
        "ビューアー",
        AppIcon::BookOpen,
        "PDF・画像（ZIP）・EPUB をアプリ内で閲覧。単一・見開き・スクロールの 3 表示、拡大縮小、\
         ページ一覧とコンテンツ（PDF版 / 画像版）の切替、読書進捗の自動保存に対応。",
    ),
    (
        "付箋",
        AppIcon::StickyNote,
        "ページ単位のメモ。リーダーのページ右上の付箋アイコンで付け外しでき、外してもメモは\
         残ります。付箋画面ではページ画像つきの一覧とキーワード検索ができます。",
    ),
    (
        "閲覧履歴",
        AppIcon::History,
        "読んだ日ごとに集約した履歴。期間とサイトで絞り込み、カード・リストの 2 表示で見られます。",
    ),
    (
        "並び替えと絞り込み",
        AppIcon::List,
        "購入日・発売日・タイトル・最終閲覧日・閲覧回数・閲覧時間・サイズの 7 項目を昇順・降順で\
         並び替え。データが無い項目はメニューに出しません。",
    ),
    (
        "読書統計",
        AppIcon::Database,
        "1 ページごとの閲覧回数・滞在時間と、書籍ごとの閲覧セッションを記録。閲覧回数・累計閲覧\
         時間・最終閲覧を統計として並び替えに使えます。",
    ),
    (
        "チェックリスト",
        AppIcon::ListChecks,
        "イベントごとの購入管理。**現在は技術書典専用**（他ストアのイベントには未対応）。技術書典と\
         同期、お気に入り登録、試し読み機能付き。",
    ),
    (
        "ダウンロード管理",
        AppIcon::Download,
        "各ストア（技術書典・BOOTH・FANZA・DLsite）からのダウンロード状況を一元管理。\
         完了・未完了・エラーの可視化と再試行ができます。",
    ),
    (
        "Google Drive 同期",
        AppIcon::HardDrive,
        "書籍ファイルと本棚の DB を Google Drive と双方向同期。別の PC や Web 版と本棚を\
         共有できます。",
    ),
    (
        "関連書籍のショートカット",
        AppIcon::ArrowRight,
        "リスト行から同じサークル・同じ作者の本へすぐ移動できるショートカット。未ダウンロードの本は\
         確認してから取り込み、完了後にそのまま開きます。",
    ),
    (
        "自動タグ生成",
        AppIcon::Tag,
        "タグの追加・編集は全ストアで使えます。**自動生成は技術書典の同期時のみ**（他ストアは手動）。\
         形態素解析による名詞抽出で、書籍の分類・検索を強力にサポートします。",
    ),
    (
        "複数アカウントの切替",
        AppIcon::CircleUserRound,
        "Google アカウントを切替できます。データはアカウントごとに保持・属性付けされるため、\
         切替で消えることはありません。",
    ),
    (
        "対応ストア",
        AppIcon::Cloud,
        "技術書典・BOOTH・FANZA・DLsite の購入本を同期・管理。ストアごとにログインして本棚へ\
         取り込めます。",
    ),
];

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
                    // 対象コンテンツの注意（最も誤解されやすい点。上部で強調する）
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .rounded_xl()
                            .border_1()
                            .border_color(cx.theme().warning.opacity(0.35))
                            .bg(cx.theme().warning.opacity(0.10))
                            .p_4()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(cx.theme().warning)
                                    .child("対象は購入済み・非DRM の同人誌のみです"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(muted_fg)
                                    .line_height(relative(1.7))
                                    .child(TARGET_NOTICE),
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
                                        FEATURES.into_iter()
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
                                                    "BOOTH・FANZA・DLsite も、それぞれのログインから同期できます（本棚の同期ボタンから）",
                                                    "複数の Google アカウントを切替できます（データはアカウントごとに保持されます）",
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
                                    .child(LOGIN_SECTION_TITLE),
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
                                            "1. 取り込みたいストアで、同期したいアカウントとしてログイン済みか確認する（技術書典は PC 版サイト）",
                                            "2. サイドバー下部のアカウントアイコン、または本棚のログインダイアログで「〜でログイン」を選ぶ",
                                            "3. そのストアの認証情報（メールアドレスとパスワードなど）を入力する",
                                            "4. 送信するとセッション登録が完了します",
                                            "対応ストア: 技術書典 / BOOTH / FANZA / DLsite（ストアごとにログインしてください）",
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

#[cfg(test)]
mod tests {
    use super::*;

    fn description(title: &str) -> &'static str {
        FEATURES
            .iter()
            .find(|(name, _, _)| *name == title)
            .map(|(_, _, desc)| *desc)
            .unwrap_or_else(|| panic!("「{title}」が説明画面に無い"))
    }

    /// 説明画面の「主な機能」が実装済みの機能を網羅していること。
    /// （説明画面は利用者向けの機能一覧なので、実装との乖離はそのまま誤情報になる）
    #[test]
    fn about_lists_the_implemented_features() {
        let titles: Vec<&str> = FEATURES.iter().map(|(title, _, _)| *title).collect();
        for expected in [
            "本棚",
            "ビューアー",
            "付箋",
            "閲覧履歴",
            "並び替えと絞り込み",
            "読書統計",
            "チェックリスト",
            "ダウンロード管理",
            "Google Drive 同期",
            "関連書籍のショートカット",
            "自動タグ生成",
            "複数アカウントの切替",
            "対応ストア",
        ] {
            assert!(
                titles.contains(&expected),
                "説明画面に「{expected}」が無い: {titles:?}"
            );
        }
        assert!(
            !titles.contains(&"リーダー"),
            "「リーダー」表記が残っている（ビューアーに統一した）: {titles:?}"
        );
        assert!(
            !titles.iter().any(|title| title.contains("カルーセル")),
            "「カルーセル」表記が残っている（ショートカットに変更した）: {titles:?}"
        );
    }

    /// 古い記述（技術書典限定のダウンロード / PDF 限定のビューアー）が残っていないこと。
    #[test]
    fn about_describes_the_current_supported_formats_and_stores() {
        let all = FEATURES
            .iter()
            .map(|(title, _, desc)| format!("{title} {desc}"))
            .collect::<Vec<_>>()
            .join(" ");
        for keyword in ["EPUB", "スクロール", "BOOTH", "FANZA", "DLsite"] {
            assert!(all.contains(keyword), "説明文に {keyword} が無い: {all}");
        }
        assert!(
            !all.contains("技術書典からのダウンロード"),
            "ダウンロードの説明が技術書典限定のまま: {all}"
        );
        assert!(
            !all.contains("アプリ内で PDF を閲覧"),
            "ビューアーの説明が PDF 限定のまま: {all}"
        );
    }

    /// 対象が「購入済み・非DRM の同人誌」であることを強く明記していること。
    #[test]
    fn about_states_the_non_drm_scope() {
        assert!(
            TARGET_NOTICE.contains("非DRM"),
            "非DRM の明記が無い: {TARGET_NOTICE}"
        );
        assert!(
            TARGET_NOTICE.contains("購入") && TARGET_NOTICE.contains("同人誌"),
            "対象（購入済みの同人誌）が明記されていない: {TARGET_NOTICE}"
        );
        assert!(
            TARGET_NOTICE.contains("DRM"),
            "DRM 付きが対象外である旨が無い: {TARGET_NOTICE}"
        );
    }

    /// ストア限定の機能にはその旨を書く（チェックリストと自動タグ生成は技術書典のみ）。
    #[test]
    fn about_marks_store_specific_features() {
        assert!(
            description("チェックリスト").contains("技術書典専用"),
            "チェックリストが技術書典専用である旨が無い: {}",
            description("チェックリスト")
        );
        let tags = description("自動タグ生成");
        assert!(
            tags.contains("技術書典") && tags.contains("自動"),
            "自動タグ生成が技術書典のみである旨が無い: {tags}"
        );
        assert!(
            LOGIN_SECTION_TITLE.contains("ストア"),
            "ログイン手順の見出しが技術書典限定のまま: {LOGIN_SECTION_TITLE}"
        );
    }
}
