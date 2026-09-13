//! 本棚ビュー: ローカル本のグリッド + 検索/タグ/既読フィルタ + 技術書典同期と
//! ダウンロード導線 + インポート。

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use gpui_kit::StyledImage as _;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::component::carousel::{Carousel, CarouselContent, CarouselItem, CarouselState};
use gpui_kit::component::dialog::Dialog;
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::tag::{Tag, TagVariant};
use gpui_kit::component::theme::Colorize as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName, Sizable as _, Size};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AppContext as _, InteractiveElement as _, ReadGlobal as _,
    StatefulInteractiveElement as _, StyleRefinement, Styled as _,
};
use gpui_kit::{
    App, Context, Entity, Hsla, IntoElement, KeyDownEvent, ParentElement, Render, RenderImage,
    SharedString, Window, div, img, px, relative,
};
use thundoku_core::booth::BoothClient;
use thundoku_core::db;
use thundoku_core::db::{books, bookshelf, documents, progress};
use thundoku_core::dlsite::client::DlsiteClient;
use thundoku_core::fanza::client::FanzaClient;
use thundoku_core::tbf::{self, TBF_DOWNLOAD_BASE, UreqTransport};

use crate::actions::{
    DeleteBook, EditBookTags, HideBook, OpenAuth, OpenAuthProvider, OpenReader, SyncDrive,
};
use crate::app_state::AppState;
use crate::icons::AppIcon;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadFilter {
    All,
    Unread,
    Read,
    Favorite,
}

/// サークル名 / 作者名リンクの種別（表示ラベルと絞り込み対象）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EntityLink {
    Circle,
    Author,
}

impl EntityLink {
    /// チップの手前に置くラベル。チップ（タグ）には含めない。
    fn prefix(self) -> &'static str {
        match self {
            EntityLink::Circle => "サークル:",
            EntityLink::Author => "作者:",
        }
    }

    /// カード内で一意になる要素 id（クリック判定・テストからの検索に使う）。
    fn element_id(self, database_id: &str) -> String {
        match self {
            EntityLink::Circle => format!("circle-link-{database_id}"),
            EntityLink::Author => format!("author-link-{database_id}"),
        }
    }

    /// お気に入りハートの要素 id（チップ右端。タグチップと同じ位置）。
    fn heart_id(self, database_id: &str) -> String {
        match self {
            EntityLink::Circle => format!("circle-heart-{database_id}"),
            EntityLink::Author => format!("author-heart-{database_id}"),
        }
    }

    /// チップ本体（値 + ハート）の要素 id。ラベルはこれに含めない。
    fn chip_id(self, database_id: &str) -> String {
        match self {
            EntityLink::Circle => format!("circle-chip-{database_id}"),
            EntityLink::Author => format!("author-chip-{database_id}"),
        }
    }

    /// チップの外に置くラベルの要素 id。
    fn label_id(self, database_id: &str) -> String {
        match self {
            EntityLink::Circle => format!("circle-label-{database_id}"),
            EntityLink::Author => format!("author-label-{database_id}"),
        }
    }

    /// DB（`favorite_entities`）に保存する種別。
    fn db_kind(self) -> db::favorites::EntityKind {
        match self {
            EntityLink::Circle => db::favorites::EntityKind::Circle,
            EntityLink::Author => db::favorites::EntityKind::Author,
        }
    }
}

/// 本棚の表示モード（Web の viewMode 相当）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewMode {
    Card,
    List,
}

/// お気に入りハートの丸ボタンの一辺。文字だけだと押しにくいのでクリック領域を広げる。
pub(crate) const CHIP_HEART_BUTTON: f32 = 18.0;
/// 丸ボタンの中に置くハートアイコンの一辺。
pub(crate) const CHIP_HEART_ICON: f32 = 12.0;

/// 絞り込み選択中の背景（半透明の青）。ライトの白背景でもダークの黒背景でも
/// 「選択中」に見えるように、テーマ共通で半透明の青を重ねる。
/// ※ `Hsla.h` は 0..1 の正規化値。度（例: 217.0）を渡すと clamp されて別の色になる。
const CHIP_SELECTED_BG: Hsla = gpui_kit::hsla(0.6028, 0.85, 0.55, 0.30);

/// チップ（タグ / サークル / 作者）の配色。
///
/// - 状態: **絞り込み選択（青）> お気に入り（ピンク）> 既定** の優先度で塗る。
///   選択とお気に入りが重なっても、ハートの色でお気に入りが分かる。
/// - ダークテーマは塗りを半透明にしてカード背景となじませ、明るい文字色 +
///   輪郭（ボーダー）で沈まないようにする。ライトテーマは不透明の淡色。
#[derive(Clone, Copy)]
struct ChipPalette {
    favorite_bg: Hsla,
    favorite_text: Hsla,
    favorite_heart: Hsla,
    selected_bg: Hsla,
    selected_text: Hsla,
    /// 既定 / お気に入り / 選択中それぞれの輪郭色（タグだと分かるように付ける）
    default_border: Hsla,
    favorite_border: Hsla,
    selected_border: Hsla,
    /// ハートの丸ボタンの地色（チップの上に重ねる）とホバー色
    heart_button: Hsla,
    heart_button_hover: Hsla,
}

impl ChipPalette {
    /// ライトテーマ: お気に入りは不透明の淡色、選択中は半透明の青 + 控えめな輪郭。
    fn light() -> Self {
        Self {
            favorite_bg: gpui_kit::rgb(0xfce7f3).into(),
            favorite_text: gpui_kit::rgb(0x9d174d).into(),
            favorite_heart: gpui_kit::rgb(0xdb2777).into(),
            selected_bg: CHIP_SELECTED_BG,
            selected_text: gpui_kit::rgb(0x1e40af).into(),
            default_border: gpui_kit::hsla(0.0, 0.0, 0.0, 0.12),
            favorite_border: gpui_kit::hsla(0.925, 0.71, 0.51, 0.35),
            selected_border: gpui_kit::hsla(0.6028, 0.91, 0.60, 0.40),
            heart_button: gpui_kit::hsla(0.0, 0.0, 0.0, 0.06),
            heart_button_hover: gpui_kit::hsla(0.0, 0.0, 0.0, 0.14),
        }
    }

    /// ダークテーマ: 塗りを半透明にして、明るい文字 + 強めの輪郭で読みやすくする。
    fn dark() -> Self {
        Self {
            favorite_bg: gpui_kit::hsla(0.925, 0.55, 0.50, 0.24),
            favorite_text: gpui_kit::rgb(0xf9a8d4).into(),
            favorite_heart: gpui_kit::rgb(0xf472b6).into(),
            selected_bg: CHIP_SELECTED_BG,
            selected_text: gpui_kit::rgb(0x93c5fd).into(),
            default_border: gpui_kit::hsla(0.0, 0.0, 1.0, 0.22),
            favorite_border: gpui_kit::hsla(0.925, 0.80, 0.72, 0.45),
            selected_border: gpui_kit::hsla(0.6028, 0.90, 0.72, 0.45),
            heart_button: gpui_kit::hsla(0.0, 0.0, 1.0, 0.14),
            heart_button_hover: gpui_kit::hsla(0.0, 0.0, 1.0, 0.24),
        }
    }

    fn for_theme(theme: &gpui_kit::component::Theme) -> Self {
        if theme.is_dark() {
            Self::dark()
        } else {
            Self::light()
        }
    }

    /// 背景色。選択 > お気に入り > 既定（`muted`）の優先度。
    fn background(&self, muted: Hsla, is_favorite: bool, is_selected: bool) -> Hsla {
        if is_selected {
            self.selected_bg
        } else if is_favorite {
            self.favorite_bg
        } else {
            muted
        }
    }

    /// 文字色（[`Self::background`] と対）。
    fn foreground(&self, muted: Hsla, is_favorite: bool, is_selected: bool) -> Hsla {
        if is_selected {
            self.selected_text
        } else if is_favorite {
            self.favorite_text
        } else {
            muted
        }
    }

    /// 輪郭色（状態ごとに色を変えてタグだと分かるようにする）。
    fn border(&self, is_favorite: bool, is_selected: bool) -> Hsla {
        if is_selected {
            self.selected_border
        } else if is_favorite {
            self.favorite_border
        } else {
            self.default_border
        }
    }

    /// ハートの色。お気に入り専用のチャンネルなので、選択中でも色を変えない。
    fn heart(&self, muted: Hsla, is_favorite: bool) -> Hsla {
        if is_favorite {
            self.favorite_heart
        } else {
            muted
        }
    }

    /// 「絞込中」ボタンの配色（絞り込み中に使う）。
    /// チップの「選択中」と同じ青系にして、選択状態が同じ色の系統で伝わるようにする。
    /// 返り値: (背景, 文字, ホバー, 押下)
    fn filter_all_colors(&self) -> (Hsla, Hsla, Hsla, Hsla) {
        (
            self.selected_bg,
            self.selected_text,
            self.selected_bg.alpha(0.45),
            self.selected_bg.alpha(0.55),
        )
    }
}

#[derive(Clone)]
struct BookEntry {
    book: books::Book,
    tags: Vec<String>,
    is_read: bool,
    /// (current_page, total_pages) — None when no reading progress exists.
    progress: Option<(i64, Option<i64>)>,
    /// Decoded thumbnail/cover image.
    cover: Option<Arc<RenderImage>>,
}

/// A single bookshelf card: a `bookshelf_items` row plus its resolved cover
/// and optional local book (downloaded pack).
#[derive(Clone)]
pub(crate) struct ShelfCard {
    shelf: bookshelf::BookshelfItem,
    local: Option<Box<BookEntry>>,
    /// Tags shown on the card: local book tags when downloaded, otherwise
    /// the `bookshelf_items.tags_json` snapshot (Web `getBookTags` parity).
    tags: Vec<String>,
    cover: Option<Arc<RenderImage>>,
    /// 表紙の取得・デコードが失敗したカード（NoImage ダミーを表示する）
    cover_fetch_failed: bool,
    /// 関連書籍（同一サークル / 同一作者）の `shelf_cards` インデックス。`reload` で作る。
    related: Vec<usize>,
}

/// カード / 行のチップ表示に必要な状態のスナップショット
/// （お気に入り = ハート、絞り込み選択 = 背景色）。
/// お気に入りタグの判定は `TagOrder`（表示順も持つ）に集約している。
#[derive(Clone, Default)]
struct ChipState {
    favorite_circles: Vec<String>,
    favorite_authors: Vec<String>,
    circle_filter: Option<String>,
    author_filter: Option<String>,
}

/// タグチップの表示順（選択中 → お気に入り → 集計数の多い順 → 名前順）を決めるスナップショット。
///
/// 1 回の描画につき 1 つ作り、可視カードのタグ並び替えで使い回す（カードごとに
/// 作るとお気に入り判定の集合と集計表をカード数分コピーすることになる）。
/// 集計表は `Arc` で共有するので、描画ごとのコピーは起こらない。
/// 集計数は「そのタグを持つカード数」で、`reload` のたびに作り直す。
#[derive(Clone)]
pub(crate) struct TagOrder {
    /// 絞り込みで選択中のタグ。折りたたみ中でも見えるよう最優先で先頭に出す
    /// （解除すると集計数 / 名前順の元の位置に戻る）。
    selected: std::collections::HashSet<String>,
    favorites: std::collections::HashSet<String>,
    counts: std::sync::Arc<std::collections::HashMap<String, usize>>,
}

impl TagOrder {
    pub(crate) fn new(
        favorite_tags: &[String],
        selected_tags: &[String],
        counts: std::sync::Arc<std::collections::HashMap<String, usize>>,
    ) -> Self {
        Self {
            selected: selected_tags.iter().cloned().collect(),
            favorites: favorite_tags.iter().cloned().collect(),
            counts,
        }
    }

    /// 絞り込みで選択中のタグか（チップの選択色にも使う）。
    pub(crate) fn is_selected(&self, tag: &str) -> bool {
        self.selected.contains(tag)
    }

    /// お気に入りタグか（ハートの塗り分けにも使う）。
    pub(crate) fn is_favorite(&self, tag: &str) -> bool {
        self.favorites.contains(tag)
    }

    /// タグ名 → 集計数（そのタグを持つカード数）。
    fn count(&self, tag: &str) -> usize {
        self.counts.get(tag).copied().unwrap_or(0)
    }

    /// タグを表示順に並べ替えた新しい Vec を返す。
    /// 選択 / お気に入り / 集計数・名前で決まるので、同じ入力なら常に同じ順になる。
    pub(crate) fn sorted(&self, tags: &[String]) -> Vec<String> {
        let mut sorted = tags.to_vec();
        sorted.sort_by(|a, b| {
            self.is_selected(b)
                .cmp(&self.is_selected(a))
                .then_with(|| self.is_favorite(b).cmp(&self.is_favorite(a)))
                .then_with(|| self.count(b).cmp(&self.count(a)))
                .then_with(|| a.cmp(b))
        });
        sorted
    }
}

/// タグ情報エリア（タグ列）の幅。行の幅 × `LIST_TAGS_W_RATIO` で、行の左右パディング
/// （`p_2` の 8+8）を引いてから割合を掛ける。行の幅は窓幅からサイドバー（`SIDEBAR_W`）と
/// ビューの左右パディング（`p_3` の 12+12）を引いたもの（カードの幅計算と同じ前提）。
pub(crate) fn list_tag_area_width(window: &Window) -> f32 {
    let row_width = window.bounds().size.width.as_f32() - SIDEBAR_W - 24.0;
    (row_width - 16.0) * LIST_TAGS_W_RATIO
}

/// 折りたたみ時にリストのタグ列へ出せるタグ数。個数の固定上限は持たず、
/// **タグ列の幅 × 折り返し行数**から計算する（窓が広いほど多く出る）。
/// タグの幅は GPUI のテキスト計測で実測する。
pub(crate) fn list_tags_visible_count(
    window: &Window,
    theme: &gpui_kit::component::Theme,
    ordered: &[String],
) -> usize {
    let area_w = list_tag_area_width(window);
    let widths: Vec<f32> = ordered
        .iter()
        .map(|tag| tag_chip_width(window, theme, tag))
        .collect();
    // 最後の行には末尾要素（タグ編集ボタン + 「+n」チップ）が入る。
    // 「+n」のラベル幅は残り件数で変わるので、最大桁数（+99）で見積もる。
    let trailing = TAG_EDIT_BUTTON_W
        + TAG_CHIP_GAP
        + tag_text_width(window, theme, "+99")
        + TAG_TOGGLE_CHROME_W;
    let visible = packed_tag_count(&widths, area_w, LIST_TAG_MAX_ROWS, trailing);
    if visible == ordered.len() {
        // 全部入るので「+n」は不要 → 編集ボタンぶんだけ確保して詰め直す
        packed_tag_count(&widths, area_w, LIST_TAG_MAX_ROWS, TAG_EDIT_BUTTON_W)
    } else {
        visible
    }
}

/// 表示対象のローカル本（所有者フィルタ）。ログイン中は現在の sub の本、
/// 未ログインは未所属（NULL）の本だけ。本棚と履歴で同じ判定を使う。
pub(crate) fn owned_book_ids(state: &AppState) -> std::collections::HashSet<String> {
    let db = &state.db_pool;
    let google_sub = state.google_profile.lock().as_ref().map(|p| p.sub.clone());
    let key = state.secrets.db_key().ok();
    match (google_sub, key) {
        (Some(sub), Some(key)) => {
            db::books::owned_book_ids(db, &key, Some(&sub)).unwrap_or_default()
        }
        // ログイン中だが key が無い → 復号不能なので表示しない。
        (Some(_), None) => std::collections::HashSet::new(),
        // 未ログイン → 未所属(NULL)。NULL 判定は key を使わないのでダミーで良い。
        (None, key) => db::books::owned_book_ids(db, key.as_ref().unwrap_or(&[0u8; 32]), None)
            .unwrap_or_default(),
    }
}

/// タグチップのラベル（タグ名 / 「+n」）の幅を GPUI のテキスト計測で実測する。
/// チップは `.text_xs()`（= rem の 0.75 倍）なので、フォントサイズは rem から求める。
/// 計測結果は GPUI 側でフレーム単位にキャッシュされる（同じ文字列は次フレームで再利用）。
pub(crate) fn tag_text_width(
    window: &Window,
    theme: &gpui_kit::component::Theme,
    text: &str,
) -> f32 {
    let font_size = window.rem_size() * 0.75;
    let run = gpui_kit::TextRun {
        len: text.len(),
        font: gpui_kit::Font {
            family: theme.font_family.clone(),
            ..Default::default()
        },
        ..Default::default()
    };
    window
        .text_system()
        .layout_line(text, font_size, &[run], None)
        .width
        .as_f32()
}

/// タグチップ 1 個の幅（タグ名の実測幅 + チップの装飾）。
pub(crate) fn tag_chip_width(
    window: &Window,
    theme: &gpui_kit::component::Theme,
    tag: &str,
) -> f32 {
    tag_text_width(window, theme, tag) + CHIP_CHROME_W
}

/// 幅 `widths`（表示順）のチップを `row_width` の行に詰め、最後に使った行の使用幅と
/// 個数を返す（`max_rows` 行を超えない）。
pub(crate) fn packed_rows(widths: &[f32], row_width: f32, max_rows: usize) -> (usize, f32) {
    let mut rows = 1usize;
    let mut used = 0.0f32;
    let mut count = 0usize;
    for width in widths {
        if used > 0.0 && used + TAG_CHIP_GAP + width > row_width {
            if rows >= max_rows {
                break;
            }
            rows += 1;
            used = 0.0;
        }
        used += if used > 0.0 {
            TAG_CHIP_GAP + width
        } else {
            *width
        };
        count += 1;
    }
    (count, used)
}

/// 折りたたみ時に表示するタグ数を、チップの幅と表示エリアの幅から計算する。
///
/// 幅の広い順（＝表示順）に詰めていき、`max_rows` 行に収まる個数を返す。最後に使った
/// 行には末尾要素（タグ編集ボタン / 「+n」チップ）の幅 `trailing` を確保するので、
/// 入らなければ 1 個ずつ減らす。幅が足りないときでも 1 個は出す（「+n」だけの行を作らない）。
pub(crate) fn packed_tag_count(
    widths: &[f32],
    row_width: f32,
    max_rows: usize,
    trailing: f32,
) -> usize {
    if widths.is_empty() || max_rows == 0 || row_width <= 0.0 {
        return 0;
    }
    let (mut count, _) = packed_rows(widths, row_width, max_rows);
    while count > 1 {
        let (_, used) = packed_rows(&widths[..count], row_width, max_rows);
        if used + TAG_CHIP_GAP + trailing <= row_width {
            break;
        }
        count -= 1;
    }
    count
}

/// タグの折りたたみトグルのラベル。折りたたみ中は残り件数（「+n」）、展開中は「閉じる」。
pub(crate) fn tag_toggle_label(hidden: usize, expanded: bool) -> String {
    if expanded {
        "閉じる".to_string()
    } else {
        format!("+{hidden}")
    }
}

/// タグの使用数（タグ名 → そのタグを持つカード数）を数える。
/// 同じカードに同じタグが複数あっても 1 冊として数える。
pub(crate) fn count_tag_usage<'a>(
    cards: impl IntoIterator<Item = &'a [String]>,
) -> std::collections::HashMap<String, usize> {
    let mut counts = std::collections::HashMap::new();
    for tags in cards {
        for tag in tags.iter().collect::<std::collections::HashSet<_>>() {
            *counts.entry(tag.clone()).or_insert(0) += 1;
        }
    }
    counts
}

/// Download/import progress for a bookshelf card, fraction 0..1.
#[derive(Debug, Clone, Copy, PartialEq)]
enum DownloadState {
    Downloading(f32),
    Processing(f32),
}

impl DownloadState {
    fn fraction(self) -> f32 {
        match self {
            DownloadState::Downloading(f) | DownloadState::Processing(f) => f.clamp(0.0, 1.0),
        }
    }
    fn label(self) -> &'static str {
        match self {
            DownloadState::Downloading(_) => "Download",
            DownloadState::Processing(_) => "取込中",
        }
    }
}

pub struct BookshelfView {
    entries: Vec<BookEntry>,
    shelf_items: Vec<bookshelf::BookshelfItem>,
    shelf_cards: Vec<ShelfCard>,
    download_states: HashMap<String, DownloadState>,
    favorite_tags: Vec<String>,
    /// お気に入りサークル（チップのハート。`favorite_entities`）
    favorite_circles: Vec<String>,
    /// お気に入り作者（チップのハート。`favorite_entities`）
    favorite_authors: Vec<String>,
    search_state: Option<Entity<InputState>>,
    selected_tags: Vec<String>,
    all_tags: Vec<String>,
    /// タグ名 → そのタグを持つカード数（タグチップの並び替えキー）。`reload` で作る。
    /// 描画ごとに `TagOrder` が `Arc` で共有するため、コピーは `reload` 時だけ。
    tag_counts: std::sync::Arc<std::collections::HashMap<String, usize>>,
    selected_events: Vec<String>,
    available_events: Vec<String>,
    /// サイトフィルタ（None = すべての本、Some(site_id) = そのサイトのみ）
    site_filter: Option<String>,
    /// サークル名での絞り込み（None = なし）。カードのサークル名クリックで設定
    circle_filter: Option<String>,
    /// 作者名での絞り込み（None = なし）。カードの作者名クリックで設定
    author_filter: Option<String>,
    /// インラインタグ編集中の本（Web の TagList 編集モード相当）
    editing_book_id: Option<String>,
    /// 編集中の本のサイト id（FANZA ジャンル再取得ボタン/保存先の判定用）
    editing_site_id: Option<String>,
    /// 編集中のタグ一覧
    editing_tags: Vec<String>,
    /// タグ編集のサジェスチョン（後で読む + お気に入りタグ）
    editing_suggestions: Vec<String>,
    /// タグ編集の入力状態
    editing_input: Option<Entity<InputState>>,
    /// アクション経由でリクエストされたタグ編集（window が必要なため render で処理）
    pending_tag_edit: Option<String>,
    /// タグ列を展開している本（「+n」→「閉じる」）。行ごとに独立。
    expanded_tag_rows: std::collections::HashSet<String>,
    read_filter: ReadFilter,
    view_mode: ViewMode,
    tag_fetch_enabled: bool,
    /// 実行中の同期タスク数（TBF / BOOTH が並列に走るためカウンタで管理）
    sync_busy: usize,
    /// 表紙取得（fetch_remote_covers）の実行中フラグ（二重実行防止）
    fetching_covers: bool,
    /// 表紙取得の再実行済みフラグ（同期 reload で後から増えたカード分を 1 回だけ再取得）
    cover_fetch_retried: bool,
    /// 取り込み確認モーダル（§6.3。曖昧な構造のときだけ出る）
    pending_import: Option<PendingImport>,
    /// 表紙取得中で開始できなかったダウンロード（取得完了後に実行する）
    pending_download: Option<bookshelf::BookshelfItem>,
    auto_download_started: bool,
    /// フィルタ済みカードのインデックス（List 仮想化用キャッシュ）
    filtered: Vec<usize>,
    /// フィルタ条件が変わったら true（render で filtered を再計算）
    filtered_dirty: bool,
    /// 直前の検索文字列（変更検出用）
    last_search: String,
    /// 本棚グリッドの仮想化リスト状態
    list_state: gpui_kit::ListState,
    /// リスト表示のスクロール追跡（選択行のスクロール連動用）
    scroll_handle: gpui_kit::ScrollHandle,
    /// リスト表示の行ごとの関連書籍カルーセル状態（`reload` で作る）。
    /// render 中に作ると毎描画で実体が増えるため、view 側で保持して使い回す。
    carousel_states: HashMap<String, Entity<CarouselState>>,
    /// キーバインド用フォーカス
    focus_handle: gpui_kit::FocusHandle,
    /// フォーカス初回付与済みフラグ
    focus_initialized: bool,
    /// 選択中のカード（filtered 内のインデックス）
    selected_index: Option<usize>,
    error: Option<String>,
    toast: Option<String>,
}

/// 取り込みの成功結果（読み飛ばしたエントリの警告付き）。
struct ImportOutcome {
    title: String,
    warnings: Vec<String>,
}

/// 取り込み確認モーダルの内容（§6.3）。
///
/// ダウンロード済みの bytes は worker スレッドが保持したまま `reply` を待つ
/// （モーダル側は要約だけを持つ）。
struct PendingImport {
    /// 本のタイトル（見出しに出す）
    title: String,
    /// コンテンツごとの要約
    choices: Vec<ImportChoice>,
    /// 選択中の添字（初期値は計画の既定表示）
    selected: usize,
    /// 選択（`None` = キャンセル）を返す先。worker が `recv` で待っている。
    reply: std::sync::mpsc::Sender<Option<usize>>,
}

/// 確認モーダルに出す 1 コンテンツ分の要約。
#[derive(Clone)]
struct ImportChoice {
    display_name: String,
    /// `画像` / `PDF` / `EPUB` / `音声` / `動画`
    kind: String,
    /// `画像 48ファイル / PDF 1ファイル` のようなレンディションの要約
    detail: String,
}

/// 確認モーダルを出すか（§6.3: 形式が複数 / コンテンツが複数 / 差分セット）。
fn import_needs_confirmation(plan: &thundoku_core::import::ImportPlan) -> bool {
    let renditions: usize = plan
        .contents
        .iter()
        .map(|content| content.renditions.len())
        .sum();
    plan.contents.len() > 1 || renditions > 1
}

/// 計画から確認モーダルの要約を作る。
fn import_choices(plan: &thundoku_core::import::ImportPlan) -> Vec<ImportChoice> {
    plan.contents
        .iter()
        .map(|content| ImportChoice {
            display_name: content.display_name.clone(),
            kind: content.media_kind.label().to_string(),
            detail: content
                .renditions
                .iter()
                .map(|rendition| {
                    // PDF / EPUB はページ数が展開するまで不明なのでファイル数で示す
                    format!("{} {}ファイル", rendition.label, rendition.entries.len())
                })
                .collect::<Vec<_>>()
                .join(" / "),
        })
        .collect()
}

/// 取り込み確認モーダルを出して選択を待つ（worker スレッドをブロックする。UI は動く）。
///
/// 戻り値: `Some(index)` = 選ばれた既定表示 / `None` = キャンセル（UI が閉じた場合も含む）。
/// ダウンロード済みの `bytes` はこの間 worker が保持し続ける（モーダルは要約だけ持つ）。
fn ask_import_confirmation(
    prompt_tx: &std::sync::mpsc::Sender<PendingImport>,
    title: &str,
    plan: &thundoku_core::import::ImportPlan,
) -> Option<usize> {
    let (reply, answer) = std::sync::mpsc::channel();
    let request = PendingImport {
        title: title.to_string(),
        choices: import_choices(plan),
        selected: plan.primary,
        reply,
    };
    if prompt_tx.send(request).is_err() {
        return None;
    }
    answer.recv().ok().flatten()
}

/// 取り込みの失敗。UI 文言を出し分けるために型で持つ。
enum ImportFailure {
    /// 読めるコンテンツが無い（txt のみ / ゲーム / HTML 閲覧型など）
    NotAReadable,
    /// 取り込み確認モーダルでキャンセルされた
    Cancelled,
    /// それ以外（DRM・通信・解析失敗など）。文言はそのまま出す
    Message(String),
}

impl From<String> for ImportFailure {
    fn from(message: String) -> Self {
        Self::Message(message)
    }
}

/// 取り込みエラーを UI 用の失敗種別に変換する（`NotAReadableWork` だけ特別扱い）。
fn import_failure(error: thundoku_core::import::ImportError) -> ImportFailure {
    match error {
        thundoku_core::import::ImportError::NotAReadableWork => ImportFailure::NotAReadable,
        other => ImportFailure::Message(other.to_string()),
    }
}

/// 取り込み結果を (トースト, 赤いエラー行) に変換する。
/// I/O と切り離した純粋関数にして文言を試せるようにしている。
fn download_messages(
    result: &Result<ImportOutcome, ImportFailure>,
) -> (Option<String>, Option<String>) {
    match result {
        Ok(outcome) if outcome.warnings.is_empty() => (
            Some(format!("「{}」をダウンロードしました", outcome.title)),
            None,
        ),
        Ok(outcome) => {
            // 壊れた画像などで読み飛ばした分は件数と先頭 2 件を出す
            let head: Vec<&str> = outcome
                .warnings
                .iter()
                .take(2)
                .map(|warning| warning.as_str())
                .collect();
            let rest = outcome.warnings.len().saturating_sub(head.len());
            let detail = if rest > 0 {
                format!("{} ほか {rest} 件", head.join(" / "))
            } else {
                head.join(" / ")
            };
            (
                Some(format!(
                    "「{}」をダウンロードしました（一部を読み飛ばし: {detail}）",
                    outcome.title
                )),
                None,
            )
        }
        Err(ImportFailure::NotAReadable) => (
            None,
            Some(
                "取り込めるコンテンツがありません（txt のみ・ゲーム・HTML 閲覧型など）。この作品はビューアーで読めません"
                    .to_string(),
            ),
        ),
        Err(ImportFailure::Cancelled) => (Some("取り込みをキャンセルしました".to_string()), None),
        Err(ImportFailure::Message(message)) => (None, Some(message.clone())),
    }
}

impl BookshelfView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let handle = cx.weak_entity();
        let edit_tags_handle = handle.clone();
        App::on_action(cx, move |action: &EditBookTags, cx: &mut App| {
            let book_id = action.book_id.to_string();
            edit_tags_handle
                .update(cx, |this, _cx| {
                    this.pending_tag_edit = Some(book_id);
                })
                .ok();
        });
        let delete_handle = handle.clone();
        App::on_action(cx, move |action: &DeleteBook, cx: &mut App| {
            let book_id = action.book_id.to_string();
            delete_handle
                .update(cx, |this, cx| this.delete_book(cx, &book_id))
                .ok();
        });
        let redownload_handle = handle.clone();
        App::on_action(
            cx,
            move |action: &crate::actions::RedownloadBook, cx: &mut App| {
                let database_id = action.database_id.to_string();
                let site_id = action.site_id.to_string();
                redownload_handle
                    .update(cx, |this, cx| {
                        let Some(card) = this
                            .shelf_cards
                            .iter()
                            .find(|c| {
                                c.shelf.database_id == database_id && c.shelf.site_id == site_id
                            })
                            .cloned()
                        else {
                            return;
                        };
                        this.redownload_item(cx, &card);
                    })
                    .ok();
            },
        );
        let handle = handle.clone();
        App::on_action(cx, move |action: &HideBook, cx: &mut App| {
            let database_id = action.database_id.to_string();
            let site_id = action.site_id.to_string();
            handle
                .update(cx, |this, cx| {
                    let Some(card) = this
                        .shelf_cards
                        .iter()
                        .find(|c| c.shelf.database_id == database_id && c.shelf.site_id == site_id)
                        .cloned()
                    else {
                        return;
                    };
                    this.toggle_hidden(cx, &card);
                })
                .ok();
        });
        let mut view = Self {
            entries: Vec::new(),
            shelf_items: Vec::new(),
            shelf_cards: Vec::new(),
            download_states: HashMap::new(),
            favorite_tags: Vec::new(),
            favorite_circles: Vec::new(),
            favorite_authors: Vec::new(),
            search_state: None,
            selected_tags: Vec::new(),
            all_tags: Vec::new(),
            tag_counts: std::sync::Arc::new(std::collections::HashMap::new()),
            selected_events: Vec::new(),
            site_filter: Self::read_site_filter(cx),
            circle_filter: None,
            author_filter: None,
            available_events: Vec::new(),
            editing_book_id: None,
            editing_site_id: None,
            editing_tags: Vec::new(),
            editing_suggestions: Vec::new(),
            editing_input: None,
            pending_tag_edit: None,
            expanded_tag_rows: std::collections::HashSet::new(),
            read_filter: ReadFilter::All,
            view_mode: ViewMode::Card,
            tag_fetch_enabled: true,
            sync_busy: 0,
            fetching_covers: false,
            cover_fetch_retried: false,
            pending_import: None,
            pending_download: None,
            auto_download_started: false,
            filtered: Vec::new(),
            filtered_dirty: true,
            last_search: String::new(),
            list_state: gpui_kit::ListState::new(
                0,
                gpui_kit::ListAlignment::Top,
                gpui_kit::px(100.0),
            )
            .measure_all(),
            scroll_handle: gpui_kit::ScrollHandle::new(),
            carousel_states: HashMap::new(),
            focus_handle: cx.focus_handle(),
            focus_initialized: false,
            selected_index: Some(0),
            error: None,
            toast: None,
        };
        view.reload(cx);
        view
    }

    fn app_state(cx: &App) -> &AppState {
        AppState::global(cx)
    }
    /// 終了時の「アップロードして終了」実行中か（ダウンロード・ビューアー起動を
    /// ブロックし、アップロードとバッティングしないようにする）。
    fn is_exit_uploading(cx: &App) -> bool {
        AppState::global(cx)
            .exit_uploading
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    /// Reload bookshelf items + local books; resolve covers (local pack ->
    /// cached file -> placeholder), then scheduled remote fetch for the rest.
    /// サイトで本棚を絞り込む（None = すべての本、Some("techbookfest") = 技術書典）。
    /// shelf_cards は既に構築済みで、表示は visible_shelf_cards がフィルタする
    /// ため reload（DB 再読込 + カバー再ロード）は行わない（即時反映・軽量）。
    pub fn set_site_filter(&mut self, cx: &mut Context<Self>, site: Option<&str>) {
        // 展開中・同期中に切り替えると busy 表示や処理がバッティングするため無視する
        if self.fetching_covers || self.sync_busy > 0 {
            return;
        }
        // 個別サイトへ切り替えたときは、そのサイトでは無効になり得る絞り込みを解除する
        // （FANZA のタグで絞ったまま技術書典へ移ると 0 件になる、など）。
        // 検索・既読モードはサイトに依存しないので維持し、「すべての本」へ戻すときは
        // 何も解除しない。
        if site.is_some() && self.site_filter.as_deref() != site {
            self.selected_tags.clear();
            self.selected_events.clear();
            self.circle_filter = None;
            self.author_filter = None;
        }
        self.site_filter = site.map(String::from);
        self.filtered_dirty = true;
        // 次回起動時に同じ表示を復元する
        let _ = db::settings::set(
            &Self::app_state(cx).db_pool,
            "bookshelf.site_filter",
            site.unwrap_or("all"),
        );
        cx.notify();
    }

    /// 現在のサイトフィルタ（サイドバーのアクティブ表示用）。
    pub(crate) fn site_filter(&self) -> Option<String> {
        self.site_filter.clone()
    }

    /// 保存済みのサイトフィルタを復元する（"all" は None）。
    fn read_site_filter(cx: &Context<Self>) -> Option<String> {
        db::settings::get(&Self::app_state(cx).db_pool, "bookshelf.site_filter")
            .ok()
            .flatten()
            .filter(|v| v != "all")
    }

    /// グリッドの列数（Web と同じブレークポイント）
    pub(crate) fn columns_for_width(window_width: f32) -> usize {
        if window_width >= 2560.0 {
            // 4K 等の超広幅では、タイル幅を理想値に近づけるよう列数を増やす
            // （固定 5 列だとタイルが横に間延びするため）。
            let content = window_width - SIDEBAR_W - 24.0; // サイドバー + パディング
            ((content / 320.0).round() as usize).clamp(5, 16)
        } else if window_width >= 1280.0 {
            5
        } else if window_width >= 1024.0 {
            4
        } else if window_width >= 640.0 {
            3
        } else {
            2
        }
    }

    /// キーボード操作: Enter で開く、矢印 / hjkl で選択移動、ESC で絞り込み解除
    fn handle_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => self.activate_selected(cx),
            "right" | "l" => self.shift_selection(1, 0, window, cx),
            "left" | "h" => self.shift_selection(-1, 0, window, cx),
            "down" | "j" => self.shift_selection(0, 1, window, cx),
            "up" | "k" => self.shift_selection(0, -1, window, cx),
            // ESC: 絞り込み中ならすべて解除（全項目へ戻す）。絞り込みが無いときは
            // 何もせず、親（ワークスペース）の ESC 処理へ流す。
            "escape" if self.is_filtering(cx) => {
                cx.stop_propagation();
                self.clear_filters(window, cx);
            }
            _ => {}
        }
    }

    /// 選択中のカードを開く（カードクリックと同じ動作: ローカル本はビューアー、
    /// リモート本はダウンロード）
    fn activate_selected(&mut self, cx: &mut Context<Self>) {
        if Self::is_exit_uploading(cx) {
            return;
        }
        let Some(idx) = self.selected_index else {
            return;
        };
        let Some(&card_idx) = self.filtered.get(idx) else {
            return;
        };
        let card = self.shelf_cards[card_idx].clone();
        if self.download_states.contains_key(&card.shelf.database_id) {
            return;
        }
        if let Some(book_id) = card.local.as_ref().map(|e| e.book.id.clone()) {
            self.open_book(cx, &book_id);
        } else {
            self.download_item(cx, card.shelf.clone());
        }
    }

    /// 選択位置を移動（dx: 左右、dy: 上下 — 上下は列数分移動）
    fn shift_selection(&mut self, dx: i64, dy: i64, window: &mut Window, cx: &mut Context<Self>) {
        if self.filtered.is_empty() {
            return;
        }
        // リスト表示は 1 列（上下・左右とも ±1）、カード表示は列数分のグリッド移動。
        let cols = if self.view_mode == ViewMode::List {
            1
        } else {
            Self::columns_for_width(window.bounds().size.width.as_f32())
        };
        let len = self.filtered.len() as i64;
        let current = self.selected_index.unwrap_or(0) as i64;
        let next = (current + dx + dy * cols as i64).clamp(0, len - 1);
        if next != current {
            self.selected_index = Some(next as usize);
            // 選択行が見えるようにスクロールを連動させる
            if self.view_mode == ViewMode::List {
                self.scroll_handle.scroll_to_item(next as usize);
            } else {
                let row = (next as usize) / cols;
                self.list_state.scroll_to_reveal_item(row);
            }
            cx.notify();
        }
    }

    /// 設定画面の非表示解除・切り替えをカードに軽量反映する
    /// （全 reload は 315 件のカード再生成で数秒かかりビジーになるため、
    ///   DB から is_hidden を取得して該当カードのフラグだけ更新する）
    pub fn refresh_hidden(&mut self, cx: &Context<Self>) {
        let state = Self::app_state(cx);
        let items = db::bookshelf::list_all(&state.db_pool).unwrap_or_default();
        let hidden: std::collections::HashMap<(String, String), i64> = items
            .iter()
            .map(|item| {
                (
                    (item.site_id.clone(), item.database_id.clone()),
                    item.is_hidden,
                )
            })
            .collect();
        for card in &mut self.shelf_cards {
            if let Some(&is_hidden) =
                hidden.get(&(card.shelf.site_id.clone(), card.shelf.database_id.clone()))
            {
                card.shelf.is_hidden = is_hidden;
            }
        }
        self.filtered_dirty = true;
    }

    /// 本棚キャッシュ上の 1 冊の進捗表示状態（(current_page, total_pages, is_read)）。
    /// 本棚カードの表示に使うキャッシュ（`entries`）の値を返す。テストからも参照する。
    #[cfg(test)]
    pub(crate) fn progress_for_book(&self, book_id: &str) -> Option<(i64, Option<i64>, bool)> {
        self.entries.iter().find(|e| e.book.id == book_id).map(|e| {
            let (current, total) = e.progress.unwrap_or((0, None));
            (current, total, e.is_read)
        })
    }

    /// チップ表示（お気に入り / 絞り込み選択）に使う状態をまとめて取り出す。
    fn chip_state(&self) -> ChipState {
        ChipState {
            favorite_circles: self.favorite_circles.clone(),
            favorite_authors: self.favorite_authors.clone(),
            circle_filter: self.circle_filter.clone(),
            author_filter: self.author_filter.clone(),
        }
    }

    /// 関連書籍（同一サークル / 同一作者）のインデックスと、行ごとのカルーセル状態を作る。
    /// `shelf_cards` が確定した後に呼ぶ（`reload` の最後）。
    fn rebuild_related(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<(String, String)> = self
            .shelf_cards
            .iter()
            .map(|card| (card.shelf.circle_name.clone(), card.shelf.author.clone()))
            .collect();
        for index in 0..self.shelf_cards.len() {
            let related = related_book_indices(&keys, index, LIST_RELATED_LIMIT);
            self.shelf_cards[index].related = related;
        }
        // カルーセルの状態は関連件数が変わったときだけ作り直す（毎描画で作ると実体が増える）。
        let counts: Vec<(String, usize)> = self
            .shelf_cards
            .iter()
            .filter(|card| !card.related.is_empty())
            .map(|card| (card.shelf.database_id.clone(), card.related.len()))
            .collect();
        let live: std::collections::HashSet<String> =
            counts.iter().map(|(id, _)| id.clone()).collect();
        self.carousel_states.retain(|id, _| live.contains(id));
        for (id, count) in counts {
            let needs_new = match self.carousel_states.get(&id) {
                Some(state) => state.read(cx).item_count() != count,
                None => true,
            };
            if needs_new {
                let state = cx.new(|_| CarouselState::new(count));
                self.carousel_states.insert(id, state);
            }
        }
    }

    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        log::info!("reload: 開始");
        let reload_start = std::time::Instant::now();
        let (
            entries,
            shelf_items,
            all_tags,
            favorite_tags,
            favorite_circles,
            favorite_authors,
            available_events,
            shelf_cards,
        ) = {
            let state = Self::app_state(cx);
            let db = &state.db_pool;
            let packs_dir = state.packs_dir.clone();
            let thumbnails_dir = state.data_dir.join("thumbnails");
            // 所有者フィルタ：ログイン中は現在 sub の本、未ログインは未所属(NULL)の本だけ表示。
            let owned = owned_book_ids(state);
            let mut entries = Vec::new();
            for book in books::list(db)
                .unwrap_or_default()
                .into_iter()
                .filter(|b| owned.contains(&b.id))
            {
                let tags: Vec<String> = db::tags::list_for_book(db, &book.id)
                    .unwrap_or_default()
                    .into_iter()
                    .map(|tag| tag.tag_name)
                    .collect();
                let progress = progress::get(db, &book.id).ok().flatten();
                // 一度でも最終ページまで表示したら読了（finished_at が立つと戻っても維持）。
                // upsert 時に最終ページ到達で finished_at がセットされる。
                let is_read = progress.as_ref().is_some_and(|p| p.finished_at.is_some());
                // ダウンロード直後は reading_progress が無いため、インポート時の
                // total_pages（imported_documents）から表示用の進捗を作る。
                // ページ数は 1-indexed（未読 = 1 ページ目）
                let progress_tuple =
                    progress
                        .map(|p| (p.current_page, p.total_pages))
                        .or_else(|| {
                            documents::get_document_by_book_id(db, &book.id)
                                .ok()
                                .flatten()
                                .filter(|d| d.total_pages > 0)
                                .map(|d| (1, Some(d.total_pages)))
                        });
                let cover = load_cover_image(&packs_dir, &book);
                entries.push(BookEntry {
                    book,
                    tags,
                    is_read,
                    progress: progress_tuple,
                    cover,
                });
            }
            let mut shelf_items = bookshelf::list_all(db).unwrap_or_default();
            // イベント名の補完（Web の同期は eventName を保存するが、デスクトップの
            // GraphQL 応答で空になることがある。tbf_events のスラグから引く）
            {
                log::info!("reload: イベント名補完開始（{:?}）", reload_start.elapsed());
                let events = db::checklist::list_events(db).unwrap_or_default();
                let name_by_slug: std::collections::HashMap<&str, &str> = events
                    .iter()
                    .filter_map(|e| e.slug.as_deref().map(|slug| (slug, e.event_name.as_str())))
                    .collect();
                for item in &mut shelf_items {
                    // 1) event_slug から tbf_events を引いて補完
                    if item
                        .event_name
                        .as_deref()
                        .is_none_or(|name| name.is_empty())
                        && let Some(slug) = item.event_slug.as_deref()
                        && let Some(name) = name_by_slug.get(slug)
                    {
                        item.event_name = Some(name.to_string());
                        // DB 書き込みはしない（315 件 × UPDATE が reload を
                        // 100 秒超まで遅くする原因のため。表示・フィルタは
                        // 毎回の補完で同じ結果になる）
                    }
                    // 2) 購入日（caused_at）が開催期間内ならイベント名を補完
                    //    （Web の findEventInfoByDate 相当）
                    if item
                        .event_name
                        .as_deref()
                        .is_none_or(|name| name.is_empty())
                        && let Some(caused) = item.caused_at.as_deref()
                    {
                        let date = caused.split('T').next().unwrap_or(caused);
                        let matched = events
                            .iter()
                            .filter(|event| {
                                event.event_start_date.as_deref().is_some_and(|start| {
                                    start <= date
                                        && event
                                            .event_end_date
                                            .as_deref()
                                            .is_some_and(|end| end >= date)
                                })
                            })
                            .max_by_key(|event| event.event_start_date.clone());
                        if let Some(event) = matched {
                            let name = event.event_name.clone();
                            item.event_name = Some(name.clone());
                            // DB 書き込みはしない（上記と同じ理由）
                        }
                    }
                }
                log::info!("reload: イベント名補完完了（{:?}）", reload_start.elapsed());
            }
            // 既存のダウンロード済み本（tbf_product_id 未設定）を本棚と紐付ける
            // （ダウンロード時は file_name をそのまま使うため一致する）。
            for entry in &mut entries {
                if entry.book.tbf_product_id.is_none() {
                    let matched = shelf_items.iter().find(|shelf| {
                        shelf
                            .file_name
                            .as_deref()
                            .is_some_and(|f| f == entry.book.file_name)
                            || (!shelf.title.is_empty() && shelf.title == entry.book.title)
                    });
                    if let Some(shelf) = matched {
                        let _ = books::set_tbf_product_id(db, &entry.book.id, &shelf.database_id);
                        entry.book.tbf_product_id = Some(shelf.database_id.clone());
                    }
                }
            }
            let mut all_tags: Vec<String> = db::tags::all_tags(db).unwrap_or_default();
            // bookshelf_items の tags_json 由来タグもフィルタ候補に含める
            for item in &shelf_items {
                for tag in bookshelf::tags_of(item) {
                    if !all_tags.contains(&tag) {
                        all_tags.push(tag);
                    }
                }
            }
            let favorite_tags: Vec<String> = db::tags::list_favorites(db).unwrap_or_default();
            let favorite_circles: Vec<String> =
                db::favorites::list_favorites(db, db::favorites::EntityKind::Circle)
                    .unwrap_or_default();
            let favorite_authors: Vec<String> =
                db::favorites::list_favorites(db, db::favorites::EntityKind::Author)
                    .unwrap_or_default();
            let mut available_events: Vec<String> = Vec::new();
            for item in &shelf_items {
                if let Some(event) = item.event_name.as_deref()
                    && !event.is_empty()
                    && !available_events.contains(&event.to_string())
                {
                    available_events.push(event.to_string());
                }
            }
            // 技術書典N は番号の降順（最新が一番上）、それ以外は名前順
            fn event_rank(name: &str) -> Option<u32> {
                let trimmed = name.trim();
                let lower = trimmed.to_lowercase();
                for prefix in ["techbookfest", "技術書典"] {
                    if let Some(rest) = lower.strip_prefix(prefix) {
                        let digits = rest.trim_start();
                        if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                            return digits.parse().ok();
                        }
                    }
                }
                None
            }
            available_events.sort_by(|a, b| match (event_rank(a), event_rank(b)) {
                (Some(x), Some(y)) => y.cmp(&x),
                (Some(_), None) => std::cmp::Ordering::Less,
                (None, Some(_)) => std::cmp::Ordering::Greater,
                (None, None) => a.cmp(b),
            });

            let mut shelf_cards = Vec::new();
            for shelf in &shelf_items {
                let local = entries.iter().find(|entry| {
                    entry.book.tbf_product_id.as_deref() == Some(shelf.database_id.as_str())
                });
                // カードの表紙は**サイトから取得した画像**（同期時のサムネイル）を優先する。
                // 取得できていないときだけ pack の表紙（ローカル取り込み）へ落とす。
                let cover = load_cached_cover(&thumbnails_dir, shelf)
                    .or_else(|| local.and_then(|entry| entry.cover.clone()))
                    .or_else(|| placeholder_cover(&shelf.title, &shelf.circle_name));
                let tags = if shelf.site_id == "fanza" || shelf.site_id == "dlsite" {
                    // FANZA / DLsite: タグは shelf.tags_json を正とする（book_tags は重複本で
                    // 分かれるため）。保存/ジャンル取得で両方に書くが、表示は安定。
                    bookshelf::tags_of(shelf)
                } else {
                    local
                        .map(|entry| entry.tags.clone())
                        .unwrap_or_else(|| bookshelf::tags_of(shelf))
                };
                shelf_cards.push(ShelfCard {
                    shelf: shelf.clone(),
                    cover_fetch_failed: false,
                    local: local.map(|entry| {
                        Box::new(BookEntry {
                            book: entry.book.clone(),
                            tags: entry.tags.clone(),
                            is_read: entry.is_read,
                            progress: entry.progress,
                            cover: entry.cover.clone(),
                        })
                    }),
                    tags,
                    cover,
                    related: Vec::new(),
                });
            }
            // ローカルの本（インポート済みで技術書典の本棚に無いもの）も
            // 「すべての本」に含める。技術書典フィルタでは除外される
            // （site_id が techbookfest でないため）。
            for entry in &entries {
                let linked = shelf_cards.iter().any(|card| {
                    card.shelf.database_id
                        == entry.book.tbf_product_id.as_deref().unwrap_or_default()
                });
                if !linked {
                    shelf_cards.push(ShelfCard {
                        shelf: bookshelf::BookshelfItem {
                            site_id: entry.book.site_id.clone().unwrap_or_default(),
                            database_id: entry.book.id.clone(),
                            title: entry.book.title.clone(),
                            circle_name: entry.book.circle_name.clone(),
                            author: entry.book.author.clone(),
                            thumbnail_url: None,
                            format: "BOOK".into(),
                            caused_at: None,
                            event_name: None,
                            event_slug: None,
                            event_id: None,
                            file_name: None,
                            download_url: None,
                            is_downloadable: 0,
                            is_checked: 0,
                            is_purchased: 0,
                            is_new: 0,
                            is_active: 1,
                            is_favorite: entry.book.is_favorite,
                            is_hidden: entry.book.is_hidden,
                            hidden_at: None,
                            tags_json: None,
                            synced_at: String::new(),
                            created_at: entry.book.created_at.clone(),
                            updated_at: entry.book.updated_at.clone(),
                            media_category: None,
                            ai_type: None,
                            is_drm: 0,
                            release_date: None,
                            description: None,
                            theme: None,
                            maker_id: None,
                            page_count: None,
                            age_rating: None,
                            series_name: None,
                        },
                        local: Some(Box::new(entry.clone())),
                        tags: entry.tags.clone(),
                        cover: entry.cover.clone(),
                        cover_fetch_failed: false,
                        related: Vec::new(),
                    });
                }
            }
            (
                entries,
                shelf_items,
                all_tags,
                favorite_tags,
                favorite_circles,
                favorite_authors,
                available_events,
                shelf_cards,
            )
        };
        log::info!(
            "reload: データ取得 + カード生成（{} 件）（{:?}）",
            shelf_cards.len(),
            reload_start.elapsed()
        );
        // タグチップの並び替えに使う集計（カード数分の線形走査。追加の SQL は無し）
        let tag_counts = std::sync::Arc::new(count_tag_usage(
            shelf_cards.iter().map(|card| card.tags.as_slice()),
        ));
        self.entries = entries;
        self.shelf_items = shelf_items;
        self.shelf_cards = shelf_cards;
        self.all_tags = all_tags;
        self.tag_counts = tag_counts;
        self.favorite_tags = favorite_tags;
        self.favorite_circles = favorite_circles;
        self.favorite_authors = favorite_authors;
        self.available_events = available_events;
        self.selected_tags.retain(|tag| self.all_tags.contains(tag));
        self.filtered_dirty = true;
        // 関連書籍（同一サークル / 同一作者）と、行ごとのカルーセル状態を作る
        self.rebuild_related(cx);
        // タグ取得トグル（Web 版の tagFetchEnabled 相当、デフォルト OFF）
        self.tag_fetch_enabled = {
            let state = Self::app_state(cx);
            let db = &state.db_pool;
            db::settings::get(db, "tag.fetch.enabled")
                .ok()
                .flatten()
                .map(|v| v == "true")
                .unwrap_or(true)
        };
        log::info!(
            "reload: fetch_remote_covers 呼び出し（{:?}）",
            reload_start.elapsed()
        );
        // Kick off remote cover fetching for cards that only have a placeholder.
        self.fetch_remote_covers(cx);
        log::info!("reload: 完了（{:?}）", reload_start.elapsed());
        cx.notify();
    }

    /// For cards that only have a placeholder cover, fetch the remote
    /// thumbnail (TBF session), save it to `thumbnails/{site}_{db}.{ext}`
    /// and apply it to the card as each image finishes (1 枚ずつ追加表示).
    fn fetch_remote_covers(&mut self, cx: &mut Context<Self>) {
        let pending: Vec<(String, String, String)> = self
            .shelf_cards
            .iter()
            .filter(|card| card.cover.is_none())
            .filter_map(|card| {
                card.shelf.thumbnail_url.as_ref().map(|url| {
                    (
                        card.shelf.site_id.clone(),
                        card.shelf.database_id.clone(),
                        url.clone(),
                    )
                })
            })
            .collect();
        if pending.is_empty() {
            return;
        }
        // 既に取得中なら二重実行しない（同期完了 reload が複数回走るため）
        if self.fetching_covers {
            log::info!("fetch_remote_covers: 既に実行中のためスキップ");
            return;
        }
        self.fetching_covers = true;
        log::info!(
            "fetch_remote_covers: 表紙の取得を開始（{} 件）",
            pending.len()
        );
        // 処理中であることを提示する（同期完了トーストの後に出る）
        self.toast = Some("書籍情報を展開中です".into());
        cx.notify();
        let state = Self::app_state(cx);
        let tbf_client = state.tbf.clone();
        let booth_session = state.booth_session.lock().clone();
        let data_dir = state.data_dir.clone();
        let handle = cx.entity();
        // タイムアウト付きのエージェント（ハングすると 5 分以上待つ原因になるため）
        let agent = ureq::AgentBuilder::new()
            .timeout_connect(std::time::Duration::from_secs(5))
            .timeout_read(std::time::Duration::from_secs(15))
            .build();
        // 取得完了した表紙を 1 枚ずつ UI に通知するチャネル
        // （デコード済みの縮小サムネイルを送る — UI 側でデコードしない）
        let (tx, rx) = std::sync::mpsc::channel::<(String, String, Arc<RenderImage>)>();
        // 表紙の取得に失敗したカードの通知（NoImage ダミー表示用）
        let (fail_tx, fail_rx) = std::sync::mpsc::channel::<(String, String)>();
        let ok_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let fail_count = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let ok_count_ui = ok_count.clone();
        let fail_count_ui = fail_count.clone();
        // 表紙ダウンロード（同期 + thread::scope）は GPUI のワーカーをブロックしないよう
        // 専用スレッドで実行する（Task を drop するとキャンセルされる問題も回避）
        let _cover_handle = std::thread::spawn(move || {
            let thumbnails_dir = data_dir.join("thumbnails");
            let _ = std::fs::create_dir_all(&thumbnails_dir);
            // 表紙のダウンロードは 1 件ずつだと初回同期で数十秒かかるため
            // 4 並列で取得する。
            let pending = std::sync::Arc::new(pending);
            let next = std::sync::atomic::AtomicUsize::new(0);
            log::info!("cover: スレッドスコープ開始");
            std::thread::scope(|s| {
                for _ in 0..4 {
                    let pending = pending.clone();
                    let next = &next;
                    let tbf_client = &tbf_client;
                    let booth_session = &booth_session;
                    let thumbnails_dir = &thumbnails_dir;
                    let agent = &agent;
                    let tx = tx.clone();
                    let fail_tx = fail_tx.clone();
                    let ok_count = ok_count.clone();
                    let fail_count = fail_count.clone();
                    s.spawn(move || {
                        log::info!("cover worker: スレッド起動");
                        loop {
                            let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            if i < 8 || i.is_multiple_of(100) {
                                log::info!("cover worker: i={i} / {}", pending.len());
                            }
                            let Some((site_id, database_id, url)) = pending.get(i) else {
                                log::info!("cover worker: 終了 (i={i}, len={})", pending.len());
                                break;
                            };
                            let resolved = if url.starts_with("//") {
                                // プロトコル相対（//img.dlsite.jp/... 等）を絶対化する
                                format!("https:{url}")
                            } else if url.starts_with('/') {
                                format!("https://techbookfest.org{url}")
                            } else {
                                url.clone()
                            };
                            let bytes = fetch_cover_bytes(
                                agent,
                                site_id,
                                database_id,
                                &resolved,
                                booth_session.as_ref(),
                                tbf_client,
                            );
                            let Some(bytes) = bytes else {
                                fail_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                log::warn!(
                                    "表紙の取得失敗: {} / {} ({resolved})",
                                    site_id,
                                    database_id
                                );
                                let _ = fail_tx.send((site_id.clone(), database_id.clone()));
                                continue;
                            };
                            // 縮小済みサムネイルを PNG で保存する（reload 時のキャッシュ
                            // 読み込みがオリジナル（1MB 超）だと 300 件で 100 秒超かかるため）
                            write_cover_cache(thumbnails_dir, site_id, database_id, &bytes);
                            // デコード + 縮小はこのスレッド（4 並列）で行い、UI には
                            // デコード済みサムネイルだけ送る（UI スレッドで 307 枚
                            // デコードすると固まるため）
                            let Some(image) = decode_and_resize(&bytes, 448) else {
                                fail_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                log::warn!("表紙のデコード失敗: {site_id} / {database_id}");
                                let _ = fail_tx.send((site_id.clone(), database_id.clone()));
                                continue;
                            };
                            ok_count.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                            // 1 枚取得完了 → UI に通知
                            let _ = tx.send((site_id.clone(), database_id.clone(), image));
                        }
                    });
                }
            });
            log::info!("cover: スレッドスコープ終了");
            Ok::<(), String>(())
        });
        cx.spawn(async move |_window, cx| {
            // 取得完了したカードから順に cover を反映する（まとめて reload しない）。
            // 1 枚ずつ notify すると 315 回の全カード再描画が走ってビジーになるため、
            // 数枚ずつバッチにしてまとめて反映する。
            let mut batch: Vec<(String, String, Arc<RenderImage>)> = Vec::new();
            let apply_batch = |batch: Vec<(String, String, Arc<RenderImage>)>,
                               handle: &gpui_kit::Entity<BookshelfView>,
                               cx: &mut gpui_kit::AsyncApp| {
                handle.update(cx, |this, cx| {
                    for (site_id, database_id, image) in batch {
                        if let Some(card) = this.shelf_cards.iter_mut().find(|c| {
                            c.shelf.site_id == site_id && c.shelf.database_id == database_id
                        }) {
                            card.cover = Some(image);
                        } else {
                            log::warn!(
                                "表紙の適用先カードが見つからない: {site_id} / {database_id}"
                            );
                        }
                    }
                    // 表紙が届いたカードをフィルタ結果に反映する（表示対象の再計算）
                    this.filtered_dirty = true;
                    cx.notify();
                });
            };
            loop {
                match rx.try_recv() {
                    Ok(update) => {
                        batch.push(update);
                        if batch.len() >= 8 {
                            let items = std::mem::take(&mut batch);
                            apply_batch(items, &handle, cx);
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        if !batch.is_empty() {
                            let items = std::mem::take(&mut batch);
                            apply_batch(items, &handle, cx);
                        }
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(50))
                            .await;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        if !batch.is_empty() {
                            let items = std::mem::take(&mut batch);
                            apply_batch(items, &handle, cx);
                        }
                        break;
                    }
                }
            }
            // 表紙の取得に失敗したカードに NoImage フラグを立てる
            for (site_id, database_id) in fail_rx.try_iter() {
                handle.update(cx, |this, cx| {
                    if let Some(card) = this.shelf_cards.iter_mut().find(|c| {
                        c.shelf.site_id == site_id
                            && c.shelf.database_id == database_id
                            && c.cover.is_none()
                    }) {
                        card.cover_fetch_failed = true;
                    }
                    this.filtered_dirty = true;
                    cx.notify();
                });
            }
            // 表紙取得スレッドの完了は不要（デタッチ — チャネルの Disconnected で判定済み）
            let ok = ok_count_ui.load(std::sync::atomic::Ordering::SeqCst);
            let fail = fail_count_ui.load(std::sync::atomic::Ordering::SeqCst);
            log::info!("fetch_remote_covers 完了: 成功 {ok} 件 / 失敗 {fail} 件");
            handle.update(cx, |this, cx| {
                this.fetching_covers = false;
                // 表紙取得中に要求された再取得をここで実行する。
                // 同期中（sync_busy > 0）は download_item が無視するので保留したままにする
                // （同期後の reload でも表紙取得が走るため、その完了時に実行される）。
                if this.sync_busy == 0
                    && let Some(item) = this.pending_download.take()
                {
                    this.download_item(cx, item);
                }
                // 同期 reload のタイミングで後からカードが増えた場合
                // （例: BOOTH 完了 → reload → fetch 後に技術書典 307 件が reload される）、
                // 残っているカードの表紙を取得するため 1 回だけ再実行する
                let still_pending = this.shelf_cards.iter().any(|c| {
                    c.local.is_none() && c.cover.is_none() && c.shelf.thumbnail_url.is_some()
                });
                if still_pending && ok > 0 && !this.cover_fetch_retried {
                    this.cover_fetch_retried = true;
                    this.fetch_remote_covers(cx);
                    return;
                }
                this.cover_fetch_retried = false;
                if ok == 0 {
                    // エラーは赤の 1 行だけ表示する（toast と重複させない）
                    this.toast = None;
                    this.error = Some(format!("表紙を取得できませんでした（{fail} 件）"));
                } else {
                    crate::app_state::set_toast(cx, "書籍情報の展開が完了しました");
                    this.error = None;
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Shelf cards visible under the current filters.
    fn visible_shelf_cards(&self, cx: &App) -> Vec<&ShelfCard> {
        self.shelf_cards
            .iter()
            .filter(|card| self.matches_filter(cx, card))
            .collect()
    }

    /// 1 枚のカードが現在のフィルタ条件に一致するか（visible_shelf_cards と
    /// List 仮想化用の filtered キャッシュで共通の判定ロジック）。
    fn matches_filter(&self, cx: &App, card: &ShelfCard) -> bool {
        let shelf = &card.shelf;
        // 非表示にした本は本棚に出さない
        if shelf.is_hidden == 1 {
            return false;
        }
        // 表紙画像がまだ取得できていないリモート本は、
        // 画像取得完了（fetch_remote_covers → reload）まで表示しない。
        // ローカル本（ダウンロード済み）は pack から表紙を読むため常に表示する。
        // 最初から thumbnail_url の無い本は取得対象外なので表示する。
        if card.local.is_none() && card.cover.is_none() && card.shelf.thumbnail_url.is_some() {
            return false;
        }
        if let Some(site) = &self.site_filter
            && shelf.site_id != *site
        {
            return false;
        }
        if let Some(query) = self.current_search(cx) {
            let query = query.to_lowercase();
            // placeholder（タイトル・サークル・著者）に合わせて author も検索対象にする
            let haystack =
                format!("{} {} {}", shelf.title, shelf.circle_name, shelf.author).to_lowercase();
            if !haystack.contains(&query) {
                return false;
            }
        }
        if !self.selected_events.is_empty()
            && !self
                .selected_events
                .iter()
                .any(|event| shelf.event_name.as_deref() == Some(event.as_str()))
        {
            return false;
        }
        if !self.selected_tags.is_empty() {
            let card_tags: Vec<String> = card
                .local
                .as_ref()
                .map(|e| e.tags.clone())
                .unwrap_or_default();
            if !self
                .selected_tags
                .iter()
                .any(|tag| card_tags.iter().any(|t| t == tag))
            {
                return false;
            }
        }
        if let Some(circle) = &self.circle_filter
            && shelf.circle_name != *circle
        {
            return false;
        }
        if let Some(author) = &self.author_filter
            && shelf.author != *author
        {
            return false;
        }
        match self.read_filter {
            ReadFilter::All => true,
            ReadFilter::Unread => !card.local.as_ref().is_some_and(|e| e.is_read),
            ReadFilter::Read => card.local.as_ref().is_some_and(|e| e.is_read),
            ReadFilter::Favorite => card.shelf.is_favorite == 1,
        }
    }

    fn current_search(&self, cx: &App) -> Option<String> {
        self.search_state
            .as_ref()
            .map(|state| state.read(cx).value().to_string())
            .filter(|value| !value.is_empty())
    }

    fn ensure_search_state(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.search_state.is_none() {
            self.search_state = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder("検索（タイトル・サークル・著者）")
            }));
        }
    }

    /// 同期ボタン: 技術書典の本棚同期に加えて、Google にログイン済みなら
    /// Drive 同期も実施する（Web 版の「同期」ボタン + Drive 同期の統合）。
    pub fn sync_all(&mut self, cx: &mut Context<Self>) {
        // 同期中に再実行されないようにする（ボタンは busy 中も押せる見た目のため）
        if self.sync_busy > 0 {
            return;
        }
        // サイドバーでサイトを選択中は、そのサイトだけ同期する
        match self.site_filter.as_deref() {
            Some("techbookfest") => self.sync_tbf(cx),
            Some("booth") => self.sync_booth(cx),
            Some("fanza") => self.sync_fanza(cx),
            Some("dlsite") => self.sync_dlsite(cx),
            _ => {
                self.sync_tbf(cx);
                self.sync_booth(cx);
                self.sync_fanza(cx);
                self.sync_dlsite(cx);
            }
        }
        let drive_ready = *AppState::global(cx).google_logged_in.lock();
        if drive_ready {
            cx.defer(move |cx| cx.dispatch_action(&SyncDrive));
        }
    }

    /// BOOTH から本棚を同期する（ライブラリ + 購入履歴 + 表紙）。
    pub fn sync_booth(&mut self, cx: &mut Context<Self>) {
        let logged_in = *AppState::global(cx).booth_logged_in.lock();
        if !logged_in {
            self.toast = Some("BOOTH にログインしてから同期してください".into());
            cx.defer(move |cx| {
                cx.dispatch_action(&OpenAuthProvider {
                    provider: crate::views::auth::AuthProvider::Booth,
                })
            });
            cx.notify();
            return;
        }
        log::info!("sync_booth: 開始");
        self.sync_busy += 1;
        self.error = None;
        self.toast = Some("BOOTH サイトのデータを取得中です".into());
        let handle = cx.entity();
        let state = Self::app_state(cx);
        let session = state.booth_session.lock().clone();
        let db = state.db_pool.clone();
        let (tx, rx) = std::sync::mpsc::channel::<Result<usize, String>>();
        // 同期のネットワーク処理は GPUI のワーカーをブロックしないよう専用スレッドで実行する
        std::thread::spawn(move || {
            let result = (|| -> Result<usize, String> {
                let session = session.ok_or_else(|| "BOOTH セッションがありません".to_string())?;
                let client = BoothClient::new(&session);
                // ライブラリ（購入品一覧）と購入履歴（購入日）を取得
                let library = client.library().map_err(|e| e.to_string())?;
                let orders = client.orders().map_err(|e| e.to_string())?;
                // 商品名 → 購入日時の照合マップ（先勝ち = 最新）
                let mut bought_at = std::collections::HashMap::new();
                for order in &orders {
                    bought_at
                        .entry(order.item_title.clone())
                        .or_insert_with(|| order.ordered_at.clone());
                }
                // 各商品の表紙（オリジナルサイズ）URL を取得。
                // 商品ページが消えている（「お探しの本は見つかりませんでした」= 404）場合は
                // 削除対象として記録する（一時的なネットワークエラーでは削除しない）。
                // 初回同期で商品数が多いと 1 件ずつの HTTP 待ちで数十秒かかるため、
                // 4 並列で取得する（BOOTH API への負荷を抑えるため並列度は固定）。
                let covers = std::sync::Mutex::new(HashMap::new());
                let vanished = std::sync::Mutex::new(Vec::new());
                let next = std::sync::atomic::AtomicUsize::new(0);
                let client = BoothClient::new(&session);
                std::thread::scope(|s| {
                    for _ in 0..4 {
                        let client = &client;
                        let covers = &covers;
                        let vanished = &vanished;
                        let next = &next;
                        let library = &library;
                        s.spawn(move || {
                            loop {
                                let i = next.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                                let Some(item) = library.get(i) else { break };
                                match client.item_detail(item.item_id) {
                                    Ok(detail) => {
                                        if let Some(first) = detail.images.first() {
                                            covers
                                                .lock()
                                                .unwrap()
                                                .insert(item.item_id, first.clone());
                                        }
                                    }
                                    Err(thundoku_core::booth::BoothError::NotFound) => {
                                        vanished.lock().unwrap().push(item.item_id);
                                    }
                                    Err(_) => {}
                                }
                            }
                        });
                    }
                });
                let covers = covers.into_inner().unwrap();
                let vanished = vanished.into_inner().unwrap();
                // bookshelf_items へ upsert（site_id = booth）

                // ダウンロードリンクのない商品（物理本のみ等）は本棚に表示しない
                for item in &library {
                    if item.download_url.is_none() {
                        continue;
                    }
                    bookshelf::upsert(
                        &db,
                        &bookshelf::BookshelfItem {
                            site_id: "booth".into(),
                            database_id: item.item_id.to_string(),
                            title: item.title.clone(),
                            circle_name: item.shop_name.clone(),
                            // BOOTH の shop は作成者（作者）を兼ねる。作者名として表示する
                            author: item.shop_name.clone(),
                            thumbnail_url: covers.get(&item.item_id).cloned(),
                            format: "PDF".into(),
                            caused_at: bought_at.get(&item.title).cloned(),
                            event_name: None,
                            event_slug: None,
                            event_id: None,
                            file_name: item.file_name.clone(),
                            download_url: item.download_url.clone(),
                            is_downloadable: 1,
                            is_checked: 0,
                            is_purchased: 1,
                            is_new: 0,
                            is_active: 1,
                            is_favorite: 0,
                            is_hidden: 0,
                            hidden_at: None,
                            tags_json: None,
                            synced_at: "2026-08-25 00:00:00".into(),
                            created_at: "2026-08-25 00:00:00".into(),
                            updated_at: "2026-08-25 00:00:00".into(),
                            media_category: None,
                            ai_type: None,
                            is_drm: 0,
                            release_date: None,
                            description: None,
                            theme: None,
                            maker_id: None,
                            page_count: None,
                            age_rating: None,
                            series_name: None,
                        },
                    )
                    .map_err(|e| e.to_string())?;
                }
                // ライブラリに DL URL が無くなった商品は本棚から取り除く
                let _ = thundoku_core::db::block_on(async {
                    sqlx::query(
                    "DELETE FROM bookshelf_items WHERE site_id = 'booth'                      AND (download_url IS NULL OR download_url = '')",
                )
                .execute(&db)
                .await
                });
                // ライブラリに存在しない商品（購入キャンセル・返品等）も削除する。
                // ダウンロード済み（books に tbf_product_id で紐づく）は残す。
                let current_ids: Vec<String> =
                    library.iter().map(|i| i.item_id.to_string()).collect();
                let delete_not_in_library =
                    |pool: &thundoku_core::db::SqlitePool, ids: &[String]| {
                        thundoku_core::db::block_on(async {
                            if ids.is_empty() {
                                sqlx::query(
                            "DELETE FROM bookshelf_items WHERE site_id = 'booth'                              AND database_id NOT IN                              (SELECT tbf_product_id FROM books WHERE site_id = 'booth')",
                        )
                        .execute(pool)
                        .await
                            } else {
                                let placeholders =
                                    ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                                let sql = format!(
                                    "DELETE FROM bookshelf_items WHERE site_id = 'booth'                              AND database_id NOT IN ({placeholders})                              AND database_id NOT IN                              (SELECT tbf_product_id FROM books WHERE site_id = 'booth')"
                                );
                                let mut query = sqlx::query(&sql);
                                for id in ids {
                                    query = query.bind(id);
                                }
                                query.execute(pool).await
                            }
                        })
                    };
                let _ = delete_not_in_library(&db, &current_ids);
                // 商品ページが消えた本（お探しの本は見つかりませんでした）も削除する。
                // ダウンロード済み（books に紐づく）は残す。
                if !vanished.is_empty() {
                    let placeholders = vanished.iter().map(|_| "?").collect::<Vec<_>>().join(",");
                    let sql = format!(
                        "DELETE FROM bookshelf_items WHERE site_id = 'booth'                      AND database_id IN ({placeholders})                      AND database_id NOT IN                      (SELECT tbf_product_id FROM books WHERE site_id = 'booth')"
                    );
                    let mut query = sqlx::query(&sql);
                    for id in &vanished {
                        query = query.bind(id.to_string());
                    }
                    let _ = thundoku_core::db::block_on(async { query.execute(&db).await });
                }
                Ok::<_, String>(library.len())
            })();
            let _ = tx.send(result);
        });
        cx.spawn(async move |_window, cx| {
            // 読み込み中もスピナーが回るよう notify し続けながら完了を待つ
            let result = loop {
                match rx.try_recv() {
                    Ok(result) => break result,
                    Err(_) => {
                        handle.update(cx, |_, cx| cx.notify());
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(120))
                            .await;
                    }
                }
            };
            handle.update(cx, |this, cx| {
                this.sync_busy = this.sync_busy.saturating_sub(1);
                match result {
                    Ok(count) => {
                        log::info!("sync_booth: 完了（{count} 件）");
                        this.toast = Some(format!("BOOTH サイトから {count} 件取得しました"));
                        this.reload(cx);
                    }
                    Err(message) => {
                        log::error!("sync_booth failed: {message}");
                        this.error = Some(message.clone());
                        if message.contains("not logged in") {
                            cx.defer(move |cx| {
                                cx.dispatch_action(&OpenAuthProvider {
                                    provider: crate::views::auth::AuthProvider::Booth,
                                })
                            });
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// FANZA同人から本棚を同期する（購入済み一覧 → 画像系のみ bookshelf_items へ保存）。
    pub fn sync_fanza(&mut self, cx: &mut Context<Self>) {
        let logged_in = *AppState::global(cx).fanza_logged_in.lock();
        if !logged_in {
            self.toast = Some("FANZA にログインしてから同期してください".into());
            cx.defer(move |cx| {
                cx.dispatch_action(&OpenAuthProvider {
                    provider: crate::views::auth::AuthProvider::Fanza,
                })
            });
            cx.notify();
            return;
        }
        log::info!("sync_fanza: 開始");
        self.sync_busy += 1;
        self.error = None;
        self.toast = Some("FANZA サイトのデータを取得中です".into());
        let handle = cx.entity();
        let state = Self::app_state(cx);
        let session = state.fanza_session.lock().clone();
        let db = state.db_pool.clone();
        let (tx, rx) = std::sync::mpsc::channel::<Result<usize, String>>();
        std::thread::spawn(move || {
            let result = (|| -> Result<usize, String> {
                let session = session.ok_or_else(|| "FANZA セッションがありません".to_string())?;
                let mut client =
                    FanzaClient::with_transport(Box::new(UreqTransport::new()), session);
                thundoku_core::fanza::sync::save_purchases(&db, &mut client)
                    .map_err(|e| e.to_string())
            })();
            let _ = tx.send(result);
        });
        cx.spawn(async move |_window, cx| {
            let result = loop {
                match rx.try_recv() {
                    Ok(result) => break result,
                    Err(_) => {
                        handle.update(cx, |_, cx| cx.notify());
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(120))
                            .await;
                    }
                }
            };
            handle.update(cx, |this, cx| {
                this.sync_busy = this.sync_busy.saturating_sub(1);
                match result {
                    Ok(count) => {
                        log::info!("sync_fanza: 完了（{count} 件）");
                        this.toast = Some(format!("FANZA サイトから {count} 件取得しました"));
                        this.reload(cx);
                    }
                    Err(message) => {
                        log::error!("sync_fanza failed: {message}");
                        this.error = Some(message.clone());
                        if message.contains("not logged in") || message.contains("セッション")
                        {
                            cx.defer(move |cx| {
                                cx.dispatch_action(&OpenAuthProvider {
                                    provider: crate::views::auth::AuthProvider::Fanza,
                                })
                            });
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// DLsite から本棚を同期する（購入済み一覧 → 画像系のみ bookshelf_items へ保存）。
    pub fn sync_dlsite(&mut self, cx: &mut Context<Self>) {
        let logged_in = *AppState::global(cx).dlsite_logged_in.lock();
        if !logged_in {
            self.toast = Some("DLsite にログインしてから同期してください".into());
            cx.defer(move |cx| {
                cx.dispatch_action(&OpenAuthProvider {
                    provider: crate::views::auth::AuthProvider::Dlsite,
                })
            });
            cx.notify();
            return;
        }
        log::info!("sync_dlsite: 開始");
        self.sync_busy += 1;
        self.error = None;
        self.toast = Some("DLsite サイトのデータを取得中です".into());
        let handle = cx.entity();
        let state = Self::app_state(cx);
        let session = state.dlsite_session.lock().clone();
        let db = state.db_pool.clone();
        let (tx, rx) = std::sync::mpsc::channel::<Result<usize, String>>();
        std::thread::spawn(move || {
            let result = (|| -> Result<usize, String> {
                let session = session.ok_or_else(|| "DLsite セッションがありません".to_string())?;
                let mut client =
                    DlsiteClient::with_transport(Box::new(UreqTransport::new()), session);
                thundoku_core::dlsite::sync::save_purchases(&db, &mut client)
                    .map_err(|e| e.to_string())
            })();
            let _ = tx.send(result);
        });
        cx.spawn(async move |_window, cx| {
            let result = loop {
                match rx.try_recv() {
                    Ok(result) => break result,
                    Err(_) => {
                        handle.update(cx, |_, cx| cx.notify());
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(120))
                            .await;
                    }
                }
            };
            handle.update(cx, |this, cx| {
                this.sync_busy = this.sync_busy.saturating_sub(1);
                match result {
                    Ok(count) => {
                        log::info!("sync_dlsite: 完了（{count} 件）");
                        this.toast = Some(format!("DLsite サイトから {count} 件取得しました"));
                        this.reload(cx);
                    }
                    Err(message) => {
                        log::error!("sync_dlsite failed: {message}");
                        this.error = Some(message.clone());
                        if message.contains("not logged in") || message.contains("セッション")
                        {
                            cx.defer(move |cx| {
                                cx.dispatch_action(&OpenAuthProvider {
                                    provider: crate::views::auth::AuthProvider::Dlsite,
                                })
                            });
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 技術書典から本棚を同期する（bookshelf_items upsert）。
    pub fn sync_tbf(&mut self, cx: &mut Context<Self>) {
        let logged_in = *AppState::global(cx).tbf_logged_in.lock();
        if !logged_in {
            self.toast = Some("ログインしてから同期してください".into());
            cx.defer(move |cx| cx.dispatch_action(&OpenAuth));
            cx.notify();
            return;
        }
        log::info!("sync_tbf: 開始");
        self.sync_busy += 1;
        self.error = None;
        self.toast = Some("技術書典サイトのデータを取得中です".into());
        let handle = cx.entity();
        let state = Self::app_state(cx);
        let tbf_client = state.tbf.clone();
        let db = state.db_pool.clone();
        let (tx, rx) = std::sync::mpsc::channel::<Result<usize, String>>();
        // 同期のネットワーク処理は GPUI のワーカーをブロックしないよう専用スレッドで実行する
        std::thread::spawn(move || {
            let result = (|| -> Result<usize, String> {
                let mut client = tbf_client.lock();
                let items = client.bookshelf().map_err(|e| e.to_string())?;

                tbf::sync::save_bookshelf(&db, &items)
                    .map(|_| items.len())
                    .map_err(|e| e.to_string())
            })();
            let _ = tx.send(result);
        });
        cx.spawn(async move |_window, cx| {
            // 読み込み中もスピナーが回るよう notify し続けながら完了を待つ
            let result = loop {
                match rx.try_recv() {
                    Ok(result) => break result,
                    Err(_) => {
                        handle.update(cx, |_, cx| cx.notify());
                        cx.background_executor()
                            .timer(std::time::Duration::from_millis(120))
                            .await;
                    }
                }
            };
            handle.update(cx, |this, cx| {
                this.sync_busy = this.sync_busy.saturating_sub(1);
                match result {
                    Ok(count) => {
                        log::info!("sync_tbf: 完了（{count} 件）");
                        this.toast = Some(format!("技術書典サイトから {count} 件取得しました"));
                        this.reload(cx);
                    }
                    Err(message) => {
                        log::error!("sync_tbf failed: {message}");
                        this.error = Some(message.clone());
                        if message.contains("session expired") {
                            cx.defer(move |cx| cx.dispatch_action(&OpenAuth));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 技術書典の本をダウンロードしてインポートする。
    pub fn download_item(&mut self, cx: &mut Context<Self>, item: bookshelf::BookshelfItem) {
        // 書籍情報の展開中・同期中はダウンロード処理がバッティングするため無視する
        if self.fetching_covers || self.sync_busy > 0 {
            return;
        }
        // 終了時アップロード中はダウンロードを開始しない
        if Self::is_exit_uploading(cx) {
            return;
        }
        self.error = None;
        let database_id = item.database_id.clone();
        self.download_states
            .insert(database_id.clone(), DownloadState::Downloading(0.0));
        let handle = cx.entity();
        let state = Self::app_state(cx);
        let site_id = item.site_id.clone();
        let tbf_client = state.tbf.clone();
        let booth_session = state.booth_session.lock().clone();
        let fanza_session = state.fanza_session.lock().clone();
        let dlsite_session = state.dlsite_session.lock().clone();
        let db = state.db_pool.clone();
        let packs_dir = state.packs_dir.clone();
        let title = item.title.clone();
        let product_id = item.database_id.clone();
        let tag_fetch_enabled = self.tag_fetch_enabled;
        // 所有者（owner_sub）の付け方: ログイン中なら現在 sub で暗号化した pack にして
        // owner_sub を記録する。未ログインなら未暗号化（owner_sub = NULL）。
        let google_sub = state.google_profile.lock().as_ref().map(|p| p.sub.clone());
        let db_key = state.secrets.db_key().ok();
        // Progress is reported from the background task through a channel and
        // applied on the UI thread (the task itself must stay Send).
        // NOTE: sync_channel(64) はバッファが満杯になると send がブロックする。
        // UI がカードの再描画（表紙 307 件の反映など）で忙しいと取り込みが
        // 数分ストールする原因になるため、unbounded の channel を使う。
        let (progress_tx, progress_rx) = std::sync::mpsc::channel::<(String, DownloadState)>();
        // 取り込み確認モーダル（§6.3）を UI に依頼するチャネルと、選択を返すチャネル
        let (prompt_tx, prompt_rx) = std::sync::mpsc::channel::<PendingImport>();
        // ダウンロード + インポート（レンダリング含む）は GPUI のワーカーを
        // 数分ブロックすると他の処理（表紙取得など）が止まってビジーになるため、
        // 専用スレッドで実行して結果をチャネルで受け取る。
        let (result_tx, result_rx) =
            std::sync::mpsc::channel::<Result<ImportOutcome, ImportFailure>>();
        std::thread::spawn(move || {
            let result = (|| -> Result<ImportOutcome, ImportFailure> {
                // ダウンロード（サイトで分岐）:
                // - BOOTH: セッション Cookie で downloadables/{id} を GET → 302 の
                //   Location（署名付き S3 URL）を自動追跡してファイル本体を取得
                // - 技術書典: GraphQL の downloadURL を resolve して取得
                let mut genre_tags: Vec<String> = Vec::new();
                // 作者（作品ページから取得。FANZA / DLsite のみ）
                let mut site_author: Option<String> = None;
                let bytes = if site_id == "booth" {
                    let session =
                        booth_session.ok_or_else(|| "BOOTH セッションがありません".to_string())?;
                    let client = BoothClient::new(&session);
                    // 作者（ショップページの表示名）を商品ページから取得する。
                    // 商品詳細 API の `shop.name` はサークル名なので使わない（作者名は
                    // ショップ情報の avatar title）。公開 HTML から取れるので権限・CF にも依存しない。
                    site_author = product_id
                        .parse::<u64>()
                        .ok()
                        .and_then(|id| client.item_author(id).ok().flatten());
                    let url = item.download_url.as_deref().unwrap_or_default().to_string();
                    let download_tx = progress_tx.clone();
                    let download_progress_id = product_id.clone();
                    let mut on_download = move |downloaded: u64, total: u64| {
                        let fraction = if total > 0 {
                            downloaded as f32 / total as f32
                        } else {
                            0.0
                        };
                        let _ = download_tx.send((
                            download_progress_id.clone(),
                            DownloadState::Downloading(fraction),
                        ));
                    };
                    client
                        .download_with_progress(&url, &mut on_download)
                        .map_err(|e| e.to_string())?
                } else if site_id == "fanza" {
                    // FANZA: 一覧では download_url を持たないため、details API で
                    // download_link を取得してから ZIP をダウンロードする。
                    let session =
                        fanza_session.ok_or_else(|| "FANZA セッションがありません".to_string())?;
                    let mut client =
                        FanzaClient::with_transport(Box::new(UreqTransport::new()), session);
                    let detail = client.detail(&product_id).map_err(|e| e.to_string())?;
                    if detail.is_drm {
                        return Err(ImportFailure::Message(
                            "DRM 付き作品は取り込めません".to_string(),
                        ));
                    }
                    let url = detail
                        .download_link
                        .ok_or_else(|| "FANZA ダウンロード URL がありません".to_string())?;
                    // ジャンルタグと作者（作品ページから。取得失敗してもダウンロードは続行）
                    if let Ok(page) = client.product_page(&product_id) {
                        genre_tags = page.genre_tags;
                        site_author = page.author;
                    }
                    let download_tx = progress_tx.clone();
                    let download_progress_id = product_id.clone();
                    let mut on_download = move |downloaded: u64, total: u64| {
                        let fraction = if total > 0 {
                            downloaded as f32 / total as f32
                        } else {
                            0.0
                        };
                        let _ = download_tx.send((
                            download_progress_id.clone(),
                            DownloadState::Downloading(fraction),
                        ));
                    };
                    client
                        .download_with_progress(&url, &mut on_download)
                        .map_err(|e| e.to_string())?
                } else if site_id == "dlsite" {
                    // DLsite: 一覧 sync で保存した down_url（`.../download/=/product_id/{id}.html`）
                    // から 302 → download.dlsite.com（jwt 署名 Cookie）で ZIP を取得する。
                    let session = dlsite_session
                        .ok_or_else(|| "DLsite セッションがありません".to_string())?;
                    let mut client =
                        DlsiteClient::with_transport(Box::new(UreqTransport::new()), session);
                    // 一覧 sync で保存した down_url を優先。無ければ product_info で補う。
                    let url = match item.download_url.as_deref() {
                        Some(u) if !u.is_empty() => u.to_string(),
                        _ => {
                            let metas = client
                                .product_info(&[product_id.as_str()])
                                .map_err(|e| e.to_string())?;
                            metas
                                .get(&product_id)
                                .and_then(|m| m.down_url.clone())
                                .ok_or_else(|| "DLsite ダウンロード URL がありません".to_string())?
                        }
                    };
                    // 作者（作品ページから。取得失敗してもダウンロードは続行）
                    site_author = client.work_page_author(&product_id).unwrap_or_default();
                    let download_tx = progress_tx.clone();
                    let download_progress_id = product_id.clone();
                    let mut on_download = move |downloaded: u64, total: u64| {
                        let fraction = if total > 0 {
                            downloaded as f32 / total as f32
                        } else {
                            0.0
                        };
                        let _ = download_tx.send((
                            download_progress_id.clone(),
                            DownloadState::Downloading(fraction),
                        ));
                    };
                    client
                        .download_with_progress(&url, &mut on_download)
                        .map_err(|e| e.to_string())?
                } else {
                    let mut client = tbf_client.lock();
                    // The bookshelf item's `downloadURL` (GraphQL
                    // `downloadContent.downloadURL`) carries the DLC id, which may
                    // differ from `database_id`; fall back to the database-id URL
                    // only when the item has no download URL (mirrors the Web
                    // `urlToResolve` selection).
                    let fallback = format!("{TBF_DOWNLOAD_BASE}/{product_id}/download");
                    let url = item
                        .download_url
                        .as_deref()
                        .unwrap_or(&fallback)
                        .to_string();
                    let resolved = client
                        .resolve_download_url(&url)
                        .map_err(|e| e.to_string())?;
                    let download_tx = progress_tx.clone();
                    let download_progress_id = product_id.clone();
                    let mut on_download = move |downloaded: u64, total: u64| {
                        let fraction = if total > 0 {
                            downloaded as f32 / total as f32
                        } else {
                            0.0
                        };
                        let _ = download_tx.send((
                            download_progress_id.clone(),
                            DownloadState::Downloading(fraction),
                        ));
                    };
                    let bytes = client
                        .download_with_progress(&resolved, &mut on_download)
                        .map_err(|e| e.to_string())?;
                    // TBF クライアントのロックを解放してから重い処理（PDF レンダリング）
                    // に入る。保持したままだと同期等の他操作がブロックされる。
                    drop(client);
                    bytes
                };
                let mut file_name = item_file_name(&title, &item);
                // FANZA は ZIP（画像セット）または PDF。ファイル名由来の拡張子
                // （既定 .pdf）で誤判定して PDF レンダリングするのを防ぐため、
                // 実バイトのマジックナンバーから拡張子を判定する。
                if (site_id == "fanza" || site_id == "dlsite")
                    && let Some(ext) = sniff_extension(&bytes)
                {
                    let stem = file_name
                        .rsplit_once('.')
                        .map(|(s, _)| s)
                        .unwrap_or(&file_name);
                    file_name = format!("{stem}.{ext}");
                }
                let extension = file_name.rsplit('.').next().unwrap_or("").to_lowercase();
                let import_tx = progress_tx.clone();
                let import_progress_id = product_id.clone();
                let mut last_import_pct = u32::MAX;
                let mut on_import = move |fraction: f32| {
                    let pct = (fraction * 100.0).round() as u32;
                    if pct != last_import_pct {
                        last_import_pct = pct;
                        let _ = import_tx.send((
                            import_progress_id.clone(),
                            DownloadState::Processing(fraction),
                        ));
                    }
                };
                let identity = google_sub.as_deref().and_then(|sub| {
                    let key = db_key.as_ref()?;
                    // (source, owner) で既存の所属行を再利用（P5）。無ければ新規 UUID。
                    let reuse_id =
                        books::resolve_reuse_id(&db, key, &site_id, &product_id, Some(sub))
                            .ok()
                            .flatten();
                    let pack_id = reuse_id
                        .clone()
                        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                    Some(opfspack::Identity {
                        sub: sub.to_string(),
                        pack_id,
                    })
                });
                // 再ダウンロード防止: 同一 (site, product) の既存本（ログイン有無どちらも）
                // の book_id を再利用する（identity が None＝未ログインでも重複を作らない）。
                let reuse_book_id: Option<String> = {
                    let key = db_key.as_ref();
                    match (google_sub.as_deref(), key) {
                        (Some(sub), Some(key)) => {
                            books::resolve_reuse_id(&db, key, &site_id, &product_id, Some(sub))
                                .ok()
                                .flatten()
                        }
                        _ => books::resolve_reuse_id(&db, &[0u8; 32], &site_id, &product_id, None)
                            .ok()
                            .flatten(),
                    }
                };
                let imported = if extension == "pdf" {
                    // PDF レンダリング（重い）は DB ロック外で行い、UI スレッドの
                    // DB 操作をブロックしないようにする。
                    let pages =
                        thundoku_core::import::pdf::render_pdf_pages(&bytes, &mut on_import)
                            .map_err(|e| e.to_string())?;

                    let imported = thundoku_core::import::import_rendered_pdf_pages(
                        &db,
                        &file_name,
                        bytes.len() as i64,
                        pages,
                        &packs_dir,
                        identity.as_ref(),
                        reuse_book_id.as_deref(),
                    )
                    .map_err(import_failure)?;
                    // ダウンロード元のサイトを記録（ビューアー設定のサイト別キー用）
                    if !site_id.is_empty() {
                        let _ = books::set_site_id(&db, &imported.book.id, &site_id);
                    }
                    // 本棚の bookshelf_items.database_id と対応付け、カードを
                    // 「ダウンロード済み」として表示・ビューアーで開けるようにする。
                    let _ = books::set_tbf_product_id(&db, &imported.book.id, &product_id);
                    // サイト側のメタ（作者名・サークル名・購入日）を反映する。
                    // PDF 経路にも必要（無いと BOOTH / FANZA / DLsite の PDF で作者が入らない）。
                    apply_site_metadata(
                        &db,
                        &site_id,
                        &product_id,
                        &imported.book.id,
                        &item,
                        site_author.as_deref(),
                    );
                    // ダウンロード直後からページ数を表示できるように進捗行を作る
                    // （既にあるときは触らない = 再取得で読書位置を消さない）
                    seed_progress_if_absent(&db, &imported.book.id, imported.document.total_pages);
                    // タグ取得が OFF なら自動生成タグを取り除く（Web 版の
                    // disableTagGeneration 相当）。
                    if !tag_fetch_enabled {
                        let _ = db::tags::delete_generated(&db, &imported.book.id);
                    }
                    // 所有者（owner_sub）を記録（ログイン中のみ）。
                    if let (Some(sub), Some(key)) = (&google_sub, &db_key) {
                        let _ = books::set_owner_sub(
                            &db,
                            &imported.book.id,
                            Some(thundoku_core::owner::encrypt(key, sub)),
                        );
                    }
                    Ok::<_, ImportFailure>(imported)
                } else {
                    let imported = match extension.as_str() {
                        "epub" => thundoku_core::import::import_epub_bytes(
                            &db,
                            &file_name,
                            &bytes,
                            &packs_dir,
                            identity.as_ref(),
                            reuse_book_id.as_deref(),
                        ),
                        "zip" => {
                            // §6.3: 曖昧な構造（コンテンツが複数 / 形式が複数）は
                            // サマリー付きモーダルで既定表示を選んでもらってから取り込む
                            let mut plan = thundoku_core::import::analyze_zip(&bytes)
                                .map_err(import_failure)?;
                            if import_needs_confirmation(&plan) {
                                match ask_import_confirmation(&prompt_tx, &item.title, &plan) {
                                    Some(index) => plan.primary = index,
                                    None => return Err(ImportFailure::Cancelled),
                                }
                            }
                            thundoku_core::import::commit_zip(
                                &db,
                                &file_name,
                                &bytes,
                                &packs_dir,
                                identity.as_ref(),
                                &mut on_import,
                                reuse_book_id.as_deref(),
                                &plan,
                            )
                        }
                        // BOOTH は PDF だけでなく画像ファイル（イラスト等）もある
                        "jpg" | "jpeg" | "png" | "webp" | "gif" => {
                            thundoku_core::import::import_image_bytes(
                                &db,
                                &file_name,
                                &bytes,
                                &packs_dir,
                                identity.as_ref(),
                                reuse_book_id.as_deref(),
                            )
                        }
                        other => Err(thundoku_core::import::ImportError::UnsupportedType(
                            other.to_string(),
                        )),
                    };
                    if let Ok(imported) = &imported {
                        // ダウンロード元のサイトを記録（ビューアー設定のサイト別キー用）
                        if !site_id.is_empty() {
                            let _ = books::set_site_id(&db, &imported.book.id, &site_id);
                        }
                        let _ = books::set_tbf_product_id(&db, &imported.book.id, &product_id);
                        // BOOTH / FANZA / DLsite: インポート後にサイト側のメタ
                        //（タイトル・作者名・サークル名・購入日）を反映する
                        apply_site_metadata(
                            &db,
                            &site_id,
                            &product_id,
                            &imported.book.id,
                            &item,
                            site_author.as_deref(),
                        );
                        // FANZA: サイトから取得したジャンルタグを book_tags に保存する
                        if site_id == "fanza" && !genre_tags.is_empty() {
                            let pairs: Vec<(&str, &str)> = genre_tags
                                .iter()
                                .map(|t| (t.as_str(), "fanza_genre"))
                                .collect();
                            let _ = db::tags::set_for_book(&db, &imported.book.id, &pairs);
                            // 本棚アイテムの tags_json にも書く（owned フィルタで
                            // local が外れてもカードに表示できるように）
                            let _ = bookshelf::update_tags(&db, "fanza", &product_id, &genre_tags);
                        }
                        // DLsite: インポート後に共有メタ列（media_category / ai_type / is_drm /
                        // release_date / maker_id / age_rating / series_name）とカスタムジャンル
                        // （tags_json）を books / book_tags へ反映する。
                        if site_id == "dlsite" {
                            let _ = books::set_source_metadata(
                                &db,
                                &imported.book.id,
                                item.media_category.as_deref(),
                                item.ai_type.as_deref(),
                                item.is_drm,
                                item.release_date.as_deref(),
                                item.description.as_deref(),
                                item.theme.as_deref(),
                                item.maker_id.as_deref(),
                                item.page_count,
                                item.age_rating.as_deref(),
                                item.series_name.as_deref(),
                            );
                            if let Some(tags_json) = &item.tags_json
                                && let Ok(tags) = serde_json::from_str::<Vec<String>>(tags_json)
                                && !tags.is_empty()
                            {
                                let pairs: Vec<(&str, &str)> =
                                    tags.iter().map(|t| (t.as_str(), "dlsite_genre")).collect();
                                let _ = db::tags::set_for_book(&db, &imported.book.id, &pairs);
                            }
                        }
                        // 進捗行は既にあるときは触らない（再取得で読書位置を消さない）
                        seed_progress_if_absent(
                            &db,
                            &imported.book.id,
                            imported.document.total_pages,
                        );
                        if !tag_fetch_enabled {
                            let _ = db::tags::delete_generated(&db, &imported.book.id);
                        }
                        // 所有者（owner_sub）を記録（ログイン中のみ）。
                        if let (Some(sub), Some(key)) = (&google_sub, &db_key) {
                            let _ = books::set_owner_sub(
                                &db,
                                &imported.book.id,
                                Some(thundoku_core::owner::encrypt(key, sub)),
                            );
                        }
                    }
                    imported.map_err(import_failure)
                };
                imported.map(|imported| ImportOutcome {
                    title: imported.book.title,
                    warnings: imported.warnings,
                })
            })();
            let _ = result_tx.send(result);
        });
        cx.spawn(async move |_window, cx| {
            // Apply download/import progress on the UI thread until the
            // background task finishes (channel closes with the last sender).
            // Never use blocking `recv()` — this runs on the UI thread. Keep
            // only the LATEST state and apply it when the channel drains, so
            // progress updates never flood the renderer.
            let progress_handle = handle.clone();
            let mut pending: Option<(String, DownloadState)> = None;
            let mut last_percent: Option<u32> = None;
            let mut last_label: Option<&'static str> = None;
            let mut disconnected = false;
            while !disconnected {
                // Drain the channel, keeping only the latest state per poll.
                loop {
                    match progress_rx.try_recv() {
                        Ok(update) => pending = Some(update),
                        Err(std::sync::mpsc::TryRecvError::Empty) => break,
                        Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                            disconnected = true;
                            break;
                        }
                    }
                }
                // 取り込み確認モーダルの依頼（§6.3）
                if let Ok(request) = prompt_rx.try_recv() {
                    progress_handle.update(cx, |this, cx| {
                        this.pending_import = Some(request);
                        cx.notify();
                    });
                }
                if let Some((id, state)) = pending.take() {
                    // 状態（Downloading → Processing）が変わったら必ず再描画する。
                    // % が同じ間は間引く（16ms ごとの notify はカード 307 件の
                    // render を毎回走らせてビジーになるため）。
                    let percent = (state.fraction() * 100.0).round() as u32;
                    let label_changed = last_label != Some(state.label());
                    if label_changed || last_percent != Some(percent) {
                        last_label = Some(state.label());
                        last_percent = Some(percent);
                        progress_handle.update(cx, |this, cx| {
                            this.download_states.insert(id, state);
                            cx.notify();
                        });
                    }
                }
                if !disconnected {
                    cx.background_executor()
                        .timer(std::time::Duration::from_millis(16))
                        .await;
                }
            }
            let result = result_rx.try_recv().unwrap_or_else(|_| {
                Err(ImportFailure::Message(
                    "ダウンロード処理が結果を返しませんでした".to_string(),
                ))
            });
            log::info!("download_item: スレッド完了、UI 反映開始");
            let complete_start = std::time::Instant::now();
            handle.update(cx, |this, cx| {
                this.download_states.remove(&database_id);
                let (toast, error) = download_messages(&result);
                let succeeded = result.is_ok();
                this.toast = toast;
                this.error = error;
                if let Some(error) = &this.error {
                    log::warn!("download_item: 失敗しました: {error}");
                }
                if succeeded {
                    this.reload(cx);
                }
                cx.notify();
            });
            log::info!(
                "download_item: UI 反映完了（reload 含む）（{:?}）",
                complete_start.elapsed()
            );
        })
        .detach();
    }

    /// 起動時: お気に入りの未ダウンロード本を自動でダウンロードする。
    fn auto_download_favorites(&mut self, cx: &mut Context<Self>) {
        let favorites: Vec<bookshelf::BookshelfItem> = {
            let state = Self::app_state(cx);
            let db = &state.db_pool;
            bookshelf::list_favorites(db).unwrap_or_default()
        };
        if favorites.is_empty() {
            return;
        }
        // ダウンロード済み判定は全本リストを 1 回だけ取得して使い回す
        let downloaded_ids: std::collections::HashSet<String> = {
            let state = Self::app_state(cx);
            let db = &state.db_pool;
            books::list(db)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|b| b.tbf_product_id)
                .collect()
        };
        for item in favorites {
            if downloaded_ids.contains(&item.database_id) || item.is_downloadable == 0 {
                continue;
            }
            self.download_item(cx, item);
        }
    }

    /// お気に入りトグル（DB: bookshelf_items + books の両方を更新）。
    fn toggle_favorite(&mut self, cx: &mut Context<Self>, card: &ShelfCard) {
        // 書籍情報の展開中・同期中は reload がバッティングするため操作を無視する
        if self.fetching_covers || self.sync_busy > 0 {
            return;
        }
        let new_state = card.shelf.is_favorite == 0;
        {
            let state = Self::app_state(cx);
            let db = &state.db_pool;
            let _ = bookshelf::set_favorite(
                db,
                &card.shelf.site_id,
                &card.shelf.database_id,
                new_state,
            );
            if let Some(local) = &card.local {
                let _ = books::set_favorite(db, &local.book.id, new_state);
            }
        }
        // reload は重い上に fetch が再開されるため、カードの状態だけ直接更新する
        if let Some(target) = self.shelf_cards.iter_mut().find(|c| {
            c.shelf.site_id == card.shelf.site_id && c.shelf.database_id == card.shelf.database_id
        }) {
            target.shelf.is_favorite = if new_state { 1 } else { 0 };
        }
        // お気に入りフィルタ中の表示を即時反映する
        self.filtered_dirty = true;
        cx.notify();
    }

    /// 非表示トグル（DB: bookshelf_items + books の両方を更新）。
    fn toggle_hidden(&mut self, cx: &mut Context<Self>, card: &ShelfCard) {
        // 書籍情報の展開中・同期中は reload がバッティングするため操作を無視する
        if self.fetching_covers || self.sync_busy > 0 {
            return;
        }
        let new_state = card.shelf.is_hidden == 0;
        {
            let state = Self::app_state(cx);
            let db = &state.db_pool;
            let _ =
                bookshelf::set_hidden(db, &card.shelf.site_id, &card.shelf.database_id, new_state);
            if let Some(local) = &card.local {
                let _ = books::set_hidden(db, &local.book.id, new_state);
            }
        }
        // reload は重い上に fetch が再開されるため、カードの状態だけ直接更新する
        if let Some(target) = self.shelf_cards.iter_mut().find(|c| {
            c.shelf.site_id == card.shelf.site_id && c.shelf.database_id == card.shelf.database_id
        }) {
            target.shelf.is_hidden = if new_state { 1 } else { 0 };
        }
        // 非表示にした本を本棚から即時除外する
        self.filtered_dirty = true;
        cx.notify();
    }

    /// ローカル削除（Drive には触れない）。
    pub fn delete_book(&mut self, cx: &mut Context<Self>, book_id: &str) {
        {
            let state = Self::app_state(cx);
            let db = &state.db_pool;
            let _ = progress::delete(db, book_id);
            let _ = books::delete(db, book_id);
        }
        {
            let state = Self::app_state(cx);
            let path = state.packs_dir.join(format!("{book_id}.opfspack"));
            let _ = std::fs::remove_file(path);
        }
        self.toast = Some("本を削除しました".into());
        self.reload(cx);
    }

    /// 本を再ダウンロードする（コンテキストメニューの「再取得」）。
    ///
    /// **ローカルの本は消さない**。取り込みは同じ `book_id` を再利用し、コンテンツの
    /// `content_id` と名前（カスタム名）・進捗・タグを維持したまま pack とページを作り直す
    /// （削除してしまうと、その本に紐づくタグ・進捗・閲覧履歴が失われる）。
    pub(crate) fn redownload_item(&mut self, cx: &mut Context<Self>, card: &ShelfCard) {
        // 表紙は消さない。消すと `matches_filter` が
        // 「ローカル無し + 表紙無し + thumbnail_url あり」でカードを隠すため、
        // 再取得中にカードが消える（再取得後は pack の表紙が reload で入る）。
        //
        // 表紙取得中・同期中は `download_item` が無視するので、完了後に実行するよう積む。
        if self.fetching_covers || self.sync_busy > 0 {
            self.pending_download = Some(card.shelf.clone());
            self.toast = Some("表紙の取得後に再取得します".into());
            cx.notify();
            return;
        }
        self.download_item(cx, card.shelf.clone());
        self.toast = Some("再取得を開始しました".into());
        cx.notify();
    }

    /// タグ取得 ON/OFF トグル（Web 版の `handleToggleTagFetch` 相当）。
    /// OFF の間はダウンロード時にタグを自動生成しない。
    /// 取り込み確認モーダルで既定表示を選ぶ（§6.3）。
    fn select_pending_import(&mut self, cx: &mut Context<Self>, index: usize) {
        if let Some(pending) = self.pending_import.as_mut() {
            pending.selected = index;
        }
        cx.notify();
    }

    /// 取り込み確認モーダルを確定する（worker が待っている選択を返す）。
    fn confirm_pending_import(&mut self, cx: &mut Context<Self>) {
        if let Some(pending) = self.pending_import.take() {
            let _ = pending.reply.send(Some(pending.selected));
        }
        cx.notify();
    }

    /// 取り込み確認モーダルをキャンセルする（worker は取り込みを中止する）。
    fn cancel_pending_import(&mut self, cx: &mut Context<Self>) {
        if let Some(pending) = self.pending_import.take() {
            let _ = pending.reply.send(None);
        }
        cx.notify();
    }

    /// 表示モードを切り替える（Web の viewMode トグル相当）。
    fn toggle_view_mode(&mut self, cx: &mut Context<Self>) {
        self.view_mode = match self.view_mode {
            ViewMode::Card => ViewMode::List,
            ViewMode::List => ViewMode::Card,
        };
        cx.notify();
    }

    fn toggle_tag_fetch(&mut self, cx: &mut Context<Self>) {
        self.tag_fetch_enabled = !self.tag_fetch_enabled;
        {
            let state = Self::app_state(cx);
            let db = &state.db_pool;
            let _ = db::settings::set(
                db,
                "tag.fetch.enabled",
                if self.tag_fetch_enabled {
                    "true"
                } else {
                    "false"
                },
            );
        }
        cx.notify();
    }

    fn toggle_tag(&mut self, cx: &mut Context<Self>, tag: &str) {
        if self.selected_tags.iter().any(|t| t == tag) {
            self.selected_tags.retain(|t| t != tag);
        } else {
            self.selected_tags.push(tag.to_string());
        }
        self.filtered_dirty = true;
        cx.notify();
    }

    /// サークル名リンクのクリック: そのサークルで絞り込む（同じ値の再クリックで解除）。
    fn toggle_circle_filter(&mut self, cx: &mut Context<Self>, circle: &str) {
        if self.circle_filter.as_deref() == Some(circle) {
            self.circle_filter = None;
        } else {
            self.circle_filter = Some(circle.to_string());
        }
        self.filtered_dirty = true;
        cx.notify();
    }

    /// 作者名リンクのクリック: その作者で絞り込む（同じ値の再クリックで解除）。
    fn toggle_author_filter(&mut self, cx: &mut Context<Self>, author: &str) {
        if self.author_filter.as_deref() == Some(author) {
            self.author_filter = None;
        } else {
            self.author_filter = Some(author.to_string());
        }
        self.filtered_dirty = true;
        cx.notify();
    }

    /// いずれかのフィルタ（検索 / イベント / タグ / サークル / 作者 / 既読モード）が
    /// 効いているか。全項目ボタンの「絞込中」表示に使う。
    /// サイト選択はサイドバーの閲覧スコープ（フィルタではない）なので数えない。
    fn is_filtering(&self, cx: &App) -> bool {
        self.current_search(cx).is_some()
            || !self.selected_events.is_empty()
            || !self.selected_tags.is_empty()
            || self.circle_filter.is_some()
            || self.author_filter.is_some()
            || self.read_filter != ReadFilter::All
    }

    /// 全項目ボタンのラベル。絞り込み中は「絞込中 ✕」（クリック / ESC で解除）。
    fn filter_all_label(&self, cx: &App) -> &'static str {
        if self.is_filtering(cx) {
            "絞込中 ✕"
        } else {
            "全項目"
        }
    }

    /// 全項目ボタン: フィルタ（検索 / イベント / タグ / サークル / 作者 / 既読モード）を
    /// 解除する。サイト（サイドバーのスコープ）は変更しない。
    fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.circle_filter = None;
        self.author_filter = None;
        self.selected_tags.clear();
        self.selected_events.clear();
        self.read_filter = ReadFilter::All;
        if let Some(state) = self.search_state.clone() {
            state.update(cx, |state, cx| state.set_value("", window, cx));
        }
        self.filtered_dirty = true;
        cx.notify();
    }

    /// お気に入りタグのトグル（Web の `handleToggleFavoriteTag` と同一）。
    fn toggle_favorite_tag(&mut self, cx: &mut Context<Self>, tag: &str) {
        let is_favorite = self.favorite_tags.iter().any(|t| t == tag);
        {
            let state = Self::app_state(cx);
            let db = &state.db_pool;
            let _ = db::tags::set_favorite(db, tag, !is_favorite);
        }
        if is_favorite {
            self.favorite_tags.retain(|t| t != tag);
        } else {
            self.favorite_tags.push(tag.to_string());
        }
        cx.notify();
    }

    /// サークル / 作者のお気に入りトグル（`favorite_entities`）。
    fn toggle_favorite_entity(&mut self, cx: &mut Context<Self>, kind: EntityLink, value: &str) {
        let is_favorite = match kind {
            EntityLink::Circle => self.favorite_circles.iter().any(|v| v == value),
            EntityLink::Author => self.favorite_authors.iter().any(|v| v == value),
        };
        {
            let state = Self::app_state(cx);
            let db = &state.db_pool;
            let _ = db::favorites::set_favorite(db, kind.db_kind(), value, !is_favorite);
        }
        let favorites = match kind {
            EntityLink::Circle => &mut self.favorite_circles,
            EntityLink::Author => &mut self.favorite_authors,
        };
        if is_favorite {
            favorites.retain(|v| v != value);
        } else {
            favorites.push(value.to_string());
        }
        cx.notify();
    }

    /// インラインタグ編集を開始（Web の TagList 編集モード相当）。
    fn start_tag_edit(&mut self, window: &mut Window, cx: &mut Context<Self>, book_id: &str) {
        let (tags, suggestions) = {
            let state = Self::app_state(cx);
            let db = &state.db_pool;
            let tags = if let Some(local_id) = Self::resolve_local_book_id(db, book_id) {
                db::tags::list_for_book(db, &local_id)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|tag| {
                        tag.source == "manual"
                            || tag.source == "fanza_genre"
                            || tag.source == "dlsite_genre"
                    })
                    .map(|tag| tag.tag_name)
                    .collect::<Vec<_>>()
            } else {
                bookshelf::list_all(db)
                    .unwrap_or_default()
                    .iter()
                    .find(|item| item.database_id == book_id)
                    .map(bookshelf::tags_of)
                    .unwrap_or_default()
            };
            // Web の tagSuggestions 相当: 後で読む + お気に入りタグ
            // 編集中の本のサイトでお気に入りタグを絞る（技術書典の本を編集するときに
            // FANZA のお気に入りタグが出ないように。サイト不明なら全表示）。
            let mut suggestions = vec!["後で読む".to_string()];
            let favorites = db::tags::list_favorites(db).unwrap_or_default();
            let book_site: Option<String> =
                if let Some(local_id) = Self::resolve_local_book_id(db, book_id) {
                    books::get(db, &local_id)
                        .ok()
                        .flatten()
                        .and_then(|b| b.site_id)
                } else {
                    bookshelf::list_all(db)
                        .ok()
                        .unwrap_or_default()
                        .into_iter()
                        .find(|i| i.database_id == book_id)
                        .map(|i| i.site_id)
                };
            let allowed: Option<std::collections::HashSet<String>> =
                book_site.as_deref().map(|site| {
                    let mut set = std::collections::HashSet::new();
                    for item in bookshelf::list_all(db).ok().unwrap_or_default() {
                        if item.site_id == site {
                            for t in bookshelf::tags_of(&item) {
                                set.insert(t);
                            }
                        }
                    }
                    for b in books::list(db).ok().unwrap_or_default() {
                        if b.site_id.as_deref() == Some(site) {
                            for t in db::tags::list_for_book(db, &b.id).unwrap_or_default() {
                                set.insert(t.tag_name);
                            }
                        }
                    }
                    set
                });
            for tag in favorites {
                if allowed.as_ref().is_none_or(|s| s.contains(&tag)) && !suggestions.contains(&tag)
                {
                    suggestions.push(tag);
                }
            }
            (tags, suggestions)
        };
        self.editing_book_id = Some(book_id.to_string());
        self.editing_site_id = {
            let state = Self::app_state(cx);
            let db = &state.db_pool;
            if let Some(local_id) = Self::resolve_local_book_id(db, book_id) {
                books::get(db, &local_id)
                    .ok()
                    .flatten()
                    .and_then(|b| b.site_id)
            } else {
                bookshelf::list_all(db)
                    .ok()
                    .unwrap_or_default()
                    .iter()
                    .find(|item| item.database_id == book_id)
                    .map(|item| item.site_id.clone())
            }
        };
        self.editing_tags = tags.clone();
        self.editing_suggestions = suggestions;
        self.editing_input =
            Some(cx.new(|cx| InputState::new(window, cx).placeholder("タグを追加...")));
        cx.notify();
    }

    /// ローカル book（id または tbf_product_id）を解決する。
    fn resolve_local_book_id(db: &thundoku_core::db::SqlitePool, book_id: &str) -> Option<String> {
        books::list(db)
            .unwrap_or_default()
            .into_iter()
            .find(|b| b.id == book_id || b.tbf_product_id.as_deref() == Some(book_id))
            .map(|b| b.id)
    }

    /// 編集中タグを追加（Web の addTag 相当: 重複無視・20 文字制限）。
    fn add_editing_tag(&mut self, cx: &mut Context<Self>, tag: &str) {
        let tag = tag.trim();
        if tag.is_empty() || tag.len() > 20 {
            return;
        }
        if !self.editing_tags.iter().any(|t| t == tag) {
            self.editing_tags.push(tag.to_string());
        }
        cx.notify();
    }

    /// 編集中タグを削除（Web の removeTag 相当）。
    fn remove_editing_tag(&mut self, cx: &mut Context<Self>, tag: &str) {
        self.editing_tags.retain(|t| t != tag);
        cx.notify();
    }

    /// 入力値（カンマ区切り対応）をタグに追加。
    fn add_editing_from_input(&mut self, cx: &mut Context<Self>) {
        let value = self
            .editing_input
            .as_ref()
            .map(|state| state.read(cx).value().to_string())
            .unwrap_or_default();
        for part in value.split(',') {
            self.add_editing_tag(cx, part);
        }
    }

    /// タグ列の折りたたみ / 展開を切り替える（「+n」↔「閉じる」）。行ごとに独立。
    fn toggle_tag_expansion(&mut self, database_id: &str, cx: &mut Context<Self>) {
        if !self.expanded_tag_rows.remove(database_id) {
            self.expanded_tag_rows.insert(database_id.to_string());
        }
        cx.notify();
    }

    /// タグ編集を保存（Web の handleSave 相当）。
    fn save_tag_edit(&mut self, cx: &mut Context<Self>) {
        let Some(book_id) = self.editing_book_id.clone() else {
            return;
        };
        self.add_editing_from_input(cx);
        let tags = self.editing_tags.clone();
        {
            let state = Self::app_state(cx);
            let db = &state.db_pool;
            if let Some(local_id) = Self::resolve_local_book_id(db, &book_id) {
                let tag_pairs: Vec<(&str, &str)> =
                    tags.iter().map(|tag| (tag.as_str(), "manual")).collect();
                let res = db::tags::set_for_book(db, &local_id, &tag_pairs);
                log::info!(
                    "save_tag_edit: book_id={book_id} -> local_id={local_id} tags={tags:?} -> set_for_book={res:?}"
                );
                // FANZA / DLsite: reload の owned フィルタで local が外れるとカードは
                // shelf.tags_json を読むため、book_tags に加えて本棚アイテムにも書く。
                if matches!(
                    self.editing_site_id.as_deref(),
                    Some("fanza") | Some("dlsite")
                ) {
                    let site = self.editing_site_id.as_deref().unwrap_or("fanza");
                    let shelf_res = bookshelf::update_tags(db, site, &book_id, &tags);
                    log::info!(
                        "save_tag_edit: {site} shelf update_tags book_id={book_id} -> {shelf_res:?}"
                    );
                } else {
                    log::info!(
                        "save_tag_edit: editing_site_id={:?} (not fanza, no shelf write)",
                        self.editing_site_id
                    );
                }
            } else {
                log::warn!(
                    "save_tag_edit: resolve_local_book_id=None book_id={book_id} site={:?} -> shelf update",
                    self.editing_site_id
                );
                let site = self
                    .editing_site_id
                    .as_deref()
                    .unwrap_or(tbf::SITE_ID_TECHBOOKFEST);
                let _ = bookshelf::update_tags(db, site, &book_id, &tags);
            }
        }
        self.editing_book_id = None;
        self.editing_tags.clear();
        self.reload(cx);
    }

    /// FANZA のジャンルタグを作品ページから再取得し、未取得のものを編集中タグに追加する。
    fn refetch_genre_tags(&mut self, cx: &mut Context<Self>) {
        let Some(book_id) = self.editing_book_id.clone() else {
            return;
        };
        if self.editing_site_id.as_deref() != Some("fanza") {
            return;
        }
        let (cid, session) = {
            let state = Self::app_state(cx);
            let db = &state.db_pool;
            let cid = if let Some(local_id) = Self::resolve_local_book_id(db, &book_id) {
                books::get(db, &local_id)
                    .ok()
                    .flatten()
                    .and_then(|b| b.tbf_product_id)
            } else {
                Some(book_id.clone())
            };
            (cid, state.fanza_session.lock().clone())
        };
        let Some(cid) = cid else {
            return;
        };
        let Some(session) = session else {
            self.toast = Some("FANZA にログインしてください".into());
            cx.notify();
            return;
        };
        // 1 リクエストなので同期で取得する（非同期にすると保存との競合が起きる）
        let mut client = FanzaClient::with_transport(Box::new(UreqTransport::new()), session);
        let genre_tags = client
            .product_page(&cid)
            .map(|p| p.genre_tags)
            .unwrap_or_default();
        let mut added = 0;
        for tag in genre_tags {
            if !tag.is_empty() && !self.editing_tags.contains(&tag) {
                self.editing_tags.push(tag);
                added += 1;
            }
        }
        self.toast = Some(format!("ジャンルを再取得しました（{added} 件追加）"));
        cx.notify();
    }

    /// タグ編集をキャンセル（Web の handleCancel 相当）。
    fn cancel_tag_edit(&mut self, cx: &mut Context<Self>) {
        self.editing_book_id = None;
        self.editing_tags.clear();
        self.editing_input = None;
        cx.notify();
    }

    fn set_read_filter(&mut self, cx: &mut Context<Self>, filter: ReadFilter) {
        self.read_filter = filter;
        self.filtered_dirty = true;
        cx.notify();
    }

    /// 行 / 関連サムネイルのクリック。ローカル本なら開き、未ダウンロードなら取り込む。
    /// ダウンロード中の本は無視する（二重ダウンロード防止。行クリックと同じ扱い）。
    fn open_or_download(
        &mut self,
        cx: &mut Context<Self>,
        database_id: &str,
        book_id: Option<String>,
        item: &bookshelf::BookshelfItem,
    ) {
        if self.download_states.contains_key(database_id) {
            return;
        }
        if let Some(book_id) = book_id {
            self.open_book(cx, &book_id);
        } else {
            self.download_item(cx, item.clone());
        }
    }

    fn open_book(&mut self, cx: &mut Context<Self>, book_id: &str) {
        // 終了時アップロード中はビューアーの起動をブロックする
        if Self::is_exit_uploading(cx) {
            return;
        }
        // 開こうとしている本を選択状態にする（ビューアーから戻った時に
        // 読んでいた本が選択されているようにする）。
        self.select_book(cx, book_id);
        let action = OpenReader {
            book_id: book_id.into(),
        };
        cx.defer(move |cx| cx.dispatch_action(&action));
    }

    /// 指定した book_id のカードを選択状態にする。`filtered` 内で該当する
    /// カードを探し、見つかれば選択インデックスを更新する。
    fn select_book(&mut self, cx: &App, book_id: &str) {
        // filtered が未構築なら再計算する。
        if self.filtered_dirty {
            self.filtered = (0..self.shelf_cards.len())
                .filter(|&i| self.matches_filter(cx, &self.shelf_cards[i]))
                .collect();
            self.filtered_dirty = false;
        }
        if let Some(pos) = self.filtered.iter().position(|&card_idx| {
            self.shelf_cards[card_idx]
                .local
                .as_ref()
                .is_some_and(|e| e.book.id == book_id)
        }) {
            self.selected_index = Some(pos);
        }
    }

    /// ビューアーから本棚に戻った時に呼ばれる。読んでいた本を選択状態に戻し、
    /// 次の描画でフォーカスを取り直す（矢印キー・hjkl 入力を再開する）。
    pub(crate) fn restore_selection(&mut self, cx: &App, book_id: &str) {
        self.select_book(cx, book_id);
        // render の初回フォーカス付与を再度行わせる（ビューアー表示中に
        // フォーカスが外れているため、戻ったら取り直す）。
        self.focus_initialized = false;
    }

    #[allow(clippy::too_many_arguments)]
    fn render_card(
        window: &mut Window,
        theme: &gpui_kit::component::Theme,
        handle: &gpui_kit::Entity<BookshelfView>,
        card: &ShelfCard,
        card_width: f32,
        download_state: Option<DownloadState>,
        editing: bool,
        chips: &ChipState,
        tag_order: &TagOrder,
        tags_expanded: bool,
        editing_tags: &[String],
        editing_suggestions: &[String],
        editing_input: Option<&gpui_kit::Entity<InputState>>,
        selected: bool,
    ) -> impl IntoElement {
        let shelf = &card.shelf;
        let title = shelf.title.clone();
        // サークル名（技術書典: organization / BOOTH: shop）
        let circle_name = shelf.circle_name.clone();
        // 作者名（技術書典は空のため非表示。BOOTH 等は作成者名）
        let author = shelf.author.clone();
        let event = shelf
            .event_name
            .clone()
            .map(|name| format_event_label(&name));
        let event_text = event.unwrap_or_else(|| "イベント不明".to_string());
        // 購入日（caused_at "2026/01/01 19:36:23" → "2026/01/01"）。BOOTH はイベント名が
        // ないため、イベント名の代わりに購入日を表示する
        let purchase_date = shelf.caused_at.as_deref().map(format_purchase_date);
        let database_id = shelf.database_id.clone();
        let cover = card.cover.clone().or_else(|| {
            if card.cover_fetch_failed {
                no_image_cover()
            } else {
                placeholder_cover(&shelf.title, &shelf.circle_name)
            }
        });
        let local = card.local.as_ref();
        let is_read = local.map(|e| e.is_read).unwrap_or(false);
        let is_downloaded = local.is_some();
        let is_favorite = shelf.is_favorite == 1;
        let progress_text = local.and_then(|e| {
            e.progress.map(|(current, total)| match total {
                Some(total) => format!("{} / {total}ページ", current.max(1)),
                None => format!("{current}ページ閲覧中"),
            })
        });
        let handle = handle.clone();
        let open_id = database_id.clone();
        let delete_id = local.map(|e| e.book.id.clone());
        let _ = window;

        // -- 表紙（カードのヘッダー）: カード幅いっぱい + 4:3 の枠 --
        // 画像は**比率を保って枠に収める**。横長は幅いっぱい（高さは比率なり）、
        // **縦長は高さいっぱい**（幅は比率なり）にして中央に置く。カード幅に合わせて
        // 縦長を拡大すると上下が切れて表紙の一部しか見えなくなるため。
        // 角はカードと同じ丸み（`rounded_lg`）を上辺に付ける（GPUI の overflow_hidden は
        // 矩形マスクなので、角丸のクリップは各要素側で指定する必要がある）。
        let cover_h: f32 = (card_width * 0.75).clamp(120.0, 320.0);
        let draw_size = |render: &Arc<RenderImage>| -> (f32, f32) {
            let size = render.size(0);
            fit_cover_size(
                size.width.0.max(1) as f32,
                size.height.0.max(1) as f32,
                card_width,
                cover_h,
            )
        };
        // バッジ（未読/♡/↓）とオーバーレイは、枠ではなく**この画像の矩形**を基準に置く。
        // 縦長の表紙は左右にバーが出るため、枠基準だとバッジが画像の外に浮いてしまう。
        let (draw_w, draw_h) = match &cover {
            Some(render) => draw_size(render),
            None => (card_width, cover_h),
        };
        // 上辺の角丸は**カード幅いっぱいのときだけ**（横長の表紙はカードの角に接するため
        // 丸みを合わせる）。縦長は画像が中央に浮いてカードの角に接しないので、画像にも
        // バッジにも丸みを付けない。
        let fits_width = draw_w >= card_width - 0.5;
        let mut cover_el = div().relative().w(px(draw_w)).h(px(draw_h)).flex_shrink_0();
        if fits_width {
            cover_el = cover_el.rounded_t_lg();
        }
        cover_el = cover_el
            .child(match &cover {
                Some(render) => {
                    let mut el = img(render.clone()).w_full().h_full();
                    if fits_width {
                        el = el.rounded_t_lg();
                    }
                    el.into_any_element()
                }
                None => {
                    let mut el = div().w_full().h_full().bg(theme.muted);
                    if fits_width {
                        el = el.rounded_t_lg();
                    }
                    el.into_any_element()
                }
            })
            // 左上: 未読/既読バッジ（Web の statusText と同じ）
            .child({
                let mut badge = div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .rounded_br_md()
                    .px_1()
                    .py_0p5();
                if fits_width {
                    badge = badge.rounded_tl_lg();
                }
                if is_read {
                    badge
                        .bg(gpui_kit::rgb(0xd1fae5))
                        .text_color(gpui_kit::rgb(0x047857))
                        .text_xs()
                        .child("読了")
                } else {
                    badge
                        .bg(gpui_kit::rgb(0xfef3c7))
                        .text_color(gpui_kit::rgb(0xb45309))
                        .text_xs()
                        .child("未読")
                }
                .into_any_element()
            })
            // 右上: お気に入りハート（クリックでトグル）
            .child(
                div()
                    .id(SharedString::from(format!("fav-{database_id}")))
                    .absolute()
                    .right_1()
                    .top_1()
                    .cursor_pointer()
                    .on_click({
                        let handle = handle.clone();
                        let card = card.clone();
                        move |_, _window, cx| {
                            cx.stop_propagation();
                            handle.update(cx, |this, cx| this.toggle_favorite(cx, &card));
                        }
                    })
                    .rounded_full()
                    .w(px(24.0))
                    .h(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui_kit::rgba(0x00000040))
                    .text_color(if is_favorite {
                        gpui_kit::rgb(0xf43f5e)
                    } else {
                        gpui_kit::rgb(0xffffff)
                    })
                    .text_sm()
                    .child(if is_favorite { "♥" } else { "♡" })
                    .into_any_element(),
            )
            // 右下: ダウンロード済み/未ダウンロードアイコン（Web の ArrowDownCircle 相当）
            .child(if is_downloaded {
                div()
                    .absolute()
                    .right_1()
                    .bottom_1()
                    .rounded_full()
                    .w(px(24.0))
                    .h(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui_kit::rgba(0x05966933))
                    .child(
                        div()
                            .text_color(gpui_kit::rgb(0x059669))
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::BOLD)
                            .child("✓"),
                    )
                    .into_any_element()
            } else {
                div()
                    .absolute()
                    .right_1()
                    .bottom_1()
                    .rounded_full()
                    .w(px(24.0))
                    .h(px(24.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui_kit::rgba(0x00000033))
                    .child(
                        div()
                            .text_color(gpui_kit::white())
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::BOLD)
                            .child("↓"),
                    )
                    .into_any_element()
            });

        // 中央: ダウンロード/取込中オーバーレイ（Web の CircularProgress 相当）
        if let Some(state) = download_state {
            let fraction = state.fraction();
            let percentage = (fraction * 100.0).round() as u32;
            let ring = progress_ring_image(fraction);
            let mut overlay = div()
                .absolute()
                .inset_0()
                .flex()
                .items_center()
                .justify_center()
                .bg(gpui_kit::rgba(0x00000080));
            if fits_width {
                overlay = overlay.rounded_t_lg();
            }
            cover_el = cover_el.child(
                overlay.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_center()
                        .gap_1()
                        .child(img(ring).w(px(72.0)).h(px(72.0)))
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .child(
                                    div()
                                        .text_color(gpui_kit::white())
                                        .text_xs()
                                        .child(state.label()),
                                )
                                .child(
                                    div()
                                        .text_color(gpui_kit::white())
                                        .text_sm()
                                        .font_weight(gpui_kit::FontWeight::BOLD)
                                        .child(format!("{percentage}%")),
                                ),
                        ),
                ),
            );
        }

        // 枠（カード幅 × 4:3）に表紙を中央寄せする。余りは theme.secondary（バー）。
        let image: gpui_kit::AnyElement = div()
            .w_full()
            .h(px(cover_h))
            .flex()
            .items_center()
            .justify_center()
            .rounded_t_lg()
            .bg(theme.secondary)
            .child(cover_el)
            .into_any_element();

        let card_selector = format!("book-card-{}", shelf.database_id);
        let mut card_el = div()
            .id(SharedString::from(card_selector.clone()))
            .debug_selector(move || card_selector.clone())
            .w(px(card_width))
            .flex()
            .flex_col()
            .rounded_lg()
            .overflow_hidden()
            .border_1()
            .border_color(if selected {
                // ダークモードでは枠の明るさを少し落として目立ちすぎないように
                if theme.is_dark() {
                    theme.primary.darken(0.35)
                } else {
                    theme.primary
                }
            } else {
                theme.border
            })
            .bg(if selected {
                if theme.is_dark() {
                    theme.secondary.lighten(0.12)
                } else {
                    theme.secondary
                }
            } else {
                theme.muted
            })
            .hover(|style| style.bg(theme.secondary));

        card_el = card_el.cursor_pointer().on_click({
            let handle = handle.clone();
            let open_book_id = delete_id.clone();
            let download_item = card.shelf.clone();
            let click_database_id = database_id.clone();
            move |_, _window, cx| {
                handle.update(cx, |this, cx| {
                    // ダウンロード/取込中は再クリックを無視（二重ダウンロード防止）
                    if this.download_states.contains_key(&click_database_id) {
                        return;
                    }
                    if let Some(book_id) = &open_book_id {
                        this.open_book(cx, book_id);
                    } else {
                        this.download_item(cx, download_item.clone());
                    }
                });
            }
        });

        card_el = card_el
            // 表紙（カードのヘッダー）: 端まで出す。読了は少し薄く表示する
            .child(if is_read {
                div().opacity(0.75).child(image)
            } else {
                div().child(image)
            })
            // 以降はパディング付きの内容ブロック（ヘッダーだけ端まで）
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    // タイトル（Web の BookInfo: line-clamp-2 font-semibold）
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(title),
                    )
                    // イベント名 or 購入日（BOOTH はイベントがないため購入日を表示）
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            // Web の formatEventLabel と同じ: イベント不明のときは
                            // 「イベント不明」を表示
                            .child(match (shelf.site_id.as_str(), purchase_date.as_deref()) {
                                ("booth", Some(date))
                                | ("fanza", Some(date))
                                | ("dlsite", Some(date)) => format!("購入日: {date}"),
                                _ => event_text.clone(),
                            }),
                    )
                    // サークル名 / 作者名（タグと同じチップ。本体クリックで絞り込み、
                    // 右端のハートでお気に入り）
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap_1()
                            .items_center()
                            .child(BookshelfView::render_entity_chip(
                                theme,
                                &handle,
                                EntityLink::Circle,
                                &circle_name,
                                &database_id,
                                chips.favorite_circles.contains(&circle_name),
                                chips.circle_filter.as_deref() == Some(circle_name.as_str()),
                            ))
                            .child(BookshelfView::render_entity_chip(
                                theme,
                                &handle,
                                EntityLink::Author,
                                &author,
                                &database_id,
                                chips.favorite_authors.contains(&author),
                                chips.author_filter.as_deref() == Some(author.as_str()),
                            )),
                    )
                    .child(match progress_text.as_deref() {
                        Some(text) => div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(text.to_string())
                            .into_any_element(),
                        None => div().into_any_element(),
                    }),
            );

        // タグ行: 編集中なら Web の TagsInput 風エディタを表示
        card_el = card_el.child(div().px_3().pb_3().child(if editing {
            BookshelfView::render_tag_editor(
                window,
                theme,
                &handle,
                editing_tags,
                editing_suggestions,
                editing_input.expect("editing input"),
                card.shelf.site_id == "fanza",
            )
            .into_any_element()
        } else {
            // タグ行のどこをクリックしてもカードのクリック（開く/ダウンロード）
            // が発火しないようにする（Web 版のタグ行と同じ挙動）
            let ordered = tag_order.sorted(&card.tags);
            // タグが多い本はカードでも折りたたむ（行の高さを共有するため、1 冊の
            // タグ数で行全体が間延びする）。「+n」で全件表示、「閉じる」で畳む。
            let visible = if tags_expanded {
                ordered.len()
            } else {
                ordered.len().min(CARD_TAGS_COLLAPSED_MAX)
            };
            let chips = BookshelfView::render_tag_chips(
                theme,
                tag_order,
                &database_id,
                &ordered[..visible],
                {
                    let handle = handle.clone();
                    move |tag: &str, _window: &mut Window, cx: &mut App| {
                        let tag = tag.to_string();
                        handle.update(cx, |this, cx| this.toggle_tag(cx, &tag));
                    }
                },
                {
                    let handle = handle.clone();
                    move |tag: &str, _window: &mut Window, cx: &mut App| {
                        let tag = tag.to_string();
                        handle.update(cx, |this, cx| this.toggle_favorite_tag(cx, &tag));
                    }
                },
            );
            let hidden = ordered.len() - visible;
            let mut tag_row = div()
                .id(SharedString::from(format!("tag-row-{database_id}")))
                .flex()
                .flex_row()
                .flex_wrap()
                .gap_1()
                .items_center()
                .cursor_pointer()
                .on_click(|_, _, cx| cx.stop_propagation())
                .children(chips);
            // 展開中も閉じられるようにトグルを出す（折りたたみ中は残り件数があるときだけ）
            if tags_expanded || hidden > 0 {
                tag_row = tag_row.child(BookshelfView::render_tag_toggle(
                    theme,
                    format!("tag-toggle-{database_id}"),
                    hidden,
                    tags_expanded,
                    {
                        let handle = handle.clone();
                        let database_id = database_id.clone();
                        move |_window, cx| {
                            handle
                                .update(cx, |this, cx| this.toggle_tag_expansion(&database_id, cx));
                        }
                    },
                ));
            }
            tag_row
                .child(BookshelfView::render_tag_edit_button(
                    window,
                    theme,
                    &handle,
                    &database_id,
                ))
                .into_any_element()
        }));

        card_el.context_menu({
            let has_local = delete_id.is_some();
            let open_id_for_menu = delete_id.clone().unwrap_or_else(|| open_id.clone());
            let edit_id_for_menu = database_id.clone();
            let delete_id_for_menu = delete_id.clone();
            let site_id_for_menu = shelf.site_id.clone();
            move |menu, _window, cx| {
                let mut menu = menu;
                // 閲覧回数を一番上に表示（ローカル本のみ。クリック不可）
                if let Some(book_id) = delete_id_for_menu.as_ref() {
                    let state = AppState::global(cx);
                    if let Ok(count) = db::view_history::view_count(&state.db_pool, book_id) {
                        menu = menu.menu_with_disabled(
                            format!("閲覧回数: {count} 回"),
                            Box::new(crate::actions::Noop),
                            true,
                        );
                    }
                }
                if has_local {
                    menu = menu.menu(
                        "開く",
                        Box::new(OpenReader {
                            book_id: open_id_for_menu.clone().into(),
                        }),
                    );
                }
                menu = menu.menu(
                    "タグ編集",
                    Box::new(EditBookTags {
                        book_id: edit_id_for_menu.clone().into(),
                    }),
                );
                menu = menu.menu(
                    "再取得",
                    Box::new(crate::actions::RedownloadBook {
                        database_id: edit_id_for_menu.clone().into(),
                        site_id: site_id_for_menu.clone().into(),
                    }),
                );
                menu = menu.menu(
                    "非表示にする",
                    Box::new(HideBook {
                        database_id: edit_id_for_menu.clone().into(),
                        site_id: site_id_for_menu.clone().into(),
                    }),
                );
                menu
            }
        })
    }

    /// サークル名 / 作者名のチップ（タグチップと同じ見た目）。
    /// ラベル（`サークル:` / `作者:`）は**チップの外**に置き、タグ化しない。
    /// 値クリックでその値に絞り込み（選択中は青）、右端のハートでお気に入り。
    fn render_entity_chip(
        theme: &gpui_kit::component::Theme,
        handle: &gpui_kit::Entity<BookshelfView>,
        kind: EntityLink,
        value: &str,
        database_id: &str,
        is_favorite: bool,
        is_selected: bool,
    ) -> gpui_kit::AnyElement {
        if value.is_empty() {
            return div().into_any_element();
        }
        let value = value.to_string();
        let chip_selector = kind.chip_id(database_id);
        let label_selector = kind.label_id(database_id);
        let link_selector = kind.element_id(database_id);
        let heart_selector = kind.heart_id(database_id);
        let value_for_link = value.clone();
        let value_for_heart = value.clone();
        let palette = ChipPalette::for_theme(theme);
        let icon_selector = format!("{heart_selector}-icon");
        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_0p5()
            // ラベルはチップの外（タグに含めない）
            .child(
                div()
                    .id(SharedString::from(label_selector.clone()))
                    .debug_selector({
                        let label_selector = label_selector.clone();
                        move || label_selector.clone()
                    })
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(kind.prefix()),
            )
            // チップ本体 = 値 + ハート（タグチップと同じ見た目）
            .child(
                div()
                    .id(SharedString::from(chip_selector.clone()))
                    .debug_selector({
                        let chip_selector = chip_selector.clone();
                        move || chip_selector.clone()
                    })
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1p5()
                    .px_1()
                    .py_0p5()
                    .rounded_full()
                    .border_1()
                    .border_color(palette.border(is_favorite, is_selected))
                    .bg(palette.background(theme.muted, is_favorite, is_selected))
                    .text_color(palette.foreground(
                        theme.muted_foreground,
                        is_favorite,
                        is_selected,
                    ))
                    .text_xs()
                    .child(
                        div()
                            .id(SharedString::from(link_selector.clone()))
                            .debug_selector({
                                let link_selector = link_selector.clone();
                                move || link_selector.clone()
                            })
                            .cursor_pointer()
                            .on_click({
                                let handle = handle.clone();
                                move |_, _, cx| {
                                    // カード / 行のクリック（開く・ダウンロード）を発火させない
                                    cx.stop_propagation();
                                    handle.update(cx, |this, cx| match kind {
                                        EntityLink::Circle => {
                                            this.toggle_circle_filter(cx, &value_for_link)
                                        }
                                        EntityLink::Author => {
                                            this.toggle_author_filter(cx, &value_for_link)
                                        }
                                    });
                                }
                            })
                            .child(value),
                    )
                    .child(
                        div()
                            .id(SharedString::from(heart_selector.clone()))
                            .debug_selector({
                                let heart_selector = heart_selector.clone();
                                move || heart_selector.clone()
                            })
                            // 押しやすいように丸ボタンにしてハートを中に置く
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(CHIP_HEART_BUTTON))
                            .h(px(CHIP_HEART_BUTTON))
                            .rounded_full()
                            .bg(palette.heart_button)
                            .hover(move |style| style.bg(palette.heart_button_hover))
                            .text_color(palette.heart(theme.muted_foreground, is_favorite))
                            .cursor_pointer()
                            .on_click({
                                let handle = handle.clone();
                                move |_, _, cx| {
                                    // ハートはお気に入りのみ。値の絞り込みへ伝播させない
                                    cx.stop_propagation();
                                    handle.update(cx, |this, cx| {
                                        this.toggle_favorite_entity(cx, kind, &value_for_heart);
                                    });
                                }
                            })
                            // アイコンを丸の中心に置く（文字グリフのフォント依存のズレを避ける）
                            .child(
                                div()
                                    .id(SharedString::from(icon_selector.clone()))
                                    .debug_selector({
                                        let icon_selector = icon_selector.clone();
                                        move || icon_selector.clone()
                                    })
                                    .child(
                                        Icon::new(if is_favorite {
                                            AppIcon::HeartFilled
                                        } else {
                                            AppIcon::Heart
                                        })
                                        .size(px(CHIP_HEART_ICON)),
                                    ),
                            ),
                    ),
            )
            .into_any_element()
    }

    /// タグチップ行（Web の TagList 相当）。本棚と履歴で共用する。
    /// 並び順は `tag_order`（選択中 → お気に入り → 集計数の多い順 → 名前順）。
    /// クリックで絞り込み選択（青）、ハートでお気に入り（ピンク / ♥）。
    pub(crate) fn render_tag_chips(
        theme: &gpui_kit::component::Theme,
        tag_order: &TagOrder,
        database_id: &str,
        tags: &[String],
        on_click_tag: impl Fn(&str, &mut Window, &mut App) + Clone + 'static,
        on_heart: impl Fn(&str, &mut Window, &mut App) + Clone + 'static,
    ) -> Vec<gpui_kit::AnyElement> {
        tags.iter()
            .cloned()
            .map(|tag| {
                let is_favorite = tag_order.is_favorite(&tag);
                let is_selected = tag_order.is_selected(&tag);
                let tag_for_text = tag.clone();
                let tag_for_heart = tag.clone();
                let click_tag = on_click_tag.clone();
                let click_heart = on_heart.clone();
                let label_selector = format!("tag-label-{database_id}-{tag}");
                let heart_selector = format!("tag-heart-{database_id}-{tag}");
                let icon_selector = format!("{heart_selector}-icon");
                let palette = ChipPalette::for_theme(theme);
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_1p5()
                    .px_1()
                    .py_0p5()
                    .rounded_full()
                    .border_1()
                    .border_color(palette.border(is_favorite, is_selected))
                    .bg(palette.background(theme.muted, is_favorite, is_selected))
                    .text_color(palette.foreground(
                        theme.muted_foreground,
                        is_favorite,
                        is_selected,
                    ))
                    .text_xs()
                    // タグ文字のクリックでそのタグに絞り込む
                    .child(
                        div()
                            .id(SharedString::from(label_selector.clone()))
                            .debug_selector({
                                let label_selector = label_selector.clone();
                                move || label_selector.clone()
                            })
                            .cursor_pointer()
                            .on_click(move |_event, window, cx| {
                                cx.stop_propagation();
                                click_tag(&tag_for_text, window, cx);
                            })
                            .child(tag),
                    )
                    // ハート（文字の後ろ = 右端）は押しやすい丸ボタン。クリックでお気に入り
                    .child(
                        div()
                            .id(SharedString::from(heart_selector.clone()))
                            .debug_selector({
                                let heart_selector = heart_selector.clone();
                                move || heart_selector.clone()
                            })
                            .flex()
                            .items_center()
                            .justify_center()
                            .w(px(CHIP_HEART_BUTTON))
                            .h(px(CHIP_HEART_BUTTON))
                            .rounded_full()
                            .bg(palette.heart_button)
                            .hover(move |style| style.bg(palette.heart_button_hover))
                            .text_color(palette.heart(theme.muted_foreground, is_favorite))
                            .cursor_pointer()
                            .on_click(move |_event, window, cx| {
                                cx.stop_propagation();
                                click_heart(&tag_for_heart, window, cx);
                            })
                            // アイコンを丸の中心に置く（文字グリフのフォント依存のズレを避ける）
                            .child(
                                div()
                                    .id(SharedString::from(icon_selector.clone()))
                                    .debug_selector({
                                        let icon_selector = icon_selector.clone();
                                        move || icon_selector.clone()
                                    })
                                    .child(
                                        Icon::new(if is_favorite {
                                            AppIcon::HeartFilled
                                        } else {
                                            AppIcon::Heart
                                        })
                                        .size(px(CHIP_HEART_ICON)),
                                    ),
                            ),
                    )
                    .into_any_element()
            })
            .collect()
    }

    /// インラインタグエディタ（Web の TagsInput 相当: 枠付きチップ + 入力 +
    /// サジェスチョン + 保存/キャンセル）。
    fn render_tag_editor(
        _window: &mut Window,
        theme: &gpui_kit::component::Theme,
        handle: &gpui_kit::Entity<BookshelfView>,
        editing_tags: &[String],
        editing_suggestions: &[String],
        editing_input: &gpui_kit::Entity<InputState>,
        show_refetch: bool,
    ) -> impl IntoElement {
        let handle = handle.clone();
        let tags = editing_tags.to_vec();
        let suggestions = editing_suggestions.to_vec();
        let input = editing_input.clone();
        // クロージャ用にテーマ色をコピー
        let border_color = theme.border;
        let muted = theme.muted;
        let muted_fg = theme.muted_foreground;
        let background = theme.background;
        let primary = theme.primary;

        // エディタ内のどこをクリックしてもカードのクリック（開く/ダウンロード）
        // が発火しないように、エディタ全体で伝播を止める
        div()
            .id("tag-editor-root")
            .debug_selector(|| "tag-editor-root".into())
            .relative()
            .flex()
            .flex_col()
            .gap_2()
            .w_full()
            .cursor_pointer()
            .on_click(|_, _, cx| cx.stop_propagation())
            // 枠付きコンテナ（Web の rounded-md border px-2 py-1.5 相当）
            .child(
                div()
                    .relative()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .items_center()
                    .gap_1p5()
                    .rounded_md()
                    .border_1()
                    .border_color(border_color)
                    .bg(background)
                    .px_2()
                    .py_1p5()
                    // タグチップ + ✕（Web の TagChip 相当）
                    .children(tags.iter().map({
                        let handle = handle.clone();
                        move |tag| {
                            let handle = handle.clone();
                            let tag_id = tag.clone();
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_1()
                                .max_w(px(140.0))
                                .px_2()
                                .py_0p5()
                                .rounded_full()
                                .bg(muted)
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .child(div().truncate().child(tag.clone()))
                                .child(
                                    div()
                                        .id(SharedString::from(format!("edit-tag-x-{tag_id}")))
                                        .flex()
                                        .items_center()
                                        .justify_center()
                                        .w(px(14.0))
                                        .h(px(14.0))
                                        .rounded_full()
                                        .text_color(muted_fg)
                                        .hover(|style| style.bg(muted_fg.opacity(0.2)))
                                        .cursor_pointer()
                                        .on_click(move |_, _window, cx| {
                                            cx.stop_propagation();
                                            handle.update(cx, |this, cx| {
                                                this.remove_editing_tag(cx, &tag_id);
                                            });
                                        })
                                        .child(Icon::new(IconName::Close).size(px(10.0))),
                                )
                        }
                    }))
                    // 入力（Enter/カンマで追加）
                    .child(
                        div()
                            .min_w(px(80.0))
                            .flex_1()
                            .on_key_down({
                                let handle = handle.clone();
                                move |event, _window, cx| {
                                    if event.keystroke.key == "enter" || event.keystroke.key == ","
                                    {
                                        handle.update(cx, |this, cx| {
                                            this.add_editing_from_input(cx);
                                        });
                                    }
                                }
                            })
                            .child(Input::new(&input).cursor_text().w_full()),
                    )
                    // 保存 / キャンセル（Web の Button 相当）
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(
                                div().debug_selector(|| "tag-edit-save-btn".into()).child(
                                    Button::new("tag-edit-save")
                                        .cursor_pointer()
                                        .primary()
                                        .label("保存")
                                        .cursor_pointer()
                                        .on_click({
                                            let handle = handle.clone();
                                            move |_, _window, cx| {
                                                cx.stop_propagation();
                                                handle
                                                    .update(cx, |this, cx| this.save_tag_edit(cx));
                                            }
                                        }),
                                ),
                            )
                            // FANZA: ジャンルタグをサイトから再取得（未取得分を追加）。
                            // 保存ボタンの右に配置する。
                            .when(show_refetch, |this| {
                                this.child(
                                    div()
                                        .debug_selector(|| "tag-edit-refetch-btn".into())
                                        .child(
                                            Button::new("tag-edit-refetch")
                                                .cursor_pointer()
                                                .outline()
                                                .label("再取得")
                                                .cursor_pointer()
                                                .on_click({
                                                    let handle = handle.clone();
                                                    move |_, _window, cx| {
                                                        cx.stop_propagation();
                                                        handle.update(cx, |this, cx| {
                                                            this.refetch_genre_tags(cx);
                                                        });
                                                    }
                                                }),
                                        ),
                                )
                            })
                            .child(
                                div().debug_selector(|| "tag-edit-cancel-btn".into()).child(
                                    Button::new("tag-edit-cancel")
                                        .cursor_pointer()
                                        .outline()
                                        .label("キャンセル")
                                        .cursor_pointer()
                                        .on_click({
                                            let handle = handle.clone();
                                            move |_, _window, cx| {
                                                cx.stop_propagation();
                                                handle.update(cx, |this, cx| {
                                                    this.cancel_tag_edit(cx)
                                                });
                                            }
                                        }),
                                ),
                            ),
                    ),
            )
            // サジェスチョン（Web の suggestions 相当: 後で読む + お気に入りタグ）
            // カード（グリッドの List 行）にはみ出して後続カードに上書きされるのを避けるため、
            // absolute ではなくエディタ内の通常フロー（flex_col）で下に展開する
            .child(if !suggestions.is_empty() {
                div()
                    .w_full()
                    .rounded_md()
                    .border_1()
                    .border_color(border_color)
                    .bg(background)
                    .shadow_lg()
                    .p_2()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_1p5()
                    .children(suggestions.iter().map({
                        let handle = handle.clone();
                        move |suggestion| {
                            let handle = handle.clone();
                            let suggestion_id = suggestion.clone();
                            let suggestion_selector = format!("edit-suggestion-{suggestion_id}");
                            div()
                                .id(SharedString::from(format!(
                                    "edit-suggestion-{suggestion_id}"
                                )))
                                .debug_selector(move || suggestion_selector.clone())
                                .px_2()
                                .py_0p5()
                                .rounded_full()
                                .bg(muted)
                                .text_xs()
                                .text_color(muted_fg)
                                .hover(|style| style.bg(primary))
                                .cursor_pointer()
                                .on_click(move |_, _window, cx| {
                                    cx.stop_propagation();
                                    handle.update(cx, |this, cx| {
                                        this.add_editing_tag(cx, &suggestion_id);
                                    });
                                })
                                .child(suggestion.clone())
                        }
                    }))
                    .into_any_element()
            } else {
                div().into_any_element()
            })
    }

    /// 編集入力の状態を確保（初回のみ生成）。
    fn ensure_editing_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.editing_input.is_none() {
            self.editing_input =
                Some(cx.new(|cx| InputState::new(window, cx).placeholder("タグを追加...")));
        }
    }

    /// タグ編集ボタン（✎）。
    fn render_tag_edit_button(
        window: &mut Window,
        theme: &gpui_kit::component::Theme,
        handle: &gpui_kit::Entity<BookshelfView>,
        database_id: &str,
    ) -> gpui_kit::AnyElement {
        let _ = window;
        let edit_id = database_id.to_string();
        let handle = handle.clone();
        let edit_selector = format!("tag-edit-{database_id}");
        div()
            .id(SharedString::from(format!("tag-edit-{database_id}")))
            .debug_selector(move || edit_selector.clone())
            .flex()
            .items_center()
            .justify_center()
            .w(px(24.0))
            .h(px(24.0))
            .rounded_full()
            .bg(theme.background)
            .text_color(theme.muted_foreground)
            .text_sm()
            .child("✎")
            .cursor_pointer()
            .on_click(move |_event, window, cx| {
                cx.stop_propagation();
                handle.update(cx, |this, cx| {
                    this.start_tag_edit(window, cx, &edit_id);
                });
            })
            .into_any_element()
    }

    /// 状態（未読 / 既読 / ダウンロード済み / お気に入り）をタイトルの横に出す。
    ///
    /// 文字タグは短い「未読 / 既読」だけにする（「ダウンロード済み」「お気に入り」は
    /// 長くて変な折り返しになるためアイコン + tooltip で出す）。
    /// 配色は「注意が必要なものだけ色を付ける」方針:
    /// 未読 = 落ち着いた灰色、既読 = 緑、未ダウンロード = 黄、ダウンロード済み = 緑、
    /// お気に入り = ピンク（カードのハートと同じ色味）。
    fn render_status_tags(
        card: &ShelfCard,
        theme: &gpui_kit::component::Theme,
        handle: &gpui_kit::Entity<BookshelfView>,
    ) -> Vec<gpui_kit::AnyElement> {
        let database_id = card.shelf.database_id.as_str();
        let downloaded = card.local.is_some();
        let read = card
            .local
            .as_ref()
            .map(|entry| entry.is_read)
            .unwrap_or(false);
        let mut tags = Vec::with_capacity(3);
        if downloaded {
            if read {
                tags.push(status_tag(database_id, "read", "既読", TagVariant::Success));
            } else {
                tags.push(status_tag(
                    database_id,
                    "unread",
                    "未読",
                    TagVariant::Secondary,
                ));
            }
            tags.push(status_icon(
                database_id,
                "downloaded",
                AppIcon::HardDrive,
                if theme.is_dark() {
                    gpui_kit::rgb(0x34d399).into()
                } else {
                    gpui_kit::rgb(0x059669).into()
                },
                "ダウンロード済み",
                None,
            ));
        } else {
            // 未ダウンロードの本は読めないので「未読」は出さない
            tags.push(status_icon(
                database_id,
                "not-downloaded",
                AppIcon::Cloud,
                if theme.is_dark() {
                    gpui_kit::rgb(0xfbbf24).into()
                } else {
                    gpui_kit::rgb(0xb45309).into()
                },
                "未ダウンロード",
                None,
            ));
        }
        // お気に入りは登録 / 未登録の両方を出し、**クリックでトグル**する
        // （アイコンだけだと行クリックに伝播してビューアーが開いてしまうため止める）。
        let is_favorite = card.shelf.is_favorite == 1;
        let palette = ChipPalette::for_theme(theme);
        let favorite_card = card.clone();
        let favorite_handle = handle.clone();
        tags.push(status_icon(
            database_id,
            if is_favorite {
                "favorite"
            } else {
                "not-favorite"
            },
            if is_favorite {
                AppIcon::HeartFilled
            } else {
                AppIcon::Heart
            },
            palette.heart(theme.muted_foreground, is_favorite),
            if is_favorite {
                "お気に入り（クリックで解除）"
            } else {
                "お気に入りにする"
            },
            Some(Box::new(move |_window: &mut Window, cx: &mut App| {
                // 行クリック（ビューアー / ダウンロード）へ伝播させない
                cx.stop_propagation();
                favorite_handle.update(cx, |this, cx| {
                    this.toggle_favorite(cx, &favorite_card);
                });
            })),
        ));
        tags
    }

    /// カルーセルの前へ / 次へバー。**四角**で、高さは列（サムネイル）いっぱい、
    /// 幅はアイコン程度。組み込みのコントロールは円形で、しかもコンテンツ枠の外側に
    /// 絶対配置されるため見切れる。ここでは通常の flex 要素として並べる。
    fn render_carousel_bar(
        database_id: &str,
        direction: &str,
        icon: IconName,
        label: &str,
        enabled: bool,
        state: &Entity<CarouselState>,
        theme: &gpui_kit::component::Theme,
    ) -> gpui_kit::AnyElement {
        let selector = format!("list-carousel-{direction}-{database_id}");
        let element_id = format!("related-{direction}-{database_id}");
        let tooltip = label.to_string();
        let state = state.clone();
        let next = direction == "next";
        let foreground = if enabled {
            theme.foreground
        } else {
            // 無効時は Button の disabled と同じ薄さにする（アイコンが通常色のままだと
            // 押せるように見えてしまう）
            theme.muted_foreground.opacity(0.5)
        };
        div()
            .id(SharedString::from(element_id))
            .debug_selector(move || selector.clone())
            .flex_none()
            .w(px(LIST_CAROUSEL_BAR_W))
            // 高さは列（サムネイル）いっぱい。親が auto 高さなので `h_full()` では
            // 効かない（% 高さが auto に解決される）→ align-self: stretch で伸ばす
            .self_stretch()
            .flex()
            .items_center()
            .justify_center()
            // 四角（既定は角丸）
            .rounded(px(0.0))
            .bg(if enabled {
                theme.muted
            } else {
                // 無効時はボタンに見えないよう背景も落とす
                theme.muted.opacity(0.4)
            })
            // ホバーで応答（移動できる向きだけ）。`theme.secondary` はダークで背景の
            // `muted` と同色になるため、明示のホバー色を使う。
            .when(enabled, |this| {
                this.hover(move |style| style.bg(crate::views::hover_bg(theme)))
            })
            .when(enabled, |this| this.cursor_pointer())
            .on_click(move |_, _, cx| {
                // 行クリック（ビューアー / ダウンロード）へ伝播させない。
                // **移動できない向きでもハンドラを必ず登録する**（登録が無いと
                // クリックが行へ抜けてビューアーが開いてしまう）。
                cx.stop_propagation();
                if enabled {
                    state.update(cx, |state, cx| {
                        if next {
                            state.select_next(cx);
                        } else {
                            state.select_previous(cx);
                        }
                    });
                }
            })
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .child(
                div()
                    .text_color(foreground)
                    .child(Icon::new(icon).size(px(LIST_STATUS_ICON))),
            )
            .into_any_element()
    }

    /// タグ列の折りたたみトグル（「+n」/「閉じる」）。タグチップと同じ配色にして、
    /// タグ列の一部として見せる。クリックは行の動作（ビューアー / ダウンロード）へ
    /// 伝播させない。
    pub(crate) fn render_tag_toggle(
        theme: &gpui_kit::component::Theme,
        selector: String,
        hidden: usize,
        expanded: bool,
        on_toggle: impl Fn(&mut Window, &mut App) + 'static,
    ) -> gpui_kit::AnyElement {
        let label = tag_toggle_label(hidden, expanded);
        let tooltip = if expanded {
            "タグを閉じる".to_string()
        } else {
            format!("残り {hidden} 件のタグを表示")
        };
        let palette = ChipPalette::for_theme(theme);
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector(move || selector.clone())
            .flex()
            .flex_row()
            .items_center()
            .px_1()
            .py_0p5()
            .rounded_full()
            .border_1()
            .border_color(palette.border(false, false))
            .bg(palette.background(theme.muted, false, false))
            .text_color(palette.foreground(theme.muted_foreground, false, false))
            .text_xs()
            .cursor_pointer()
            .hover(|style| style.bg(theme.muted_foreground.opacity(0.2)))
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                on_toggle(window, cx);
            })
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            .child(label)
            .into_any_element()
    }

    /// タグ情報エリアの右側に、同一サークル / 同一作者の関連書籍をカルーセルで出す。
    /// **残りの幅を全部使う**（アイテム幅は固定なので、幅が広いほど多く見える）。
    /// サムネイルをクリックすると、行と同じ経路で開く / ダウンロードする。
    fn render_related_carousel(
        theme: &gpui_kit::component::Theme,
        handle: &gpui_kit::Entity<BookshelfView>,
        database_id: &str,
        state: &Entity<CarouselState>,
        thumbs: &[RelatedThumb],
        has_previous: bool,
        has_next: bool,
    ) -> gpui_kit::AnyElement {
        let selector = format!("list-carousel-{database_id}");
        div()
            .debug_selector(move || selector.clone())
            .flex_1()
            .min_w_0()
            .child(
                Carousel::new(
                    SharedString::from(format!("related-carousel-{database_id}")),
                    state,
                )
                // ルートは既定で縦積み（コンテンツ + コントロール）なので横に並べる。
                // 前へ / 次へは自前のバー（組み込みコントロールは円形 + 枠外配置で見切れる）。
                // 行が高くなったとき（タグ展開）は表紙と同じく**上揃え**にする
                // （中央揃えだと上下に余白ができて浮いて見える）。
                .flex()
                .flex_row()
                .items_center()
                .gap_1()
                .child(Self::render_carousel_bar(
                    database_id,
                    "prev",
                    IconName::ChevronLeft,
                    "前へ",
                    has_previous,
                    state,
                    theme,
                ))
                .child(
                    CarouselContent::new(state)
                        .flex_1()
                        .min_w_0()
                        // アイテム側の pl_1 と対で、先頭の余白を打ち消す
                        .track_style(StyleRefinement::default().ml_neg_1())
                        .children(thumbs.iter().map(|thumb| {
                            CarouselItem::new(
                                SharedString::from(format!(
                                    "related-item-{database_id}-{}",
                                    thumb.position
                                )),
                                thumb.position,
                                state,
                            )
                            // カルーセル列の幅を N 等分して**埋める**（端に切れかけを出さない）。
                            // 大きさの上限は表紙と同じ（窓が広いときはこの上限で並ぶ）
                            .w(relative(1.0 / LIST_RELATED_PER_VIEW))
                            .max_w(px(LIST_COVER_W))
                            .flex_none()
                            .pl_1()
                            .child(Self::render_related_thumb(
                                theme,
                                handle,
                                database_id,
                                thumb,
                            ))
                        })),
                )
                .child(Self::render_carousel_bar(
                    database_id,
                    "next",
                    IconName::ChevronRight,
                    "次へ",
                    has_next,
                    state,
                    theme,
                )),
            )
            .into_any_element()
    }

    /// 関連書籍 1 件のサムネイル（ツールチップで作品名、クリックでビューアー）。
    fn render_related_thumb(
        theme: &gpui_kit::component::Theme,
        handle: &gpui_kit::Entity<BookshelfView>,
        database_id: &str,
        thumb: &RelatedThumb,
    ) -> gpui_kit::AnyElement {
        let selector = format!("list-related-{database_id}-{}", thumb.position);
        let img_selector = format!("list-related-img-{database_id}-{}", thumb.position);
        let element_id = format!("related-thumb-{database_id}-{}", thumb.position);
        let tooltip_title = thumb.title.clone();
        let click_database_id = thumb.item.database_id.clone();
        let click_book_id = thumb.book_id.clone();
        let click_item = thumb.item.clone();
        let cover = thumb.cover.clone();
        let handle = handle.clone();
        div()
            .id(SharedString::from(element_id))
            .debug_selector(move || selector.clone())
            .cursor_pointer()
            // アイテム（カルーセル列幅の 1/N）いっぱいに広げて 3:2 を保つ
            .relative()
            .w_full()
            .aspect_ratio(LIST_COVER_ASPECT)
            .overflow_hidden()
            .rounded(px(2.0))
            .bg(theme.muted)
            .tooltip(move |window, cx| Tooltip::new(tooltip_title.clone()).build(window, cx))
            .on_click(move |_, _, cx| {
                // 行クリック（この行の本を開く / ダウンロードする）へ伝播させない
                cx.stop_propagation();
                let handle = handle.clone();
                let book_id = click_book_id.clone();
                let item = click_item.clone();
                let database_id = click_database_id.clone();
                handle.update(cx, |this, cx| {
                    this.open_or_download(cx, &database_id, book_id, &item);
                });
            })
            // 画像は比率を保つ（切り抜きしない）。リストの表紙と同じ見た目にする
            .child(cover_fit_inside_frame(cover.as_ref(), img_selector))
            .into_any_element()
    }

    /// リスト表示の 1 行（Web の table 行相当: サムネイル + タイトル + イベント + 進捗 + タグ）。
    fn render_list_row(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        card: &ShelfCard,
        selected: bool,
        tag_order: &TagOrder,
    ) -> impl IntoElement {
        let shelf = &card.shelf;
        let title = shelf.title.clone();
        // サークル名（技術書典: organization / BOOTH: shop）
        let circle_name = shelf.circle_name.clone();
        // 作者名（技術書典は空のため非表示。BOOTH 等は作成者名）
        let author = shelf.author.clone();
        let event = shelf
            .event_name
            .clone()
            .map(|name| format_event_label(&name));
        let event_text = event.unwrap_or_else(|| "イベント不明".to_string());
        let purchase_date = shelf.caused_at.as_deref().map(format_purchase_date);
        let database_id = shelf.database_id.clone();
        let cover = card.cover.clone().or_else(|| {
            if card.cover_fetch_failed {
                no_image_cover()
            } else {
                placeholder_cover(&shelf.title, &shelf.circle_name)
            }
        });
        let local = card.local.as_ref();
        let download_state = self.download_states.get(&shelf.database_id).copied();
        let progress_text = local.and_then(|e| {
            e.progress.map(|(current, total)| match total {
                Some(total) => format!("{} / {total}ページ", current.max(1)),
                None => format!("{current}ページ閲覧中"),
            })
        });
        let handle = cx.entity();
        let open_id = database_id.clone();
        let delete_id = local.map(|e| e.book.id.clone());
        let _ = window;

        // 表紙: 表示エリアは**全行で同じ比率・同じ大きさ**（3:2 / 162x108 固定）。
        // 画像は枠に対する**実寸（%）を明示指定**して比率を保つ。`object_fit` に頼らないので
        // 切り抜きは起き得ない（縦長は左右、横長は上下に余白ができる）。
        let image = cover_fit_inside_frame(cover.as_ref(), format!("list-cover-img-{database_id}"));

        let cover_selector = format!("list-cover-{database_id}");
        // 大きさは固定。未読 / 既読 / ダウンロード済みはサムネイルに重ねず、
        // タイトルの横に出す（重ねると行の内容でも大きさが変わり、ガタつきの原因になる）。
        let mut cover_el = div()
            .relative()
            .w(px(LIST_COVER_W))
            .h(px(LIST_COVER_MIN_H))
            .flex_shrink_0()
            .overflow_hidden()
            // 縦長表紙で余る左右の余白を周囲と馴染ませる
            .bg(cx.theme().muted)
            .debug_selector(move || cover_selector.clone())
            .child(image);

        if let Some(state) = download_state {
            let fraction = state.fraction();
            let percentage = (fraction * 100.0).round() as u32;
            let ring = progress_ring_image(fraction);
            cover_el = cover_el.child(
                div()
                    .absolute()
                    .inset_0()
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui_kit::rgba(0x00000080))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap_1()
                            .child(img(ring).w(px(48.0)).h(px(48.0)))
                            .child(
                                div()
                                    .text_color(gpui_kit::white())
                                    .text_xs()
                                    .child(format!("{percentage}%")),
                            ),
                    ),
            );
        }

        let row_selector = format!("book-list-{database_id}");
        let mut row = div()
            .id(SharedString::from(row_selector.clone()))
            .debug_selector(move || row_selector.clone())
            .flex()
            .flex_row()
            // スクロール領域（flex_col）内で行が圧縮されないようにする
            .flex_shrink_0()
            .gap_3()
            .p_2()
            // 行の高さをサムネイル枠の下限以上にして、表紙が行に収まって見えるようにする
            .min_h(px(LIST_COVER_MIN_H + 16.0))
            .border_b_1()
            .border_color(cx.theme().border)
            .when(selected, |style| style.bg(cx.theme().secondary))
            .hover(|style| style.bg(cx.theme().secondary));

        row = row.cursor_pointer().on_click({
            let handle = handle.clone();
            let open_book_id = delete_id.clone();
            let download_item = card.shelf.clone();
            let click_database_id = database_id.clone();
            move |_, _window, cx| {
                handle.update(cx, |this, cx| {
                    this.open_or_download(
                        cx,
                        &click_database_id,
                        open_book_id.clone(),
                        &download_item,
                    );
                });
            }
        });

        // 情報列（幅固定）: タイトル / 購入日 / サークル・作者 / ページ数 / 状態
        let info_selector = format!("list-info-{database_id}");
        let title_selector = format!("list-title-{database_id}");
        let progress_selector = format!("list-progress-{database_id}");
        let status_selector = format!("list-status-row-{database_id}");
        let info_column = div()
            .debug_selector(move || info_selector.clone())
            .flex()
            .flex_col()
            .gap_1()
            .min_w_0()
            .w(px(LIST_INFO_W))
            // 幅は固定だが、窓が狭いときは縮められるようにする
            // （`flex_shrink_0` にすると狭い窓で行が横にはみ出す）
            .child(
                div()
                    .debug_selector(move || title_selector.clone())
                    .text_sm()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .child(title),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    // Web の formatEventLabel と同じ: イベント不明のときは
                    // 「イベント不明」を表示
                    .child(match (shelf.site_id.as_str(), purchase_date.as_deref()) {
                        ("booth", Some(date)) | ("fanza", Some(date)) | ("dlsite", Some(date)) => {
                            format!("購入日: {date}")
                        }
                        _ => event_text.clone(),
                    }),
            )
            // サークル名 / 作者名（タグと同じチップ。本体クリックで絞り込み、
            // 右端のハートでお気に入り）
            .child(
                div()
                    .flex()
                    .flex_row()
                    .flex_wrap()
                    .gap_1()
                    .items_center()
                    .child(BookshelfView::render_entity_chip(
                        cx.theme(),
                        &handle,
                        EntityLink::Circle,
                        &circle_name,
                        &database_id,
                        self.favorite_circles.contains(&circle_name),
                        self.circle_filter.as_deref() == Some(circle_name.as_str()),
                    ))
                    .child(BookshelfView::render_entity_chip(
                        cx.theme(),
                        &handle,
                        EntityLink::Author,
                        &author,
                        &database_id,
                        self.favorite_authors.contains(&author),
                        self.author_filter.as_deref() == Some(author.as_str()),
                    )),
            )
            // ページ数（未取得の本は進捗が無いので出さない）
            .when_some(progress_text, |this, text| {
                this.child(
                    div()
                        .debug_selector({
                            let selector = progress_selector.clone();
                            move || selector.clone()
                        })
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(text),
                )
            })
            // 状態（未読 / 既読 / ダウンロード済み / お気に入り）はページ数の下
            .child(
                div()
                    .debug_selector(move || status_selector.clone())
                    .flex()
                    .flex_row()
                    .gap_2()
                    .items_center()
                    .children(BookshelfView::render_status_tags(card, cx.theme(), &handle)),
            );

        // タグ情報エリア: タグだけを**行の幅の約 20%** に収め、入りきらなければ折り返す。
        // 状態タグ（未読 / ダウンロード済み 等）はタイトルの横に出す。
        let tags_selector = format!("list-tags-{database_id}");
        let tag_area = div()
            .debug_selector(move || tags_selector.clone())
            .flex()
            .flex_row()
            .flex_wrap()
            // 折り返した行を**上詰め**にする。既定は align-content: stretch で、行が
            // タグ列の高さいっぱいまで伸びるため、タグが無い / 少ない本では編集ボタンが
            // 縦中央に落ちてしまう（上の行と揃わない）。
            .content_start()
            .gap_1()
            .items_center()
            .w(relative(LIST_TAGS_W_RATIO))
            .min_w_0()
            .child(
                if self.editing_book_id.as_deref() == Some(database_id.as_str()) {
                    BookshelfView::render_tag_editor(
                        window,
                        cx.theme(),
                        &handle,
                        &self.editing_tags,
                        &self.editing_suggestions,
                        &self.editing_input.clone().expect("editing input"),
                        self.editing_site_id.as_deref() == Some("fanza"),
                    )
                    .into_any_element()
                } else {
                    let ordered = tag_order.sorted(&card.tags);
                    let expanded = self.expanded_tag_rows.contains(&database_id);
                    let visible = if expanded {
                        ordered.len()
                    } else {
                        list_tags_visible_count(window, cx.theme(), &ordered)
                    };
                    let chips = BookshelfView::render_tag_chips(
                        cx.theme(),
                        tag_order,
                        &database_id,
                        &ordered[..visible],
                        {
                            let handle = handle.clone();
                            move |tag: &str, _window: &mut Window, cx: &mut App| {
                                let tag = tag.to_string();
                                handle.update(cx, |this, cx| this.toggle_tag(cx, &tag));
                            }
                        },
                        {
                            let handle = handle.clone();
                            move |tag: &str, _window: &mut Window, cx: &mut App| {
                                let tag = tag.to_string();
                                handle.update(cx, |this, cx| this.toggle_favorite_tag(cx, &tag));
                            }
                        },
                    );
                    // タグが多い本は列の中で折りたたむ（行の高さをタグ列に支配させない）。
                    // 「+n」で全件表示、「閉じる」で折りたたみへ戻す。
                    let hidden = ordered.len() - visible;
                    let mut tag_row = div()
                        .id(SharedString::from(format!("tag-row-{database_id}")))
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap_1()
                        .items_center()
                        .flex_1()
                        .min_w_0()
                        .cursor_pointer()
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .children(chips);
                    // 展開中も閉じられるようにトグルを出す（折りたたみ中は残り件数があるときだけ）
                    if expanded || hidden > 0 {
                        tag_row = tag_row.child(BookshelfView::render_tag_toggle(
                            cx.theme(),
                            format!("tag-toggle-{database_id}"),
                            hidden,
                            expanded,
                            {
                                let handle = handle.clone();
                                let database_id = database_id.clone();
                                move |_window, cx| {
                                    handle.update(cx, |this, cx| {
                                        this.toggle_tag_expansion(&database_id, cx)
                                    });
                                }
                            },
                        ));
                    }
                    tag_row
                        .child(BookshelfView::render_tag_edit_button(
                            window,
                            cx.theme(),
                            &handle,
                            &database_id,
                        ))
                        .into_any_element()
                },
            );

        // タグ情報エリアの右側: 同一サークル / 同一作者の関連書籍カルーセル。
        // 関連が無い本には出さない（空のカルーセル枠を残さない）。
        let carousel = self
            .carousel_states
            .get(&database_id)
            .filter(|_| !card.related.is_empty())
            .cloned()
            .map(|state| {
                let thumbs: Vec<RelatedThumb> = card
                    .related
                    .iter()
                    .enumerate()
                    .filter_map(|(position, index)| {
                        let target = self.shelf_cards.get(*index)?;
                        Some(RelatedThumb {
                            position,
                            title: target.shelf.title.clone(),
                            book_id: target.local.as_ref().map(|entry| entry.book.id.clone()),
                            item: target.shelf.clone(),
                            cover: target.cover.clone(),
                        })
                    })
                    .collect();
                let has_previous = state.read(cx).has_previous();
                let has_next = state.read(cx).has_next();
                BookshelfView::render_related_carousel(
                    cx.theme(),
                    &handle,
                    &database_id,
                    &state,
                    &thumbs,
                    has_previous,
                    has_next,
                )
            });

        row = row
            .child(cover_el)
            .child(info_column)
            .child(tag_area)
            .children(carousel);

        row.context_menu({
            let has_local = delete_id.is_some();
            let open_id_for_menu = delete_id.clone().unwrap_or_else(|| open_id.clone());
            let edit_id_for_menu = database_id.clone();
            let delete_id_for_menu = delete_id.clone();
            let site_id_for_menu = shelf.site_id.clone();
            move |menu, _window, cx| {
                let mut menu = menu;
                // 閲覧回数を一番上に表示（ローカル本のみ。クリック不可）
                if let Some(book_id) = delete_id_for_menu.as_ref() {
                    let state = AppState::global(cx);
                    if let Ok(count) = db::view_history::view_count(&state.db_pool, book_id) {
                        menu = menu.menu_with_disabled(
                            format!("閲覧回数: {count} 回"),
                            Box::new(crate::actions::Noop),
                            true,
                        );
                    }
                }
                if has_local {
                    menu = menu.menu(
                        "開く",
                        Box::new(OpenReader {
                            book_id: open_id_for_menu.clone().into(),
                        }),
                    );
                }
                menu = menu.menu(
                    "タグ編集",
                    Box::new(EditBookTags {
                        book_id: edit_id_for_menu.clone().into(),
                    }),
                );
                menu = menu.menu(
                    "再取得",
                    Box::new(crate::actions::RedownloadBook {
                        database_id: edit_id_for_menu.clone().into(),
                        site_id: site_id_for_menu.clone().into(),
                    }),
                );
                menu = menu.menu(
                    "非表示にする",
                    Box::new(HideBook {
                        database_id: edit_id_for_menu.clone().into(),
                        site_id: site_id_for_menu.clone().into(),
                    }),
                );
                menu
            }
        })
    }
}

impl Render for BookshelfView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 設定画面の非表示解除などの変更を反映する（全 reload は重いため軽量更新）
        if *AppState::global(cx).bookshelf_invalidated.lock() {
            *AppState::global(cx).bookshelf_invalidated.lock() = false;
            self.refresh_hidden(cx);
        }
        self.ensure_search_state(window, cx);
        if self.editing_book_id.is_some() {
            self.ensure_editing_input(window, cx);
        }
        // 初回表示時に一度だけフォーカスを付与（キーボード選択を受け付ける）
        if !self.focus_initialized {
            self.focus_initialized = true;
            window.focus(&self.focus_handle, cx);
        }
        // 選択インデックスをフィルタ結果に合わせる（初回・範囲外は 1 つ目を選択）
        if self.filtered.is_empty() {
            self.selected_index = None;
        } else if self.selected_index.is_none_or(|i| i >= self.filtered.len()) {
            self.selected_index = Some(0);
        }
        // 検索文字列の変更を検出してフィルタキャッシュを更新する
        let search = self.current_search(cx);
        if search.as_deref() != Some(self.last_search.as_str()) {
            self.last_search = search.clone().unwrap_or_default();
            self.filtered_dirty = true;
        }
        if self.filtered_dirty {
            self.filtered = (0..self.shelf_cards.len())
                .filter(|&i| self.matches_filter(cx, &self.shelf_cards[i]))
                .collect();
            self.filtered_dirty = false;
        }
        if !self.auto_download_started {
            self.auto_download_started = true;
            self.auto_download_favorites(cx);
        }
        if let Some(book_id) = self.pending_tag_edit.take() {
            self.start_tag_edit(window, cx, &book_id);
        }
        // 技術書典以外のサイト（BOOTH）ではイベント/タグ取得を非表示にする
        let is_booth = self.site_filter.as_deref() == Some("booth");
        let visible: Vec<&ShelfCard> = self.visible_shelf_cards(cx);
        let visible_count = visible.len();
        let view_mode = self.view_mode;
        let tag_fetch_enabled = self.tag_fetch_enabled;
        let available_events = self.available_events.clone();
        let selected_events = self.selected_events.clone();
        // タグチップ / お気に入りタグ一覧の並び替えキー（お気に入り + 集計数）。
        // 描画ごとに 1 つ作り、可視カード / 行 / ポップオーバーで使い回す。お気に入りを
        // 変えると次の描画でここが作り直され、ハートのクリックだけで並び替わる。
        let tag_order = TagOrder::new(
            &self.favorite_tags,
            &self.selected_tags,
            self.tag_counts.clone(),
        );
        let favorite_tags = {
            let favs = self.favorite_tags.clone();
            let sorted = match self.site_filter.as_deref() {
                // サイト選択中はそのサイトのタグでお気に入りタグフィルタを絞る
                Some(site) => {
                    let state = Self::app_state(cx);
                    let db = &state.db_pool;
                    let mut site_tags = std::collections::HashSet::new();
                    for item in bookshelf::list_all(db).ok().unwrap_or_default() {
                        if item.site_id == site {
                            for t in bookshelf::tags_of(&item) {
                                site_tags.insert(t);
                            }
                        }
                    }
                    for b in books::list(db).ok().unwrap_or_default() {
                        if b.site_id.as_deref() == Some(site) {
                            for t in db::tags::list_for_book(db, &b.id).unwrap_or_default() {
                                site_tags.insert(t.tag_name);
                            }
                        }
                    }
                    favs.into_iter()
                        .filter(|t| site_tags.contains(t))
                        .collect::<Vec<_>>()
                }
                None => favs,
            };
            // 一覧もタグチップと同じ並び（集計数の多い順 → 名前順）にする
            tag_order.sorted(&sorted)
        };
        let search_state = self.search_state.clone().expect("search state");
        // Popover の content クロージャは 'static なのでテーマ色を先にコピーする
        let theme_border = cx.theme().border;
        let theme_popover = cx.theme().popover;
        let theme_muted = cx.theme().muted;
        let theme_muted_fg = cx.theme().muted_foreground;
        let theme_primary = cx.theme().primary;
        let theme_primary_fg = cx.theme().primary_foreground;
        let theme_secondary = cx.theme().secondary;
        let selected_tags = self.selected_tags.clone();
        let read_filter = self.read_filter;
        let busy = self.sync_busy > 0;
        let toast = self.toast.clone();
        let error = self.error.clone();
        // 取り込み確認モーダル（§6.3）: 要約だけなので clone して描画に使う
        let pending_import = self.pending_import.as_ref().map(|pending| {
            (
                pending.title.clone(),
                pending.choices.clone(),
                pending.selected,
            )
        });
        let handle = cx.entity();
        let card_tag_order = tag_order.clone();

        div()
            .id("bookshelf-root")
            .debug_selector(|| "bookshelf-root".into())
            .track_focus(&self.focus_handle)
            .on_key_down({
                let handle = cx.entity();
                move |event, window, cx| {
                    handle.update(cx, |this, cx| this.handle_key(event, window, cx));
                }
            })
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .bg(cx.theme().background)
            // 1 段目: サイト情報（選択中のサイトに応じて表示）
            .child({
                let (site_title, site_subtitle) = match self.site_filter.as_deref() {
                    Some("booth") => ("BOOTH", "BOOTH の本棚"),
                    Some("techbookfest") => ("技術書典", "TechBookFest の本棚"),
                    Some("fanza") => ("FANZA同人", "FANZA同人の本棚"),
                    Some("dlsite") => ("DLsite", "DLsite の本棚"),
                    _ => ("すべての本", "すべてのサイトの本棚"),
        };
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui_kit::FontWeight::BOLD)
                            .child(site_title),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(site_subtitle),
                    )
            })
            // 2 段目: 件数・フィルタ・検索（左）と表示切替・タグ取得・同期（右）
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .flex_1()
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("{visible_count}件")),
                            )
                            .child(
                                div()
                                    .id("filter-all-btn")
                                    .debug_selector(|| "filter-all-btn".into())
                                    .cursor_pointer()
                                    .on_click({
                                        let handle = handle.clone();
                                        move |_, window, cx| {
                                            handle.update(cx, |this, cx| {
                                                this.clear_filters(window, cx)
                                            });
                                        }
                                    })
                                    .child({
                                        let mut button = Button::new("filter-all")
                                            .cursor_pointer()
                                            .label(self.filter_all_label(cx));
                                        // 絞り込み中はタグ選択中と同じ青系（チップの選択色）にして、
                                        // 解除のショートカット（ESC）をツールチップで示す。
                                        // 未絞り込み（= 全項目が選択中）は既定の選択色
                                        button = if self.is_filtering(cx) {
                                            let (bg, fg, hover, active) =
                                                ChipPalette::for_theme(cx.theme()).filter_all_colors();
                                            button
                                                .custom(
                                                    ButtonCustomVariant::new(cx)
                                                        .color(bg)
                                                        .foreground(fg)
                                                        .hover(hover)
                                                        .active(active),
                                                )
                                                .tooltip("絞り込みを解除（ESC）")
                                        } else {
                                            button.primary()
                                        };
                                        button
                                    }),
                            )
                            .child(
                                {
                                    let mut button = Button::new("filter-unread").cursor_pointer().label("未読");
                                    if read_filter == ReadFilter::Unread {
                                        button = button.primary();
                                    }
                                    button
                                }.cursor_pointer().on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        handle.update(cx, |this, cx| {
                                            this.set_read_filter(cx, ReadFilter::Unread)
                                        });
                                    }
                                }),
                            )
                            .child(
                                {
                                    let mut button = Button::new("filter-read").cursor_pointer().label("既読");
                                    if read_filter == ReadFilter::Read {
                                        button = button.primary();
                                    }
                                    button
                                }.cursor_pointer().on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        handle.update(cx, |this, cx| {
                                            this.set_read_filter(cx, ReadFilter::Read)
                                        });
                                    }
                                }),
                            )
                            .child(
                                {
                                    let mut button =
                                        Button::new("filter-favorite").cursor_pointer().label("お気に入り");
                                    if read_filter == ReadFilter::Favorite {
                                        button = button.primary();
                                    }
                                    button
                                }.cursor_pointer().on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        handle.update(cx, |this, cx| {
                                            this.set_read_filter(cx, ReadFilter::Favorite)
                                        });
                                    }
                                }),
                            )
                            // イベントフィルタ（Web の eventDropdown 相当: Popover + チェックボックス行）。
                            // 技術書典専用機能のため BOOTH フィルタ時は非表示
                            .child(
                                div()
                                    .when(is_booth, |this| this.hidden())
                                    .relative()
                                    .debug_selector(|| "event-filter-trigger".into())
                                    .child(
                                        Popover::new("event-filter-popover")
                                            .appearance(false)
                                            .anchor(Anchor::TopLeft)
                                            .trigger(
                                                Button::new("bookshelf-event-filter").cursor_pointer()
                                                    .label("イベント")
                                                    .outline()
                                                    // Web と同じ: 選択中は数バッジを表示
                                                    .child(if !selected_events.is_empty() {
                                                        div()
                                                            .ml_1p5()
                                                            .flex()
                                                            .items_center()
                                                            .justify_center()
                                                            .h(px(16.0))
                                                            .w(px(16.0))
                                                            .rounded_full()
                                                            .bg(theme_primary)
                                                            .text_color(theme_primary_fg)
                                                            .text_size(px(10.0))
                                                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                                                            .child(
                                                                selected_events.len().to_string(),
                                                            )
                                                            .into_any_element()
                                                    } else {
                                                        div().into_any_element()
                                                    }),
                                            )
                                            .content({
                                                let handle = handle.clone();
                                                let available_events = available_events.clone();
                                                let selected_events = selected_events.clone();
                                                move |_state, _window, _cx| {
                                                    div()
                                                        .w(px(224.0))
                                                        .rounded_md()
                                                        .border_1()
                                                        .border_color(theme_border)
                                                        .bg(theme_popover)
                                                        .shadow_lg()
                                                        .p_2()
                                                        .flex()
                                                        .flex_col()
                                                        .gap_1()
                                                        .children(available_events.iter().map({
                                                            let handle = handle.clone();
                                                            let selected_events =
                                                                selected_events.clone();
                                                            move |event| {
                                                                let is_selected =
                                                                    selected_events.contains(event);
                                                                let handle = handle.clone();
                                                                let event_id = event.clone();
                                                                let event_selector = format!(
                                                                    "event-option-{event_id}"
                                                                );
                                                                div()
                                                                                                                            .id(SharedString::from(format!(
                                                                "event-option-{event_id}"
                                                                                                                            )))
                                                                                                                            .debug_selector(move || {
                                                                event_selector.clone()
                                                                                                                            })
                                                                                                                            .flex()
                                                                                                                            .flex_row()
                                                                                                                            .items_center()
                                                                                                                            .gap_2()
                                                                                                                            .px_2()
                                                                                                                            .py_1p5()
                                                                                                                            .rounded_sm()
                                                                                                                            .hover(|style| {
                                                                style.bg(theme_secondary)
                                                                                                                            }).cursor_pointer().on_click(move |_, _window, cx| {
                                                                    handle.update(cx, |this, cx| {
                                                                        if this
                                                                            .selected_events
                                                                            .iter()
                                                                            .any(|e| {
                                                                                e == &event_id
                                                                            })
                                                                        {
                                                                            this.selected_events
                                                                                .retain(|e| {
                                                                                    e != &event_id
                                                                                });
                                                                        } else {
                                                                            this.selected_events
                                                                                .push(event_id.clone());
                                                                        }
                                                                        this.filtered_dirty = true;
                                                                        cx.notify();
                                                                    });
                                                                })
                                                            // Web の checkbox 相当
                                                            .child(
                                                                div()
                                                                    .flex()
                                                                    .items_center()
                                                                    .justify_center()
                                                                    .w(px(16.0))
                                                                    .h(px(16.0))
                                                                    .rounded_sm()
                                                                    .border_1()
                                                                    .border_color(if is_selected {
                                                                        theme_primary
                                                                    } else {
                                                                        theme_border
                                                                    })
                                                                    .bg(if is_selected {
                                                                        theme_primary
                                                                    } else {
                                                                        gpui_kit::transparent_black()
                                                                    })
                                                                    .child(if is_selected {
                                                                        Icon::new(IconName::Check)
                                                                            .size(px(12.0))
                                                                            .text_color(
                                                                                theme_primary_fg,
                                                                            )
                                                                            .into_any_element()
                                                                    } else {
                                                                        div().into_any_element()
                                                                    }),
                                                            )
                                                            .child(
                                                                div()
                                                                    .text_sm()
                                                                    .truncate()
                                                                    .child(event.clone()),
                                                            )
                                                            }
                                                        }))
                                                        .into_any_element()
                                                }
                                            }),
                                    ),
                            )
                            // お気に入りタグフィルタ（Web の tagDropdown 相当: Popover + 丸チップ）
                            .child(
                                div()
                                    .relative()
                                    .debug_selector(|| "tag-filter-trigger".into())
                                    .child(
                                        Popover::new("tag-filter-popover")
                                            .appearance(false)
                                            .anchor(Anchor::TopLeft)
                                            .trigger(
                                                Button::new("bookshelf-tag-filter").cursor_pointer()
                                                    .label(if selected_tags.is_empty() {
                                                        "お気に入りタグ".to_string()
                                                    } else {
                                                        format!("{}タグ選択", selected_tags.len())
                                                    })
                                                    .outline(),
                                            )
                                            .content({
                                                let handle = handle.clone();
                                                let favorite_tags = favorite_tags.clone();
                                                let selected_tags = selected_tags.clone();
                                                move |_state, _window, _cx| {
                                                    if favorite_tags.is_empty() {
                                                        // イベントドロップダウンと同じ枠付きスタイル
                                                        // （背景・枠線がないと透明に見える）
                                                        div()
                                                            .w(px(256.0))
                                                            .rounded_md()
                                                            .border_1()
                                                            .border_color(theme_border)
                                                            .bg(theme_popover)
                                                            .shadow_lg()
                                                            .p_3()
                                                            .flex()
                                                            .flex_col()
                                                            .gap_1()
                                                            .child(
                                                                div()
                                                                    .text_sm()
                                                                    .text_color(theme_muted_fg)
                                                                    .child(
                                                                        "お気に入りタグがありません",
                                                                    ),
                                                            )
                                                            .child(
                                                                div()
                                                                    .text_xs()
                                                                    .text_color(
                                                                        theme_muted_fg
                                                                            .opacity(0.8),
                                                                    )
                                                                    .child(
                                                                        "カードのタグチップの ♡ をクリックすると登録できます",
                                                                    ),
                                                            )
                                                            .into_any_element()
                                                    } else {
                                                        div()
                                                            .w(px(256.0))
                                                            .rounded_md()
                                                            .border_1()
                                                            .border_color(theme_border)
                                                            .bg(theme_popover)
                                                            .shadow_lg()
                                                            .p_3()
                                                            .flex()
                                                            .flex_row()
                                                            .flex_wrap()
                                                            .gap_1p5()
                                                            .children(favorite_tags.iter().map({
                                                                let handle = handle.clone();
                                                                let selected_tags =
                                                                    selected_tags.clone();
                                                                move |tag| {
                                                                    let is_selected =
                                                                        selected_tags.contains(tag);
                                                                    let handle = handle.clone();
                                                                    let tag_id = tag.clone();
                                                                    let tag_selector = format!(
                                                                        "tag-option-{tag_id}"
                                                                    );
                                                                    div()
                                                                                                                                    .id(SharedString::from(format!(
                                                                    "tag-option-{tag_id}"
                                                                                                                                    )))
                                                                                                                                    .debug_selector(move || {
                                                                    tag_selector.clone()
                                                                                                                                    })
                                                                                                                                    .px_3()
                                                                                                                                    .py_1()
                                                                                                                                    .rounded_full()
                                                                                                                                    .bg(if is_selected {
                                                                    theme_primary
                                                                                                                                    } else {
                                                                    theme_muted
                                                                                                                                    })
                                                                                                                                    .text_color(if is_selected {
                                                                    theme_primary_fg
                                                                                                                                    } else {
                                                                    theme_muted_fg
                                                                                                                                    })
                                                                                                                                    .text_xs()
                                                                                                                                    .font_weight(
                                                                    gpui_kit::FontWeight::MEDIUM,
                                                                                                                                    ).cursor_pointer().on_click(move |_, _window, cx| {
                                                                        handle.update(cx, |this, cx| {
                                                                            this.toggle_tag(
                                                                                cx, &tag_id,
                                                                            );
                                                                        });
                                                                    })
                                                                .child(tag.clone())
                                                                }
                                                            }))
                                                            .into_any_element()
                                                    }
                                                }
                                            }),
                                    ),
                            )
                            .child(
                                // Web と同じ: 検索ボックス左に Search アイコン。
                                // absolute で重ねると Input の背景に隠れて見えず、
                                // placeholder が 28px 右に寄って見えるため、
                                // Input の prefix（インフロー）でインプット内に配置する
                                Input::new(&search_state)
                                    .cursor_text()
                                    // placeholder「検索（タイトル・サークル・著者）」と打ち込み文字が
                                    // 切れない幅を確保する
                                    .w(px(320.0))
                                    .prefix(
                                        Icon::new(IconName::Search)
                                            .size(px(14.0))
                                            .text_color(cx.theme().muted_foreground),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            // タグ取得 ON/OFF（Web の tagFetchEnabled トグル、デフォルト OFF）
                            // 表示切替（Web の viewMode トグル: タイル/リスト）
                            .child(
                                Button::new("bookshelf-view-toggle").cursor_pointer()
                                    .icon(if view_mode == ViewMode::Card {
                                        AppIcon::List
                                    } else {
                                        AppIcon::LayoutGrid
                                    })
                                    .outline().cursor_pointer().on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        handle.update(cx, |this, cx| {
                                            this.toggle_view_mode(cx);
                                        });
                                    }
                                }),
                            )
                            .child(
                                if is_booth {
                                    div().hidden().into_any_element()
                                } else {
                                {
                                    let mut button =
                                        Button::new("bookshelf-tag-fetch").cursor_pointer().label(format!(
                                            "タグ取得{}",
                                            if tag_fetch_enabled { "ON" } else { "OFF" }
                                        ));
                                    if tag_fetch_enabled {
                                        button = button.primary();
                                    } else {
                                        button = button.outline();
                                    }
                                    button
                                }.cursor_pointer().on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        handle.update(cx, |this, cx| {
                                            this.toggle_tag_fetch(cx);
                                        });
                                    }
                                })
                                .into_any_element()
                            },
                            )
                            .child(
                                Button::new("bookshelf-sync").cursor_pointer()
                                    .icon(Icon::new(AppIcon::RefreshCw).size(px(14.0)))
                                    .label(if busy { "同期中" } else { "同期" })
                                    .loading(busy)
                                    .cursor_pointer().on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        handle.update(cx, |this, cx| this.sync_all(cx));
                                    }
                                }),
                            ),
                    ),
            )
            .child(
                div()
                    .id("bookshelf-grid")
                    .debug_selector(|| "bookshelf-grid".into())
                    .flex_1()
                    .min_h_0()
                    .child(match view_mode {
                        // タイル表示（Web の grid-cols-2 sm:3 lg:4 xl:5 相当）
                        ViewMode::Card => {
                            // Web と同じブレークポイントで列数を決め、
                            // サイドバー(255px)・パディング・gap を差し引いてカード幅を計算
                            let window_width = window.bounds().size.width.as_f32();
                            let columns = Self::columns_for_width(window_width);
                            let content_width = window_width - SIDEBAR_W - 24.0;
                            let card_width = ((content_width - (columns as f32 - 1.0) * 12.0)
                                / columns as f32)
                                .max(160.0);
                            // 仮想化: 行単位の List（可視行のみ描画）でスクロールを軽くする。
                            // 各行は同じカード幅・gap の横並び（行内左寄せは justify_start）。
                            let rows = self.filtered.len().div_ceil(columns);
                            if self.list_state.item_count() != rows {
                                self.list_state.reset(rows);
                            }
                            let handle = cx.entity();
                            let list_state = self.list_state.clone();
                            let theme = cx.theme().clone();
                            // グリッド全体は中央、行内のタイルは左寄せ
                            div()
                                .flex()
                                .justify_center()
                                .w_full()
                                .h_full()
                                .child(
                                     div()
                                         .w(px(content_width))
                                         .h_full()
                                         .child(gpui_kit::list(
                                             list_state,
                                            move |ix, window, cx| {
                                                // 借用を閉じるため可視行のカードと状態を先にコピーする
                                                // （可視行のみなので全カード構築より桁違いに軽い）
                                                let (cards, chips, editing_tags, editing_suggestions, editing_input) = {
                                                    let view = handle.read(cx);
                                                    let start = ix * columns;
                                                    let end =
                                                        (start + columns).min(view.filtered.len());
                                                    let cards = view.filtered[start..end]
                                                        .iter()
                                                        .enumerate()
                                                        .map(|(k, &i)| {
                                                            let card = view.shelf_cards[i].clone();
                                                            let ds = view
                                                                .download_states
                                                                .get(&card.shelf.database_id)
                                                                .copied();
                                                            let editing = view
                                                                .editing_book_id
                                                                .as_deref()
                                                                == Some(
                                                                    card.shelf
                                                                        .database_id
                                                                        .as_str(),
                                                                );
                                                            let selected =
                                                                view.selected_index
                                                                    == Some(start + k);
                                                            let tags_expanded = view
                                                                .expanded_tag_rows
                                                                .contains(
                                                                    &card.shelf.database_id,
                                                                );
                                                            (card, ds, editing, selected, tags_expanded)
                                                        })
                                                        .collect::<Vec<_>>();
                                                    (
                                                        cards,
                                                        view.chip_state(),
                                                        view.editing_tags.clone(),
                                                        view.editing_suggestions.clone(),
                                                        view.editing_input.clone(),
                                                    )
        };
                                                div()
                                                    .flex()
                                                    .flex_row()
                                                    .gap_3()
                                                    .pb_3()
                                                    .children(cards.iter().map(
                                                        |(card, ds, editing, selected, tags_expanded)| {
                                                            BookshelfView::render_card(
                                                                window,
                                                                &theme,
                                                                &handle,
                                                                card,
                                                                card_width,
                                                                *ds,
                                                                *editing,
                                                                &chips,
                                                                &card_tag_order,
                                                                *tags_expanded,
                                                                &editing_tags,
                                                                &editing_suggestions,
                                                                editing_input.as_ref(),
                                                                *selected,
                                                            )
                                                            .into_any_element()
                                                        },
                                                    ))
                                                    .into_any_element()
                                            },
                                        )
                                        .h_full()
                                        .w_full()),
                                )
                                .into_any_element()
                        }
                          ViewMode::List => div()
                              .id("bookshelf-list")
                              .debug_selector(|| "bookshelf-list".into())
                              .flex()
                              .flex_col()
                              // 高さを親（bookshelf-grid = flex_1 + min_h_0）に合わせて束縛する。
                              // 指定しないと内容高さまで伸び、overflow_y_scroll が効かずスクロールできない。
                              .h_full()
                              .min_h_0()
                              .rounded_lg()
                              .border_1()
                              .border_color(cx.theme().border)
                              .overflow_hidden()
                              .overflow_y_scroll()
                              .track_scroll(&self.scroll_handle)
                              .children(visible.iter().enumerate().map(|(idx, entry)| {
                                let selected = self.selected_index == Some(idx);
                                self.render_list_row(window, cx, entry, selected, &tag_order)
                                    .into_any_element()
                            }))
                            .into_any_element(),
                    }),
            )
            .child(if let Some(toast) = toast {
                div().text_sm().child(toast).into_any_element()
            } else {
                div().into_any_element()
            })
            .child(if let Some(error) = error {
                div()
                    .text_sm()
                    .text_color(gpui_kit::red())
                    .child(error)
                    .into_any_element()
            } else {
                div().into_any_element()
            })
            // 取り込み確認モーダル（§6.3: 曖昧な構造のときだけ）
            .child(
                if let Some((title, choices, selected)) = pending_import {
                    let handle = handle.clone();
                    let content_handle = handle.clone();
                    Dialog::new(cx)
                        .title(div().child("取り込み内容の確認"))
                        .content(move |content, _window, cx| {
                            let mut list = content.child(div().text_sm().child(format!(
                                "「{title}」には複数のコンテンツが含まれています。既定で表示するものを選んでください。"
                            )));
                            for (index, choice) in choices.iter().enumerate() {
                                let handle = content_handle.clone();
                                let is_selected = index == selected;
                                list = list.child(
                                    div()
                                        .id(SharedString::from(format!("import-choice-{index}")))
                                        .debug_selector(move || {
                                            format!("import-choice-{index}")
                                        })
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .gap_2()
                                        .py_1()
                                        .cursor_pointer()
                                        .on_click(move |_, _window, cx| {
                                            handle.update(cx, |this, cx| {
                                                this.select_pending_import(cx, index);
                                            });
                                        })
                                        .child(
                                            div()
                                                .flex()
                                                .items_center()
                                                .justify_center()
                                                .w(px(16.0))
                                                .h(px(16.0))
                                                .rounded_full()
                                                .border_1()
                                                .border_color(if is_selected {
                                                    cx.theme().primary
                                                } else {
                                                    cx.theme().border
                                                })
                                                .child(if is_selected {
                                                    div()
                                                        .w(px(8.0))
                                                        .h(px(8.0))
                                                        .rounded_full()
                                                        .bg(cx.theme().primary)
                                                } else {
                                                    div()
                                                }),
                                        )
                                        .child(
                                            div()
                                                .flex()
                                                .flex_col()
                                                .child(
                                                    div()
                                                        .text_sm()
                                                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                                                        .child(choice.display_name.clone()),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child(format!(
                                                            "{} ・ {}",
                                                            choice.kind, choice.detail
                                                        )),
                                                ),
                                        ),
                                );
                            }
                            list
                        })
                        .footer(
                            div()
                                .flex()
                                .flex_row()
                                .gap_2()
                                .child(
                                    Button::new("import-confirm-cancel")
                                        .cursor_pointer()
                                        .label("キャンセル")
                                        .cursor_pointer()
                                        .on_click({
                                            let handle = handle.clone();
                                            move |_, _window, cx| {
                                                handle.update(cx, |this, cx| {
                                                    this.cancel_pending_import(cx);
                                                });
                                            }
                                        }),
                                )
                                .child(
                                    Button::new("import-confirm-ok")
                                        .cursor_pointer()
                                        .primary()
                                        .label("この内容で取り込む")
                                        .cursor_pointer()
                                        .on_click({
                                            let handle = handle.clone();
                                            move |_, _window, cx| {
                                                handle.update(cx, |this, cx| {
                                                    this.confirm_pending_import(cx);
                                                });
                                            }
                                        }),
                                ),
                        )
                        .into_any_element()
                } else {
                    div().into_any_element()
                },
            )
    }
}

/// 保存 URL から実際に取得を試す URL の並びを返す（失敗したら次を試す）。
/// サイトごとの表紙画像の取得（URL 規則をここに集約。本棚と設定の両方から使う）。
///
/// - BOOTH: 商品ページの共有画像（オリジナル・高解像度）を優先し、保存 URL をフォールバック
/// - DLsite: 公開 CDN（`img.dlsite.jp`）をそのまま
/// - FANZA: `-200x150` を外した**原寸**を優先（実測: `pl-200x150` = 200x150 に対し
///   `pl` = 560x420）。失敗したら保存 URL に戻す
/// - 技術書典: 公開 URL を直接。失敗時のみセッション付きクライアントで再試行
pub(crate) fn cover_url_candidates(site_id: &str, stored_url: &str) -> Vec<String> {
    if site_id == "fanza" {
        let full = thundoku_core::fanza::sync::full_size_thumb(stored_url);
        if full != stored_url {
            return vec![full, stored_url.to_string()];
        }
    }
    vec![stored_url.to_string()]
}

/// リスト表示のサムネイル**表示エリア**の比率（幅 ÷ 高さ）。**全行で一定**（3:2）。
///
/// 実データの表紙比率は縦長 0.7 が最多（609 件中 310 件）、横長は FANZA の 1.3〜1.43。
/// エリアを 3:2 にすると:
/// - 横長（〜1.43）は**高さいっぱい**に収まる → 下に余白が出ない
/// - 縦長（0.7）は左右に余白ができる（要望どおり）
///
/// 画像は比率を保ってこのエリアに縮小して収める（切り抜きは起きない）。
/// エリアの大きさは**固定**（行の高さに追従させない）。追従させると行高 × 1.5 で
/// 表紙の幅が行ごとに変わり、右側のテキスト開始 X がずれる（ガタつきの原因）。
const LIST_COVER_ASPECT: f32 = 3.0 / 2.0;
/// 表紙エリアの高さ（固定）。行の高さはこの値 + 余白で決まる。
pub(crate) const LIST_COVER_MIN_H: f32 = 133.0;
/// 表紙エリアの幅（固定）。
pub(crate) const LIST_COVER_W: f32 = LIST_COVER_MIN_H * LIST_COVER_ASPECT;
/// 情報列の幅（固定）。表紙と同じく、行ごとに開始 X がずれないようにする。
/// タイトルの横に状態タグを並べるため、タイトルが折り返さない程度の幅を確保する。
pub(crate) const LIST_INFO_W: f32 = 320.0;
/// タグ情報エリアの幅（行の幅に対する割合）。タグはこの中で折り返す。
pub(crate) const LIST_TAGS_W_RATIO: f32 = 0.20;
/// サイドバーの幅。カード / リストの表示幅を窓幅から計算するときに差し引く。
pub(crate) const SIDEBAR_W: f32 = 255.0;
/// リストのタグ列で折り返してよい行数。表紙の高さ（`LIST_COVER_MIN_H` = 133px）に
/// 収まる数にする（チップ 1 行 ≈ 29px の実測から 4 行 = 116px）。
/// これ以上は行の高さがタグ列に支配され、固定サイズの表紙 / カルーセルとの間に
/// 余白ができる（＝行が間延びする）。表示する個数はこの行数と**タグ列の幅**から計算する
/// （`packed_tag_count`）。
pub(crate) const LIST_TAG_MAX_ROWS: usize = 4;
/// タグチップの装飾ぶんの幅（左右パディング 4+4・文字とハートの間隔 6・
/// ハートの丸ボタン `CHIP_HEART_BUTTON` 18・枠線 1+1）。
pub(crate) const CHIP_CHROME_W: f32 = 34.0;
/// 折りたたみトグル（「+n」）の装飾ぶんの幅（左右パディング 4+4・枠線 1+1）。
pub(crate) const TAG_TOGGLE_CHROME_W: f32 = 10.0;
/// タグチップ同士の間隔（`gap_1`）。
pub(crate) const TAG_CHIP_GAP: f32 = 4.0;
/// タグ編集ボタン（✎）の幅。
pub(crate) const TAG_EDIT_BUTTON_W: f32 = 24.0;
/// カード（グリッド）のタグを折りたたむときの最大表示数。
///
/// カードは行の高さを共有する（同じ行のカードは一番高いカードに揃う）ため、
/// タグをそのまま全部出すと 1 冊のタグ数で行全体が間延びする。幅 190px のカード
/// （`p_3` の内容幅 166px・「タグ01」= 2 文字 + 2 桁）での実測は
/// タグ無し = 296.5px、6 件 = チップ 3 行・+90.0px、20 件 = 10 行・+295.0px。
/// 長いタグ名（「長いタグ名前01」）だと 6 件 = 6 行・+149.0px まで伸びる。
/// ここを既定の上限にして残りは「+n」に畳む（お気に入りタグは並び替えで先頭に
/// 来るので折りたたまれない）。
pub(crate) const CARD_TAGS_COLLAPSED_MAX: usize = 6;
/// カルーセルに出す関連書籍の最大件数。
const LIST_RELATED_LIMIT: usize = 5;
/// カルーセルの 1 画面あたりの表示枚数（列幅をこの数で等分してサムネを埋める）。
/// 実測（1484px 幅・カルーセル列 598px）: 2 枚 = 196x130 = 表紙（200x133）とほぼ同じ大きさ。
/// 3 枚にすると 179x119 まで小さくなるため、表紙と同じ大きさを優先して 2 枚にしている。
const LIST_RELATED_PER_VIEW: f32 = 2.0;
/// カルーセルの前へ / 次へバーの幅（アイコン程度。高さは列＝サムネイルいっぱい）。
const LIST_CAROUSEL_BAR_W: f32 = 26.0;

/// 同一サークル / 同一作者の関連書籍のインデックスを、**サークル一致 → 作者一致**の順で
/// 最大 `limit` 件返す（自分自身は含めない。空のキーは一致とみなさない）。
///
/// 技術書典の作者は空なので、空キー同士を一致させると無関係な本が全部つながる。
/// `keys` は `(circle_name, author)` を並べたもの（`shelf_cards` と同じ順）。
fn related_book_indices(keys: &[(String, String)], index: usize, limit: usize) -> Vec<usize> {
    let Some((circle, author)) = keys.get(index) else {
        return Vec::new();
    };
    let mut related: Vec<usize> = Vec::new();
    if !circle.is_empty() {
        for (i, (other, _)) in keys.iter().enumerate() {
            if i != index && other == circle {
                related.push(i);
            }
        }
    }
    if !author.is_empty() {
        for (i, (_, other)) in keys.iter().enumerate() {
            if i != index && other == author && !related.contains(&i) {
                related.push(i);
            }
        }
    }
    related.truncate(limit);
    related
}

/// 表紙エリア（3:2 の枠）の中に、画像を**比率のまま**収めた要素（切り抜きしない）。
/// リストの表紙と関連書籍サムネイルで同じ見た目・同じ比率にするための共通処理。
/// `selector` は画像要素のデバッグ用 id（テストが枠との比率を検証する）。
pub(crate) fn cover_fit_inside_frame(
    cover: Option<&Arc<RenderImage>>,
    selector: String,
) -> gpui_kit::AnyElement {
    let image_aspect = match cover {
        Some(render) => {
            let size = render.size(0);
            size.width.0.max(1) as f32 / size.height.0.max(1) as f32
        }
        None => LIST_COVER_ASPECT,
    };
    // 縦長（枠より縦長）は高さいっぱい、横長は幅いっぱいに合わせる
    let (img_w, img_h) = if image_aspect <= LIST_COVER_ASPECT {
        (relative(image_aspect / LIST_COVER_ASPECT), relative(1.0))
    } else {
        (relative(1.0), relative(LIST_COVER_ASPECT / image_aspect))
    };
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .w(img_w)
                .h(img_h)
                .debug_selector(move || selector.clone())
                .child(match cover {
                    Some(render) => img(render.clone())
                        .w_full()
                        .h_full()
                        .object_fit(gpui_kit::ObjectFit::Fill)
                        .into_any_element(),
                    None => div().into_any_element(),
                }),
        )
        .into_any_element()
}

/// 状態アイコン（ダウンロード済み / お気に入り）の一辺。
/// 文字だと長くて変な折り返しになるためアイコンで出す（チップのハート 12px より大きめ）。
const LIST_STATUS_ICON: f32 = 16.0;

/// 状態アイコンのクリックハンドラ（お気に入りのトグル）。
type StatusIconClick = Box<dyn Fn(&mut Window, &mut App) + 'static>;

/// 状態アイコン 1 つ。意味は tooltip で補う（アイコンだけでは伝わらないため）。
/// `on_click` を渡すと押せるようになる（お気に入りのトグル）。
fn status_icon(
    database_id: &str,
    kind: &str,
    icon: AppIcon,
    color: Hsla,
    tooltip: &str,
    on_click: Option<StatusIconClick>,
) -> gpui_kit::AnyElement {
    let selector = format!("list-status-{kind}-{database_id}");
    let element_id = format!("list-status-icon-{kind}-{database_id}");
    let tooltip = tooltip.to_string();
    let clickable = on_click.is_some();
    div()
        .id(SharedString::from(element_id))
        .debug_selector(move || selector.clone())
        .flex()
        .items_center()
        .when(clickable, |this| this.cursor_pointer())
        .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
        .when_some(on_click, |this, click| {
            this.on_click(move |_, window, cx| click(window, cx))
        })
        .child(
            div()
                .text_color(color)
                .child(Icon::new(icon).size(px(LIST_STATUS_ICON))),
        )
        .into_any_element()
}

/// 状態タグ 1 つ（四角い `Tag`）。`Tag` には `debug_selector` を付けられないため、
/// div で包んで付ける（テストが位置を検証する）。既定の `Tag` は角丸なので 0 にする。
fn status_tag(
    database_id: &str,
    kind: &str,
    label: &str,
    variant: TagVariant,
) -> gpui_kit::AnyElement {
    let selector = format!("list-status-{kind}-{database_id}");
    div()
        .debug_selector(move || selector.clone())
        .child(
            Tag::new()
                .with_variant(variant)
                .rounded(px(0.0))
                .with_size(Size::XSmall)
                .child(label.to_string()),
        )
        .into_any_element()
}

/// カルーセルの 1 件を描くための、`shelf_cards` から切り出した情報
/// （`render_list_row` は `&self` なので、借用を跨がずに描けるようにする）。
struct RelatedThumb {
    /// 関連リスト内の位置（要素 id とカルーセルの index に使う）
    position: usize,
    title: String,
    /// ローカル本の id（あれば開く。無ければダウンロード）
    book_id: Option<String>,
    item: bookshelf::BookshelfItem,
    cover: Option<Arc<RenderImage>>,
}

/// インポート直後に**サイト側のメタ**（タイトル / 作者名 / サークル名 / 購入日）を
/// `books` と `bookshelf_items` に反映する。
///
/// PDF とそれ以外の**両方の取り込み経路**から呼ぶ。PDF 経路にこの処理が無く、
/// BOOTH / FANZA / DLsite の PDF で作者名が入らない不具合があった。
/// 対象サイト以外（技術書典など）は何もしない。
fn apply_site_metadata(
    db: &db::SqlitePool,
    site_id: &str,
    product_id: &str,
    book_id: &str,
    item: &bookshelf::BookshelfItem,
    site_author: Option<&str>,
) {
    if !matches!(site_id, "booth" | "fanza" | "dlsite") {
        return;
    }
    // 作者名が取れなかったときは既存の author を残す（空文字で消さない）
    let author = match site_author {
        Some(author) => author.to_string(),
        None => books::get(db, book_id)
            .ok()
            .flatten()
            .map(|book| book.author)
            .unwrap_or_default(),
    };
    let _ = books::set_metadata(
        db,
        book_id,
        &item.title,
        &author,
        &item.circle_name,
        item.caused_at.clone(),
    );
    // カードの「作者:」絞り込み用に本棚アイテムへも書く
    if let Some(author) = site_author {
        let _ = bookshelf::update_author(db, site_id, product_id, author);
    }
}

/// ページ数表示用の進捗行を、**まだ無いときだけ**作る。
///
/// 再取得（同じ `book_id`・同じ `content_id` を再利用する取り込み）で読書位置を
/// 消さないため、既存行は上書きしない（`progress::upsert` は `current_page` を上書きする）。
fn seed_progress_if_absent(db: &db::SqlitePool, book_id: &str, total_pages: i64) {
    if total_pages <= 0 {
        return;
    }
    let content_id = db::contents::primary_for_book(db, book_id)
        .ok()
        .flatten()
        .map(|content| content.content_id)
        .unwrap_or_default();
    if progress::get_for(db, book_id, &content_id)
        .ok()
        .flatten()
        .is_some()
    {
        return;
    }
    let _ = progress::upsert(
        db,
        &progress::ReadingProgress {
            book_id: book_id.to_string(),
            content_id,
            current_page: 0,
            total_pages: Some(total_pages),
            finished_at: None,
            last_read_at: "2026-01-01 00:00:00".to_string(),
            scroll_position: 0.0,
        },
    );
}

/// 表紙画像を枠（`box_w` × `box_h`）に比率を保って収めた描画サイズを返す。
/// 横長は幅いっぱい（高さは比率なり）、縦長は高さいっぱい（幅は比率なり）になる。
/// 枠に合わせて拡大すると縦長の上下が切れるため、カード / リスト共通で使う。
pub(crate) fn fit_cover_size(image_w: f32, image_h: f32, box_w: f32, box_h: f32) -> (f32, f32) {
    let image_w = image_w.max(1.0);
    let image_h = image_h.max(1.0);
    let scale = (box_w / image_w).min(box_h / image_h);
    ((image_w * scale).max(1.0), (image_h * scale).max(1.0))
}

pub(crate) fn fetch_cover_bytes(
    agent: &ureq::Agent,
    site_id: &str,
    database_id: &str,
    stored_url: &str,
    booth_session: Option<&thundoku_core::booth::BoothSession>,
    tbf_client: &parking_lot::Mutex<thundoku_core::tbf::TbfClient>,
) -> Option<Vec<u8>> {
    // プロトコル相対（`//img.dlsite.jp/...`）とサイト相対を絶対化する
    let resolved = if stored_url.starts_with("//") {
        format!("https:{stored_url}")
    } else if stored_url.starts_with('/') {
        format!("https://techbookfest.org{stored_url}")
    } else {
        stored_url.to_string()
    };
    if site_id == "booth" {
        // BOOTH の表紙は公開画像（booth.pximg.net — Cookie 不要）
        let primary = booth_session.and_then(|session| {
            let item_id: u64 = database_id.parse().ok()?;
            let detail = BoothClient::new(session).item_detail(item_id).ok()?;
            detail.images.into_iter().next()
        });
        match primary {
            Some(image_url) => {
                fetch_bytes(agent, &image_url).or_else(|| fetch_bytes(agent, &resolved))
            }
            None => fetch_bytes(agent, &resolved),
        }
    } else if site_id == "dlsite" {
        // TBF クライアントにはフォールバックしない（誤った経路で失敗するため）
        fetch_bytes(agent, &resolved)
    } else if site_id == "fanza" {
        // 原寸 → 縮小の順に試す（原寸が無い作品があるためフォールバックする）
        let mut bytes = None;
        for candidate in cover_url_candidates(site_id, &resolved) {
            bytes = fetch_bytes(agent, &candidate);
            if bytes.is_some() {
                break;
            }
            log::warn!("表紙 URL で取得できず次を試す: {candidate}");
        }
        bytes
    } else {
        // TBF の表紙も公開 URL なら直接取得する（4 並列が機能する）。
        // 失敗した場合のみセッション付きクライアントにフォールバック。
        match fetch_bytes(agent, &resolved) {
            Some(bytes) => Some(bytes),
            None => {
                log::warn!("TBF 表紙を直接取得できずフォールバック: {resolved}");
                tbf_client.lock().download(&resolved).ok()
            }
        }
    }
}

/// 取得した表紙画像を 448px の PNG キャッシュとして保存する（本棚と設定で共通）。
/// カードのヘッダーは最大 ~320px 幅なので、粗くならないよう 448px で持つ。
pub(crate) fn write_cover_cache(
    thumbnails_dir: &std::path::Path,
    site_id: &str,
    database_id: &str,
    bytes: &[u8],
) {
    if let Some(cached) = resize_for_cache(bytes, 448) {
        let path = cover_cache_path(thumbnails_dir, site_id, database_id);
        let _ = std::fs::write(&path, &cached);
        remove_legacy_cover_cache(thumbnails_dir, site_id, database_id);
    }
}

/// タイムアウト付きで画像をダウンロードする（ハング防止）。
fn fetch_bytes(agent: &ureq::Agent, url: &str) -> Option<Vec<u8>> {
    use std::io::Read;
    match agent
        .get(url)
        .set(
            "User-Agent",
            "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36",
        )
        .call()
    {
        Ok(response) => {
            let mut buf = Vec::new();
            match response.into_reader().read_to_end(&mut buf) {
                Ok(_) => Some(buf),
                Err(e) => {
                    log::warn!("fetch_bytes 読み込み失敗: {url}: {e}");
                    None
                }
            }
        }
        Err(e) => {
            log::warn!("fetch_bytes リクエスト失敗: {url}: {e}");
            None
        }
    }
}

/// 画像を縮小して PNG バイト列として保存用に変換する（キャッシュ用）。
fn resize_for_cache(data: &[u8], max_width: u32) -> Option<Vec<u8>> {
    let img = image::load_from_memory(data).ok()?;
    let (w, h) = (img.width(), img.height());
    let resized = if w > max_width {
        let scale = max_width as f32 / w as f32;
        let nw = (w as f32 * scale).max(1.0) as u32;
        let nh = (h as f32 * scale).max(1.0) as u32;
        img.resize(nw, nh, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    let mut out = std::io::Cursor::new(Vec::new());
    resized.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// 画像を縮小（最大幅 max_width px）して BGRA の RenderImage に変換する。
/// 元画像のアスペクト比を保つ（クロップしない）。表示側（カード / リスト）で
/// `fit_cover_size` により枠内に比率のまま収める（FANZA 等はサムネの比率がバラバラのため）。
fn decode_and_resize(data: &[u8], max_width: u32) -> Option<Arc<RenderImage>> {
    let decoded = image::load_from_memory(data).ok()?;
    let (w, h) = (decoded.width(), decoded.height());
    let resized = if w > max_width {
        let scale = max_width as f32 / w as f32;
        let nw = (w as f32 * scale).max(1.0) as u32;
        let nh = (h as f32 * scale).max(1.0) as u32;
        decoded.resize(nw, nh, image::imageops::FilterType::Lanczos3)
    } else {
        decoded
    };
    let mut rgba = resized.into_rgba8();
    // RenderImage は BGRA を期待するため R/B を入れ替える
    for pixel in rgba.pixels_mut() {
        pixel.0.swap(0, 2);
    }
    let frame = image::Frame::new(rgba);
    Some(Arc::new(RenderImage::new([frame])))
}

fn decode_bytes_to_render_image(data: &[u8]) -> Option<Arc<RenderImage>> {
    // カード枠比（0.75）へのクロップを全経路（キャッシュ・ローカル本）に適用する
    decode_and_resize(data, 448)
}

/// 表紙キャッシュのパス。**解像度をファイル名に埋め込む**ことで、縮小サイズを
/// 変えたときに古い低解像度キャッシュを自動的に無効化する（`_448` = 最大 448px 幅）。
pub(crate) fn cover_cache_path(
    thumbnails_dir: &std::path::Path,
    site_id: &str,
    database_id: &str,
) -> std::path::PathBuf {
    thumbnails_dir.join(format!("{site_id}_{database_id}_448.png"))
}

/// 旧解像度のキャッシュ（`{site}_{db}.png`）を消す。パス変更前の残骸で、
/// 二度と読まれないファイルがディスクに残るのを防ぐ。
pub(crate) fn remove_legacy_cover_cache(
    thumbnails_dir: &std::path::Path,
    site_id: &str,
    database_id: &str,
) {
    let _ = std::fs::remove_file(thumbnails_dir.join(format!("{site_id}_{database_id}.png")));
}

/// `thumbnails/{site_id}_{database_id}_448.png` キャッシュ -> RenderImage。
pub(crate) fn load_cached_cover(
    thumbnails_dir: &std::path::Path,
    shelf: &bookshelf::BookshelfItem,
) -> Option<Arc<RenderImage>> {
    let path = cover_cache_path(thumbnails_dir, &shelf.site_id, &shelf.database_id);
    let data = std::fs::read(&path).ok()?;
    decode_bytes_to_render_image(&data)
}

/// Web-style placeholder SVG (`data:image/svg+xml;utf8,...`) rasterized to a
/// RenderImage. Color is derived from a hash of the title.
/// アプリロゴ（assets/app-icon/icon_1024.png）をデコードした RenderImage。
/// 説明画面のロゴ表示（64px）で使う（プロセス内で 1 回だけデコード）。
/// 1024px のまま渡すと GPUI の縮小補間（二重線形）で文字が滲むため、
/// 表示サイズの 2 倍（Retina 対応）へ Lanczos3 で事前縮小して返す。
pub fn app_logo_image() -> Option<Arc<RenderImage>> {
    static LOGO: LazyLock<Option<Arc<RenderImage>>> = LazyLock::new(|| {
        let data = include_bytes!("../../assets/app-icon/icon_1024.png");
        let decoded = image::load_from_memory(data).ok()?;
        // 表示 64px の 2 倍で十分（16 倍縮小のエイリアシングを避ける）
        let resized = decoded.resize(128, 128, image::imageops::FilterType::Lanczos3);
        let mut rgba = resized.to_rgba8();
        // GPUI は BGRA を期待する
        for pixel in rgba.chunks_exact_mut(4) {
            pixel.swap(0, 2);
        }
        Some(Arc::new(gpui_kit::RenderImage::new([image::Frame::new(
            rgba,
        )])))
    });
    LOGO.clone()
}

/// 表紙画像が取得できなかったカード用の NoImage ダミー（グレー背景 + NoImage 表記）。
pub(crate) fn no_image_cover() -> Option<Arc<RenderImage>> {
    let svg = "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 240 320' width='240' height='320'><rect width='240' height='320' fill='#e5e7eb'/><rect x='60' y='90' width='120' height='90' fill='none' stroke='#9ca3af' stroke-width='6'/><path d='M70 165 L100 135 L125 155 L150 130 L170 165 Z' fill='#9ca3af'/><text x='120' y='200' text-anchor='middle' font-family='sans-serif' font-size='16' font-weight='bold' fill='#6b7280'>NoImage</text></svg>";
    decode_bytes_to_render_image(svg.as_bytes())
}

pub(crate) fn placeholder_cover(title: &str, circle: &str) -> Option<Arc<RenderImage>> {
    let palette = [
        "#6366f1", "#ec4899", "#14b8a6", "#f59e0b", "#8b5cf6", "#06b6d4", "#ef4444", "#22c55e",
    ];
    let hash: u32 = title
        .chars()
        .fold(0u32, |acc, c| acc.wrapping_mul(31).wrapping_add(c as u32));
    let color = palette[(hash % palette.len() as u32) as usize];
    let display_title: String = title.chars().take(12).collect();
    let display_circle: String = circle.chars().take(10).collect();
    let svg = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 240 320' width='240' height='320'>         <rect width='240' height='320' fill='{color}'/>         <text x='120' y='140' text-anchor='middle' font-family='sans-serif' font-size='20' font-weight='bold' fill='white'>{}</text>         <text x='120' y='180' text-anchor='middle' font-family='sans-serif' font-size='14' fill='rgba(255,255,255,0.8)'>{}</text>         </svg>",
        display_title
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;"),
        display_circle
            .replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;"),
    );
    decode_bytes_to_render_image(svg.as_bytes())
}

/// Web の `CircularProgress` と同じ円形リング（SVG）を RGBA に rasterize する。
/// パーセント単位でキャッシュし、進捗更新ごとの再 rasterize を防ぐ。
fn progress_ring_image(fraction: f32) -> Arc<RenderImage> {
    let percentage = (fraction.clamp(0.0, 1.0) * 100.0).round() as u32;
    static CACHE: LazyLock<std::sync::Mutex<HashMap<u32, Arc<RenderImage>>>> =
        LazyLock::new(|| std::sync::Mutex::new(HashMap::new()));
    if let Some(image) = CACHE.lock().unwrap().get(&percentage) {
        return image.clone();
    }
    let size = 72u32;
    let stroke = 5u32;
    let radius = (size - stroke) as f64 / 2.0;
    let circumference = 2.0 * std::f64::consts::PI * radius;
    let offset = circumference * (1.0 - percentage as f64 / 100.0);
    let svg = format!(
        "<svg xmlns='http://www.w3.org/2000/svg' width='{size}' height='{size}' viewBox='0 0 {size} {size}'>\
         <circle cx='36' cy='36' r='{radius}' fill='none' stroke='rgba(255,255,255,0.2)' stroke-width='{stroke}'/>\
         <circle cx='36' cy='36' r='{radius}' fill='none' stroke='#34d399' stroke-width='{stroke}' stroke-linecap='round' \
         stroke-dasharray='{circumference}' stroke-dashoffset='{offset}' transform='rotate(-90 36 36)'/>\
         </svg>"
    );
    let opt = usvg::Options::default();
    let tree = usvg::Tree::from_str(&svg, &opt).expect("valid progress svg");
    let mut pixmap = tiny_skia::Pixmap::new(size, size).expect("progress pixmap");
    resvg::render(&tree, tiny_skia::Transform::default(), &mut pixmap.as_mut());
    let mut data = pixmap.data().to_vec();
    // GPUI は BGRA を期待（R/B を入れ替えないと色が入れ替わる）
    for pixel in data.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    let frame =
        image::Frame::new(image::RgbaImage::from_raw(size, size, data).expect("progress rgba"));
    let image = Arc::new(RenderImage::new([frame]));
    CACHE.lock().unwrap().insert(percentage, image.clone());
    image
}

/// "技術書典20"-style compact label (mirrors the Web `formatEventLabel`:
/// `^(?:TechBookFest|技術書典)\s*(\d+)$` case-insensitive).
pub(crate) fn format_event_label(event_name: &str) -> String {
    let trimmed = event_name.trim();
    let lower = trimmed.to_lowercase();
    for prefix in ["techbookfest", "技術書典"] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let digits = rest.trim_start();
            if !digits.is_empty() && digits.chars().all(|c| c.is_ascii_digit()) {
                return format!("技術書典{digits}");
            }
        }
    }
    trimmed.to_string()
}

pub(crate) fn load_cover_image(
    packs_dir: &std::path::Path,
    book: &books::Book,
) -> Option<Arc<RenderImage>> {
    let pack_id = book.pack_id.as_deref().unwrap_or(&book.id);
    let path = packs_dir.join(format!("{pack_id}.opfspack"));
    let bytes = std::fs::read(path).ok()?;
    let reader = opfspack::PackReader::open(&bytes).ok()?;
    for entry in ["thumbnail.webp", "cover.webp"] {
        if let Ok(data) = reader.read_entry(entry, None)
            && let Ok(decoded) = image::load_from_memory(&data)
        {
            let mut rgba = decoded.to_rgba8();
            // GPUI は BGRA を期待（R/B を入れ替えないと赤と青が入れ替わる）
            for pixel in rgba.chunks_exact_mut(4) {
                pixel.swap(0, 2);
            }
            let frame = image::Frame::new(rgba);
            return Some(Arc::new(RenderImage::new([frame])));
        }
    }
    None
}

fn item_file_name(title: &str, item: &bookshelf::BookshelfItem) -> String {
    match &item.file_name {
        Some(name) if !name.is_empty() => name.clone(),
        _ => format!("{title}.pdf"),
    }
}

/// 購入日を `YYYY/MM/DD` に正規化する（"2026年09月03日" / "2026-08-25 00:00:00" 対応）。
pub(crate) fn format_purchase_date(raw: &str) -> String {
    let s = raw.trim();
    if s.contains('年') {
        let y = s.split('年').next().unwrap_or("");
        let m = s
            .split('年')
            .nth(1)
            .and_then(|p| p.split('月').next())
            .unwrap_or("");
        let d = s
            .split('月')
            .nth(1)
            .and_then(|p| p.split('日').next())
            .unwrap_or("");
        let m = m.trim();
        let d = d.trim();
        if !y.is_empty() && !m.is_empty() && !d.is_empty() {
            return format!("{y}/{m:0>2}/{d:0>2}");
        }
    }
    let date_part = s.split_whitespace().next().unwrap_or(s);
    date_part.replace('-', "/")
}

/// ダウンロードしたバイト列のマジックナンバーから拡張子を推定する。
/// FANZA はファイル名由来の拡張子（既定 `.pdf`）が実際と異なることがあるため使う
/// （画像セット ZIP が PDF として誤レンダリングされるのを防ぐ）。
fn sniff_extension(bytes: &[u8]) -> Option<&'static str> {
    if bytes.starts_with(b"PK\x03\x04") {
        return Some("zip");
    }
    if bytes.starts_with(b"%PDF") {
        return Some("pdf");
    }
    if bytes.starts_with(&[0xFF, 0xD8]) {
        return Some("jpg");
    }
    if bytes.starts_with(b"\x89PNG") {
        return Some("png");
    }
    if bytes.starts_with(b"GIF8") {
        return Some("gif");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("webp");
    }
    None
}

#[cfg(test)]
mod tests {

    use super::{
        ImportChoice, ImportFailure, ImportOutcome, PendingImport, cover_url_candidates,
        download_messages, import_choices, import_needs_confirmation,
    };
    use gpui_kit::AppContext as _;
    use gpui_kit::TestAppContext;
    use thundoku_core::import::{ImportPlan, MediaKind, PlannedContent, PlannedRendition};

    use thundoku_core::db::{books, progress};

    use super::*;

    /// サイトを指定して本棚アイテムを seed する（`seed_shelf_item` は技術書典固定のため）。
    fn seed_shelf_item_for_site(
        cx: &mut TestAppContext,
        site_id: &str,
        database_id: &str,
        title: &str,
        circle: &str,
    ) {
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            bookshelf::upsert(
                db,
                &bookshelf::BookshelfItem {
                    site_id: site_id.into(),
                    database_id: database_id.into(),
                    title: title.into(),
                    circle_name: circle.into(),
                    author: String::new(),
                    thumbnail_url: None,
                    format: "PDF".into(),
                    caused_at: None,
                    event_name: None,
                    event_slug: None,
                    event_id: None,
                    file_name: None,
                    download_url: None,
                    is_downloadable: 1,
                    is_checked: 0,
                    is_purchased: 1,
                    is_new: 0,
                    is_active: 1,
                    is_favorite: 0,
                    is_hidden: 0,
                    hidden_at: None,
                    tags_json: None,
                    synced_at: "2026-08-21 00:00:00".into(),
                    created_at: "2026-08-21 00:00:00".into(),
                    updated_at: "2026-08-21 00:00:00".into(),
                    media_category: None,
                    ai_type: None,
                    is_drm: 0,
                    release_date: None,
                    description: None,
                    theme: None,
                    maker_id: None,
                    page_count: None,
                    age_rating: None,
                    series_name: None,
                },
            )
            .unwrap();
        });
    }

    /// サイト側メタ（作者名）の反映は PDF / それ以外の両経路で同じ関数を通る。
    /// 以前は PDF 経路に処理が無く、BOOTH / FANZA / DLsite の PDF で作者が入らなかった。
    #[gpui_kit::test]
    async fn apply_site_metadata_writes_author_for_supported_sites(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_shelf_item_for_site(cx, "booth", "7825209", "本1", "YORIMIYA STUDIO");
        seed_book(cx, "book-1", "本1", "YORIMIYA STUDIO");
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            let item = bookshelf::list(db, "booth").unwrap().remove(0);
            // 作者名が取れたとき: books にも本棚アイテムにも入る
            apply_site_metadata(db, "booth", "7825209", "book-1", &item, Some("YORIMIYA"));
            assert_eq!(
                books::get(db, "book-1").unwrap().unwrap().author,
                "YORIMIYA",
                "books.author に作者名が入ること（PDF 経路の不具合の回帰）"
            );
            assert_eq!(
                bookshelf::list(db, "booth").unwrap()[0].author,
                "YORIMIYA",
                "本棚カードの author も更新されること"
            );
            // 作者名が取れなかったとき: 既存の author を消さない
            apply_site_metadata(db, "booth", "7825209", "book-1", &item, None);
            assert_eq!(
                books::get(db, "book-1").unwrap().unwrap().author,
                "YORIMIYA",
                "作者が取れなくても既存を消さない"
            );
            // 対象外サイト（技術書典）は何もしない
            apply_site_metadata(db, "techbookfest", "db-1", "book-1", &item, Some("X"));
            assert_eq!(
                books::get(db, "book-1").unwrap().unwrap().author,
                "YORIMIYA"
            );
        });
    }

    /// 再取得は**カードの表紙を消さない**（消すと `matches_filter` が
    /// 「ローカル無し + 表紙無し + thumbnail_url あり」でカードを隠し、カードが消える）。
    /// また表紙取得中は `download_item` が無視するため、完了後に実行するようキューに積む。
    #[gpui_kit::test]
    async fn redownload_keeps_cover_and_queues_while_busy(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        let view = cx.new(BookshelfView::new);
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                // 表紙を持たせ、表紙取得中（busy）の状態にする
                this.shelf_cards[0].cover = Some(test_cover_image(400, 600));
                this.fetching_covers = true;
                let card = this.shelf_cards[0].clone();
                this.redownload_item(cx, &card);
                assert!(
                    this.shelf_cards[0].cover.is_some(),
                    "再取得でカードの表紙を消さない（消すとカードが隠れる）"
                );
                assert!(
                    this.pending_download.is_some(),
                    "表紙取得中は開始できないのでキューに積む"
                );
            });
        });
    }

    /// 再取得はローカルの本を削除しない（同じ `book_id` を再利用して、タグ・進捗・
    /// コンテンツの `content_id`・カスタム名を維持する）。
    #[gpui_kit::test]
    async fn redownload_keeps_local_book_row(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "book-1", "本1", "サークルA");
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            // ローカル本と本棚アイテムを紐づける（カードがローカルとして認識される）
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = 'db-1' WHERE id = 'book-1'")
                    .execute(db)
                    .await
            })
            .unwrap();
            db::tags::set_for_book(db, "book-1", &[("タグA", "manual")]).unwrap();
        });
        let view = cx.new(BookshelfView::new);
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                let card = this.shelf_cards[0].clone();
                assert!(card.local.is_some(), "ローカル本として認識されること");
                // 表紙取得中にしておく（ダウンロードは開始せずキューに積む）
                this.fetching_covers = true;
                this.redownload_item(cx, &card);
            });
        });
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            assert!(
                books::get(db, "book-1").unwrap().is_some(),
                "再取得でローカルの本を削除しない"
            );
            assert!(
                !db::tags::list_for_book(db, "book-1").unwrap().is_empty(),
                "タグも残ること"
            );
        });
    }

    /// 進捗行は**無いときだけ**作る（再取得で読書位置を消さない）。
    #[gpui_kit::test]
    async fn seed_progress_if_absent_keeps_reading_position(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "book-1", "本1", "サークルA");
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            // 未作成なら作られる
            seed_progress_if_absent(db, "book-1", 16);
            let seeded = progress::get_for(db, "book-1", "").unwrap().unwrap();
            assert_eq!(seeded.current_page, 0);
            assert_eq!(seeded.total_pages, Some(16));
            // 読書位置を進める
            let advanced = progress::ReadingProgress {
                current_page: 7,
                last_read_at: "2026-09-12 00:00:00".to_string(),
                ..seeded
            };
            progress::upsert(db, &advanced).unwrap();
            // 再取得相当（もう一度 seed）でも読書位置は残る
            seed_progress_if_absent(db, "book-1", 16);
            assert_eq!(
                progress::get_for(db, "book-1", "")
                    .unwrap()
                    .unwrap()
                    .current_page,
                7,
                "再取得で読書位置を消さない"
            );
        });
    }

    #[test]
    fn sniff_extension_detects_zip_and_pdf() {
        assert_eq!(sniff_extension(b"PK\x03\x04zipdata"), Some("zip"));
        assert_eq!(sniff_extension(b"%PDF-1.7"), Some("pdf"));
        assert_eq!(sniff_extension(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("jpg"));
        assert_eq!(sniff_extension(b"\x89PNG\r\n\x1a\n"), Some("png"));
        assert_eq!(sniff_extension(b"RIFFxxxxWEBP"), Some("webp"));
        assert_eq!(sniff_extension(b"not a file"), None);
    }

    fn seed_book(cx: &mut TestAppContext, id: &str, title: &str, circle: &str) {
        cx.update(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            books::insert(
                db,
                &books::Book {
                    id: id.into(),
                    title: title.into(),
                    author: String::new(),
                    circle_name: circle.into(),
                    purchase_date: None,
                    file_name: format!("{id}.pdf"),
                    file_size: 10,
                    opfs_path: format!("{id}.opfspack"),
                    cover_thumbnail: None,
                    tbf_product_id: None,
                    site_id: None,
                    tags_fetched: 1,
                    pack_id: Some(id.into()),
                    is_favorite: 0,
                    is_hidden: 0,
                    created_at: "2026-08-21 00:00:00".into(),
                    updated_at: "2026-08-21 00:00:00".into(),
                    media_category: None,
                    ai_type: None,
                    is_drm: 0,
                    release_date: None,
                    description: None,
                    theme: None,
                    maker_id: None,
                    page_count: None,
                    age_rating: None,
                    series_name: None,
                },
            )
            .unwrap();
        });
    }

    fn seed_shelf_item(
        cx: &mut TestAppContext,
        database_id: &str,
        title: &str,
        circle: &str,
        thumbnail_url: Option<&str>,
    ) {
        cx.update(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            bookshelf::upsert(
                db,
                &bookshelf::BookshelfItem {
                    site_id: "techbookfest".into(),
                    database_id: database_id.into(),
                    title: title.into(),
                    circle_name: circle.into(),
                    author: String::new(),
                    thumbnail_url: thumbnail_url.map(String::from),
                    format: "PDF".into(),
                    caused_at: None,
                    event_name: None,
                    event_slug: None,
                    event_id: None,
                    file_name: None,
                    download_url: None,
                    is_downloadable: 1,
                    is_checked: 0,
                    is_purchased: 1,
                    is_new: 0,
                    is_active: 1,
                    is_favorite: 0,
                    is_hidden: 0,
                    hidden_at: None,
                    tags_json: None,
                    synced_at: "2026-08-21 00:00:00".into(),
                    created_at: "2026-08-21 00:00:00".into(),
                    updated_at: "2026-08-21 00:00:00".into(),
                    media_category: None,
                    ai_type: None,
                    is_drm: 0,
                    release_date: None,
                    description: None,
                    theme: None,
                    maker_id: None,
                    page_count: None,
                    age_rating: None,
                    series_name: None,
                },
            )
            .unwrap();
        });
    }

    /// ローカル本と本棚アイテムを紐付ける。紐付けが無いとカードは `book_tags` ではなく
    /// `bookshelf_items.tags_json` を表示するため、タグのテストでは必要。
    fn link_book_to_shelf(cx: &mut TestAppContext, book_id: &str, database_id: &str) {
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = ?1 WHERE id = ?2")
                    .bind(database_id)
                    .bind(book_id)
                    .execute(db)
                    .await
            })
            .unwrap();
        });
    }

    /// 作者名つきの本棚アイテムを seed する（作者絞り込みテスト用）。
    fn seed_shelf_item_with_author(
        cx: &mut TestAppContext,
        database_id: &str,
        title: &str,
        circle: &str,
        author: &str,
    ) {
        cx.update(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            bookshelf::upsert(
                db,
                &bookshelf::BookshelfItem {
                    site_id: "booth".into(),
                    database_id: database_id.into(),
                    title: title.into(),
                    circle_name: circle.into(),
                    author: author.into(),
                    thumbnail_url: None,
                    format: "PDF".into(),
                    caused_at: None,
                    event_name: None,
                    event_slug: None,
                    event_id: None,
                    file_name: None,
                    download_url: None,
                    is_downloadable: 1,
                    is_checked: 0,
                    is_purchased: 1,
                    is_new: 0,
                    is_active: 1,
                    is_favorite: 0,
                    is_hidden: 0,
                    hidden_at: None,
                    tags_json: None,
                    synced_at: "2026-08-21 00:00:00".into(),
                    created_at: "2026-08-21 00:00:00".into(),
                    updated_at: "2026-08-21 00:00:00".into(),
                    media_category: None,
                    ai_type: None,
                    is_drm: 0,
                    release_date: None,
                    description: None,
                    theme: None,
                    maker_id: None,
                    page_count: None,
                    age_rating: None,
                    series_name: None,
                },
            )
            .unwrap();
        });
    }

    fn seed_shelf_item_with_event(
        cx: &mut TestAppContext,
        database_id: &str,
        title: &str,
        circle: &str,
        event_name: &str,
    ) {
        cx.update(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            bookshelf::upsert(
                db,
                &bookshelf::BookshelfItem {
                    site_id: "techbookfest".into(),
                    database_id: database_id.into(),
                    title: title.into(),
                    circle_name: circle.into(),
                    author: String::new(),
                    thumbnail_url: None,
                    format: "PDF".into(),
                    caused_at: None,
                    event_name: Some(event_name.into()),
                    event_slug: None,
                    event_id: None,
                    file_name: None,
                    download_url: None,
                    is_downloadable: 1,
                    is_checked: 0,
                    is_purchased: 1,
                    is_new: 0,
                    is_active: 1,
                    is_favorite: 0,
                    is_hidden: 0,
                    hidden_at: None,
                    tags_json: None,
                    synced_at: "2026-08-21 00:00:00".into(),
                    created_at: "2026-08-21 00:00:00".into(),
                    updated_at: "2026-08-21 00:00:00".into(),
                    media_category: None,
                    ai_type: None,
                    is_drm: 0,
                    release_date: None,
                    description: None,
                    theme: None,
                    maker_id: None,
                    page_count: None,
                    age_rating: None,
                    series_name: None,
                },
            )
            .unwrap();
        });
    }

    fn seed_progress(cx: &mut TestAppContext, id: &str, current: i64, total: Option<i64>) {
        cx.update(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            progress::upsert(
                db,
                &progress::ReadingProgress {
                    book_id: id.into(),
                    content_id: String::new(),
                    current_page: current,
                    total_pages: total,
                    finished_at: None,
                    last_read_at: "2026-08-21 00:00:00".into(),
                    scroll_position: 0.0,
                },
            )
            .unwrap();
        });
    }

    /// ローカル本を読了にする（状態タグ「既読」の検証用）。
    fn mark_read(cx: &mut TestAppContext, id: &str) {
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            progress::upsert(
                db,
                &progress::ReadingProgress {
                    book_id: id.into(),
                    content_id: String::new(),
                    current_page: 10,
                    total_pages: Some(10),
                    finished_at: Some("2026-08-21 00:00:00".into()),
                    last_read_at: "2026-08-21 00:00:00".into(),
                    scroll_position: 0.0,
                },
            )
            .unwrap();
        });
    }

    /// `plan_of` に渡すコンテンツ定義（ファイル名, 種別, (レンディション名, エントリ数)）。
    type TestContents<'a> = Vec<(&'a str, MediaKind, Vec<(&'a str, usize)>)>;

    /// テスト用の計画を組み立てる（`analyze_zip` を通さずに直接作る）。
    fn plan_of(contents: TestContents<'_>) -> ImportPlan {
        ImportPlan {
            contents: contents
                .into_iter()
                .map(|(name, kind, renditions)| PlannedContent {
                    display_name: name.to_string(),
                    media_kind: kind,
                    renditions: renditions
                        .into_iter()
                        .map(|(label, files)| PlannedRendition {
                            label: label.to_string(),
                            kind,
                            entries: (0..files).collect(),
                        })
                        .collect(),
                })
                .collect(),
            primary: 0,
            export_text: Vec::new(),
            warnings: Vec::new(),
            skip_reason: None,
        }
    }

    #[test]
    fn import_confirmation_is_needed_only_for_ambiguous_structures() {
        // 形式 1 つ・コンテンツ 1 つ → 自動取り込み
        let simple = plan_of(vec![("本文", MediaKind::Image, vec![("画像", 12)])]);
        assert!(!import_needs_confirmation(&simple));
        // コンテンツが複数 → 確認する
        let multi = plan_of(vec![
            ("本編", MediaKind::Image, vec![("画像", 48)]),
            ("別冊", MediaKind::Pdf, vec![("PDF", 1)]),
        ]);
        assert!(import_needs_confirmation(&multi));
        // 形式（レンディション）が複数 → 確認する
        let two_formats = plan_of(vec![(
            "本編",
            MediaKind::Image,
            vec![("画像", 48), ("PDF", 1)],
        )]);
        assert!(import_needs_confirmation(&two_formats));
    }

    #[test]
    fn import_choices_summarize_each_content() {
        let plan = plan_of(vec![
            ("本編", MediaKind::Image, vec![("画像", 48)]),
            ("別冊", MediaKind::Pdf, vec![("PDF", 1), ("画像", 3)]),
        ]);
        let choices = import_choices(&plan);
        assert_eq!(choices.len(), 2);
        assert_eq!(choices[0].display_name, "本編");
        assert_eq!(choices[0].kind, "画像");
        assert_eq!(choices[0].detail, "画像 48ファイル");
        assert_eq!(choices[1].display_name, "別冊");
        assert_eq!(choices[1].kind, "PDF");
        assert_eq!(choices[1].detail, "PDF 1ファイル / 画像 3ファイル");
    }

    #[gpui_kit::test]
    async fn import_confirmation_modal_returns_the_selection(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        let view = cx.new(BookshelfView::new);
        // モーダルを開いた状態にする（worker が待っている想定のチャネル）
        let (reply, answer) = std::sync::mpsc::channel();
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.pending_import = Some(PendingImport {
                    title: "総集編".into(),
                    choices: vec![
                        ImportChoice {
                            display_name: "本編".into(),
                            kind: "画像".into(),
                            detail: "画像 48ファイル".into(),
                        },
                        ImportChoice {
                            display_name: "別冊".into(),
                            kind: "PDF".into(),
                            detail: "PDF 1ファイル".into(),
                        },
                    ],
                    selected: 0,
                    reply,
                });
                cx.notify();
            });
        });
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(900.0),
                height: gpui_kit::px(700.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        visual.update(|window, cx| {
            let arena_clear = window.draw(cx);
            arena_clear.clear(cx);
        });
        // 選択肢が描画されている
        assert!(
            visual.debug_bounds("import-choice-0").is_some(),
            "選択肢がモーダルに出ること"
        );
        assert!(
            visual.debug_bounds("import-choice-1").is_some(),
            "2 つ目の選択肢も出ること"
        );

        // 別冊を選んで確定すると、その添字が worker に返る
        cx.update(|cx| view.update(cx, |this, cx| this.select_pending_import(cx, 1)));
        cx.update(|cx| view.update(cx, |this, cx| this.confirm_pending_import(cx)));
        assert_eq!(answer.recv().unwrap(), Some(1));
        assert!(
            view.read_with(cx, |this, _| this.pending_import.is_none()),
            "確定したらモーダルは閉じる"
        );

        // キャンセルは None を返す
        let (reply, answer) = std::sync::mpsc::channel();
        cx.update(|cx| {
            view.update(cx, |this, _| {
                this.pending_import = Some(PendingImport {
                    title: "総集編".into(),
                    choices: Vec::new(),
                    selected: 0,
                    reply,
                });
            });
        });
        cx.update(|cx| view.update(cx, |this, cx| this.cancel_pending_import(cx)));
        assert_eq!(answer.recv().unwrap(), None);
    }

    /// 表紙を枠に比率を保って収めた描画サイズを返す（カード / リスト共通）。
    /// 横長は幅いっぱい、縦長は高さいっぱいになり、どちらも枠からはみ出さない。
    #[test]
    fn cover_fit_size_preserves_aspect_and_fits_box() {
        // 横長 (FANZA の原寸 560x420) は 4:3 の枠にそのまま収まる
        let (w, h) = fit_cover_size(560.0, 420.0, 120.0, 90.0);
        assert!(
            (w - 120.0).abs() < 0.01 && (h - 90.0).abs() < 0.01,
            "{w}x{h}"
        );
        // 縦長 (DLsite 290x408) は高さいっぱい・幅は比率なり
        let (w, h) = fit_cover_size(290.0, 408.0, 120.0, 90.0);
        assert!((h - 90.0).abs() < 0.01, "縦長は高さいっぱい: {w}x{h}");
        assert!(
            (w - 90.0 * 290.0 / 408.0).abs() < 0.01,
            "比率を保つ: {w}x{h}"
        );
        assert!(w <= 120.0, "枠の幅を超えない: {w}");
        // 極端な横長も枠内に収まり比率を保つ
        let (w, h) = fit_cover_size(1000.0, 100.0, 120.0, 90.0);
        assert!((w - 120.0).abs() < 0.01, "横長は幅いっぱい: {w}x{h}");
        assert!(
            (h - 120.0 * 100.0 / 1000.0).abs() < 0.01,
            "比率を保つ: {w}x{h}"
        );
        assert!(h <= 90.0);
        // 壊れた画像（0）でも 0 除算しない
        let (w, h) = fit_cover_size(0.0, 0.0, 120.0, 90.0);
        assert!(w >= 1.0 && h >= 1.0, "{w}x{h}");
    }

    /// リスト表示のスクロール領域は親（`bookshelf-grid`）の高さに束縛されること。
    /// 束縛しないと内容高さまで伸び、`overflow_y_scroll` が効かずスクロールできない。
    #[gpui_kit::test]
    async fn list_view_scroll_area_is_bounded_by_viewport(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        // 画面（600px）に収まりきらない件数（1 行 ≒ 106px）を用意する
        for i in 0..30 {
            seed_shelf_item(
                cx,
                &format!("db-{i}"),
                &format!("本{i:02}"),
                "サークルA",
                None,
            );
        }
        let view = cx.new(BookshelfView::new);
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.view_mode = ViewMode::List;
                cx.notify();
            });
        });
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        let list = visual
            .debug_bounds("bookshelf-list")
            .expect("リストが描画されている");
        assert!(
            list.size.height < gpui_kit::px(600.0),
            "スクロール領域が内容高さまで伸びている（スクロール不可）: {list:?}"
        );
        assert!(
            list.size.height > gpui_kit::px(200.0),
            "スクロール領域が潰れている: {list:?}"
        );
    }

    /// エリアの比率（3:2）は、横長の最多ケース（FANZA 448x315 = 1.42）が
    /// **高さいっぱい**に収まる幅であること（エリアが縦長すぎると下に余白が出る）。
    /// あわせて縦長（448x651 = 0.69）より横長＝左右に余白ができることも固定する。
    #[test]
    fn list_cover_area_fits_widest_common_cover() {
        // 定数同士の比較なのでコンパイル時に検証する
        const _: () = assert!(
            LIST_COVER_ASPECT >= 448.0 / 315.0,
            "FANZA の表紙が高さいっぱいに収まらない（下に余白が出る）"
        );
        const _: () = assert!(
            LIST_COVER_ASPECT > 448.0 / 651.0,
            "縦長の表紙に左右の余白ができない"
        );
    }

    /// 関連書籍は「同一サークル → 同一作者」の順に並ぶ（サークルの方が結び付きが強い）。
    #[test]
    fn related_book_indices_prefers_same_circle_then_author() {
        let keys = vec![
            ("サークルA".to_string(), "作者X".to_string()),
            ("サークルB".to_string(), "作者X".to_string()),
            ("サークルA".to_string(), "作者Y".to_string()),
            ("サークルC".to_string(), "作者Z".to_string()),
        ];
        assert_eq!(related_book_indices(&keys, 0, 5), vec![2, 1]);
    }

    /// 自分自身と空のキー（技術書典の作者は空）は関連とみなさず、上限で切る。
    #[test]
    fn related_book_indices_skips_self_and_empty_keys_and_respects_the_limit() {
        let keys = vec![
            ("サークルA".to_string(), String::new()),
            (String::new(), String::new()),
            ("サークルA".to_string(), String::new()),
            ("サークルA".to_string(), String::new()),
            ("サークルA".to_string(), String::new()),
        ];
        assert_eq!(related_book_indices(&keys, 0, 2), vec![2, 3], "上限で切る");
        assert!(
            !related_book_indices(&keys, 0, 5).contains(&0),
            "自分自身は含めない"
        );
        assert!(
            related_book_indices(&keys, 1, 5).is_empty(),
            "空キーだけの本には関連が無い"
        );
    }

    /// テスト用の単色 RenderImage（表紙の比率を固定して検証するため）。
    fn test_cover_image(w: u32, h: u32) -> Arc<RenderImage> {
        let rgba = image::RgbaImage::from_pixel(w, h, image::Rgba([30, 60, 90, 255]));
        Arc::new(RenderImage::new([image::Frame::new(rgba)]))
    }

    /// 縦長の表紙（400x600 = 0.667）は**切り抜かれず**、エリアの高さいっぱいに縮小されて
    /// 中央に置かれること（左右に余白ができる）。
    #[gpui_kit::test]
    async fn portrait_cover_is_letterboxed_not_cropped(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        let view = cx.new(BookshelfView::new);
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.view_mode = ViewMode::List;
                // 表紙を縦長（400x600）に固定する（取得経路に依存せず検証するため）
                this.shelf_cards[0].cover = Some(test_cover_image(400, 600));
                cx.notify();
            });
        });
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        let area = visual
            .debug_bounds("list-cover-db-1")
            .expect("表紙エリアが描画されている");
        let img = visual
            .debug_bounds("list-cover-img-db-1")
            .expect("表紙画像が描画されている");
        let (area_w, area_h) = (area.size.width.as_f32(), area.size.height.as_f32());
        let (img_w, img_h) = (img.size.width.as_f32(), img.size.height.as_f32());
        // 高さはエリアいっぱい（＝切り抜かず縮小して収めている）
        assert!(
            (img_h - area_h).abs() < 1.5,
            "画像がエリアの高さいっぱいになっていない: img_h={img_h} area_h={area_h}"
        );
        // 幅はエリアより狭い（＝左右に余白がある。切り抜きなら幅いっぱいになる）
        assert!(
            img_w < area_w - 1.0,
            "画像がエリアの幅いっぱい（切り抜きの疑い）: img_w={img_w} area_w={area_w}"
        );
        // 元画像の比率を保っている（400x600 = 0.667）
        let ratio = img_w / img_h;
        assert!(
            (ratio - 400.0 / 600.0).abs() < 0.05,
            "画像の比率が変わっている: {ratio}"
        );
    }

    /// 表紙エリアと情報列の幅は行の内容で変わらない（全行でテキスト開始 X が揃う）。
    ///
    /// 旧仕様: 表紙枠は `h_full().aspect_ratio(3:2)` で、行高 × 1.5 が表紙の幅になっていた。
    /// 行高はタグ数などで変わるため、表紙の幅とテキスト開始 X が行ごとにずれていた。
    /// タグ列は上限で折りたたまれる（＝通常の行は同じ高さ）ので、ここでは展開して
    /// 行の高さが内容で変わる状態を作って検証する。
    #[gpui_kit::test]
    async fn list_row_keeps_a_fixed_cover_and_info_column(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_shelf_item(cx, "db-1", "タグが多い本", "サークルA", None);
        seed_shelf_item(cx, "db-2", "タグが無い本", "サークルB", None);
        // 窓を狭くしてタグ情報エリアを狭め、db-1 のタグを折り返させる
        // （行の高さが内容で変わる状態 = 旧実装では表紙の幅と開始 X も変わっていた状態）
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            let tags: Vec<String> = (0..30).map(|i| format!("タグ{i}")).collect();
            bookshelf::update_tags(db, "techbookfest", "db-1", &tags).unwrap();
        });
        let view = cx.new(BookshelfView::new);
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.view_mode = ViewMode::List;
                cx.notify();
            });
        });
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        // タグ列は折りたたまれるので、行の高さを内容で変えるには展開する
        // （折りたたみ中は全行が同じ高さ = 表紙の高さになる）。
        let toggle = visual
            .debug_bounds("tag-toggle-db-1")
            .expect("折りたたみトグル（+n）が出ている");
        visual.simulate_click(toggle.center(), gpui_kit::Modifiers::default());
        for _ in 0..6 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        let row1 = visual.debug_bounds("book-list-db-1").expect("行1");
        let row2 = visual.debug_bounds("book-list-db-2").expect("行2");
        let cover1 = visual.debug_bounds("list-cover-db-1").expect("表紙1");
        let cover2 = visual.debug_bounds("list-cover-db-2").expect("表紙2");
        let info1 = visual.debug_bounds("list-info-db-1").expect("情報列1");
        let info2 = visual.debug_bounds("list-info-db-2").expect("情報列2");

        assert!(
            row1.size.height.as_f32() > row2.size.height.as_f32() + 8.0,
            "前提: タグを展開すると行の高さが変わる (row1={} row2={})",
            row1.size.height.as_f32(),
            row2.size.height.as_f32()
        );
        // 表紙・情報列の幅と開始 X は行に依存しない
        for (a, b, label) in [(cover1, cover2, "表紙"), (info1, info2, "情報列")] {
            assert!(
                (a.size.width.as_f32() - b.size.width.as_f32()).abs() < 0.5,
                "{label}の幅が行で違う: {} vs {}",
                a.size.width.as_f32(),
                b.size.width.as_f32()
            );
            assert!(
                (a.origin.x.as_f32() - b.origin.x.as_f32()).abs() < 0.5,
                "{label}の開始 X が行でずれている: {} vs {}",
                a.origin.x.as_f32(),
                b.origin.x.as_f32()
            );
        }
        let expected_w = LIST_COVER_MIN_H * LIST_COVER_ASPECT;
        assert!(
            (cover1.size.width.as_f32() - expected_w).abs() < 1.0,
            "表紙の幅が固定値でない: {} expected={expected_w}",
            cover1.size.width.as_f32()
        );
        // タグ情報エリアは行の幅の約 20%（その中で折り返す）
        let tags1 = visual.debug_bounds("list-tags-db-1").expect("タグエリア1");
        let expected_tags_w = (row1.size.width.as_f32() - 16.0) * LIST_TAGS_W_RATIO;
        assert!(
            (tags1.size.width.as_f32() - expected_tags_w).abs() < 2.5,
            "タグ情報エリアの幅が 20% でない: width={} expected={expected_tags_w}",
            tags1.size.width.as_f32()
        );
    }

    /// 未読 / 既読 / ダウンロード済み / お気に入り はページ数の下に出す
    /// （文字は短い「未読 / 既読」だけ。長いものはアイコン）。
    #[gpui_kit::test]
    async fn list_row_puts_status_items_below_the_page_count(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        // db-1: ダウンロード済み + 読了 + お気に入り
        seed_shelf_item(cx, "db-1", "読了した本", "サークルA", None);
        seed_book(cx, "book-1", "読了した本", "サークルA");
        mark_read(cx, "book-1");
        // db-2: 未ダウンロード
        seed_shelf_item(cx, "db-2", "未ダウンロードの本", "サークルB", None);
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            bookshelf::set_favorite(db, "techbookfest", "db-1", true).unwrap();
        });
        let view = cx.new(BookshelfView::new);
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.view_mode = ViewMode::List;
                cx.notify();
            });
        });
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        let status_row = visual.debug_bounds("list-status-row-db-1").expect("状態行");
        let tags_area = visual.debug_bounds("list-tags-db-1").expect("タグエリア");
        let progress = visual.debug_bounds("list-progress-db-1").expect("ページ数");
        // ページ数の下に出る
        assert!(
            status_row.origin.y.as_f32()
                >= progress.origin.y.as_f32() + progress.size.height.as_f32() - 0.5,
            "状態行がページ数の下にない: status_y={} progress_bottom={}",
            status_row.origin.y.as_f32(),
            progress.origin.y.as_f32() + progress.size.height.as_f32()
        );
        // 情報列の中（タグ情報エリアには入っていない）
        assert!(
            status_row.origin.x.as_f32() + status_row.size.width.as_f32()
                <= tags_area.origin.x.as_f32() + 0.5,
            "状態行がタグ情報エリアにかぶっている（情報列に収まっていない）"
        );
        for selector in [
            "list-status-read-db-1",
            "list-status-favorite-db-1",
            "list-status-downloaded-db-1",
        ] {
            let bounds = visual
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} が描画されていない"));
            // 状態行の中に並ぶ
            assert!(
                (bounds.origin.y.as_f32() - status_row.origin.y.as_f32()).abs() < 12.0,
                "{selector} が状態行にない: y={} row_y={}",
                bounds.origin.y.as_f32(),
                status_row.origin.y.as_f32()
            );
            assert!(
                bounds.origin.x.as_f32() >= status_row.origin.x.as_f32() - 0.5,
                "{selector} が状態行より左にある"
            );
        }
        // アイコン（ダウンロード済み / お気に入り / 未登録ハート）は見える大きさであること
        for selector in [
            "list-status-downloaded-db-1",
            "list-status-favorite-db-1",
            "list-status-not-downloaded-db-2",
            "list-status-not-favorite-db-2",
        ] {
            let bounds = visual
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} が描画されていない"));
            assert!(
                bounds.size.height.as_f32() >= 14.0 && bounds.size.width.as_f32() >= 14.0,
                "{selector} のアイコンが小さすぎる: {}x{}",
                bounds.size.width.as_f32(),
                bounds.size.height.as_f32()
            );
        }
        // 未ダウンロードの本は読めないので「未読」は出さない
        assert!(
            visual
                .debug_bounds("list-status-not-downloaded-db-2")
                .is_some(),
            "未ダウンロードタグが出ていない"
        );
        assert!(
            visual.debug_bounds("list-status-unread-db-2").is_none(),
            "未ダウンロードの本に「未読」が出ている"
        );
        // お気に入りは登録 / 未登録の両方を出す（未登録は控えめな輪郭ハート）
        assert!(
            visual.debug_bounds("list-status-favorite-db-2").is_none(),
            "お気に入りでない本に「登録済み」ハートが出ている"
        );
        assert!(
            visual
                .debug_bounds("list-status-not-favorite-db-2")
                .is_some(),
            "お気に入りでない本に未登録ハートが出ていない"
        );
        assert!(
            visual
                .debug_bounds("list-status-not-favorite-db-1")
                .is_none(),
            "お気に入りの本に未登録ハートが出ている"
        );
    }

    /// お気に入りアイコンのクリックは、ビューアー / ダウンロードではなく
    /// **お気に入りのトグル**になる（行クリックへ伝播させない）。
    #[gpui_kit::test]
    async fn status_heart_click_toggles_favorite(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        // 未取得の本（行クリックならダウンロードが走るので伝播を検出できる）
        seed_shelf_item(cx, "db-1", "お気に入り前の本", "サークルA", None);
        let view = cx.new(BookshelfView::new);
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.view_mode = ViewMode::List;
                cx.notify();
            });
        });
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        let heart = visual
            .debug_bounds("list-status-not-favorite-db-1")
            .expect("未登録ハートが描画されている");
        visual.simulate_click(heart.center(), gpui_kit::Modifiers::default());

        assert_eq!(
            view.read_with(cx, |this, _| this.shelf_cards[0].shelf.is_favorite),
            1,
            "クリックでお気に入りになっていない"
        );
        let stored = cx.update(|cx| {
            bookshelf::list_all(&AppState::global(cx).db_pool)
                .unwrap()
                .into_iter()
                .find(|item| item.database_id == "db-1")
                .map(|item| item.is_favorite)
        });
        assert_eq!(stored, Some(1), "DB に保存されていない");
        assert!(
            view.read_with(cx, |this, _| this.download_states.is_empty()),
            "行クリックに伝播してダウンロードが始まっている"
        );
    }

    /// 同一サークル / 同一作者の本をカルーセルに出す（関連が無い本には出さない）。
    #[gpui_kit::test]
    async fn list_row_shows_related_books_carousel(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        seed_shelf_item(cx, "db-2", "本2", "サークルA", None);
        seed_shelf_item(cx, "db-3", "本3", "サークルA", None);
        seed_shelf_item(cx, "db-4", "別サークルの本", "サークルZ", None);
        let view = cx.new(BookshelfView::new);
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.view_mode = ViewMode::List;
                cx.notify();
            });
        });
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        let carousel = visual
            .debug_bounds("list-carousel-db-1")
            .expect("カルーセルが描画されている");
        assert!(
            carousel.size.width.as_f32() > 40.0,
            "カルーセルが潰れている: {}",
            carousel.size.width.as_f32()
        );
        // 行の右端まで使う（残り幅いっぱい = 表示できる最大の幅）
        let row = visual.debug_bounds("book-list-db-1").expect("行");
        let row_right = row.origin.x.as_f32() + row.size.width.as_f32() - 8.0; // p_2
        let carousel_right = carousel.origin.x.as_f32() + carousel.size.width.as_f32();
        assert!(
            (carousel_right - row_right).abs() < 2.0,
            "カルーセルが行の右端まで届いていない: right={carousel_right} row_right={row_right}"
        );
        // 関連サムネイルは**カルーセル列の幅を LIST_RELATED_PER_VIEW 等分**して埋める
        //（端に切れかけを出さない）。大きさの上限はリストの表紙と同じ。
        let cover = visual.debug_bounds("list-cover-db-1").expect("表紙");
        let related = visual
            .debug_bounds("list-related-db-1-0")
            .expect("関連サムネイル");
        let related2 = visual
            .debug_bounds("list-related-db-1-1")
            .expect("2 件目の関連サムネイル");
        assert!(
            (related.size.width.as_f32() - related2.size.width.as_f32()).abs() < 0.5,
            "関連サムネイルの幅が揃っていない: {} vs {}",
            related.size.width.as_f32(),
            related2.size.width.as_f32()
        );
        assert!(
            related.size.width.as_f32() <= cover.size.width.as_f32() + 0.5,
            "関連サムネイルが表紙より大きい: related={} cover={}",
            related.size.width.as_f32(),
            cover.size.width.as_f32()
        );
        assert!(
            (related.size.width.as_f32() / related.size.height.as_f32() - LIST_COVER_ASPECT).abs()
                < 0.05,
            "関連サムネイルが 3:2 でない: {}x{}",
            related.size.width.as_f32(),
            related.size.height.as_f32()
        );
        // 前へ / 次へは**四角いバー**（高さは列いっぱい = サムネイルと同じ、幅はアイコン程度）で、
        // カルーセル領域の中に収まっている
        for (bar_selector, direction) in [
            ("list-carousel-prev-db-1", "前へ"),
            ("list-carousel-next-db-1", "次へ"),
        ] {
            let bar = visual
                .debug_bounds(bar_selector)
                .unwrap_or_else(|| panic!("{direction} バーが描画されていない"));
            assert!(
                (20.0..=40.0).contains(&bar.size.width.as_f32()),
                "{direction} バーの幅がアイコン程度でない: {}",
                bar.size.width.as_f32()
            );
            assert!(
                (bar.size.height.as_f32() - related.size.height.as_f32()).abs() < 1.5,
                "{direction} バーの高さがサムネイルと揃っていない: bar={} thumb={}",
                bar.size.height.as_f32(),
                related.size.height.as_f32()
            );
            assert!(
                bar.origin.x.as_f32() >= carousel.origin.x.as_f32() - 0.5
                    && bar.origin.x.as_f32() + bar.size.width.as_f32() <= carousel_right + 0.5,
                "{direction} バーがカルーセル領域の外にある"
            );
        }
        assert!(
            visual.debug_bounds("list-related-db-1-0").is_some(),
            "1 件目の関連書籍が出ていない"
        );
        assert!(
            visual.debug_bounds("list-related-db-1-1").is_some(),
            "2 件目の関連書籍が出ていない"
        );
        assert!(
            visual.debug_bounds("list-related-db-1-2").is_none(),
            "関連は 2 件のはず（同一サークルの他 2 冊）"
        );
        assert!(
            visual.debug_bounds("list-carousel-db-4").is_none(),
            "関連が無い本にカルーセルを出している"
        );
    }

    /// 関連書籍のサムネイルをクリックすると、その本を開く / ダウンロードする
    /// （行クリックと同じ経路）。
    #[gpui_kit::test]
    async fn related_thumbnail_click_starts_the_download(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        seed_shelf_item(cx, "db-2", "本2", "サークルA", None);
        let view = cx.new(BookshelfView::new);
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.view_mode = ViewMode::List;
                cx.notify();
            });
        });
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        let item = visual
            .debug_bounds("list-related-db-1-0")
            .expect("関連サムネイルが描画されている");
        visual.simulate_click(item.center(), gpui_kit::Modifiers::default());
        assert!(
            view.read_with(cx, |this, _| this.download_states.contains_key("db-2")),
            "関連サムネイルのクリックでダウンロードが始まっていない"
        );
    }

    /// 移動できる向きのバーをクリックすると 1 つ進む（前へ / 次への動作）。
    #[gpui_kit::test]
    async fn enabled_carousel_bar_navigates(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        // 同一サークル 5 冊（窓を狭くすると全部は収まらない）
        for i in 1..=5 {
            seed_shelf_item(
                cx,
                &format!("db-{i}"),
                &format!("本{i:02}"),
                "サークルA",
                None,
            );
        }
        let view = cx.new(BookshelfView::new);
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.view_mode = ViewMode::List;
                cx.notify();
            });
        });
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1000.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        let selected = |view: &gpui_kit::Entity<BookshelfView>, cx: &mut TestAppContext| {
            view.read_with(cx, |this, cx| {
                this.carousel_states
                    .get("db-1")
                    .and_then(|state| state.read(cx).selected_index())
            })
        };
        assert_eq!(selected(&view, cx), Some(0), "初期選択が先頭でない");
        let bar = visual
            .debug_bounds("list-carousel-next-db-1")
            .expect("次へバーが描画されている");
        visual.simulate_click(bar.center(), gpui_kit::Modifiers::default());
        assert_eq!(
            selected(&view, cx),
            Some(1),
            "次へバーのクリックで進んでいない"
        );
        assert!(
            view.read_with(cx, |this, _| this.download_states.is_empty()),
            "バーのクリックが行へ伝播してダウンロードが始まっている"
        );
    }

    /// 移動できない向きのバーをクリックしても、行クリック（ビューアー / ダウンロード）へ
    /// 伝播しない（ハンドラを登録しないと抜けてビューアーが開いてしまう）。
    #[gpui_kit::test]
    async fn disabled_carousel_bar_does_not_trigger_the_row_click(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        // 同一サークル 2 冊 + 広い窓 → 2 冊とも収まり、前へ / 次へは移動できない
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        seed_shelf_item(cx, "db-2", "本2", "サークルA", None);
        let view = cx.new(BookshelfView::new);
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.view_mode = ViewMode::List;
                cx.notify();
            });
        });
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1600.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        let bar = visual
            .debug_bounds("list-carousel-next-db-1")
            .expect("次へバーが描画されている");
        visual.simulate_click(bar.center(), gpui_kit::Modifiers::default());
        assert!(
            view.read_with(cx, |this, _| this.download_states.is_empty()),
            "移動不可のバーのクリックが行へ伝播してダウンロードが始まっている"
        );
        let selected = view.read_with(cx, |this, cx| {
            this.carousel_states
                .get("db-1")
                .and_then(|state| state.read(cx).selected_index())
        });
        assert_eq!(selected, Some(0), "移動不可なのに選択が動いている");
    }

    /// タグが無い本でも、タグ編集ボタンは行の**上**に揃う（縦中央に落ちない）。
    #[gpui_kit::test]
    async fn tag_edit_button_sits_at_the_top_of_the_row(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        let view = cx.new(BookshelfView::new);
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.view_mode = ViewMode::List;
                cx.notify();
            });
        });
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1400.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        let row = visual.debug_bounds("book-list-db-1").expect("行");
        let button = visual
            .debug_bounds("tag-edit-db-1")
            .expect("タグ編集ボタンが描画されている");
        assert!(
            button.center().y - row.origin.y < row.size.height * 0.25,
            "タグ編集ボタンが行の上に揃っていない（縦中央に落ちている）: row={row:?} button={button:?}"
        );
    }

    /// タグが多い本はタグ列が**表示エリアの幅に収まる数**で折りたたまれ、行の高さが伸びない。
    /// 「+n」をクリックすると全タグが出て行が伸び、「閉じる」で元に戻る。
    #[gpui_kit::test]
    async fn tag_column_collapses_and_expands(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "b1", "本1", "サークルA");
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        // 行が 4 列そろうよう、関連書籍（カルーセル）も出る状態にする
        seed_shelf_item(cx, "db-2", "本2", "サークルA", None);
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = 'db-1' WHERE id = 'b1'")
                    .execute(db)
                    .await
            })
            .unwrap();
            // 長めのタグ名にして折り返し行数を増やす（展開で行がしっかり伸びる状態を作る）
            let tags: Vec<String> = (1..=20).map(|i| format!("長いタグ名前{i:02}")).collect();
            let pairs: Vec<(&str, &str)> = tags.iter().map(|t| (t.as_str(), "manual")).collect();
            db::tags::set_for_book(db, "b1", &pairs).expect("seed tags");
        });
        let view = cx.new(BookshelfView::new);
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.view_mode = ViewMode::List;
                cx.notify();
            });
        });
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1400.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        let draw = |visual: &mut gpui_kit::VisualTestContext| {
            for _ in 0..6 {
                visual.update(|window, cx| {
                    let arena_clear = window.draw(cx);
                    arena_clear.clear(cx);
                });
            }
        };
        draw(visual);
        let row_h = |visual: &mut gpui_kit::VisualTestContext| {
            visual
                .debug_bounds("book-list-db-1")
                .expect("行が描画されている")
                .size
                .height
        };

        // 折りたたみ: 表示エリアに収まる数だけ出す（20 件目は出ない）
        assert!(
            visual
                .debug_bounds("tag-label-db-1-長いタグ名前20")
                .is_none(),
            "折りたたまれていない（20 件目まで出ている）"
        );
        let collapsed_h = row_h(visual);
        let toggle = visual
            .debug_bounds("tag-toggle-db-1")
            .expect("折りたたみトグル（+n）が出ていない");

        // 展開: 全タグが出て行が伸びる
        visual.simulate_click(toggle.center(), gpui_kit::Modifiers::default());
        draw(visual);
        assert!(
            visual
                .debug_bounds("tag-label-db-1-長いタグ名前20")
                .is_some(),
            "展開しても全タグが出ていない"
        );
        let expanded_h = row_h(visual);
        assert!(
            expanded_h > collapsed_h,
            "展開しても行が伸びていない: {collapsed_h:?} -> {expanded_h:?}"
        );
        // 閉じる: 折りたたみに戻り、行の高さも戻る
        let toggle = visual
            .debug_bounds("tag-toggle-db-1")
            .expect("「閉じる」トグルが出ていない");
        visual.simulate_click(toggle.center(), gpui_kit::Modifiers::default());
        draw(visual);
        assert!(
            visual
                .debug_bounds("tag-label-db-1-長いタグ名前20")
                .is_none(),
            "「閉じる」で折りたたまれていない"
        );
        assert_eq!(
            row_h(visual),
            collapsed_h,
            "「閉じる」で行の高さが元に戻っていない"
        );

        // タグをクリックしても行の動作（開く / ダウンロード）へ伝播しない
        assert!(
            view.read_with(cx, |this, _| this.download_states.is_empty()),
            "タグ列のクリックが行へ伝播している"
        );
    }

    /// 折りたたみ個数は、チップ幅と表示エリアの幅から計算する（折り返しを模擬）。
    /// 末尾要素（タグ編集ボタン / 「+n」チップ）の幅は最後の行に確保する。
    #[test]
    fn packed_tag_count_fills_rows_from_the_available_width() {
        // 1 行 250px・チップ 60px・末尾 40px: 3 個（60*3 + gap 4*2 = 188）入り、
        // 188 + gap 4 + 40 = 232 ≤ 250 なので末尾も入る
        assert_eq!(packed_tag_count(&[60.0, 60.0, 60.0], 250.0, 1, 40.0), 3);
        // 1 行 200px: 3 個だと 188 + 44 = 232 > 200 で末尾が入らない → 2 個まで減らす
        assert_eq!(packed_tag_count(&[60.0, 60.0, 60.0], 200.0, 1, 40.0), 2);
        // 1 行 130px・2 行: 1 行 2 個（124）まで → 2 行で 4 個
        assert_eq!(
            packed_tag_count(&[60.0; 5], 130.0, 2, 0.0),
            4,
            "行数を超えて詰めている"
        );
        // 幅が足りないときでも 1 個は出す（「+n」だけの行を作らない）
        assert_eq!(packed_tag_count(&[60.0], 50.0, 4, 40.0), 1);
        // 幅もタグも無い
        assert_eq!(packed_tag_count(&[], 200.0, 4, 40.0), 0);
        assert_eq!(packed_tag_count(&[60.0], 0.0, 4, 40.0), 0);
    }

    /// リストの折りたたみ個数は表示エリアの幅から決まる（件数の固定上限ではない）。
    /// 同じ本でも窓が広いほど多くのタグが出て、狭いほど少なくなる。
    /// また、折りたたみ中の行の高さはタグ無しの行と変わらない（表紙の高さを超えない）。
    #[gpui_kit::test]
    async fn list_tag_collapse_count_follows_the_available_width(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "b1", "本01", "サークルA");
        seed_shelf_item(cx, "db-1", "本01", "サークルA", None);
        seed_shelf_item(cx, "db-2", "本02", "サークルB", None);
        link_book_to_shelf(cx, "b1", "db-1");
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            let tags: Vec<String> = (1..=20).map(|i| format!("タグ{i:02}")).collect();
            let pairs: Vec<(&str, &str)> = tags.iter().map(|t| (t.as_str(), "manual")).collect();
            db::tags::set_for_book(db, "b1", &pairs).expect("seed tags");
        });
        let view = cx.new(BookshelfView::new);
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.view_mode = ViewMode::List;
                cx.notify();
            });
        });
        // 折りたたみ中に描画されているタグ数（01..20 のうち出ているもの）
        let visible_tags = |visual: &mut gpui_kit::VisualTestContext| -> usize {
            let mut count = 0;
            for selector in [
                "tag-label-db-1-タグ01",
                "tag-label-db-1-タグ02",
                "tag-label-db-1-タグ03",
                "tag-label-db-1-タグ04",
                "tag-label-db-1-タグ05",
                "tag-label-db-1-タグ06",
                "tag-label-db-1-タグ07",
                "tag-label-db-1-タグ08",
                "tag-label-db-1-タグ09",
                "tag-label-db-1-タグ10",
                "tag-label-db-1-タグ11",
                "tag-label-db-1-タグ12",
                "tag-label-db-1-タグ13",
                "tag-label-db-1-タグ14",
                "tag-label-db-1-タグ15",
                "tag-label-db-1-タグ16",
                "tag-label-db-1-タグ17",
                "tag-label-db-1-タグ18",
                "tag-label-db-1-タグ19",
                "tag-label-db-1-タグ20",
            ] {
                if visual.debug_bounds(selector).is_some() {
                    count += 1;
                }
            }
            count
        };
        let draw = |visual: &mut gpui_kit::VisualTestContext| {
            for _ in 0..6 {
                visual.update(|window, cx| {
                    let arena_clear = window.draw(cx);
                    arena_clear.clear(cx);
                });
            }
        };

        let mut measure = |width: f32| -> (usize, f32, f32) {
            let window = cx.open_window(
                gpui_kit::Size {
                    width: gpui_kit::px(width),
                    height: gpui_kit::px(600.0),
                },
                |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
            );
            let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
            draw(visual);
            let with_tags = visual
                .debug_bounds("book-list-db-1")
                .expect("タグ付きの行")
                .size
                .height
                .as_f32();
            let without = visual
                .debug_bounds("book-list-db-2")
                .expect("タグ無しの行")
                .size
                .height
                .as_f32();
            (visible_tags(visual), with_tags, without)
        };

        let (wide, wide_row, plain_row) = measure(1400.0);
        let (narrow, _, _) = measure(700.0);
        assert!(
            wide > narrow,
            "窓が広いほうが多くのタグが出ていない: 1400px={wide} / 700px={narrow}"
        );
        assert!(narrow >= 1, "狭い窓でタグが 1 つも出ていない");
        assert!(
            wide >= 8,
            "広い窓で表示個数が少なすぎる（幅から計算できていない）: {wide}"
        );
        assert!(wide < 20, "折りたたまれずに全タグが出ている: {wide}");
        assert!(
            (wide_row - plain_row).abs() < 1.0,
            "折りたたみ中の行がタグで伸びている: タグ付き={wide_row} / タグ無し={plain_row}"
        );
    }

    /// 選択中のタグは折りたたみ中でも先頭に出る（畳まれて見えなくならない）。
    /// 選択を解除すると元の位置（集計数 / 名前順）へ戻る。
    #[gpui_kit::test]
    async fn selected_tag_stays_visible_when_collapsed(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "b1", "本01", "サークルA");
        seed_shelf_item(cx, "db-1", "本01", "サークルA", None);
        link_book_to_shelf(cx, "b1", "db-1");
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            let tags: Vec<String> = (1..=20).map(|i| format!("タグ{i:02}")).collect();
            let pairs: Vec<(&str, &str)> = tags.iter().map(|t| (t.as_str(), "manual")).collect();
            db::tags::set_for_book(db, "b1", &pairs).expect("seed tags");
        });
        let view = cx.new(BookshelfView::new);
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.view_mode = ViewMode::List;
                cx.notify();
            });
        });
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1000.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        let draw = |visual: &mut gpui_kit::VisualTestContext| {
            for _ in 0..6 {
                visual.update(|window, cx| {
                    let arena_clear = window.draw(cx);
                    arena_clear.clear(cx);
                });
            }
        };
        let click = |visual: &mut gpui_kit::VisualTestContext, selector: &'static str| {
            let bounds = visual
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} が描画されていない"));
            visual.simulate_click(bounds.center(), gpui_kit::Modifiers::default());
        };
        let first_tag = |visual: &mut gpui_kit::VisualTestContext| -> &'static str {
            let mut first: Option<(&'static str, (f32, f32))> = None;
            for selector in [
                "tag-label-db-1-タグ01",
                "tag-label-db-1-タグ02",
                "tag-label-db-1-タグ15",
            ] {
                let Some(bounds) = visual.debug_bounds(selector) else {
                    continue;
                };
                let pos = (bounds.origin.y.as_f32(), bounds.origin.x.as_f32());
                if first.is_none_or(|(_, found)| pos < found) {
                    first = Some((selector, pos));
                }
            }
            first.expect("タグチップが描画されている").0
        };

        draw(visual);
        // 折りたたみ中は後ろのタグ（15 件目）は出ていない
        assert!(
            visual.debug_bounds("tag-label-db-1-タグ15").is_none(),
            "折りたたみ中に 15 件目が出ている"
        );
        // 展開して 15 件目（既定では隠れるタグ）を選択し、閉じる
        click(visual, "tag-toggle-db-1");
        draw(visual);
        click(visual, "tag-label-db-1-タグ15");
        draw(visual);
        assert!(
            view.read_with(cx, |this, _| this
                .selected_tags
                .contains(&"タグ15".to_string())),
            "タグを選択できていない"
        );
        click(visual, "tag-toggle-db-1");
        draw(visual);
        assert!(
            visual.debug_bounds("tag-label-db-1-タグ15").is_some(),
            "選択したタグが折りたたみで隠れている"
        );
        assert_eq!(
            first_tag(visual),
            "tag-label-db-1-タグ15",
            "選択したタグが先頭に来ていない"
        );
        // 選択を解除すると元の位置（隠れる位置）に戻る
        click(visual, "tag-label-db-1-タグ15");
        draw(visual);
        assert!(
            !view.read_with(cx, |this, _| this
                .selected_tags
                .contains(&"タグ15".to_string())),
            "選択を解除できていない"
        );
        assert!(
            visual.debug_bounds("tag-label-db-1-タグ15").is_none(),
            "選択解除後も先頭に出たままになっている"
        );
        assert_eq!(
            first_tag(visual),
            "tag-label-db-1-タグ01",
            "選択解除で元の並びに戻っていない"
        );
    }

    /// 折りたたみトグルのラベル: 折りたたみ中は残り件数の「+n」、展開中は「閉じる」。
    #[test]
    fn tag_toggle_label_shows_close_when_expanded() {
        assert_eq!(tag_toggle_label(16, false), "+16");
        assert_eq!(tag_toggle_label(0, true), "閉じる");
    }

    /// タグの表示順: 選択中 → お気に入り → 集計数（そのタグを持つカード数）の多い順 → 名前順。
    /// 集計が同じときは名前順で決定的に並ぶ。
    #[test]
    fn tag_display_order_puts_selected_then_favorites_then_frequent_tags() {
        let favorites = vec!["react".to_string()];
        let selected = vec!["go".to_string()];
        let mut counts = HashMap::new();
        counts.insert("rust".to_string(), 7);
        counts.insert("react".to_string(), 1);
        counts.insert("zenn".to_string(), 7);
        let order = TagOrder::new(&favorites, &selected, std::sync::Arc::new(counts));
        let sorted = order.sorted(&["go", "rust", "zenn", "react"].map(String::from));
        assert_eq!(
            sorted,
            vec!["go", "react", "rust", "zenn"],
            "選択中 → お気に入り → 集計数の多い順 → 名前順で並んでいない"
        );
        // 選択を解除すると、お気に入り / 集計数 / 名前順の元の位置に戻る
        let order = TagOrder::new(
            &favorites,
            &[],
            std::sync::Arc::new(
                [
                    ("rust".to_string(), 7),
                    ("react".to_string(), 1),
                    ("zenn".to_string(), 7),
                ]
                .into_iter()
                .collect(),
            ),
        );
        assert_eq!(
            order.sorted(&["go", "rust", "zenn", "react"].map(String::from)),
            vec!["react", "rust", "zenn", "go"],
            "選択解除で元の並びに戻っていない"
        );
    }

    /// 集計は「そのタグを持つカード数」。同じカード内の重複タグは 1 冊と数える。
    #[test]
    fn tag_usage_counts_each_card_once() {
        let cards = [
            vec!["rust".to_string(), "rust".to_string(), "react".to_string()],
            vec!["rust".to_string()],
        ];
        let counts = count_tag_usage(cards.iter().map(|tags| tags.as_slice()));
        assert_eq!(counts.get("rust"), Some(&2));
        assert_eq!(counts.get("react"), Some(&1));
    }

    /// 並び替えの負荷: 集計はカード数、並び替えはカード内のタグ数に比例する。
    /// 実測（debug）: 1000 冊 × 30 タグ（= 集計 30,000 件）で 20ms 未満、
    /// 1 フレーム分（可視 25 冊 × 30 タグの並び替え × 30 回の平均）で 1ms 未満。
    /// お気に入りの付け外しは `TagOrder` を作り直すだけなので、この並び替えしか走らない。
    #[test]
    fn tag_ordering_stays_cheap_for_a_realistic_shelf() {
        let cards: Vec<Vec<String>> = (0..1000)
            .map(|i| {
                (0..30)
                    .map(|tag| format!("タグ{:03}", (i * 7 + tag) % 200))
                    .collect()
            })
            .collect();
        let start = std::time::Instant::now();
        let counts = std::sync::Arc::new(count_tag_usage(cards.iter().map(|tags| tags.as_slice())));
        let count_elapsed = start.elapsed();
        let order = TagOrder::new(&[], &[], counts);
        // 1 フレーム分（可視 25 冊）を 30 回繰り返して平均を取る
        let start = std::time::Instant::now();
        for _ in 0..30 {
            for tags in cards.iter().take(25) {
                std::hint::black_box(order.sorted(tags));
            }
        }
        let frame_elapsed = start.elapsed() / 30;
        assert!(
            count_elapsed < std::time::Duration::from_millis(500),
            "集計が遅すぎる: {count_elapsed:?}"
        );
        assert!(
            frame_elapsed < std::time::Duration::from_millis(10),
            "1 フレーム分の並び替えが遅すぎる: {frame_elapsed:?}"
        );
        // 実測値を確認できるようにしておく（`--nocapture` で表示）
        println!("tag ordering: counts={count_elapsed:?} frame={frame_elapsed:?}");
    }

    /// カード（グリッド）のタグは `CARD_TAGS_COLLAPSED_MAX` 件で折りたたみ、「+n」で
    /// 全件表示できる。折りたたみ中はタグの折り返しでカードが間延びしない。
    #[gpui_kit::test]
    async fn card_tags_are_capped_and_expandable(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "b1", "本01", "サークルA");
        // 本棚の並びは title ASC なので、db-1 が先頭（1 行目）に来るよう 0 埋めする
        seed_shelf_item(cx, "db-1", "本01", "サークルA", None);
        link_book_to_shelf(cx, "b1", "db-1");
        // タグ無しのカード（同じ行に並べる / 2 行目にも 1 枚置く）
        for i in 2..=6 {
            seed_shelf_item(
                cx,
                &format!("db-{i}"),
                &format!("本{i:02}"),
                "サークルA",
                None,
            );
        }
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            let tags: Vec<String> = (1..=20).map(|i| format!("タグ{i:02}")).collect();
            let pairs: Vec<(&str, &str)> = tags.iter().map(|t| (t.as_str(), "manual")).collect();
            db::tags::set_for_book(db, "b1", &pairs).expect("seed tags");
        });
        let view = cx.new(BookshelfView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1280.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        let draw = |visual: &mut gpui_kit::VisualTestContext| {
            for _ in 0..6 {
                visual.update(|window, cx| {
                    let arena_clear = window.draw(cx);
                    arena_clear.clear(cx);
                });
            }
        };
        draw(visual);
        // 折りたたみ: 上限までだけ出て、「+n」トグルが付く
        assert!(
            visual.debug_bounds("tag-label-db-1-タグ06").is_some(),
            "折りたたみ時の上限までタグが出ていない"
        );
        assert!(
            visual.debug_bounds("tag-label-db-1-タグ07").is_none(),
            "カードのタグが折りたたまれていない"
        );
        assert!(
            visual.debug_bounds("tag-toggle-db-1").is_some(),
            "「+n」トグルが出ていない"
        );
        // タグ 6 件のカードは、タグ無しのカード（別の行）より 3 行分しか高くならない
        // （この幅で実測 90.0px。上限なしの 20 件 = 10 行にすると +295px まで伸びる）
        let with_tags = visual
            .debug_bounds("book-card-db-1")
            .expect("タグ付きカード");
        let without = visual
            .debug_bounds("book-card-db-6")
            .expect("タグ無しカード");
        assert!(
            with_tags.size.height.as_f32() - without.size.height.as_f32() <= 100.0,
            "カードがタグで間延びしている: {} vs {}",
            with_tags.size.height.as_f32(),
            without.size.height.as_f32()
        );
        // 展開: 上限を超えたタグが出てカードが伸びる
        // （全 20 件だと下端は 600px のビューポート外になるため、上限 +3 件目で確認する。
        //   「全件出る」ことはリスト側の tag_column_collapses_and_expands で確認している）
        let toggle = visual.debug_bounds("tag-toggle-db-1").expect("+n");
        visual.simulate_click(toggle.center(), gpui_kit::Modifiers::default());
        draw(visual);
        assert!(
            visual.debug_bounds("tag-label-db-1-タグ09").is_some(),
            "展開しても上限を超えたタグが出ていない"
        );
        let expanded = visual.debug_bounds("book-card-db-1").expect("展開カード");
        assert!(
            expanded.size.height.as_f32() > with_tags.size.height.as_f32() + 40.0,
            "展開してもカードが伸びていない: {} vs {}",
            expanded.size.height.as_f32(),
            with_tags.size.height.as_f32()
        );
    }

    /// カードのタグはお気に入りが先頭、次に集計数（使用冊数）の多い順。
    /// ハートを押した瞬間に並び替わる（reload 不要）。
    #[gpui_kit::test]
    async fn card_tag_order_prefers_favorites_then_frequent_tags(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        for (book, shelf, circle) in [
            ("b1", "db-1", "サークルA"),
            ("b2", "db-2", "サークルB"),
            ("b3", "db-3", "サークルC"),
        ] {
            seed_book(cx, book, shelf, circle);
            seed_shelf_item(cx, shelf, shelf, circle, None);
            link_book_to_shelf(cx, book, shelf);
        }
        // db-1 は 4 タグ、db-2 / db-3 は「いか」だけ → 「いか」が 3 冊で最多
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            db::tags::set_for_book(
                db,
                "b1",
                &[
                    ("あか", "manual"),
                    ("いか", "manual"),
                    ("うか", "manual"),
                    ("えか", "manual"),
                ],
            )
            .expect("seed tags");
            db::tags::set_for_book(db, "b2", &[("いか", "manual")]).expect("seed tags");
            db::tags::set_for_book(db, "b3", &[("いか", "manual")]).expect("seed tags");
        });
        let view = cx.new(BookshelfView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1280.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        let draw = |visual: &mut gpui_kit::VisualTestContext| {
            for _ in 0..6 {
                visual.update(|window, cx| {
                    let arena_clear = window.draw(cx);
                    arena_clear.clear(cx);
                });
            }
        };
        // 先頭（一番上・左）のタグチップを座標から求める
        let first_tag = |visual: &mut gpui_kit::VisualTestContext| -> &'static str {
            let mut first: Option<(&'static str, (f32, f32))> = None;
            for selector in [
                "tag-label-db-1-あか",
                "tag-label-db-1-いか",
                "tag-label-db-1-うか",
                "tag-label-db-1-えか",
            ] {
                let Some(bounds) = visual.debug_bounds(selector) else {
                    continue;
                };
                let pos = (bounds.origin.y.as_f32(), bounds.origin.x.as_f32());
                if first.is_none_or(|(_, found)| pos < found) {
                    first = Some((selector, pos));
                }
            }
            first.expect("タグチップが描画されている").0
        };
        draw(visual);
        assert_eq!(
            first_tag(visual),
            "tag-label-db-1-いか",
            "集計数が多いタグが先頭に来ていない"
        );
        // お気に入りにすると集計数に関係なく先頭へ移動する
        let heart = visual
            .debug_bounds("tag-heart-db-1-えか")
            .expect("えかのハート");
        visual.simulate_click(heart.center(), gpui_kit::Modifiers::default());
        draw(visual);
        assert_eq!(
            first_tag(visual),
            "tag-label-db-1-えか",
            "お気に入りにしたタグが先頭に来ていない"
        );
        // 解除すると集計数順に戻る
        let heart = visual
            .debug_bounds("tag-heart-db-1-えか")
            .expect("えかのハート");
        visual.simulate_click(heart.center(), gpui_kit::Modifiers::default());
        draw(visual);
        assert_eq!(
            first_tag(visual),
            "tag-label-db-1-いか",
            "お気に入り解除で集計数順に戻っていない"
        );
    }

    #[test]
    fn cover_url_candidates_prefers_full_size_for_fanza() {
        // FANZA はサイズ指定を外した原寸を先に試し、失敗したら保存 URL に戻す
        assert_eq!(
            cover_url_candidates(
                "fanza",
                "https://doujin-assets.dmm.co.jp/digital/comic/d_1/d_1pl-200x150.jpg"
            ),
            vec![
                "https://doujin-assets.dmm.co.jp/digital/comic/d_1/d_1pl.jpg".to_string(),
                "https://doujin-assets.dmm.co.jp/digital/comic/d_1/d_1pl-200x150.jpg".to_string(),
            ]
        );
        // サイズ指定が無ければ 1 本だけ
        assert_eq!(
            cover_url_candidates("fanza", "https://example.com/cover.jpg"),
            vec!["https://example.com/cover.jpg".to_string()]
        );
        // 他サイトは保存 URL のまま
        assert_eq!(
            cover_url_candidates(
                "dlsite",
                "https://img.dlsite.jp/modpub/images2/work/doujin/RJ1/RJ1_img_main.jpg"
            ),
            vec![
                "https://img.dlsite.jp/modpub/images2/work/doujin/RJ1/RJ1_img_main.jpg".to_string()
            ]
        );
    }

    #[test]
    fn download_messages_reports_skips_and_unreadable_works() {
        // 成功（警告なし）
        let (toast, error) = download_messages(&Ok(ImportOutcome {
            title: "本".into(),
            warnings: Vec::new(),
        }));
        assert_eq!(toast.as_deref(), Some("「本」をダウンロードしました"));
        assert!(error.is_none());

        // 一部を読み飛ばした（件数と先頭 2 件を出す）
        let (toast, error) = download_messages(&Ok(ImportOutcome {
            title: "本".into(),
            warnings: vec![
                "a.png: 壊れている".into(),
                "b.png: 壊れている".into(),
                "c.png: 壊れている".into(),
            ],
        }));
        let toast = toast.expect("toast");
        assert!(toast.contains("a.png"), "{toast}");
        assert!(toast.contains("ほか 1 件"), "{toast}");
        assert!(error.is_none());

        // 読めるコンテンツが無い（txt のみ / ゲーム等）は理由を出す
        let (toast, error) = download_messages(&Err(ImportFailure::NotAReadable));
        assert!(toast.is_none());
        let error = error.expect("error");
        assert!(error.contains("txt のみ"), "{error}");

        // それ以外の失敗はそのまま出す
        let (_, error) = download_messages(&Err(ImportFailure::Message("通信に失敗".into())));
        assert_eq!(error.as_deref(), Some("通信に失敗"));
    }

    #[gpui_kit::test]
    async fn search_filters_entries(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_shelf_item(cx, "db-1", "React 入門", "サークルA", None);
        seed_shelf_item(cx, "db-2", "Rust の本", "サークルB", None);
        let view = cx.new(BookshelfView::new);
        assert_eq!(view.read_with(cx, |v, _| v.shelf_cards.len()), 2);

        // set search value through a window-bound state
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(800.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        cx.update_window(*window, |_root, window, cx| {
            view.update(cx, |this, cx| {
                this.ensure_search_state(window, cx);
                let state = this.search_state.clone().expect("state");
                state.update(cx, |state, cx| state.set_value("react", window, cx));
            });
        })
        .unwrap();
        let visible = view.read_with(cx, |this, cx| this.visible_shelf_cards(cx).len());
        assert_eq!(visible, 1);
        let titles = view.read_with(cx, |this, cx| {
            this.visible_shelf_cards(cx)
                .iter()
                .map(|card| card.shelf.title.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(titles, vec!["React 入門".to_string()]);
    }

    #[gpui_kit::test]
    async fn search_filters_by_author(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        // seed_shelf_item は author 空のため、author を直接 upsert して seeded する
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            for (id, title, author) in [("db-1", "本A", "田中"), ("db-2", "本B", "佐藤")] {
                bookshelf::upsert(
                    db,
                    &bookshelf::BookshelfItem {
                        site_id: "techbookfest".into(),
                        database_id: id.into(),
                        title: title.into(),
                        circle_name: "circle".into(),
                        author: author.into(),
                        thumbnail_url: None,
                        format: "PDF".into(),
                        caused_at: None,
                        event_name: None,
                        event_slug: None,
                        event_id: None,
                        file_name: None,
                        download_url: None,
                        is_downloadable: 1,
                        is_checked: 0,
                        is_purchased: 1,
                        is_new: 0,
                        is_active: 1,
                        is_favorite: 0,
                        is_hidden: 0,
                        hidden_at: None,
                        tags_json: None,
                        synced_at: "2026-08-21 00:00:00".into(),
                        created_at: "2026-08-21 00:00:00".into(),
                        updated_at: "2026-08-21 00:00:00".into(),
                        media_category: None,
                        ai_type: None,
                        is_drm: 0,
                        release_date: None,
                        description: None,
                        theme: None,
                        maker_id: None,
                        page_count: None,
                        age_rating: None,
                        series_name: None,
                    },
                )
                .unwrap();
            }
        });
        let view = cx.new(BookshelfView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(800.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        // 著者名「佐藤」で検索 → author が一致した本だけ表示される
        cx.update_window(*window, |_root, window, cx| {
            view.update(cx, |this, cx| {
                this.ensure_search_state(window, cx);
                let state = this.search_state.clone().expect("state");
                state.update(cx, |state, cx| state.set_value("佐藤", window, cx));
            });
        })
        .unwrap();
        let titles = view.read_with(cx, |this, cx| {
            this.visible_shelf_cards(cx)
                .iter()
                .map(|card| card.shelf.title.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(titles, vec!["本B".to_string()]);
    }

    #[gpui_kit::test]
    async fn select_book_sets_selection_from_book_id(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "book-1", "本1", "サークルA");
        seed_book(cx, "book-2", "本2", "サークルB");
        let view = cx.new(BookshelfView::new);
        assert_eq!(view.read_with(cx, |v, _| v.shelf_cards.len()), 2);
        // filtered を構築してから select_book で 2 冊目を選択
        cx.update(|cx| {
            view.update(cx, |this, _cx| {
                this.filtered_dirty = true;
            });
        });
        view.update(cx, |this, cx| this.restore_selection(cx, "book-2"));
        let sel = view.read_with(cx, |v, _| {
            v.selected_index.map(|idx| {
                let card_idx = v.filtered[idx];
                v.shelf_cards[card_idx]
                    .local
                    .as_ref()
                    .map(|e| e.book.id.clone())
            })
        });
        assert_eq!(sel, Some(Some("book-2".to_string())));
    }

    #[gpui_kit::test]
    async fn read_filter_separates_unread(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "b1", "既読本", "サークルA");
        seed_book(cx, "b2", "未読本", "サークルB");
        seed_progress(cx, "b1", 10, Some(10)); // read
        seed_progress(cx, "b2", 2, Some(10)); // unread
        // map local books to shelf items via tbf_product_id
        cx.update(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = 'db-1' WHERE id = 'b1'")
                    .execute(db)
                    .await
            })
            .unwrap();
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = 'db-2' WHERE id = 'b2'")
                    .execute(db)
                    .await
            })
            .unwrap();
        });
        seed_shelf_item(cx, "db-1", "既読本", "サークルA", None);
        seed_shelf_item(cx, "db-2", "未読本", "サークルB", None);
        let view = cx.new(BookshelfView::new);

        assert_eq!(
            view.read_with(cx, |v, cx| v.visible_shelf_cards(cx).len()),
            2
        );
        cx.update(|cx| view.update(cx, |this, cx| this.set_read_filter(cx, ReadFilter::Read)));
        let read = view.read_with(cx, |this, cx| {
            this.visible_shelf_cards(cx)
                .iter()
                .map(|card| card.shelf.database_id.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(read, vec!["db-1".to_string()]);
        cx.update(|cx| view.update(cx, |this, cx| this.set_read_filter(cx, ReadFilter::Unread)));
        let unread = view.read_with(cx, |this, cx| {
            this.visible_shelf_cards(cx)
                .iter()
                .map(|card| card.shelf.database_id.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(unread, vec!["db-2".to_string()]);
    }

    #[test]
    fn columns_for_width_grows_on_wide_screens() {
        // 小〜中画面は従来どおりのブレークポイント
        assert_eq!(BookshelfView::columns_for_width(800.0), 3);
        assert_eq!(BookshelfView::columns_for_width(1024.0), 4);
        assert_eq!(BookshelfView::columns_for_width(1280.0), 5);
        // フルHD (1920) は 5 列のまま（従来と変わらない）
        assert_eq!(BookshelfView::columns_for_width(1920.0), 5);
        // 超広幅（4K 等）は列数を増やしてタイルの間延びを防ぐ
        assert_eq!(BookshelfView::columns_for_width(2560.0), 7);
        assert!(BookshelfView::columns_for_width(3840.0) > 5);
    }

    /// チップの色は「絞り込み選択（青）」と「お気に入り（ピンク）」を区別する。
    /// ライト / ダーク両方で成立し、ダークは半透明 + 輪郭で背景に沈まない。
    #[test]
    fn chip_colors_distinguish_selection_from_favorite_in_both_themes() {
        let muted = gpui_kit::hsla(0.0, 0.0, 0.5, 1.0);
        for (name, palette) in [
            ("light", ChipPalette::light()),
            ("dark", ChipPalette::dark()),
        ] {
            let plain = palette.background(muted, false, false);
            let favorite = palette.background(muted, true, false);
            let selected = palette.background(muted, false, true);
            assert_eq!(plain, muted, "{name}: 未選択・未お気に入りは既定色");
            assert_ne!(favorite, plain, "{name}: お気に入りは既定色と別の色");
            assert_ne!(selected, favorite, "{name}: 選択中はお気に入りと別の色");
            assert_ne!(selected, plain, "{name}: 選択中は既定色と別の色");

            // 選択 + お気に入りは背景・文字が選択色、ハートはお気に入りの色のまま
            assert_eq!(
                palette.background(muted, true, true),
                selected,
                "{name}: 重なったら選択色"
            );
            assert_eq!(
                palette.foreground(muted, true, true),
                palette.foreground(muted, false, true),
                "{name}"
            );
            assert_ne!(
                palette.heart(muted, true),
                palette.heart(muted, false),
                "{name}: ハートはお気に入りで色が変わる"
            );
            assert_ne!(
                palette.heart(muted, true),
                selected,
                "{name}: 選択中でもハートは選択色と別（お気に入りが分かる）"
            );

            // 輪郭（ボーダー）は状態ごとに色が変わり、塗りとも別の色
            assert_ne!(
                palette.border(false, false),
                palette.border(true, false),
                "{name}: お気に入りの輪郭は既定と別"
            );
            assert_ne!(
                palette.border(false, false),
                palette.border(false, true),
                "{name}: 選択中の輪郭は既定と別"
            );
            assert_ne!(palette.border(true, false), favorite, "{name}");
            assert_ne!(palette.heart_button, palette.border(false, false), "{name}");

            // `Hsla.h` は 0..1 の正規化値。度で書くと clamp されて別の色になる
            // （過去に hsla(333.0, ..) が赤になっていた）ので全色で範囲を固定する。
            for (label, color) in [
                ("favorite_bg", palette.favorite_bg),
                ("favorite_text", palette.favorite_text),
                ("favorite_heart", palette.favorite_heart),
                ("selected_bg", palette.selected_bg),
                ("selected_text", palette.selected_text),
                ("default_border", palette.default_border),
                ("favorite_border", palette.favorite_border),
                ("selected_border", palette.selected_border),
                ("heart_button", palette.heart_button),
                ("heart_button_hover", palette.heart_button_hover),
            ] {
                assert!(
                    (0.0..=1.0).contains(&color.h),
                    "{name}/{label}: h は 0..1 正規化（現在 {}）",
                    color.h
                );
            }
            // お気に入りはピンク、選択は青（色相で見分けられる）
            assert!(
                palette.favorite_bg.h > 0.85 || palette.favorite_bg.h < 0.05,
                "{name}: お気に入りの背景はピンク系（h={}）",
                palette.favorite_bg.h
            );
            assert!(
                (0.55..=0.70).contains(&palette.selected_bg.h),
                "{name}: 選択中の背景は青系（h={}）",
                palette.selected_bg.h
            );
        }

        // 選択中はどちらのテーマでも「半透明の青」を重ねて、既定のグレーや白に
        // 紛れないようにする（ライトは不透明の淡色だと選択中に見えなかった）
        let dark = ChipPalette::dark();
        let light = ChipPalette::light();
        for (name, palette) in [("light", light), ("dark", dark)] {
            let selected = palette.background(muted, false, true);
            assert!(selected.a < 1.0, "{name}: 選択中の背景は半透明");
            assert!(selected.s > 0.5, "{name}: 選択中の背景は青が十分濃い");
            assert!(
                selected.l < 0.95,
                "{name}: 選択中の背景は白っぽすぎない（l={}）",
                selected.l
            );
        }

        // ダークはお気に入りも半透明でカードの背景になじませ、ライトは不透明のまま
        assert!(dark.favorite_bg.a < 1.0, "ダークのお気に入り背景は半透明");
        assert!(dark.heart_button.a < 1.0, "ダークのハートボタンは半透明");
        assert_eq!(light.favorite_bg.a, 1.0, "ライトのお気に入りは不透明");
        // ダークの文字・ハートは明るい色（暗い背景の上で読める）
        assert!(dark.favorite_text.l > 0.6, "ダークの文字は明るい色");
        assert!(dark.favorite_heart.l > 0.6, "ダークのハートは明るい色");
        assert!(dark.selected_text.l > 0.6, "ダークの選択中文字も明るい色");
    }

    /// 「絞込中」ボタンはタグ選択中と同じ青系（チップの選択色）を使い、
    /// 絞り込み中であることが同じ色の系統で伝わるようにする。
    #[test]
    fn filter_all_button_uses_the_chip_selected_color() {
        let muted = gpui_kit::hsla(0.0, 0.0, 0.5, 1.0);
        for palette in [ChipPalette::light(), ChipPalette::dark()] {
            let (bg, fg, hover, active) = palette.filter_all_colors();
            assert_eq!(
                bg,
                palette.background(muted, false, true),
                "背景はチップの選択中と同じ色"
            );
            assert_eq!(
                fg,
                palette.foreground(muted, false, true),
                "文字はチップの選択中と同じ色"
            );
            assert!(hover.a > bg.a, "ホバーは少し濃く");
            assert!(active.a > hover.a, "押下はさらに濃く");
        }
    }

    #[gpui_kit::test]
    async fn card_rows_left_align_while_grid_is_centered(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        // 800px 幅 → 3 列: 1 行目 db-1..3、2 行目 db-4
        // （tag_editor テストと同じ構成: book id と shelf database_id を分離してリンク）
        for i in 1..=4 {
            seed_book(cx, &format!("b{i}"), &format!("本{i}"), "サークル");
            seed_shelf_item(cx, &format!("db-{i}"), &format!("本{i}"), "サークル", None);
            cx.update(|cx| {
                let state = AppState::global(cx);
                let db = &state.db_pool;
                thundoku_core::db::block_on(async {
                    sqlx::query("UPDATE books SET tbf_product_id = ?1 WHERE id = ?2")
                        .bind(format!("db-{i}"))
                        .bind(format!("b{i}"))
                        .execute(db)
                        .await
                })
                .unwrap();
            });
        }
        let view = cx.new(BookshelfView::new);
        assert_eq!(
            view.read_with(cx, |v, _| v.shelf_cards.len()),
            4,
            "shelf cards must be built from seeded items"
        );
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(800.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        // List（仮想化グリッド）はアイテム測定に複数フレーム必要なため追加描画する
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        // List（仮想化グリッド）はアイテム測定に複数フレーム必要なため追加描画する
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        // カード内のタグ編集ボタン（カード描画の証明）の x 座標で配置を検証する
        let first = visual
            .debug_bounds("tag-edit-db-1")
            .expect("first row card rendered");
        let last = visual
            .debug_bounds("tag-edit-db-4")
            .expect("second row card rendered");
        // 2 行目の先頭は 1 枚目と同じ x（左端）に揃う（行内左寄せ）
        assert!(
            (last.origin.x - first.origin.x).abs() < gpui_kit::px(1.0),
            "second row must start at the same x as the first: {} vs {}",
            last.origin.x.as_f32(),
            first.origin.x.as_f32()
        );
    }

    #[gpui_kit::test]
    async fn tag_filter_uses_or_semantics(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "b1", "本1", "サークルA");
        seed_book(cx, "b2", "本2", "サークルB");
        seed_book(cx, "b3", "本3", "サークルC");
        cx.update(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            db::tags::set_for_book(db, "b1", &[("react", "manual")]).unwrap();
            db::tags::set_for_book(db, "b2", &[("rust", "manual")]).unwrap();
            db::tags::set_for_book(db, "b3", &[("rust", "manual"), ("react", "manual")]).unwrap();
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = 'db-1' WHERE id = 'b1'")
                    .execute(db)
                    .await
            })
            .unwrap();
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = 'db-2' WHERE id = 'b2'")
                    .execute(db)
                    .await
            })
            .unwrap();
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = 'db-3' WHERE id = 'b3'")
                    .execute(db)
                    .await
            })
            .unwrap();
        });
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        seed_shelf_item(cx, "db-2", "本2", "サークルB", None);
        seed_shelf_item(cx, "db-3", "本3", "サークルC", None);
        let view = cx.new(BookshelfView::new);
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.toggle_tag(cx, "react");
                this.toggle_tag(cx, "rust");
            })
        });
        let visible = view.read_with(cx, |this, cx| {
            this.visible_shelf_cards(cx)
                .iter()
                .map(|card| card.shelf.database_id.clone())
                .collect::<Vec<_>>()
        });
        // OR semantics: db-1 (react), db-2 (rust), db-3 (both)
        assert_eq!(
            visible,
            vec!["db-1".to_string(), "db-2".to_string(), "db-3".to_string()]
        );
    }

    #[gpui_kit::test]
    async fn tag_editor_save_and_cancel_do_not_trigger_card_download(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "b1", "本1", "サークルA");
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = 'db-1' WHERE id = 'b1'")
                    .execute(db)
                    .await
            })
            .unwrap();
        });
        let view = cx.new(BookshelfView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(900.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        // List（仮想化グリッド）はアイテム測定に複数フレーム必要なため追加描画する
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }

        // ✎ → 編集モード開始
        let edit = visual
            .debug_bounds("tag-edit-db-1")
            .expect("tag edit button rendered");
        visual.simulate_click(edit.center(), gpui_kit::Modifiers::default());
        assert!(view.read_with(cx, |this, _| this.editing_book_id.is_some()));
        // List（仮想化グリッド）はアイテム測定に複数フレーム必要なため追加描画する
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }

        // エディタの枠（空き領域）をクリックしてもカードのダウンロードは発火しない
        let editor = visual
            .debug_bounds("tag-editor-root")
            .expect("tag editor rendered");
        // 枠の中央付近（チップや入力に当たらない余白）をクリック
        let empty_point = gpui_kit::Point::new(
            editor.origin.x + gpui_kit::px(10.0),
            editor.origin.y + gpui_kit::px(6.0),
        );
        visual.simulate_click(empty_point, gpui_kit::Modifiers::default());
        assert!(
            view.read_with(cx, |this, _| this.download_states.is_empty()),
            "clicking the tag editor must not trigger a card download"
        );
        // 編集モードは継続している
        assert!(
            view.read_with(cx, |this, _| this.editing_book_id.is_some()),
            "clicking the editor must not cancel editing"
        );

        // サジェスチョンクリック → タグ追加（既存フロー）
        let suggestion = visual
            .debug_bounds("edit-suggestion-後で読む")
            .expect("suggestion chip rendered");
        visual.simulate_click(suggestion.center(), gpui_kit::Modifiers::default());
        assert!(
            view.read_with(cx, |this, _| this
                .editing_tags
                .contains(&"後で読む".to_string())),
            "suggestion click must add the tag"
        );

        // キャンセルボタンをクリック → 編集モード解除 + ダウンロード発火しない
        let cancel = visual
            .debug_bounds("tag-edit-cancel-btn")
            .expect("cancel button rendered");
        visual.simulate_click(cancel.center(), gpui_kit::Modifiers::default());
        assert_eq!(
            view.read_with(cx, |this, _| this.editing_book_id.clone()),
            None
        );
        assert!(
            view.read_with(cx, |this, _| this.download_states.is_empty()),
            "cancel button must not trigger a card download"
        );
    }

    #[gpui_kit::test]
    async fn tag_editor_save_button_persists_tags(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "b1", "本1", "サークルA");
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = 'db-1' WHERE id = 'b1'")
                    .execute(db)
                    .await
            })
            .unwrap();
        });
        let view = cx.new(BookshelfView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(900.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        // List（仮想化グリッド）はアイテム測定に複数フレーム必要なため追加描画する
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }

        // ✎ → 編集モード → サジェスチョンでタグ追加 → 保存ボタン
        let edit = visual
            .debug_bounds("tag-edit-db-1")
            .expect("tag edit button rendered");
        visual.simulate_click(edit.center(), gpui_kit::Modifiers::default());
        // List（仮想化グリッド）はアイテム測定に複数フレーム必要なため追加描画する
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        let suggestion = visual
            .debug_bounds("edit-suggestion-後で読む")
            .expect("suggestion chip rendered");
        visual.simulate_click(suggestion.center(), gpui_kit::Modifiers::default());
        let save = visual
            .debug_bounds("tag-edit-save-btn")
            .expect("save button rendered");
        visual.simulate_click(save.center(), gpui_kit::Modifiers::default());

        // 保存される + 編集モード解除 + ダウンロード発火しない
        let stored = cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            db::tags::list_for_book(db, "b1").unwrap_or_default()
        });
        assert!(
            stored
                .iter()
                .any(|t| t.tag_name == "後で読む" && t.source == "manual"),
            "save button must persist the tag"
        );
        assert_eq!(
            view.read_with(cx, |this, _| this.editing_book_id.clone()),
            None
        );
        assert!(
            view.read_with(cx, |this, _| this.download_states.is_empty()),
            "save button must not trigger a card download"
        );
    }

    #[gpui_kit::test]
    async fn tag_edit_click_does_not_trigger_card_download(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "b1", "本1", "サークルA");
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = 'db-1' WHERE id = 'b1'")
                    .execute(db)
                    .await
            })
            .unwrap();
        });
        let view = cx.new(BookshelfView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(900.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        // List（仮想化グリッド）はアイテム測定に複数フレーム必要なため追加描画する
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }

        // ✎ をクリック → 編集モードになり、カードのクリック（ダウンロード）は発火しない
        let edit = visual
            .debug_bounds("tag-edit-db-1")
            .expect("tag edit button rendered");
        visual.simulate_click(edit.center(), gpui_kit::Modifiers::default());
        assert_eq!(
            view.read_with(cx, |this, _| this.editing_book_id.clone()),
            Some("db-1".to_string()),
            "edit button click must start inline editing"
        );
        assert!(
            view.read_with(cx, |this, _| this.download_states.is_empty()),
            "card click (download) must not fire from the edit button"
        );
    }

    #[gpui_kit::test]
    async fn inline_tag_edit_adds_suggestion_and_saves(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "b1", "本1", "サークルA");
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = 'db-1' WHERE id = 'b1'")
                    .execute(db)
                    .await
            })
            .unwrap();
        });
        let view = cx.new(BookshelfView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(900.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        // List（仮想化グリッド）はアイテム測定に複数フレーム必要なため追加描画する
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }

        // ✎ クリック → インライン編集モード開始
        let edit = visual
            .debug_bounds("tag-edit-db-1")
            .expect("tag edit button rendered");
        visual.simulate_click(edit.center(), gpui_kit::Modifiers::default());
        assert_eq!(
            view.read_with(cx, |this, _| this.editing_book_id.clone()),
            Some("db-1".to_string())
        );
        // List（仮想化グリッド）はアイテム測定に複数フレーム必要なため追加描画する
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }

        // サジェスチョン（後で読む）クリック → タグに追加される
        let suggestion = visual
            .debug_bounds("edit-suggestion-後で読む")
            .expect("suggestion chip rendered");
        visual.simulate_click(suggestion.center(), gpui_kit::Modifiers::default());
        assert!(
            view.read_with(cx, |this, _| this
                .editing_tags
                .contains(&"後で読む".to_string())),
            "suggestion click must add the tag"
        );

        // 保存 → DB に manual タグが保存される
        cx.update(|cx| view.update(cx, |this, cx| this.save_tag_edit(cx)));
        let stored = cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            db::tags::list_for_book(db, "b1").unwrap_or_default()
        });
        assert!(
            stored
                .iter()
                .any(|t| t.tag_name == "後で読む" && t.source == "manual"),
            "saved tag must be stored in book_tags"
        );
        // 編集モードが解除される
        assert_eq!(
            view.read_with(cx, |this, _| this.editing_book_id.clone()),
            None
        );
    }

    #[gpui_kit::test]
    async fn last_page_marks_book_as_read(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "b1", "本1", "サークルA");
        seed_book(cx, "b2", "本2", "サークルB");
        seed_progress(cx, "b1", 10, Some(10)); // 最終ページ表示済み（1-indexed 保存）
        seed_progress(cx, "b2", 8, Some(10)); // 途中
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = 'db-1' WHERE id = 'b1'")
                    .execute(db)
                    .await
            })
            .unwrap();
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = 'db-2' WHERE id = 'b2'")
                    .execute(db)
                    .await
            })
            .unwrap();
        });
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        seed_shelf_item(cx, "db-2", "本2", "サークルB", None);
        let view = cx.new(BookshelfView::new);
        // 最終ページ（index 10 / total 10、1-indexed）まで表示した本は読了
        let read = view.read_with(cx, |this, _| {
            this.shelf_cards
                .iter()
                .find(|c| c.shelf.database_id == "db-1")
                .and_then(|c| c.local.as_ref())
                .map(|e| e.is_read)
                .unwrap_or(false)
        });
        assert!(read, "last page must mark the book as read");
        let unread = view.read_with(cx, |this, _| {
            this.shelf_cards
                .iter()
                .find(|c| c.shelf.database_id == "db-2")
                .and_then(|c| c.local.as_ref())
                .map(|e| e.is_read)
                .unwrap_or(true)
        });
        assert!(!unread, "mid-book must stay unread");
    }

    #[gpui_kit::test]
    async fn event_list_sorts_latest_techbookfest_first(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        for (id, event) in [
            ("db-1", "技術書典10"),
            ("db-2", "技術書典20"),
            ("db-3", "技術書典2"),
            ("db-4", "夏コミ"),
        ] {
            seed_shelf_item_with_event(cx, id, "本", "サークル", event);
        }
        let view = cx.new(BookshelfView::new);
        let events = view.read_with(cx, |this, _| this.available_events.clone());
        assert_eq!(
            events,
            vec![
                "技術書典20".to_string(),
                "技術書典10".to_string(),
                "技術書典2".to_string(),
                "夏コミ".to_string(),
            ],
            "latest techbookfest event must come first"
        );
    }

    #[gpui_kit::test]
    async fn event_name_matched_by_purchase_date(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        // 開催日付きイベント（tbf19: 2025-11-15〜11-30）
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            db::checklist::upsert_event(
                db,
                &db::checklist::TbfEvent {
                    id: "tbf19".into(),
                    site_id: "techbookfest".into(),
                    slug: Some("tbf19".into()),
                    tbf_event_id: None,
                    event_name: "技術書典19".into(),
                    event_date: Some("2025-11-15".into()),
                    event_start_date: Some("2025-11-15".into()),
                    event_end_date: Some("2025-11-30".into()),
                    event_format: "hybrid".into(),
                    is_cancelled: 0,
                    display_order: 0,
                    is_featured: 0,
                    poll_sync_enabled: 0,
                    created_at: "2026-08-21 00:00:00".into(),
                    updated_at: "2026-08-21 00:00:00".into(),
                },
            )
            .unwrap();
        });
        // event_name なし・caused_at が開催期間内の本
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            bookshelf::upsert(
                db,
                &bookshelf::BookshelfItem {
                    site_id: "techbookfest".into(),
                    database_id: "db-1".into(),
                    title: "本1".into(),
                    circle_name: "サークルA".into(),
                    author: String::new(),
                    thumbnail_url: None,
                    format: "PDF".into(),
                    caused_at: Some("2025-11-20T10:00:00".into()),
                    event_name: None,
                    event_slug: None,
                    event_id: None,
                    file_name: None,
                    download_url: None,
                    is_downloadable: 1,
                    is_checked: 0,
                    is_purchased: 1,
                    is_new: 0,
                    is_active: 1,
                    is_favorite: 0,
                    is_hidden: 0,
                    hidden_at: None,
                    tags_json: None,
                    synced_at: "2026-08-21 00:00:00".into(),
                    created_at: "2026-08-21 00:00:00".into(),
                    updated_at: "2026-08-21 00:00:00".into(),
                    media_category: None,
                    ai_type: None,
                    is_drm: 0,
                    release_date: None,
                    description: None,
                    theme: None,
                    maker_id: None,
                    page_count: None,
                    age_rating: None,
                    series_name: None,
                },
            )
            .unwrap();
        });
        let view = cx.new(BookshelfView::new);
        let event_name = view.read_with(cx, |this, _| {
            this.shelf_cards
                .iter()
                .find(|c| c.shelf.database_id == "db-1")
                .and_then(|c| c.shelf.event_name.clone())
        });
        assert_eq!(
            event_name.as_deref(),
            Some("技術書典19"),
            "purchase date inside the event window must match the event"
        );
    }

    #[gpui_kit::test]
    async fn reload_shows_page_count_from_document(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "b1", "本1", "サークルA");
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = 'db-1' WHERE id = 'b1'")
                    .execute(db)
                    .await
            })
            .unwrap();
            // インポート済みドキュメント（total_pages 65）を作る
            documents::insert_document(
                db,
                &documents::ImportedDocument {
                    id: "doc-1".into(),
                    book_id: "b1".into(),
                    source_type: "pdf".into(),
                    file_hash: "hash".into(),
                    total_pages: 65,
                    metadata: None,
                    status: "completed".into(),
                    created_at: "2026-08-21 00:00:00".into(),
                    updated_at: "2026-08-21 00:00:00".into(),
                },
            )
            .unwrap();
        });
        let view = cx.new(BookshelfView::new);
        // reading_progress が無くても、document の total_pages から表示用進捗が出る
        let progress = view.read_with(cx, |this, _| {
            this.shelf_cards
                .iter()
                .find(|c| c.shelf.database_id == "db-1")
                .and_then(|c| c.local.as_ref())
                .and_then(|e| e.progress)
        });
        assert_eq!(progress, Some((1, Some(65))));
    }

    #[gpui_kit::test]
    async fn reload_completes_event_name_from_tbf_events(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        // tbf_events に tbf18 のイベント名を登録
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            db::checklist::upsert_event(
                db,
                &db::checklist::TbfEvent {
                    id: "e1".into(),
                    site_id: "techbookfest".into(),
                    slug: Some("tbf18".into()),
                    tbf_event_id: None,
                    event_name: "技術書典18".into(),
                    event_date: None,
                    event_start_date: None,
                    event_end_date: None,
                    event_format: "offline".into(),
                    is_cancelled: 0,
                    display_order: 0,
                    is_featured: 0,
                    poll_sync_enabled: 0,
                    created_at: "2026-08-21 00:00:00".into(),
                    updated_at: "2026-08-21 00:00:00".into(),
                },
            )
            .unwrap();
        });
        // event_name なし・event_slug ありの本棚アイテム
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            bookshelf::upsert(
                db,
                &bookshelf::BookshelfItem {
                    site_id: "techbookfest".into(),
                    database_id: "db-1".into(),
                    title: "本1".into(),
                    circle_name: "サークルA".into(),
                    author: String::new(),
                    thumbnail_url: None,
                    format: "PDF".into(),
                    caused_at: None,
                    event_name: None,
                    event_slug: Some("tbf18".into()),
                    event_id: None,
                    file_name: None,
                    download_url: None,
                    is_downloadable: 1,
                    is_checked: 0,
                    is_purchased: 1,
                    is_new: 0,
                    is_active: 1,
                    is_favorite: 0,
                    is_hidden: 0,
                    hidden_at: None,
                    tags_json: None,
                    synced_at: "2026-08-21 00:00:00".into(),
                    created_at: "2026-08-21 00:00:00".into(),
                    updated_at: "2026-08-21 00:00:00".into(),
                    media_category: None,
                    ai_type: None,
                    is_drm: 0,
                    release_date: None,
                    description: None,
                    theme: None,
                    maker_id: None,
                    page_count: None,
                    age_rating: None,
                    series_name: None,
                },
            )
            .unwrap();
        });
        let view = cx.new(BookshelfView::new);
        // イベント名が tbf_events から補完される
        let event_name = view.read_with(cx, |this, _| {
            this.shelf_cards
                .iter()
                .find(|c| c.shelf.database_id == "db-1")
                .and_then(|c| c.shelf.event_name.clone())
        });
        assert_eq!(event_name.as_deref(), Some("技術書典18"));
    }

    #[gpui_kit::test]
    async fn heart_click_registers_favorite_tag(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "b1", "本1", "サークルA");
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = 'db-1' WHERE id = 'b1'")
                    .execute(db)
                    .await
            })
            .unwrap();
            db::tags::set_for_book(db, "b1", &[("後で読む", "manual")]).unwrap();
        });
        let view = cx.new(BookshelfView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(900.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        // List（仮想化グリッド）はアイテム測定に複数フレーム必要なため追加描画する
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        // タグチップの ♥ をクリック → お気に入り登録される（絞り込みは発火しない）
        let heart = visual
            .debug_bounds("tag-heart-db-1-後で読む")
            .expect("tag heart rendered");
        visual.simulate_click(heart.center(), gpui_kit::Modifiers::default());
        assert!(
            view.read_with(cx, |this, _| this
                .favorite_tags
                .contains(&"後で読む".to_string())),
            "heart click must add the tag to favorites"
        );
        assert!(
            view.read_with(cx, |this, _| this.selected_tags.is_empty()),
            "heart click must not filter by the tag"
        );
        // DB にも保存される
        let stored = cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            db::tags::list_favorites(db).unwrap_or_default()
        });
        assert_eq!(stored, vec!["後で読む".to_string()]);
    }

    /// タグ文字のクリックでそのタグに絞り込み、全項目ボタンが「絞込中」になる。
    #[gpui_kit::test]
    async fn tag_text_click_filters_by_tag_and_shows_refining(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "b1", "本1", "サークルA");
        seed_book(cx, "b2", "本2", "サークルB");
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            db::tags::set_for_book(db, "b1", &[("react", "manual")]).unwrap();
            db::tags::set_for_book(db, "b2", &[("rust", "manual")]).unwrap();
            for (id, product) in [("b1", "db-1"), ("b2", "db-2")] {
                thundoku_core::db::block_on(async {
                    sqlx::query("UPDATE books SET tbf_product_id = ?1 WHERE id = ?2")
                        .bind(product)
                        .bind(id)
                        .execute(db)
                        .await
                })
                .unwrap();
            }
        });
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        seed_shelf_item(cx, "db-2", "本2", "サークルB", None);
        let view = cx.new(BookshelfView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(900.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }

        // タグ文字のクリック → そのタグで絞り込み
        let label = visual
            .debug_bounds("tag-label-db-1-react")
            .expect("tag label rendered");
        visual.simulate_click(label.center(), gpui_kit::Modifiers::default());
        assert_eq!(
            view.read_with(cx, |this, _| this.selected_tags.clone()),
            vec!["react".to_string()],
            "tag text click must select the tag as a filter"
        );
        assert_eq!(
            view.read_with(cx, |this, cx| this.filter_all_label(cx)),
            "絞込中 ✕",
            "タグ絞り込み中は全項目が絞込中になる"
        );
        let visible = view.read_with(cx, |this, cx| {
            this.visible_shelf_cards(cx)
                .iter()
                .map(|card| card.shelf.database_id.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(
            visible,
            vec!["db-1".to_string()],
            "react の本だけ表示される"
        );
        // カードのダウンロード（クリック伝播）は発火しない
        assert!(
            view.read_with(cx, |this, _| this.download_states.is_empty()),
            "clicking the tag must not trigger a card download"
        );

        // 同じタグの再クリックで解除
        visual.simulate_click(label.center(), gpui_kit::Modifiers::default());
        assert!(
            view.read_with(cx, |this, _| this.selected_tags.is_empty()),
            "re-click must clear the tag filter"
        );
        assert_eq!(
            view.read_with(cx, |this, cx| this.filter_all_label(cx)),
            "全項目"
        );
    }

    /// タグチップのお気に入りハートは**タグ文字の後ろ**に置き、押しやすいように
    /// 丸ボタン（16px 以上）にして文字から少し離す。
    #[gpui_kit::test]
    async fn tag_heart_is_a_separated_round_button_after_the_tag_text(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "b1", "本1", "サークルA");
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = 'db-1' WHERE id = 'b1'")
                    .execute(db)
                    .await
            })
            .unwrap();
            db::tags::set_for_book(db, "b1", &[("後で読む", "manual")]).unwrap();
        });
        let view = cx.new(BookshelfView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(900.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        let label = visual
            .debug_bounds("tag-label-db-1-後で読む")
            .expect("tag label rendered");
        let heart = visual
            .debug_bounds("tag-heart-db-1-後で読む")
            .expect("tag heart rendered");
        assert!(
            heart.origin.x >= label.origin.x + label.size.width,
            "ハートはタグ文字の後ろに置くこと"
        );
        assert!(
            heart.size.width >= gpui_kit::px(16.0) && heart.size.height >= gpui_kit::px(16.0),
            "ハートは押しやすい丸ボタン（16px 以上）にすること（現在 {:?}）",
            heart.size
        );
        let gap = heart.origin.x - (label.origin.x + label.size.width);
        assert!(
            gap >= gpui_kit::px(6.0),
            "ハートは文字から少し離すこと（現在 {gap:?}）"
        );
        // アイコンは丸の中心に置く（文字グリフのフォント依存のズレを避ける）
        let icon = visual
            .debug_bounds("tag-heart-db-1-後で読む-icon")
            .expect("heart icon rendered");
        assert!(
            (icon.center().x - heart.center().x).as_f32().abs() < 1.0
                && (icon.center().y - heart.center().y).as_f32().abs() < 1.0,
            "ハートアイコンは丸の中心に置くこと（ズレ {:?} / {:?}）",
            (icon.center().x - heart.center().x).as_f32(),
            (icon.center().y - heart.center().y).as_f32()
        );
    }

    /// サークル名 / 作者名はタグと同じチップ表示で、右端のハートでお気に入りにできる。
    /// ハートのクリックは絞り込み（本体クリック）を発火させない。
    #[gpui_kit::test]
    async fn entity_chip_hearts_toggle_circle_and_author_favorites(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_shelf_item_with_author(cx, "db-1", "本1", "サークルA", "作者X");
        let view = cx.new(BookshelfView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(900.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }

        // ラベル（サークル: / 作者:）はタグ化しない = チップの外に置く
        let label = visual
            .debug_bounds("circle-label-db-1")
            .expect("circle label rendered");
        let chip = visual
            .debug_bounds("circle-chip-db-1")
            .expect("circle chip rendered");
        assert!(
            label.origin.x + label.size.width <= chip.origin.x,
            "ラベル（サークル:）はチップの外に置くこと"
        );

        // ハートはチップ文字の後ろにある
        let body = visual
            .debug_bounds("circle-link-db-1")
            .expect("circle link rendered");
        let heart = visual
            .debug_bounds("circle-heart-db-1")
            .expect("circle heart rendered");
        assert!(
            heart.origin.x >= body.origin.x + body.size.width,
            "ハートはサークル名の後ろに置くこと"
        );
        assert!(
            heart.size.width >= gpui_kit::px(16.0) && heart.size.height >= gpui_kit::px(16.0),
            "ハートは押しやすい丸ボタン（16px 以上）にすること（現在 {:?}）",
            heart.size
        );
        assert!(
            heart.origin.x - (body.origin.x + body.size.width) >= gpui_kit::px(6.0),
            "ハートはサークル名から少し離すこと"
        );
        let icon = visual
            .debug_bounds("circle-heart-db-1-icon")
            .expect("circle heart icon rendered");
        assert!(
            (icon.center().x - heart.center().x).as_f32().abs() < 1.0
                && (icon.center().y - heart.center().y).as_f32().abs() < 1.0,
            "ハートアイコンは丸の中心に置くこと"
        );

        // クリックでお気に入り登録（絞り込みは発火しない）
        visual.simulate_click(heart.center(), gpui_kit::Modifiers::default());
        assert!(
            view.read_with(cx, |this, _| this
                .favorite_circles
                .contains(&"サークルA".to_string())),
            "circle heart click must add the circle to favorites"
        );
        assert!(
            view.read_with(cx, |this, _| this.circle_filter.is_none()),
            "heart click must not trigger the circle filter"
        );
        let stored = cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            db::favorites::list_favorites(db, db::favorites::EntityKind::Circle).unwrap_or_default()
        });
        assert_eq!(stored, vec!["サークルA".to_string()]);

        // もう一度クリックで解除
        visual.simulate_click(heart.center(), gpui_kit::Modifiers::default());
        assert!(
            !view.read_with(cx, |this, _| this
                .favorite_circles
                .contains(&"サークルA".to_string())),
            "re-click must remove the circle from favorites"
        );

        // 作者も同じ方式
        let author_heart = visual
            .debug_bounds("author-heart-db-1")
            .expect("author heart rendered");
        visual.simulate_click(author_heart.center(), gpui_kit::Modifiers::default());
        assert!(
            view.read_with(cx, |this, _| this
                .favorite_authors
                .contains(&"作者X".to_string())),
            "author heart click must add the author to favorites"
        );
        let stored = cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            db::favorites::list_favorites(db, db::favorites::EntityKind::Author).unwrap_or_default()
        });
        assert_eq!(stored, vec!["作者X".to_string()]);
    }

    #[gpui_kit::test]
    async fn tag_filter_popover_toggles_tag_selection(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        // お気に入りタグと本を seed
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            db::tags::set_favorite(db, "react", true).unwrap();
        });
        seed_book(cx, "b1", "本1", "サークルA");
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            db::tags::set_for_book(db, "b1", &[("react", "manual")]).unwrap();
        });
        let view = cx.new(BookshelfView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(900.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        // List（仮想化グリッド）はアイテム測定に複数フレーム必要なため追加描画する
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }

        // タグフィルタボタンをクリック → ポップオーバーが開く
        let trigger = visual
            .debug_bounds("tag-filter-trigger")
            .expect("tag filter trigger rendered");
        visual.simulate_click(trigger.center(), gpui_kit::Modifiers::default());
        // List（仮想化グリッド）はアイテム測定に複数フレーム必要なため追加描画する
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }

        // チップが表示され、クリックでタグが選択される
        let chip = visual
            .debug_bounds("tag-option-react")
            .expect("tag chip should be visible in popover");
        visual.simulate_click(chip.center(), gpui_kit::Modifiers::default());
        assert!(
            view.read_with(cx, |this, _| this
                .selected_tags
                .contains(&"react".to_string())),
            "clicking the tag chip must select the tag"
        );

        // もう一度クリックで解除
        visual.simulate_click(chip.center(), gpui_kit::Modifiers::default());
        assert!(
            !view.read_with(cx, |this, _| this
                .selected_tags
                .contains(&"react".to_string())),
            "clicking the selected tag chip must deselect it"
        );
    }

    /// お気に入りタグ一覧（フィルタのポップオーバー）も、よく使うタグが先頭に来る。
    #[gpui_kit::test]
    async fn tag_filter_popover_orders_favorites_by_usage(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            db::tags::set_favorite(db, "あか", true).unwrap();
            db::tags::set_favorite(db, "いか", true).unwrap();
        });
        // 「いか」を 2 冊が持つので、名前順（あか → いか）ではなく集計数順で先頭になる
        for (book, shelf) in [("b1", "db-1"), ("b2", "db-2")] {
            seed_book(cx, book, shelf, "サークルA");
            seed_shelf_item(cx, shelf, shelf, "サークルA", None);
            link_book_to_shelf(cx, book, shelf);
        }
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            db::tags::set_for_book(db, "b1", &[("あか", "manual"), ("いか", "manual")]).unwrap();
            db::tags::set_for_book(db, "b2", &[("いか", "manual")]).unwrap();
        });
        let view = cx.new(BookshelfView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(900.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        let draw = |visual: &mut gpui_kit::VisualTestContext| {
            for _ in 0..4 {
                visual.update(|window, cx| {
                    let arena_clear = window.draw(cx);
                    arena_clear.clear(cx);
                });
            }
        };
        draw(visual);
        let trigger = visual
            .debug_bounds("tag-filter-trigger")
            .expect("tag filter trigger rendered");
        visual.simulate_click(trigger.center(), gpui_kit::Modifiers::default());
        draw(visual);
        let used = visual
            .debug_bounds("tag-option-いか")
            .expect("いかのチップが出ている");
        let unused = visual
            .debug_bounds("tag-option-あか")
            .expect("あかのチップが出ている");
        assert!(
            (used.origin.y, used.origin.x) < (unused.origin.y, unused.origin.x),
            "集計数が多いお気に入りタグが先頭に来ていない: {} vs {}",
            used.origin.x.as_f32(),
            unused.origin.x.as_f32()
        );
    }

    #[gpui_kit::test]
    async fn clicking_circle_link_filters_and_all_button_clears(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        seed_shelf_item(cx, "db-2", "本2", "サークルB", None);
        let view = cx.new(BookshelfView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(900.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }

        // サークル名リンクをクリック → そのサークルの本だけになる
        let link = visual
            .debug_bounds("circle-link-db-1")
            .expect("circle link rendered");
        visual.simulate_click(link.center(), gpui_kit::Modifiers::default());
        let visible = view.read_with(cx, |this, cx| {
            this.visible_shelf_cards(cx)
                .iter()
                .map(|card| card.shelf.database_id.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(visible, vec!["db-1".to_string()], "サークル絞り込みが効く");
        assert_eq!(
            view.read_with(cx, |this, cx| this.filter_all_label(cx)),
            "絞込中 ✕",
            "絞り込み中は全項目ボタンが絞込中になる"
        );

        // 全項目ボタン → 絞り込み解除で全件に戻る
        let all = visual
            .debug_bounds("filter-all-btn")
            .expect("filter-all button rendered");
        visual.simulate_click(all.center(), gpui_kit::Modifiers::default());
        let visible = view.read_with(cx, |this, cx| {
            this.visible_shelf_cards(cx)
                .iter()
                .map(|card| card.shelf.database_id.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(
            visible,
            vec!["db-1".to_string(), "db-2".to_string()],
            "全項目で解除される"
        );
        assert_eq!(
            view.read_with(cx, |this, cx| this.filter_all_label(cx)),
            "全項目"
        );
    }

    #[gpui_kit::test]
    async fn clicking_author_link_filters_by_author(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_shelf_item_with_author(cx, "db-1", "本1", "サークルA", "作者X");
        seed_shelf_item_with_author(cx, "db-2", "本2", "サークルB", "作者Y");
        let view = cx.new(BookshelfView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(900.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }

        let link = visual
            .debug_bounds("author-link-db-1")
            .expect("author link rendered");
        visual.simulate_click(link.center(), gpui_kit::Modifiers::default());
        let visible = view.read_with(cx, |this, cx| {
            this.visible_shelf_cards(cx)
                .iter()
                .map(|card| card.shelf.database_id.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(visible, vec!["db-1".to_string()], "作者絞り込みが効く");
        assert_eq!(
            view.read_with(cx, |this, cx| this.filter_all_label(cx)),
            "絞込中 ✕",
            "作者絞り込み中は全項目が絞込中になる"
        );

        // 同じリンクの再クリックで解除
        visual.simulate_click(link.center(), gpui_kit::Modifiers::default());
        let visible = view.read_with(cx, |this, cx| {
            this.visible_shelf_cards(cx)
                .iter()
                .map(|card| card.shelf.database_id.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(visible.len(), 2, "再クリックで解除される");
    }

    #[gpui_kit::test]
    async fn all_filter_button_shows_refining_and_clears_tag_filter(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "b1", "本1", "サークルA");
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            db::tags::set_for_book(db, "b1", &[("react", "manual")]).unwrap();
            db::tags::set_favorite(db, "react", true).unwrap();
        });
        let view = cx.new(BookshelfView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(900.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }

        cx.update(|cx| {
            view.update(cx, |this, cx| this.toggle_tag(cx, "react"));
        });
        assert_eq!(
            view.read_with(cx, |this, cx| this.filter_all_label(cx)),
            "絞込中 ✕",
            "タグ絞り込み中も絞込中になる"
        );

        let all = visual
            .debug_bounds("filter-all-btn")
            .expect("filter-all button rendered");
        visual.simulate_click(all.center(), gpui_kit::Modifiers::default());
        assert!(
            view.read_with(cx, |this, _| this.selected_tags.is_empty()),
            "全項目でタグ絞り込みが解除される"
        );
        assert_eq!(
            view.read_with(cx, |this, cx| this.filter_all_label(cx)),
            "全項目"
        );
    }

    /// 絞り込み中に ESC を押すとすべての絞り込みが解除される。
    #[gpui_kit::test]
    async fn escape_clears_active_filters(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        let view = cx.new(BookshelfView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(900.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }

        // タグ + サークル + 既読モードで絞り込む
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.toggle_tag(cx, "react");
                this.toggle_circle_filter(cx, "サークルA");
                this.read_filter = ReadFilter::Favorite;
            })
        });
        assert_eq!(
            view.read_with(cx, |this, cx| this.filter_all_label(cx)),
            "絞込中 ✕"
        );

        visual.simulate_event(gpui_kit::KeyDownEvent {
            keystroke: gpui_kit::Keystroke::parse("escape").unwrap(),
            is_held: false,
            prefer_character_input: false,
        });

        assert!(
            view.read_with(cx, |this, _| this.selected_tags.is_empty()),
            "ESC でタグ絞り込みが解除される"
        );
        assert!(
            view.read_with(cx, |this, _| this.circle_filter.is_none()),
            "ESC でサークル絞り込みが解除される"
        );
        assert!(
            view.read_with(cx, |this, _| this.read_filter == ReadFilter::All),
            "ESC で既読モードが解除される"
        );
        assert_eq!(
            view.read_with(cx, |this, cx| this.filter_all_label(cx)),
            "全項目",
            "ESC で全項目に戻る"
        );
    }

    #[gpui_kit::test]
    async fn event_filter_popover_toggles_event_selection(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_shelf_item_with_event(cx, "db-1", "本1", "サークルA", "技術書典18");
        let view = cx.new(BookshelfView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(900.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        // List（仮想化グリッド）はアイテム測定に複数フレーム必要なため追加描画する
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }

        // イベントフィルタボタンをクリック → ポップオーバーが開く
        let trigger = visual
            .debug_bounds("event-filter-trigger")
            .expect("event filter trigger rendered");
        visual.simulate_click(trigger.center(), gpui_kit::Modifiers::default());
        // List（仮想化グリッド）はアイテム測定に複数フレーム必要なため追加描画する
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }

        // イベント行をクリックで選択される
        let option = visual
            .debug_bounds("event-option-技術書典18")
            .expect("event option should be visible in popover");
        visual.simulate_click(option.center(), gpui_kit::Modifiers::default());
        assert_eq!(
            view.read_with(cx, |this, _| this.selected_events.clone()),
            vec!["技術書典18".to_string()],
            "clicking the event row must select the event"
        );
    }

    /// サイト選択（サイドバーのスコープ）は「絞込中」に数えない。
    /// 全項目 / ESC はフィルタだけを解除し、サイトは変更しない。
    #[gpui_kit::test]
    async fn site_selection_is_not_counted_as_filtering(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_shelf_item_for_site(cx, "fanza", "db-1", "FANZA本", "サークルA");
        seed_shelf_item_for_site(cx, "techbookfest", "db-2", "技術書典本", "サークルB");
        let view = cx.new(BookshelfView::new);
        cx.update(|cx| view.update(cx, |this, cx| this.set_site_filter(cx, Some("fanza"))));

        // サイトを選んだだけでは絞込中にしない
        assert_eq!(
            view.read_with(cx, |this, cx| this.filter_all_label(cx)),
            "全項目",
            "サイト選択だけでは絞込中にしない"
        );

        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(900.0),
                height: gpui_kit::px(600.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }

        // タグを選ぶと絞込中
        cx.update(|cx| view.update(cx, |this, cx| this.toggle_tag(cx, "react")));
        assert_eq!(
            view.read_with(cx, |this, cx| this.filter_all_label(cx)),
            "絞込中 ✕"
        );

        // 全項目ボタン → タグは解除、サイト（スコープ）は維持
        let all = visual
            .debug_bounds("filter-all-btn")
            .expect("filter-all button rendered");
        visual.simulate_click(all.center(), gpui_kit::Modifiers::default());
        assert!(
            view.read_with(cx, |this, _| this.selected_tags.is_empty()),
            "全項目でタグ絞り込みは解除される"
        );
        assert_eq!(
            view.read_with(cx, |this, _| this.site_filter.clone()),
            Some("fanza".to_string()),
            "全項目ボタンはサイト（スコープ）を変更しない"
        );
        let visible = view.read_with(cx, |this, cx| {
            this.visible_shelf_cards(cx)
                .iter()
                .map(|card| card.shelf.database_id.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(visible, vec!["db-1".to_string()], "FANZA の本のまま");
        assert_eq!(
            view.read_with(cx, |this, cx| this.filter_all_label(cx)),
            "全項目"
        );
    }

    /// 個別サイトへ切り替えると、そのサイトでは無効になり得る絞り込み
    /// （タグ / サークル / 作者 / イベント）は解除する。サイト非依存の既読モードは
    /// 維持し、「すべての本」へ戻すときは何も解除しない。
    #[gpui_kit::test]
    async fn switching_site_clears_site_scoped_filters(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_shelf_item(cx, "db-1", "FANZA本", "サークルA", None);
        seed_shelf_item(cx, "db-2", "技術書典本", "サークルB", None);
        let view = cx.new(BookshelfView::new);

        // FANZA でタグ / サークル / 既読モードを絞り込む
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.set_site_filter(cx, Some("fanza"));
                this.toggle_tag(cx, "react");
                this.toggle_circle_filter(cx, "サークルA");
                this.set_read_filter(cx, ReadFilter::Unread);
            })
        });
        assert!(!view.read_with(cx, |this, _| this.selected_tags.is_empty()));

        // 技術書典へ切り替え → サイト依存の絞り込みは解除される
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.set_site_filter(cx, Some("techbookfest"))
            })
        });
        assert!(
            view.read_with(cx, |this, _| this.selected_tags.is_empty()),
            "サイトを切り替えたらタグ絞り込みは解除される"
        );
        assert!(
            view.read_with(cx, |this, _| this.circle_filter.is_none()),
            "サークル絞り込みも解除される"
        );
        assert!(view.read_with(cx, |this, _| this.author_filter.is_none()));
        assert!(view.read_with(cx, |this, _| this.selected_events.is_empty()));
        assert!(
            view.read_with(cx, |this, _| this.read_filter == ReadFilter::Unread),
            "サイトに依存しない既読モードは維持する"
        );

        // 「すべての本」へ戻すときは何も解除しない
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.toggle_tag(cx, "react");
                this.set_site_filter(cx, None);
            })
        });
        assert!(
            view.read_with(cx, |this, _| this
                .selected_tags
                .contains(&"react".to_string())),
            "すべての本では絞り込みを維持する"
        );
    }

    #[gpui_kit::test]
    async fn site_filter_filters_bookshelf_items(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        // 技術書典の本 2 冊 + ローカルだけの本 1 冊（インポート済み）
        seed_shelf_item(cx, "s1", "本A", "サークルA", None);
        seed_shelf_item(cx, "s2", "本B", "サークルB", None);
        cx.update(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            books::insert(
                db,
                &books::Book {
                    id: "local-1".into(),
                    title: "ローカル本".into(),
                    author: String::new(),
                    circle_name: "サークルC".into(),
                    purchase_date: None,
                    file_name: "local-1.pdf".into(),
                    file_size: 10,
                    opfs_path: "local-1.opfspack".into(),
                    cover_thumbnail: None,
                    tbf_product_id: None,
                    site_id: None,
                    tags_fetched: 0,
                    pack_id: Some("local-1".into()),
                    is_favorite: 0,
                    is_hidden: 0,
                    created_at: "2026-08-21 00:00:00".into(),
                    updated_at: "2026-08-23 00:00:00".into(),
                    media_category: None,
                    ai_type: None,
                    is_drm: 0,
                    release_date: None,
                    description: None,
                    theme: None,
                    maker_id: None,
                    page_count: None,
                    age_rating: None,
                    series_name: None,
                },
            )
            .unwrap();
        });
        let view = cx.new(BookshelfView::new);

        // すべての本: 技術書典 2 冊 + ローカル 1 冊 = 3 件
        let all = view.read_with(cx, |v, cx| v.visible_shelf_cards(cx).len());
        assert_eq!(all, 3, "all books must include techbookfest + local");
        // 技術書典: 技術書典の 2 冊のみ（ローカルは除外）
        cx.update(|cx| view.update(cx, |v, cx| v.set_site_filter(cx, Some("techbookfest"))));
        let tbf = view.read_with(cx, |v, cx| v.visible_shelf_cards(cx).len());
        assert_eq!(tbf, 2, "techbookfest filter must show only tbf items");
    }

    #[gpui_kit::test]
    async fn event_filter_filters_by_selected_event(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_shelf_item_with_event(cx, "db-1", "本1", "サークルA", "技術書典17");
        seed_shelf_item_with_event(cx, "db-2", "本2", "サークルB", "技術書典18");
        let view = cx.new(BookshelfView::new);

        // available_events は reload で全イベントから構築される
        let events = view.read_with(cx, |this, _| this.available_events.clone());
        // 技術書典N は番号の降順（最新 = 技術書典18 が先）
        assert_eq!(
            events,
            vec!["技術書典18".to_string(), "技術書典17".to_string()]
        );

        // 未選択なら全件表示
        assert_eq!(
            view.read_with(cx, |this, cx| this.visible_shelf_cards(cx).len()),
            2
        );

        // イベント選択で絞り込まれる
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.selected_events.push("技術書典18".to_string());
                cx.notify();
            })
        });
        let visible = view.read_with(cx, |this, cx| {
            this.visible_shelf_cards(cx)
                .iter()
                .map(|card| card.shelf.database_id.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(visible, vec!["db-2".to_string()]);

        // 選択解除で全件に戻る
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.selected_events.clear();
                cx.notify();
            })
        });
        assert_eq!(
            view.read_with(cx, |this, cx| this.visible_shelf_cards(cx).len()),
            2
        );
    }

    #[gpui_kit::test]
    async fn tag_fetch_toggle_persists_setting(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        let view = cx.new(BookshelfView::new);
        // デフォルトは ON（FANZA 対応で変更）
        assert!(view.read_with(cx, |this, _| this.tag_fetch_enabled));

        // トグルで OFF になり app_settings に永続化される
        cx.update(|cx| view.update(cx, |this, cx| this.toggle_tag_fetch(cx)));
        assert!(!view.read_with(cx, |this, _| this.tag_fetch_enabled));
        let stored = cx.update(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            db::settings::get(db, "tag.fetch.enabled").ok().flatten()
        });
        assert_eq!(stored.as_deref(), Some("false"));

        // 再トグルで ON に戻り設定も更新される
        cx.update(|cx| view.update(cx, |this, cx| this.toggle_tag_fetch(cx)));
        assert!(view.read_with(cx, |this, _| this.tag_fetch_enabled));
        let stored = cx.update(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            db::settings::get(db, "tag.fetch.enabled").ok().flatten()
        });
        assert_eq!(stored.as_deref(), Some("true"));
    }

    #[gpui_kit::test]
    async fn view_mode_toggles_between_card_and_list(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        let view = cx.new(BookshelfView::new);
        assert_eq!(view.read_with(cx, |this, _| this.view_mode), ViewMode::Card);
        cx.update(|cx| view.update(cx, |this, cx| this.toggle_view_mode(cx)));
        assert_eq!(view.read_with(cx, |this, _| this.view_mode), ViewMode::List);
        cx.update(|cx| view.update(cx, |this, cx| this.toggle_view_mode(cx)));
        assert_eq!(view.read_with(cx, |this, _| this.view_mode), ViewMode::Card);
    }

    #[gpui_kit::test]
    async fn delete_book_removes_row_and_pack(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "b1", "本1", "サークルA");
        // create a pack file
        let packs_dir = cx.read(|cx| AppState::global(cx).packs_dir.clone());
        std::fs::create_dir_all(&packs_dir).unwrap();
        std::fs::write(packs_dir.join("b1.opfspack"), b"fake-pack").unwrap();

        let view = cx.new(BookshelfView::new);
        cx.update(|cx| view.update(cx, |this, cx| this.delete_book(cx, "b1")));
        let count = cx.read(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            books::list(db).unwrap().len()
        });
        assert_eq!(count, 0);
        assert!(!packs_dir.join("b1.opfspack").exists());
    }

    #[test]
    fn render_image_pixels_are_bgra_ordered() {
        // 回帰: GPUI の RenderImage は BGRA を期待する（テクスチャ BGRA8Unorm、
        // 標準デコーダーは pixel.swap(0, 2) で変換）。RGBA のまま渡すと
        // 表示で赤と青が入れ替わり「色が抜ける」。
        let mut img = image::RgbImage::new(4, 4);
        for pixel in img.pixels_mut() {
            *pixel = image::Rgb([255, 0, 0]);
        }
        let mut bytes = Vec::new();
        image::DynamicImage::ImageRgb8(img)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .unwrap();
        let render = decode_bytes_to_render_image(&bytes).expect("decoded");
        let frame = render.as_bytes(0).expect("frame bytes");
        // ピクセル 0 は (B, G, R, A) = (0, 0, 255, 255) のはず
        assert_eq!(&frame[0..4], &[0, 0, 255, 255]);
    }

    #[test]
    fn format_event_label_compacts_techbookfest_events() {
        // Web の formatEventLabel と同じ: `TechBookFest 20` / `技術書典 20` → `技術書典20`
        assert_eq!(format_event_label("TechBookFest 20"), "技術書典20");
        assert_eq!(format_event_label("技術書典 20"), "技術書典20");
        assert_eq!(format_event_label("技術書典20"), "技術書典20");
        // 大文字小文字は無視（/i フラグ）
        assert_eq!(format_event_label("techbookfest 7"), "技術書典7");
        // 一致しないものは原文
        assert_eq!(format_event_label("技術書典 20 追加"), "技術書典 20 追加");
        assert_eq!(format_event_label("コミティア 150"), "コミティア 150");
    }

    #[gpui_kit::test]
    async fn shelf_card_carries_local_progress(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "b1", "本1", "サークルA");
        seed_progress(cx, "b1", 2, Some(10));
        cx.update(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = 'db-1' WHERE id = 'b1'")
                    .execute(db)
                    .await
            })
            .unwrap();
        });
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        let view = cx.new(BookshelfView::new);
        let card = view.read_with(cx, |v, _| v.shelf_cards[0].local.clone());
        let entry = card.expect("local book");
        assert_eq!(entry.progress, Some((2, Some(10))));
    }

    #[gpui_kit::test]
    async fn favorite_filter_shows_only_favorites(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "b1", "本1", "サークルA");
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        seed_shelf_item(cx, "db-2", "本2", "サークルB", None);
        cx.update(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            thundoku_core::db::bookshelf::set_favorite(db, "techbookfest", "db-1", true).unwrap();
        });
        let view = cx.new(BookshelfView::new);
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                this.set_read_filter(cx, ReadFilter::Favorite)
            })
        });
        let visible = view.read_with(cx, |this, cx| {
            this.visible_shelf_cards(cx)
                .iter()
                .map(|c| c.shelf.database_id.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(visible, vec!["db-1".to_string()]);
    }

    #[gpui_kit::test]
    async fn hidden_book_is_excluded_from_shelf(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_book(cx, "b1", "本1", "サークルA");
        seed_shelf_item(cx, "db-1", "本1", "サークルA", None);
        seed_shelf_item(cx, "db-2", "本2", "サークルB", None);
        cx.update(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            thundoku_core::db::bookshelf::set_hidden(db, "techbookfest", "db-1", true).unwrap();
        });
        let view = cx.new(BookshelfView::new);
        let visible = view.read_with(cx, |this, cx| {
            this.visible_shelf_cards(cx)
                .iter()
                .map(|c| c.shelf.database_id.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(visible, vec!["db-2".to_string()]);
    }
}
