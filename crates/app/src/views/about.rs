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

/// 「はじめての方へ」の見出し（「このアプリについて」の次に出す）。
const FIRST_STEPS_TITLE: &str = "はじめての方へ";

/// はじめて使う人向けの導入文。
///
/// 初回の案内は短く保ち、細かい説明は下の節（サイドバー / 本棚 / 各機能）に任せる。
/// **最初の 1 冊を読むところまで**を 3 ステップで示すのが目的。
/// 「ローカルの本だけでも読める」とは書かない（アプリの管理外のダウンロードファイルまで
/// 使えると読めてしまう）。
const FIRST_STEPS_LEAD: &str = "Thundoku Shelf は「ストアにログイン → 本棚に取り込む → 読む」の \
                                3 ステップで使い始められます。";

/// はじめて使う人向けの 3 ステップ（見出し / 説明）。
const FIRST_STEPS: [(&str, &str); 3] = [
    (
        "1. ストアにログインする",
        "サイドバー下部の「アカウント」から、本を買ったストア（技術書典 / BOOTH / FANZA同人 / \
         DLsite）にログインします。一度ログインすると、次回以降は不要です",
    ),
    (
        "2. 本棚に取り込む",
        "ストアにログインすると自動で同期が始まり、購入済みの本の一覧が本棚に並びます\
         （右上の「同期」でいつでもやり直せます）。ページ画像は本を開いたときに\
         ダウンロードされます",
    ),
    (
        "3. 読む",
        "本をクリックすると取り込みが始まり、終わるとビューアーが開きます。\
         読んだページ・付箋・閲覧履歴は自動で保存され、次に開くと続きから読めます",
    ),
];

/// 3 ステップのあとに添える補足（はじめの 1 冊 / うまくいかないとき）。
const FIRST_STEPS_NOTES: [(&str, &str); 2] = [
    (
        "まずは 1 冊から",
        "全部を先に取り込む必要はありません。気になる 1 冊から始めてください",
    ),
    (
        "同期できないときは",
        "そのストアのセッションが切れていないか確認し、必要ならログインし直してください。\
         アプリの不具合は、サイドバーの「レポート」から GitHub に報告できます\
         （GitHub にログイン時のみ表示）",
    ),
];

/// 「サイドバーの使い方」の見出しと概要。
const SIDEBAR_TITLE: &str = "サイドバーの使い方";
const SIDEBAR_LEAD: &str = "画面の左端に縦に並ぶメニューです。普段はアイコンだけの幅で、\
                            マウスを乗せるとラベル付きで開きます（離れてしばらくすると元に戻ります。\
                            「表示」メニューの「サイドバーを切り替え」で開いたままにもできます）。\
                            一番上のロゴをクリックすると、この説明画面が開きます。";

/// サイドバーの項目（名前 / 説明）。**上から並ぶ順に書く**（実際の行と同じ順）。
const SIDEBAR_ITEMS: [(&str, &str); 9] = [
    (
        "本棚",
        "本の一覧。下に続くサイト別メニューで、ストアごとの本棚に切り替えられます",
    ),
    (
        "お気に入り",
        "ハートを付けた本。同期時に未ダウンロードなら自動でダウンロードされます",
    ),
    (
        "閲覧履歴",
        "ビューアーで開いた本を日付ごとにまとめて表示します",
    ),
    ("付箋", "付箋を付けた本と、そのページ・メモの一覧です"),
    (
        "チェックリスト",
        "技術書典のイベントの購入チェック（技術書典にログイン時のみ表示）",
    ),
    (
        "レポート",
        "アプリの不具合などを GitHub に報告します（GitHub にログイン時のみ表示）",
    ),
    ("設定", "保存先・バックアップ・アカウントなどの設定"),
    (
        "テーマ",
        "ライト / ダーク / システムを切り替え（現在のモード名がラベルに出ます）",
    ),
    (
        "アカウント",
        "一番下。各サービスのログイン状態と、ログイン / ログアウト",
    ),
];

/// サイドバーのアカウントからのログイン方法（見出し / 手順 / サービス別の認証方法）。
const ACCOUNT_LOGIN_TITLE: &str = "アカウントのログイン方法";
const ACCOUNT_LOGIN_STEPS: [&str; 4] = [
    "1. サイドバー下部の「アカウント」をクリックして一覧を開く",
    "2. ログインするサービスを選ぶ（Google / GitHub / 技術書典 / BOOTH / FANZA同人 / DLsite）",
    "3. 「ログイン」を押し、画面の案内に従って認証する",
    "4. ログインできると行の右端に緑のチェックが付く（ログアウトも同じ位置のアイコン）",
];
const ACCOUNT_LOGIN_METHODS: [(&str, &str); 3] = [
    (
        "Google",
        "システムブラウザが開くので、Google アカウントで認証します",
    ),
    (
        "GitHub",
        "画面に出るコードをコピーし、ブラウザで github.com/login/device に入力します",
    ),
    (
        "ストア（技術書典 / BOOTH / FANZA同人 / DLsite）",
        "そのストアのメールアドレスとパスワードを入力します",
    ),
];
const ACCOUNT_LOGIN_NOTE: &str =
    "Google ログインは Google ドライブへのバックアップに使うもので、本を読むだけなら不要です。";

/// 「本棚の基本的な使い方」の見出しと項目（見出し / 説明）。
const SHELF_BASICS_TITLE: &str = "本棚の基本的な使い方";
const SHELF_BASICS: [(&str, &str); 6] = [
    (
        "本を取り込む（同期）",
        "ストアにログインすると自動で同期が始まり、購入済みの一覧が本棚に入ります。\
         右上の「同期」でも取り込めます（サイト絞り込み中はそのサイトだけ）",
    ),
    (
        "本を読む",
        "本をクリックします。未ダウンロードなら取り込みが始まり、済んでいればビューアーが開きます",
    ),
    (
        "探す（絞り込みと検索）",
        "見出しの下の「全項目 / 未読 / 読んでいる途中 / 既読 / お気に入り」と、\
         タグ・イベント・検索欄で絞り込めます。解除は「全項目」または ESC です",
    ),
    (
        "並べ替える",
        "「並び替え」で購入日 / 発売日 / タイトル / 最終閲覧日 / 閲覧回数 / 閲覧時間 / サイズを\
         選べます（データが無い項目は出ません）",
    ),
    (
        "表示を切り替える",
        "カード / リストを切り替えられます。選んだ形式は次に開いたときも同じです",
    ),
    (
        "マウスとキーボード",
        "右クリックで「開く / ダウンロード中止 / タグ編集 / 非表示」、キーボードは \
         ← → ↑ ↓ で選択、Enter で開く、Backspace でダウンロード中止です",
    ),
];

/// Google ドライブの注意（**ストアのログイン手順の次**に出す）。
const DRIVE_NOTICE_LABEL: &str = "ご利用前の注意";
const DRIVE_NOTICE_TITLE: &str = "Google ログインは Google ドライブでのバックアップに使用します";
const DRIVE_NOTICE_BODY: &str = "書籍を読むのに Google ログインは必要ありません。Google ログインを\
                                 行うと、本棚の DB や画像を Google Drive にバックアップ・同期できます。\
                                 サイドバーのアカウントアイコンからログインしてください。";
const DRIVE_NOTICE_STORE: &str = "技術書典で購入済みの書籍を同期する場合は、サイドバーまたは\
                                  本棚の同期ボタンから技術書典にログイン（メールアドレスと\
                                  パスワード）してください。";

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

/// 説明画面のロゴの角丸（px）。
///
/// ロゴ画像（アプリアイコン）は白地の内側に濃紺の枠線が描かれている。角丸を**枠線の
/// 外側の丸み**に合わせないと、大きいと枠線の角が削れて見え、小さいと白い角が
/// はみ出して見える。値はアセットから測った（画像サイズの 8%: 1024px で外側半径
/// 81.9px → 128px へ縮小して 64px で表示するので 5.12px）。
/// `about_logo_radius_matches_the_icon_frame` がアセットと一致していることを見る。
const APP_LOGO_RADIUS: f32 = 5.12;

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
    /// 説明画面の 1 節（見出し + カード）。`debug_id` は並び順のテストから引く。
    fn about_section(
        &self,
        debug_id: &'static str,
        title: &'static str,
        body: impl IntoElement,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        div()
            .debug_selector(move || debug_id.into())
            .flex()
            .flex_col()
            .gap_3()
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .rounded_xl()
                    .border_1()
                    .border_color(cx.theme().border)
                    .bg(cx.theme().muted)
                    .p_6()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(body),
            )
    }

    /// 名前 / 説明の 2 列の行（サイドバーの項目と本棚の使い方で共有する）。
    ///
    /// 名前の列幅を固定するので、行が変わっても説明の頭が揃って縦に読める。
    fn about_rows(
        rows: &[(&'static str, &'static str)],
        name_w: f32,
        muted_fg: gpui_kit::Hsla,
    ) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .children(rows.iter().map(move |(name, desc)| {
                div()
                    .flex()
                    .flex_row()
                    .gap_3()
                    .child(
                        div()
                            .w(px(name_w))
                            .flex_shrink_0()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(*name),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(muted_fg)
                            .line_height(relative(1.6))
                            .child(*desc),
                    )
            }))
    }

    /// 「はじめての方へ」（初回の 3 ステップ + 補足）。
    fn first_steps_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted_fg = cx.theme().muted_foreground;
        let body = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .text_sm()
                    .text_color(muted_fg)
                    .line_height(relative(1.7))
                    .child(FIRST_STEPS_LEAD),
            )
            .children(FIRST_STEPS.into_iter().map(|(title, desc)| {
                div()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted_fg)
                            .line_height(relative(1.7))
                            .child(desc),
                    )
            }))
            .children(FIRST_STEPS_NOTES.into_iter().map(|(title, desc)| {
                div()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .pt_4()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(title))
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted_fg)
                            .line_height(relative(1.7))
                            .child(desc),
                    )
            }));
        self.about_section("about-first-steps", FIRST_STEPS_TITLE, body, cx)
    }

    /// 「サイドバーの使い方」（概要 + 項目一覧 + アカウントのログイン方法）。
    fn sidebar_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted_fg = cx.theme().muted_foreground;
        let body = div()
            .flex()
            .flex_col()
            .gap_4()
            .child(
                div()
                    .text_sm()
                    .text_color(muted_fg)
                    .line_height(relative(1.7))
                    .child(SIDEBAR_LEAD),
            )
            .child(Self::about_rows(&SIDEBAR_ITEMS, 160.0, muted_fg))
            .child(
                div()
                    .border_t_1()
                    .border_color(cx.theme().border)
                    .pt_4()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(FontWeight::MEDIUM)
                            .child(ACCOUNT_LOGIN_TITLE),
                    )
                    .children(
                        ACCOUNT_LOGIN_STEPS
                            .into_iter()
                            .map(|step| div().text_sm().text_color(muted_fg).child(step)),
                    )
                    .child(Self::about_rows(&ACCOUNT_LOGIN_METHODS, 260.0, muted_fg))
                    .child(
                        div()
                            .text_sm()
                            .text_color(muted_fg)
                            .line_height(relative(1.7))
                            .child(ACCOUNT_LOGIN_NOTE),
                    ),
            );
        self.about_section("about-sidebar", SIDEBAR_TITLE, body, cx)
    }

    /// 「本棚の基本的な使い方」。
    fn shelf_basics_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let muted_fg = cx.theme().muted_foreground;
        let body = Self::about_rows(&SHELF_BASICS, 200.0, muted_fg);
        self.about_section("about-shelf-basics", SHELF_BASICS_TITLE, body, cx)
    }

    /// Google ドライブの注意（**ストアのログイン手順の次**に置く）。
    fn drive_notice_section(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let warning = cx.theme().warning;
        let muted_fg = cx.theme().muted_foreground;
        div()
            .debug_selector(|| "about-drive-notice".into())
            .flex()
            .flex_col()
            .gap_2()
            .rounded_xl()
            .border_1()
            .border_color(warning.opacity(0.35))
            .bg(warning.opacity(0.10))
            .p_4()
            .child(
                div()
                    .text_sm()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(warning)
                    .child(DRIVE_NOTICE_LABEL),
            )
            .child(
                div()
                    .text_lg()
                    .font_weight(FontWeight::SEMIBOLD)
                    .child(DRIVE_NOTICE_TITLE),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(muted_fg)
                    .line_height(relative(1.7))
                    .child(DRIVE_NOTICE_BODY),
            )
            .child(
                div()
                    .text_sm()
                    .text_color(muted_fg)
                    .line_height(relative(1.7))
                    .child(DRIVE_NOTICE_STORE),
            )
    }

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
                                    // ロゴ画像の角丸（アイコンの枠線の外側の丸み）と揃える。
                                    // 揃えないと角に下地（`primary`）が覗く
                                    .rounded(px(APP_LOGO_RADIUS))
                                    .overflow_hidden()
                                    .bg(primary)
                                    .child(
                                        if let Some(logo) = crate::views::bookshelf::app_logo_image()
                                        {
                                            // ロゴ画像（アプリアイコン）は白地の内側に濃紺の
                                            // 枠線が描かれている。枠線の外側の丸みに合わせて
                                            // 角を切る（大きいと枠線の角が削れ、小さいと白い角が
                                            // はみ出して見える。`overflow_hidden` では画像は
                                            // 切れないので画像側にも角丸が要る）
                                            gpui_kit::img(logo)
                                                .w(px(64.0))
                                                .h(px(64.0))
                                                .rounded(px(APP_LOGO_RADIUS))
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
                            .debug_selector(|| "about-intro".into())
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
                    // はじめての方へ（初回の案内。「このアプリについて」の次に置く）
                    .child(self.first_steps_section(cx))
                    // サイドバーの使い方（項目の説明 + アカウントのログイン方法）
                    .child(self.sidebar_section(cx))
                    // 本棚の基本的な使い方
                    .child(self.shelf_basics_section(cx))
                    // 主な機能
                    .child(
                        div()
                            .debug_selector(|| "about-features".into())
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
                            .debug_selector(|| "about-store-login".into())
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
                    // Google ドライブの注意（ストアのログイン手順の次に置く）
                    .child(self.drive_notice_section(cx))
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

    /// 説明画面のロゴの角丸は、**アイコン自身の枠線の外側の丸み**に合わせること。
    ///
    /// ロゴ画像は白地の内側に濃紺の枠線が描かれている。角丸がこれより大きいと枠線の角が
    /// 削れて見え、小さいと白い角がはみ出して見える。アイコンを差し替えたら気づけるよう、
    /// アセットから測った値と定数の一致を見る。
    #[test]
    fn about_logo_radius_matches_the_icon_frame() {
        let decoded =
            image::load_from_memory(include_bytes!("../../assets/app-icon/icon_1024.png"))
                .expect("アプリアイコンを読む")
                .to_rgba8();
        let size = decoded.width();
        let raw = decoded.as_raw();
        let is_ink = |x: u32, y: u32| {
            let i = ((y * size + x) * 4) as usize;
            !(raw[i] > 230 && raw[i + 1] > 230 && raw[i + 2] > 230)
        };
        // 上辺の中央で、枠線の外側エッジまでの余白（直線部分）
        let inset = (0..200)
            .find(|&y| is_ink(size / 2, y))
            .expect("アイコンに枠線が無い");
        // 左上の角からの対角線で、枠線の外側エッジまでの距離
        let diagonal = (0..300)
            .find(|&t| is_ink(t, t))
            .expect("アイコンに枠線が無い");
        // 角丸の外側半径: 角から対角線上の距離 d、直線部の余白 s のとき r = (d - s) / (1 - 1/√2)
        let radius = (diagonal as f32 - inset as f32) / (1.0 - std::f32::consts::FRAC_1_SQRT_2);
        // 表示は 64px（1024px のアセットを 128px へ縮小して 64px で描く = 1/16）
        let expected = radius / 16.0;
        assert!(
            (APP_LOGO_RADIUS - expected).abs() < 0.5,
            "ロゴの角丸がアイコンの枠線と合っていない: 定数 {APP_LOGO_RADIUS} / 実測 {expected:.2}"
        );
        // 角丸が実質 0（白い四角）に戻ると上の一致で落ちる（実測は 5px 前後）
    }

    /// 初めて使う人向けの 3 ステップが、実装どおりの操作を案内していること。
    #[test]
    fn first_steps_cover_login_import_and_read() {
        assert_eq!(
            FIRST_STEPS.len(),
            3,
            "はじめての方への手順は 3 ステップに保つ（詳細は下の節に置く）"
        );
        let all = FIRST_STEPS
            .iter()
            .map(|(title, body)| format!("{title} {body}"))
            .collect::<Vec<_>>()
            .join(" ");
        for keyword in ["ログイン", "同期", "ビューアー"] {
            assert!(
                all.contains(keyword),
                "はじめての方への手順に「{keyword}」の案内が無い: {all}"
            );
        }
        for store in ["技術書典", "BOOTH", "FANZA", "DLsite"] {
            assert!(
                all.contains(store),
                "はじめての方への手順に対応ストア（{store}）の案内が無い: {all}"
            );
        }
        assert!(
            FIRST_STEPS[1].1.contains("自動"),
            "ログインすると自動で同期が始まることに触れていない: {}",
            FIRST_STEPS[1].1
        );
        assert!(
            FIRST_STEPS_LEAD.contains("3 ステップ"),
            "3 ステップで使い始められることが書かれていない: {FIRST_STEPS_LEAD}"
        );
        // ログインしていない本（管理外のダウンロードファイル）まで読めるように読める書き方はしない。
        // 「ローカルの本」と書くと、アプリの管理外のファイルを使えると誤解される。
        for text in [
            FIRST_STEPS_LEAD,
            FIRST_STEPS_NOTES[0].1,
            FIRST_STEPS_NOTES[1].1,
        ] {
            assert!(
                !text.contains("ローカルの本"),
                "管理外のファイルが使えると読める書き方が残っている: {text}"
            );
        }
        // 「同期」は購入済みの一覧を取り込む操作で、残りをまとめて取り込む機能は無い。
        assert!(
            !FIRST_STEPS_NOTES[0].1.contains("まとめて取り込め"),
            "「同期」に無い一括取り込みを案内している: {}",
            FIRST_STEPS_NOTES[0].1
        );
    }

    /// サイドバーの項目が実際の行と同じ顔ぶれ・同じ順で説明されていること。
    #[test]
    fn sidebar_items_match_the_real_rows() {
        let names: Vec<&str> = SIDEBAR_ITEMS.iter().map(|(name, _)| *name).collect();
        assert_eq!(
            names,
            vec![
                "本棚",
                "お気に入り",
                "閲覧履歴",
                "付箋",
                "チェックリスト",
                "レポート",
                "設定",
                "テーマ",
                "アカウント",
            ],
            "サイドバーの項目一覧が実際の行（上から順）と違う"
        );
        for (name, desc) in SIDEBAR_ITEMS {
            assert!(!desc.trim().is_empty(), "「{name}」の説明が空");
        }
        assert_eq!(
            ACCOUNT_LOGIN_STEPS.len(),
            4,
            "アカウントのログイン手順は 4 ステップ（開く → 選ぶ → 認証 → 完了）"
        );
        let steps = ACCOUNT_LOGIN_STEPS.join(" ");
        for keyword in ["アカウント", "Google", "GitHub"] {
            assert!(
                steps.contains(keyword),
                "ログイン手順に「{keyword}」が無い: {steps}"
            );
        }
    }

    /// 本棚の基本操作（取り込み / 読む / 探す / 並べ替え / 表示切替 / ショートカット）が揃っていること。
    #[test]
    fn shelf_basics_cover_the_main_operations() {
        let all = SHELF_BASICS
            .iter()
            .map(|(title, body)| format!("{title} {body}"))
            .collect::<Vec<_>>()
            .join(" ");
        for keyword in [
            "同期",
            "ビューアー",
            "並び替え",
            "カード",
            "未読",
            "ESC",
            "Backspace",
        ] {
            assert!(
                all.contains(keyword),
                "本棚の説明に「{keyword}」が無い: {all}"
            );
        }
    }

    /// 説明画面の節の上端 Y（節が無ければ panic）。並び順の検証に使う。
    fn section_top(visual: &mut gpui_kit::VisualTestContext, id: &'static str) -> gpui_kit::Pixels {
        visual
            .debug_bounds(id)
            .unwrap_or_else(|| panic!("説明画面に {id} が無い"))
            .origin
            .y
    }

    /// 初回の案内の並び順。
    ///
    /// 「はじめての方へ」は「このアプリについて」の次（「主な機能」より上）に置き、
    /// Google ドライブの注意はストアのログイン手順より後ろに置く。
    #[gpui_kit::test]
    async fn about_orders_the_first_run_guide_before_the_reference_sections(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        let visual = open_about(cx);
        draw(visual);
        let intro = section_top(visual, "about-intro");
        let first_steps = section_top(visual, "about-first-steps");
        let features = section_top(visual, "about-features");
        let store_login = section_top(visual, "about-store-login");
        let drive_notice = section_top(visual, "about-drive-notice");
        assert!(
            intro < first_steps,
            "「はじめての方へ」が「このアプリについて」より上にある"
        );
        assert!(
            first_steps < features,
            "「はじめての方へ」が「主な機能」より下にある"
        );
        assert!(
            features < store_login,
            "「ストアのログイン手順」が「主な機能」より上にある"
        );
        assert!(
            store_login < drive_notice,
            "Google ドライブの注意がストアのログイン手順より上にある"
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
    /// クリックできる高さが要る（はじめての方へ / サイドバー / 本棚の使い方を足したぶん、
    /// ページはさらに長くなっている）。
    fn open_about(cx: &mut gpui_kit::TestAppContext) -> &mut gpui_kit::VisualTestContext {
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        let view = cx.new(AboutView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1000.0),
                height: gpui_kit::px(8000.0),
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
