//! アプリの説明ページ（Web 版トップページ https://…/ のデスクトップ版）。
//!
//! Web 版の記載（ブラウザアプリ・OPFS・PWA インストール・Cookie セッション）
//! はネイティブアプリの実態に合わせて修正してある。

use std::collections::HashSet;

use gpui_kit::InteractiveElement as _;
use gpui_kit::StatefulInteractiveElement as _;
use gpui_kit::Styled as _;
use gpui_kit::StyledImage as _;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Icon, IconName};
use gpui_kit::{
    AnyElement, App, Context, Entity, FontWeight, IntoElement, ListAlignment, ListState,
    ParentElement, Render, SharedString, Window, div, px, relative,
};

use crate::icons::AppIcon;
use crate::views::licenses;

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
        "購入済み書籍をカードとリストの 2 表示で一覧管理。未読・読んでいる途中・読了のステータス、\
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

/// ライセンスページの説明。
const LICENSES_LEAD: &str = "このアプリが利用しているオープンソースソフトウェアの一覧です。\
                             Rust のクレートのほかに、同梱しているアセット（PDF 表示に使う \
                             PDFium、アイコンの Lucide）も含みます。各行をクリックすると\
                             ライセンス全文が開きます。全文をクレートが同梱していない場合は、\
                             配布元を確認してください。";

/// ライセンス全文をクレートが同梱していないときの案内。
const NO_LICENSE_TEXT: &str =
    "ライセンス全文はクレートに同梱されていません（配布元を確認してください）";

/// ライセンス一覧の行の高さの目安。
/// 仮想化リストのスクロールバーを最初から正しい大きさで出すために使う（行の高さは
/// レンダリング時に実測されて置き換わる）。
const LICENSE_ROW_HINT: f32 = 60.0;

/// ライセンス一覧で画面外に余分に描画しておく高さ。
const LICENSE_LIST_OVERDRAW: f32 = 200.0;

/// 案内画面が表示しているページ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AboutPage {
    /// 案内（説明）。
    About,
    /// オープンソースライセンス。
    Licenses,
}

pub struct AboutView {
    page: AboutPage,
    /// ライセンス一覧（1000 行を超えるので仮想化する）。
    licenses_list: ListState,
    /// ライセンス全文を開いている行。
    licenses_expanded: HashSet<usize>,
}

impl AboutView {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            page: AboutPage::About,
            licenses_list: ListState::new(
                licenses::CATALOG.entries.len(),
                ListAlignment::Top,
                px(LICENSE_LIST_OVERDRAW),
            )
            .with_uniform_item_height(px(LICENSE_ROW_HINT)),
            licenses_expanded: HashSet::new(),
        }
    }

    /// ライセンスページを開く（案内画面の一番下の「ライセンスについて」）。
    fn open_licenses(&mut self, cx: &mut Context<Self>) {
        self.page = AboutPage::Licenses;
        cx.notify();
    }

    /// 案内画面に戻る（ライセンスページの先頭の「戻る」）。
    fn close_licenses(&mut self, cx: &mut Context<Self>) {
        self.page = AboutPage::About;
        cx.notify();
    }

    /// 行のライセンス全文を開閉する。行の高さが変わるので仮想化リストに測り直させる。
    fn toggle_license(&mut self, ix: usize, cx: &mut Context<Self>) {
        if !self.licenses_expanded.remove(&ix) {
            self.licenses_expanded.insert(ix);
        }
        self.licenses_list.remeasure_items(ix..ix + 1);
        cx.notify();
    }
}

impl Render for AboutView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        match self.page {
            AboutPage::About => self.render_about(window, cx).into_any_element(),
            AboutPage::Licenses => self.render_licenses(window, cx).into_any_element(),
        }
    }
}

impl AboutView {
    /// 案内（説明）ページ。
    fn render_about(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
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
                                    )
                                    // 不具合報告でバージョンを書いてもらうため、ここに出す
                                    // （`.github/ISSUE_TEMPLATE/bug.yml` が案内している）。
                                    .child(
                                        div()
                                            .debug_selector(|| "about-version".into())
                                            .text_xs()
                                            .text_color(muted_fg)
                                            .child(concat!(
                                                "バージョン ",
                                                env!("CARGO_PKG_VERSION")
                                            )),
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
                    // フッター（保存場所の注意 + ライセンス表示への導線）
                    .child(
                        div()
                            .border_t_1()
                            .border_color(card_border)
                            .pt_6()
                            .flex()
                            .flex_col()
                            .items_start()
                            .gap_3()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(muted_fg)
                                    .child(
                                        "Thundoku Shelf — データはすべてお使いの PC のローカルフォルダに保存されます。",
                                    ),
                            )
                            .child(
                                Button::new("licenses-link")
                                    .debug_selector(|| "about-licenses-link".into())
                                    .label("ライセンスについて")
                                    .ghost()
                                    .small()
                                    .cursor_pointer()
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.open_licenses(cx)),
                                    ),
                            ),
                    ),
            ),
            )
    }

    /// ライセンスページ（案内画面の一番下の「ライセンスについて」から開く）。
    ///
    /// 行数が 1000 を超えるため、先頭（戻るボタン・見出し）は固定して、一覧だけを
    /// 可視行しか構築しない仮想化リストにする。
    fn render_licenses(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let _ = window;
        let border = cx.theme().border;
        let muted_fg = cx.theme().muted_foreground;
        let handle = cx.entity();
        let list_state = self.licenses_list.clone();
        let count = licenses::CATALOG.entries.len();

        div()
            .debug_selector(|| "licenses-page".into())
            .size_full()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            // 先頭: 案内に戻る + 見出し（スクロールしても残る）
            .child(
                div()
                    .w_full()
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(border)
                    .child(
                        div()
                            .mx_auto()
                            .w(px(720.0))
                            .px_4()
                            .py_6()
                            .flex()
                            .flex_col()
                            .items_start()
                            .gap_3()
                            .child(
                                Button::new("licenses-back")
                                    .debug_selector(|| "licenses-back".into())
                                    .label("← 案内に戻る")
                                    .ghost()
                                    .small()
                                    .cursor_pointer()
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.close_licenses(cx)),
                                    ),
                            )
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("オープンソースライセンス"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(muted_fg)
                                    .line_height(relative(1.7))
                                    .child(LICENSES_LEAD),
                            )
                            .child(
                                div()
                                    .debug_selector(|| "licenses-count".into())
                                    .text_xs()
                                    .text_color(muted_fg)
                                    .child(format!("{count} 件")),
                            ),
                    ),
            )
            // 一覧（見出しと同じ幅の中央寄せカラムに揃える）
            .child(
                div().flex_1().min_h_0().w_full().child(
                    div().mx_auto().w(px(720.0)).h_full().child(
                        gpui_kit::list(list_state, move |ix, _window, cx| {
                            let Some(entry) = licenses::CATALOG.entries.get(ix) else {
                                return div().into_any_element();
                            };
                            let expanded = handle.read(cx).licenses_expanded.contains(&ix);
                            Self::license_row(ix, entry, expanded, &handle, cx)
                        })
                        .h_full()
                        .w_full(),
                    ),
                ),
            )
            .into_any_element()
    }

    /// ライセンス一覧の 1 行。見出しをクリックするとライセンス全文を開閉する。
    fn license_row(
        ix: usize,
        entry: &licenses::Entry,
        expanded: bool,
        handle: &Entity<Self>,
        cx: &mut App,
    ) -> AnyElement {
        // `CATALOG` は static なので、全文は `&'static str` のまま要素に渡せる（コピーしない）
        let catalog: &'static licenses::Catalog = &licenses::CATALOG;
        let theme = cx.theme();
        let border = theme.border;
        let card_bg = theme.muted;
        let muted_fg = theme.muted_foreground;
        let primary = theme.primary;
        let hover_bg = crate::views::hover_bg(theme);

        // 行に出すチップ（種別・ライセンス表記）。`highlight` はライセンス表記用。
        let chip = move |text: String, highlight: bool| {
            let (bg, fg) = if highlight {
                (primary.opacity(0.12), primary)
            } else {
                (card_bg, muted_fg)
            };
            div()
                .flex_shrink_0()
                .px_2()
                .py_1()
                .rounded_md()
                .bg(bg)
                .text_xs()
                .text_color(fg)
                .child(text)
        };

        // 同梱アセットは「何に使っているか」、クレートは配布元を添える
        let source = match (entry.note.as_str(), entry.repository.as_str()) {
            ("", "") => String::new(),
            (note, "") => note.to_string(),
            ("", repository) => repository.to_string(),
            (note, repository) => format!("{note}・{repository}"),
        };

        let header = div()
            .id(("licenses-entry", ix))
            .debug_selector(move || format!("licenses-entry-{ix}"))
            .w_full()
            .px_4()
            .py_3()
            .flex()
            .flex_row()
            .items_center()
            .gap_3()
            .cursor_pointer()
            .hover(move |style| style.bg(hover_bg))
            .on_click({
                let handle = handle.clone();
                move |_, _, cx| handle.update(cx, |this, cx| this.toggle_license(ix, cx))
            })
            .child(
                Icon::new(if expanded {
                    IconName::ChevronDown
                } else {
                    IconName::ChevronRight
                })
                .size(px(16.0))
                .text_color(muted_fg),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(entry.name.clone()),
                            )
                            .child(if entry.version.is_empty() {
                                div().into_any_element()
                            } else {
                                div()
                                    .text_xs()
                                    .text_color(muted_fg)
                                    .child(format!("v{}", entry.version))
                                    .into_any_element()
                            }),
                    )
                    .child(if source.is_empty() {
                        div().into_any_element()
                    } else {
                        div()
                            .text_xs()
                            .text_color(muted_fg)
                            .truncate()
                            .child(source)
                            .into_any_element()
                    }),
            )
            .children(
                entry
                    .kind
                    .chip()
                    .map(|label| chip(label.to_string(), false)),
            )
            .children((!entry.license.is_empty()).then(|| chip(entry.license.clone(), true)));

        let mut body: Vec<AnyElement> = Vec::new();
        if expanded {
            if entry.texts.is_empty() {
                body.push(
                    div()
                        .text_xs()
                        .text_color(muted_fg)
                        .child(NO_LICENSE_TEXT)
                        .into_any_element(),
                );
            }
            for (ti, (label, text_ix)) in entry.texts.iter().enumerate() {
                body.push(
                    div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(muted_fg)
                                .child(label.clone()),
                        )
                        .child(
                            div()
                                .id(("licenses-text", ix * 1000 + ti))
                                .debug_selector(move || format!("licenses-text-{ix}-{ti}"))
                                .rounded_lg()
                                .border_1()
                                .border_color(border)
                                .bg(card_bg)
                                .p_3()
                                .text_xs()
                                .text_color(muted_fg)
                                .line_height(relative(1.6))
                                .child(catalog.text(*text_ix)),
                        )
                        .into_any_element(),
                );
            }
            if !entry.repository.is_empty() {
                let url = entry.repository.clone();
                body.push(
                    div()
                        .id(("licenses-repository", ix))
                        .text_xs()
                        .text_color(primary)
                        .cursor_pointer()
                        .underline()
                        .on_click(move |_, _, cx: &mut App| cx.open_url(&url))
                        .child(entry.repository.clone())
                        .into_any_element(),
                );
            }
        }

        div()
            .w_full()
            .border_b_1()
            .border_color(border)
            .flex()
            .flex_col()
            .child(header)
            .children(body)
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::AppContext as _;

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

    /// ライセンスページの説明は、実際に同梱しているものだけを名指しすること。
    ///
    /// PDF 表示は全プラットフォーム PDFium に統一した（以前は macOS / Linux が MuPDF = AGPL-3.0
    /// で、MIT 配布のアプリと両立しないため削除した）。使っていないライブラリを名指しすると、
    /// 関係ないライセンスを案内することになる。
    #[test]
    fn licenses_lead_names_only_bundled_libraries() {
        for name in ["PDFium", "Lucide"] {
            assert!(
                LICENSES_LEAD.contains(name),
                "説明文に {name}（同梱しているもの）が無い: {LICENSES_LEAD}"
            );
            assert!(
                licenses::CATALOG
                    .entries
                    .iter()
                    .any(|entry| entry.name == name),
                "ライセンス一覧に {name} が無いのに説明文が名指ししている"
            );
        }
        assert!(
            !LICENSES_LEAD.contains("MuPDF") && !LICENSES_LEAD.contains("mupdf"),
            "削除した MuPDF を説明文が名指ししている: {LICENSES_LEAD}"
        );
        assert!(
            !LICENSES_LEAD.contains("AGPL"),
            "削除したライブラリのライセンス（AGPL）を説明文が挙げている: {LICENSES_LEAD}"
        );
    }

    /// バージョンを画面に出すこと。
    ///
    /// `.github/ISSUE_TEMPLATE/bug.yml` が「「このアプリについて」画面の下部で確認できます」と
    /// 案内しているので、ここが消えると利用者がバージョンを書けなくなる。
    #[gpui_kit::test]
    async fn about_shows_the_version(cx: &mut gpui_kit::TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        let view = cx.new(AboutView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1000.0),
                height: gpui_kit::px(1400.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        draw(&mut *visual);

        assert!(
            visual.debug_bounds("about-version").is_some(),
            "バージョン表示が出ていない（不具合報告のテンプレートが案内している）"
        );
    }

    /// 案内画面を縦に長いウィンドウで開く。
    ///
    /// 「ライセンスについて」のリンクは画面の一番下にあるため、スクロールせずに
    /// クリックできる高さが要る。
    fn open_about(cx: &mut gpui_kit::TestAppContext) -> &mut gpui_kit::VisualTestContext {
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        let view = cx.new(AboutView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1000.0),
                height: gpui_kit::px(4000.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        gpui_kit::VisualTestContext::from_window(*window, cx).into_mut()
    }

    /// テスト用に数フレーム描画する（レイアウトが確定して `debug_bounds` が引けるようになる）。
    fn draw(visual: &mut gpui_kit::VisualTestContext) {
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
    }

    /// 案内画面の一番下の「ライセンスについて」をクリックしてライセンスページを開く。
    fn click_licenses_link(visual: &mut gpui_kit::VisualTestContext) {
        let link = visual
            .debug_bounds("about-licenses-link")
            .expect("案内画面の一番下に「ライセンスについて」のリンクが無い");
        visual.simulate_click(link.center(), gpui_kit::Modifiers::default());
        draw(visual);
    }

    /// 案内画面の一番下のリンクからライセンスページを開き、「戻る」で案内画面に戻ること。
    #[gpui_kit::test]
    async fn licenses_link_opens_the_page_and_back_returns_to_about(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let visual = open_about(cx);
        draw(visual);

        click_licenses_link(visual);
        assert!(
            visual.debug_bounds("licenses-page").is_some(),
            "ライセンスページが開いていない"
        );
        assert!(
            visual.debug_bounds("about-licenses-link").is_none(),
            "ライセンスページを開いても案内画面の内容が残っている"
        );

        let back = visual
            .debug_bounds("licenses-back")
            .expect("ライセンスページの先頭に「戻る」ボタンが無い");
        visual.simulate_click(back.center(), gpui_kit::Modifiers::default());
        draw(visual);

        assert!(
            visual.debug_bounds("about-licenses-link").is_some(),
            "「戻る」で案内画面に戻っていない"
        );
        assert!(
            visual.debug_bounds("licenses-page").is_none(),
            "「戻る」を押してもライセンスページが残っている"
        );
    }

    /// ライセンスページに 1 件目（アプリ本体）の行が出て、クリックで全文が開くこと。
    #[gpui_kit::test]
    async fn licenses_page_entries_expand_the_full_text(cx: &mut gpui_kit::TestAppContext) {
        let visual = open_about(cx);
        draw(visual);
        click_licenses_link(visual);

        let row = visual
            .debug_bounds("licenses-entry-0")
            .expect("ライセンスページに 1 件目の行が無い");
        assert!(
            visual.debug_bounds("licenses-text-0-0").is_none(),
            "最初からライセンス全文が開いている"
        );

        visual.simulate_click(row.center(), gpui_kit::Modifiers::default());
        draw(visual);
        assert!(
            visual.debug_bounds("licenses-text-0-0").is_some(),
            "行をクリックしてもライセンス全文が開かない"
        );
    }
}
