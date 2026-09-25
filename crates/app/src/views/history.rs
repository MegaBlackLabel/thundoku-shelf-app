//! 閲覧履歴ビュー: `view_history` を **1 日 1 本** に集約し、日付ごとにまとめて表示する。
//!
//! - 期間フィルタ（全項目 / 今日 / 今週 / 今月）とサイトフィルタ
//! - カード形式 / リスト形式（本棚と同じ見た目: 表紙枠・状態チップ・お気に入りハート）
//! - クリックでリーダーを開く（本棚と同じ `OpenReader` アクション）
//!
//! 履歴はローカルの本だけ（リーダーで開いた本 = `books` にある本）が対象。

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::popover::Popover;
use gpui_kit::component::theme::Colorize as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement, ReadGlobal as _, Render, RenderImage, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, img, px, relative,
};

use thundoku_core::db::{self, books, bookshelf, progress};

use crate::actions::OpenReader;
use crate::app_state::AppState;
use crate::icons::AppIcon;
use crate::views::bookshelf::{
    CARD_TAGS_COLLAPSED_MAX, CHIP_HEART_BUTTON, CHIP_HEART_ICON, CoverBadge, LIST_COVER_MIN_H,
    LIST_COVER_W, LIST_INFO_W, LIST_TAGS_W_RATIO, SIDEBAR_W, StatusIconPalette, TagOrder, ViewMode,
    cover_badge, cover_fit_inside_frame, fit_cover_size, list_tags_visible_count,
    load_cached_cover, load_cover_image, no_image_cover, owned_book_ids, placeholder_cover,
};

/// 期間フィルタ。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum HistoryPeriod {
    /// 全項目（すべての期間・サイトも解除する）
    #[default]
    All,
    Today,
    Week,
    Month,
}

impl HistoryPeriod {
    fn label(self) -> &'static str {
        match self {
            HistoryPeriod::All => "全項目",
            HistoryPeriod::Today => "今日",
            HistoryPeriod::Week => "今週",
            HistoryPeriod::Month => "今月",
        }
    }

    /// ボタンに並べる順（全項目 → 今日 → 今週 → 今月）。
    const ALL: [HistoryPeriod; 4] = [
        HistoryPeriod::All,
        HistoryPeriod::Today,
        HistoryPeriod::Week,
        HistoryPeriod::Month,
    ];

    /// ローカル日付 `YYYY-MM-DD` がこの期間に入るか。`today` は同じ形式の今日。
    /// 日付は辞書順 = 時刻順なので文字列比較で足りる。
    fn contains(self, date: &str, today: &str) -> bool {
        match self {
            HistoryPeriod::All => true,
            HistoryPeriod::Today => date == today,
            HistoryPeriod::Week => date >= week_start(today).as_str() && date <= today,
            HistoryPeriod::Month => date.get(..7) == today.get(..7),
        }
    }
}

/// 選択位置の移動（本棚と同じ規則）: dx は ±1、dy は行ぶん（±cols）。
/// 範囲外は端で止める。移動できないときは None。
fn next_selection_index(
    current: Option<usize>,
    len: usize,
    dx: i64,
    dy: i64,
    cols: usize,
) -> Option<usize> {
    if len == 0 {
        return None;
    }
    let current = current.unwrap_or(0) as i64;
    let next = (current + dx + dy * cols.max(1) as i64).clamp(0, len as i64 - 1);
    Some(next as usize)
}

/// 選択中の出現（何日目の何冊目か）。`selected_index` は `flat_book_ids()` の通し番号。
///
/// **本 id ではなく出現で選ぶ**（同じ本が複数の日に出ても選択枠は 1 つだけになる。
/// 本 id で比べると、同じ本の全出現が同時に選択中になってしまう）。
/// 範囲外は `None`（まだ選択していない / 表示が減った直後）。
fn selected_occurrence(
    selected_index: Option<usize>,
    days: &[HistoryDay],
) -> Option<(usize, usize)> {
    let index = selected_index?;
    let mut offset = 0;
    for (day_index, day) in days.iter().enumerate() {
        if index < offset + day.items.len() {
            return Some((day_index, index - offset));
        }
        offset += day.items.len();
    }
    None
}

/// `YYYY-MM-DD` の週の始まり（月曜）。パースできないときは同じ日を返す。
fn week_start(today: &str) -> String {
    use chrono::{Datelike as _, Duration, NaiveDate};
    let Ok(date) = NaiveDate::parse_from_str(today, "%Y-%m-%d") else {
        return today.to_string();
    };
    let monday = date - Duration::days(date.weekday().num_days_from_monday() as i64);
    monday.format("%Y-%m-%d").to_string()
}

/// `YYYY-MM-DD` を「2026年09月13日」にする（履歴の日付ヘッダ）。
fn day_label(date: &str) -> String {
    let mut parts = date.split('-');
    let (Some(year), Some(month), Some(day)) = (parts.next(), parts.next(), parts.next()) else {
        return date.to_string();
    };
    format!("{year}年{month}月{day}日")
}

/// 1 件を描くための表示状態（タグの並び替えキー / タグ展開 / 選択 / 実体のお気に入り）。
struct ItemState<'a> {
    tag_order: &'a TagOrder,
    tags_expanded: bool,
    selected: bool,
    favorite_circles: &'a [String],
    favorite_authors: &'a [String],
}

/// 履歴の 1 件（1 日 1 本）。表示する情報は本棚のカード / 行と揃える
/// （タイトル・イベント名 or 購入日・サークル / 作者・進捗・読了 / 未読・種別・お気に入り）。
#[derive(Clone)]
struct HistoryItem {
    book: books::Book,
    /// 本棚と同じ表記のイベント名（技術書典）/ 購入日（BOOTH / FANZA / DLsite）/ イベント不明。
    event_text: String,
    /// 読書状態（未読 / 読書中 / 読了）。判定は本棚と同じ `ReadingState` に集約する。
    reading_state: progress::ReadingState,
    progress: Option<(i64, Option<i64>)>,
    /// 本棚と同じタグ列を出すためのタグ（ローカル本の `book_tags`）。
    tags: Vec<String>,
    cover: Option<Arc<RenderImage>>,
}

/// 履歴の 1 日ぶん。
struct HistoryDay {
    /// ローカル日付（`YYYY-MM-DD`）。
    date: String,
    /// 表示用の日付（`2026年09月13日`）。
    label: String,
    /// 履歴に含まれるサイト（フィルタ候補）。
    items: Vec<HistoryItem>,
}

/// 一覧の仮想化 1 行（`gpui_kit::list` の 1 行）。
///
/// 一覧は日付ごとに「日付バー + その日の本」を積む。カード表示では 1 行に
/// 最大 `columns` 件、リスト表示では 1 件を並べる。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum HistoryRow {
    /// 日付ヘッダのバー。
    Day { day_index: usize },
    /// その日の本の行（`days[day_index].items[first_item_index..][..len]`）。
    Items {
        day_index: usize,
        first_item_index: usize,
        len: usize,
    },
}

/// 1 行に並べる件数（カードは列数、リストは 1）。
fn row_items(view_mode: ViewMode, columns: usize) -> usize {
    match view_mode {
        ViewMode::Card => columns.max(1),
        ViewMode::List => 1,
    }
}

/// 日付バー + 本の行（カードは 1 行 `columns` 件）の順に、仮想化リストの行を組み立てる。
fn build_rows(days: &[HistoryDay], view_mode: ViewMode, columns: usize) -> Vec<HistoryRow> {
    let per_row = row_items(view_mode, columns);
    let mut rows = Vec::new();
    for (day_index, day) in days.iter().enumerate() {
        rows.push(HistoryRow::Day { day_index });
        for first_item_index in (0..day.items.len()).step_by(per_row) {
            rows.push(HistoryRow::Items {
                day_index,
                first_item_index,
                // 末尾の行は `per_row` に満たない
                len: per_row.min(day.items.len() - first_item_index),
            });
        }
    }
    rows
}

/// 仮想化 1 行の高さの見積もり。
///
/// 未計測の行の高さは 0 として扱われるため、ヒントを与えないと全体の高さが
/// 可視行のぶんしか無く、スクロールできる範囲が足りなくなる。実際に描いた行は
/// 実測値に置き換わる（行の種類で高さが違うので、見積もりは凡その値でよい）。
fn row_height_hint(view_mode: ViewMode, card_width: f32) -> f32 {
    match view_mode {
        // カード: 表紙（幅の 3/4。`render_card` と同じクランプ）+ 情報 + タグ行
        ViewMode::Card => (card_width * 0.75).clamp(120.0, 320.0) + CARD_INFO_H,
        // リスト: 表紙枠（200x133）+ 行の余白（`render_row` の `p_2`）
        ViewMode::List => LIST_COVER_MIN_H + LIST_ROW_CHROME_H,
    }
}

/// カード 1 枚の表紙より下（タイトル・イベント名・チップ・進捗・タグ行）の高さの見積もり。
const CARD_INFO_H: f32 = 170.0;

/// リスト 1 行の表紙枠以外（`p_2` の余白）の高さの見積もり。
const LIST_ROW_CHROME_H: f32 = 24.0;

/// 仮想化リストで可視域の外に先読みする高さ（本棚のリストと同じ）。
const LIST_ROW_OVERDRAW: f32 = 300.0;

/// リストのスクロール位置を「先頭からの割合（0.0..=1.0）」で表す。
///
/// 件数が変わっても同じ位置を指せるように、アイテム番号ではなく割合で持つ
/// （本棚の `list_scroll_fraction` と同じ。行の高さが揃っていれば割合 = 見た目の位置）。
fn list_scroll_fraction(state: &gpui_kit::ListState) -> f32 {
    let count = state.item_count();
    if count <= 1 {
        return 0.0;
    }
    let top = state.logical_scroll_top();
    (top.item_ix as f32 / (count - 1) as f32).clamp(0.0, 1.0)
}

/// 仮想化リストの件数を差し替えつつ、スクロール位置を保つ。
///
/// `ListState::reset` はスクロールのアンカーを捨てる（＝先頭に戻る）ため、差し替えの
/// 直前に位置を控え、差し替え後に同じ割合の位置を指し直す（本棚の
/// `set_list_count_keeping_scroll` と同じ考え方）。
///
/// 行の高さには一律のヒント `row_height` を入れる: 未計測の行は高さ 0 として扱われ、
/// 全体の高さが可視行のぶんしか無いとスクロールできる範囲が足りなくなる
/// （実際に描いた行は実測値に置き換わる）。件数が変わらないときは位置に触らない。
fn set_list_rows(state: &gpui_kit::ListState, count: usize, row_height: f32) {
    if state.item_count() == count {
        return;
    }
    // `reset` は件数（＝割合の基準）を変えるため、控えるのは差し替えの前
    let fraction = list_scroll_fraction(state);
    state.reset_with_uniform_height(count, px(row_height));
    if count == 0 {
        return;
    }
    let item_ix = ((count - 1) as f32 * fraction).round() as usize;
    state.scroll_to(gpui_kit::ListOffset {
        item_ix,
        offset_in_item: px(0.0),
    });
}

pub struct HistoryView {
    days: Vec<HistoryDay>,
    /// 履歴に含まれるサイト（`すべてのサイト` + 各サイト）。
    sites: Vec<String>,
    view_mode: ViewMode,
    period: HistoryPeriod,
    /// サイトフィルタ（None = すべてのサイト）。
    site: Option<String>,
    /// タグ絞り込み（本棚と同じ OR 条件）。空 = すべて。
    selected_tags: Vec<String>,
    /// タグのお気に入り（チップのハート）。
    favorite_tags: Vec<String>,
    /// サークル / 作者のお気に入り（チップのハート。本棚と同じ `favorite_entities`）。
    favorite_circles: Vec<String>,
    favorite_authors: Vec<String>,
    /// タグ名 → そのタグを持つ本の数（チップの並び替えキー）。
    tag_counts: std::sync::Arc<std::collections::HashMap<String, usize>>,
    /// タグ列を展開している出現（「+n」→「閉じる」）。本 id ではなく要素 id
    /// （出現キー付き）で覚える: 同じ本が複数の日に出ても、開いた 1 枚だけが展開される。
    expanded_tag_rows: std::collections::HashSet<String>,
    /// キーボード操作の選択位置（日付 → 本の順に平坦化したインデックス）。
    selected_index: Option<usize>,
    /// 一覧の仮想化行（日付バー + 本の行）。`days` と列数から組む。
    rows: Vec<HistoryRow>,
    /// `rows` を組んだときの 1 行あたりの件数（リスト表示は 1）。
    /// ウィンドウ幅で列数が変わったら組み直す。
    rows_columns: usize,
    /// `rows` が古いか（`days` が入れ替わった / まだ組んでいない）。
    rows_stale: bool,
    /// 一覧の行単位の仮想化（可視行だけを構築する）。本棚の `list_rows_state` と同じ。
    list_state: gpui_kit::ListState,
    focus_handle: FocusHandle,
    /// 初回描画でフォーカスを取る（本棚と同じ）。
    focus_initialized: bool,
    /// テスト専用: 仮想化の行を実際に構築した回数（可視行だけであることを確かめる）。
    #[cfg(test)]
    built_rows: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl HistoryView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let view_mode = Self::read_view_mode(cx);
        let mut view = Self {
            days: Vec::new(),
            sites: Vec::new(),
            view_mode,
            period: HistoryPeriod::default(),
            site: None,
            selected_tags: Vec::new(),
            favorite_tags: Vec::new(),
            favorite_circles: Vec::new(),
            favorite_authors: Vec::new(),
            tag_counts: std::sync::Arc::new(std::collections::HashMap::new()),
            expanded_tag_rows: std::collections::HashSet::new(),
            selected_index: None,
            rows: Vec::new(),
            rows_columns: 0,
            rows_stale: true,
            // 行単位の仮想化（可視行のみ構築）。高さは行を組むときにヒントを入れる
            list_state: gpui_kit::ListState::new(
                0,
                gpui_kit::ListAlignment::Top,
                px(LIST_ROW_OVERDRAW),
            ),
            focus_handle: cx.focus_handle(),
            focus_initialized: false,
            #[cfg(test)]
            built_rows: std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0)),
        };
        view.reload(cx);
        view
    }

    /// 保存済みの表示モード（設定）を読む。
    fn read_view_mode(cx: &Context<Self>) -> ViewMode {
        let state = AppState::global(cx);
        let saved = db::settings::get(&state.db_pool, "history.view_mode")
            .ok()
            .flatten();
        match saved.as_deref() {
            Some("list") => ViewMode::List,
            _ => ViewMode::Card,
        }
    }

    /// 履歴を読み直して日付ごとに組み立てる。
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        let state = AppState::global(cx);
        let pool = &state.db_pool;
        let packs_dir = state.packs_dir.clone();
        // 本棚と同じ表紙の解決順にそろえる（サイトのサムネキャッシュを最優先）
        let thumbnails_dir = state.data_dir.join("thumbnails");
        let owned = owned_book_ids(state);
        let today = chrono::Local::now().format("%Y-%m-%d").to_string();

        let sessions = db::view_history::list_daily(pool).unwrap_or_default();
        let all_books = books::list(pool).unwrap_or_default();
        // 本棚と同じ「イベント名 or 購入日」を出すため、紐づく本棚アイテムを引く
        let shelf_items = db::bookshelf::list_all(pool).unwrap_or_default();

        let mut days: Vec<HistoryDay> = Vec::new();
        let mut sites: Vec<String> = Vec::new();
        // 表紙は同じ本が複数の日に出るので、この reload 内でデコード結果を共有する
        // （1 冊 1 回。以前は日ごとに 1MB の表紙をデコードし直していた）
        let mut cover_cache: std::collections::HashMap<String, Option<Arc<RenderImage>>> =
            std::collections::HashMap::new();
        for session in sessions {
            // 期間フィルタ（日付単位）
            if !self.period.contains(&session.day, &today) {
                continue;
            }
            // 本が消えている / 別アカウントの本は出さない
            if !owned.contains(&session.book_id) {
                continue;
            }
            let Some(book) = all_books.iter().find(|b| b.id == session.book_id) else {
                continue;
            };
            if let Some(site) = book.site_id.as_deref()
                && !sites.iter().any(|s| s == site)
            {
                sites.push(site.to_string());
            }
            // サイトフィルタ
            if let Some(filter) = self.site.as_deref()
                && book.site_id.as_deref() != Some(filter)
            {
                continue;
            }
            let shelf = shelf_items.iter().find(|item| {
                book.tbf_product_id.as_deref() == Some(item.database_id.as_str())
                    || item
                        .file_name
                        .as_deref()
                        .is_some_and(|name| name == book.file_name)
                    || (!item.title.is_empty() && item.title == book.title)
            });
            let reading_state = progress::ReadingState::from_progress(
                progress::get(pool, &book.id).ok().flatten().as_ref(),
            );
            let tags: Vec<String> = db::tags::list_for_book(pool, &book.id)
                .unwrap_or_default()
                .into_iter()
                .map(|tag| tag.tag_name)
                .collect();
            // タグ絞り込み（本棚と同じ OR 条件）
            if !self.selected_tags.is_empty()
                && !self.selected_tags.iter().any(|tag| tags.contains(tag))
            {
                continue;
            }
            let item = HistoryItem {
                // 本棚と同じ順: サイトのサムネイルキャッシュ → pack の表紙 → プレースホルダ。
                // 以前は pack の表紙しか見ておらず、本棚と違う絵が出ていた。
                // 同じ本は 1 回だけデコードして共有する（上の cover_cache）。
                cover: cover_cache
                    .entry(book.id.clone())
                    .or_insert_with(|| {
                        shelf
                            .and_then(|item| load_cached_cover(&thumbnails_dir, item))
                            .or_else(|| load_cover_image(&packs_dir, book))
                            .or_else(|| placeholder_cover(&book.title, &book.circle_name))
                    })
                    .clone(),
                book: book.clone(),
                event_text: shelf_event_text(shelf),
                reading_state,
                progress: progress::get(pool, &book.id)
                    .ok()
                    .flatten()
                    .map(|p| (p.current_page, p.total_pages)),
                tags,
            };
            match days.iter_mut().find(|day| day.date == session.day) {
                Some(day) => day.items.push(item),
                None => days.push(HistoryDay {
                    label: day_label(&session.day),
                    date: session.day.clone(),
                    items: vec![item],
                }),
            }
        }

        self.favorite_tags = db::tags::list_favorites(pool).unwrap_or_default();
        self.favorite_circles =
            db::favorites::list_favorites(pool, db::favorites::EntityKind::Circle)
                .unwrap_or_default();
        self.favorite_authors =
            db::favorites::list_favorites(pool, db::favorites::EntityKind::Author)
                .unwrap_or_default();
        // チップの並び替えに使う集計（履歴に出ている本のタグ）
        self.tag_counts = std::sync::Arc::new(crate::views::bookshelf::count_tag_usage(
            days.iter()
                .flat_map(|day| day.items.iter())
                .map(|item| item.tags.as_slice()),
        ));
        self.days = days;
        self.sites = sites;
        // 行モデルは `days` から組むので、次の描画で組み直す（列数は描画時に分かる）
        self.rows_stale = true;
        // 選択位置を表示中の件数に合わせる（初回・範囲外は先頭）
        let len = self.flat_book_ids().len();
        if len == 0 {
            self.selected_index = None;
        } else if self.selected_index.is_none_or(|index| index >= len) {
            self.selected_index = Some(0);
        }
        // サイトフィルタが選択肢から消えたら解除する
        if let Some(site) = self.site.clone()
            && !self.sites.iter().any(|s| s == &site)
        {
            self.site = None;
        }
    }

    /// 表示順（日付 → その日の並び）に平坦化した本の id（選択とキーボード操作用）。
    fn flat_book_ids(&self) -> Vec<String> {
        self.days
            .iter()
            .flat_map(|day| day.items.iter().map(|item| item.book.id.clone()))
            .collect()
    }

    /// 仮想化行を組み直し、リストの件数を合わせる（スクロール位置は保つ）。
    ///
    /// `columns` はカード表示の列数（リスト表示は無視して 1 行 1 件）。`card_width` は
    /// 行の高さのヒントに使う（幅で表紙の高さが変わるため）。
    fn rebuild_rows(&mut self, columns: usize, card_width: f32) {
        self.rows_columns = row_items(self.view_mode, columns);
        self.rows = build_rows(&self.days, self.view_mode, columns);
        self.rows_stale = false;
        set_list_rows(
            &self.list_state,
            self.rows.len(),
            row_height_hint(self.view_mode, card_width),
        );
    }

    /// 平坦化した位置の本を含む仮想化行の番号（選択を画面内へ寄せるのに使う）。
    fn row_index_of_item(&self, flat_index: usize) -> Option<usize> {
        let mut offset = 0;
        for (day_index, day) in self.days.iter().enumerate() {
            if flat_index >= offset + day.items.len() {
                offset += day.items.len();
                continue;
            }
            let item_index = flat_index - offset;
            // 行は日付バー → 本の行の順。同じ日の中では先頭の出現位置で並んでいる
            return self.rows.iter().position(|row| {
                matches!(
                    row,
                    HistoryRow::Items { day_index: row_day, first_item_index, len }
                        if *row_day == day_index
                            && (*first_item_index..*first_item_index + *len).contains(&item_index)
                )
            });
        }
        None
    }

    /// 絞り込み中か（ESC で解除する対象があるか）。
    fn is_filtering(&self) -> bool {
        self.period != HistoryPeriod::All || self.site.is_some() || !self.selected_tags.is_empty()
    }

    /// キーボード操作: Enter で開く、矢印 / hjkl で選択移動、ESC で絞り込み解除。
    fn handle_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => self.activate_selected(cx),
            "right" | "l" => self.shift_selection(1, 0, window, cx),
            "left" | "h" => self.shift_selection(-1, 0, window, cx),
            "down" | "j" => self.shift_selection(0, 1, window, cx),
            "up" | "k" => self.shift_selection(0, -1, window, cx),
            // 絞り込み中は解除だけして親（ワークスペース）の ESC 処理へ流さない
            "escape" if self.is_filtering() => {
                cx.stop_propagation();
                self.clear_filters(window, cx);
            }
            _ => {}
        }
    }

    /// 選択中の本を開く（クリックと同じ）。
    fn activate_selected(&mut self, cx: &mut Context<Self>) {
        let Some(index) = self.selected_index else {
            return;
        };
        let Some(book_id) = self.flat_book_ids().get(index).cloned() else {
            return;
        };
        self.open_book(cx, &book_id);
    }

    /// 選択位置を移動する（カードは列数ぶんのグリッド移動、リストは ±1）。
    fn shift_selection(&mut self, dx: i64, dy: i64, window: &mut Window, cx: &mut Context<Self>) {
        let len = self.flat_book_ids().len();
        let cols = if self.view_mode == ViewMode::List {
            1
        } else {
            crate::views::bookshelf::BookshelfView::columns_for_width(
                window.bounds().size.width.as_f32(),
            )
        };
        let Some(next) = next_selection_index(self.selected_index, len, dx, dy, cols) else {
            return;
        };
        if Some(next) != self.selected_index {
            self.selected_index = Some(next);
            // 選択した本の行が見える位置へスクロールを寄せる
            if let Some(row_index) = self.row_index_of_item(next) {
                self.list_state.scroll_to_reveal_item(row_index);
            }
            cx.notify();
        }
    }

    /// 絞り込みを全部解除する（ESC / 「全項目」）。
    fn clear_filters(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
        self.period = HistoryPeriod::All;
        self.site = None;
        self.selected_tags.clear();
        self.reload(cx);
        cx.notify();
    }

    /// タグの絞り込みをトグルする（チップの文字クリック。本棚と同じ）。
    fn toggle_tag(&mut self, tag: &str, cx: &mut Context<Self>) {
        if let Some(index) = self.selected_tags.iter().position(|t| t == tag) {
            self.selected_tags.remove(index);
        } else {
            self.selected_tags.push(tag.to_string());
        }
        self.reload(cx);
        cx.notify();
    }

    /// タグのお気に入りをトグルする（チップのハート。本棚と同じ）。
    fn toggle_favorite_tag(&mut self, tag: &str, cx: &mut Context<Self>) {
        let is_favorite = self.favorite_tags.iter().any(|t| t == tag);
        {
            let state = AppState::global(cx);
            let _ = db::tags::set_favorite(
                &state.db_pool,
                tag,
                !is_favorite,
                crate::app_state::owner_token(state).as_deref(),
            );
        }
        if is_favorite {
            self.favorite_tags.retain(|t| t != tag);
        } else {
            self.favorite_tags.push(tag.to_string());
        }
        cx.notify();
    }

    /// サークル / 作者のお気に入りをトグルする（本棚のチップのハートと同じ）。
    fn toggle_favorite_entity(
        &mut self,
        kind: db::favorites::EntityKind,
        value: &str,
        cx: &mut Context<Self>,
    ) {
        let is_favorite = match kind {
            db::favorites::EntityKind::Circle => self.favorite_circles.iter().any(|v| v == value),
            db::favorites::EntityKind::Author => self.favorite_authors.iter().any(|v| v == value),
        };
        {
            let state = AppState::global(cx);
            let _ = db::favorites::set_favorite(
                &state.db_pool,
                kind,
                value,
                !is_favorite,
                crate::app_state::owner_token(state).as_deref(),
            );
        }
        let favorites = match kind {
            db::favorites::EntityKind::Circle => &mut self.favorite_circles,
            db::favorites::EntityKind::Author => &mut self.favorite_authors,
        };
        if is_favorite {
            favorites.retain(|v| v != value);
        } else {
            favorites.push(value.to_string());
        }
        cx.notify();
    }

    /// タグ列の折りたたみ / 展開を切り替える（本棚と同じ。**出現ごと**）。
    /// `element_id` は出現キー付きの要素 id（`render_tag_row` と同じ組み立て）。
    fn toggle_tag_expansion(&mut self, element_id: &str, cx: &mut Context<Self>) {
        if !self.expanded_tag_rows.remove(element_id) {
            self.expanded_tag_rows.insert(element_id.to_string());
        }
        cx.notify();
    }

    /// 期間フィルタを変える（「全項目」はサイト・タグの絞り込みも解除する）。
    fn set_period(&mut self, period: HistoryPeriod, cx: &mut Context<Self>) {
        self.period = period;
        if period == HistoryPeriod::All {
            self.site = None;
            self.selected_tags.clear();
        }
        self.reload(cx);
        cx.notify();
    }

    /// サイトフィルタを変える。
    fn set_site(&mut self, site: Option<String>, cx: &mut Context<Self>) {
        self.site = site;
        self.reload(cx);
        cx.notify();
    }

    /// 表示モードを切り替えて保存する。
    fn toggle_view_mode(&mut self, cx: &mut Context<Self>) {
        self.view_mode = match self.view_mode {
            ViewMode::Card => ViewMode::List,
            ViewMode::List => ViewMode::Card,
        };
        let value = match self.view_mode {
            ViewMode::Card => "card",
            ViewMode::List => "list",
        };
        let state = AppState::global(cx);
        let _ = db::settings::set(&state.db_pool, "history.view_mode", value);
        cx.notify();
    }

    /// お気に入りのトグル（本棚と同じく `books` と、紐づく本棚アイテムの両方を更新する）。
    fn toggle_favorite(&mut self, cx: &mut Context<Self>, book_id: &str) {
        {
            let state = AppState::global(cx);
            let pool = &state.db_pool;
            let Some(book) = books::list(pool)
                .unwrap_or_default()
                .into_iter()
                .find(|b| b.id == book_id)
            else {
                return;
            };
            let new_state = book.is_favorite != 1;
            let _ = books::set_favorite(pool, &book.id, new_state);
            if let (Some(site), Some(database_id)) =
                (book.site_id.as_deref(), book.tbf_product_id.as_deref())
            {
                let _ = bookshelf::set_favorite(pool, site, database_id, new_state);
            }
        }
        self.reload(cx);
        cx.notify();
    }

    /// リーダーを開く（本棚と同じアクション経由）。
    fn open_book(&self, cx: &mut Context<Self>, book_id: &str) {
        let action = OpenReader {
            book_id: book_id.into(),
        };
        cx.defer(move |cx| cx.dispatch_action(&action));
    }

    /// ヘッダ（タイトル・件数・フィルタ・表示切替）。
    fn render_header(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let period = self.period;
        let site = self.site.clone();
        let sites = self.sites.clone();
        let view_mode = self.view_mode;
        let handle = cx.entity();

        div()
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui_kit::FontWeight::BOLD)
                    .child("閲覧履歴"),
            )
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .flex_1()
                    .children(HistoryPeriod::ALL.into_iter().map(|value| {
                        let handle = handle.clone();
                        let mut button = Button::new(SharedString::from(format!(
                            "history-period-{}",
                            value.label()
                        )))
                        .cursor_pointer()
                        .label(value.label());
                        if value == HistoryPeriod::All {
                            // 「全項目」はサイトフィルタも解除するので、どちらも無いときだけ active
                            if period == HistoryPeriod::All && site.is_none() {
                                button = button.primary();
                            }
                        } else if period == value {
                            button = button.primary();
                        } else {
                            button = button.outline();
                        }
                        if value == HistoryPeriod::All
                            && (period != HistoryPeriod::All || site.is_some())
                        {
                            button = button.outline();
                        }
                        div()
                            .debug_selector({
                                let selector = format!("history-period-{}", value.label());
                                move || selector.clone()
                            })
                            .child(button.on_click(move |_, _window, cx| {
                                handle.update(cx, |this, cx| this.set_period(value, cx));
                            }))
                    })),
            )
            // サイトフィルタ（履歴に含まれるサイトだけ出す）
            .child({
                let handle = handle.clone();
                let label = match site.as_deref() {
                    Some(site) => site_label(site),
                    None => "すべてのサイト".to_string(),
                };
                div()
                    .debug_selector(|| "history-site-trigger".into())
                    .child(
                        Popover::new("history-site-popover")
                            .appearance(false)
                            .anchor(Anchor::TopLeft)
                            .trigger(
                                Button::new("history-site-filter")
                                    .cursor_pointer()
                                    .label(label)
                                    .outline(),
                            )
                            .content(move |_state, _window, _cx| {
                                let theme = theme.clone();
                                div()
                                    .w(px(200.0))
                                    .rounded_md()
                                    .border_1()
                                    .border_color(theme.border)
                                    .bg(theme.popover)
                                    .shadow_lg()
                                    .p_2()
                                    .flex()
                                    .flex_col()
                                    .gap_1()
                                    .children(
                                        std::iter::once(None)
                                            .chain(sites.iter().cloned().map(Some))
                                            .map({
                                                let handle = handle.clone();
                                                move |value| {
                                                    let handle = handle.clone();
                                                    let text = match value.as_deref() {
                                                        Some(site) => site_label(site),
                                                        None => "すべてのサイト".to_string(),
                                                    };
                                                    let id = match value.as_deref() {
                                                        Some(site) => {
                                                            format!("history-site-{site}")
                                                        }
                                                        None => "history-site-all".to_string(),
                                                    };
                                                    div()
                                                        .id(SharedString::from(id.clone()))
                                                        .debug_selector(move || id.clone())
                                                        .px_2()
                                                        .py_1p5()
                                                        .rounded_sm()
                                                        .text_sm()
                                                        .cursor_pointer()
                                                        .hover({
                                                            let hover = theme.secondary;
                                                            move |style| style.bg(hover)
                                                        })
                                                        .on_click(move |_, _window, cx| {
                                                            handle.update(cx, |this, cx| {
                                                                this.set_site(value.clone(), cx)
                                                            });
                                                        })
                                                        .child(text)
                                                }
                                            }),
                                    )
                                    .into_any_element()
                            }),
                    )
            })
            .child(
                div().debug_selector(|| "history-view-toggle".into()).child(
                    Button::new("history-view-toggle")
                        .cursor_pointer()
                        .icon(if view_mode == ViewMode::Card {
                            AppIcon::List
                        } else {
                            AppIcon::LayoutGrid
                        })
                        .outline()
                        .on_click({
                            let handle = handle.clone();
                            move |_, _window, cx| {
                                handle.update(cx, |this, cx| this.toggle_view_mode(cx));
                            }
                        }),
                ),
            )
            .into_any_element()
    }

    /// カード 1 枚（本棚のカードと同じ情報を出す: 表紙のバッジ + タイトル + イベント名 or
    /// 購入日 + サークル / 作者 + 進捗 + 種別 + お気に入りハート）。
    /// 引数が多く clippy が警告するが、本棚の `render_card` と同じ構成にする
    /// （表示状態を引数で受け取り、ビューの状態をここへ持ち込まない）。
    ///
    /// `key` は出現キー（`{日付}-{その日の連番}`）。同じ本が複数の日に出ると
    /// 要素 id が本 id だけでは重複し、GPUI の要素 id は一意でなければならないため
    /// 2 つ目以降をクリックできなくなる。カードが描く要素 id にはすべてこれを前置する。
    #[allow(clippy::too_many_arguments)]
    fn render_card(
        window: &mut Window,
        theme: &gpui_kit::component::Theme,
        handle: &Entity<Self>,
        item: &HistoryItem,
        key: &str,
        card_width: f32,
        state: &ItemState<'_>,
    ) -> AnyElement {
        let database_id = item.book.id.clone();
        // 要素 id に前置する「この出現ぶん」の id（`{キー}-{本 id}`）。
        let element_id = format!("{key}-{database_id}");
        let cover = item
            .cover
            .clone()
            .or_else(|| placeholder_cover(&item.book.title, &item.book.circle_name))
            .or_else(no_image_cover);
        // 表紙に重ねるバッジ（右上 = お気に入り / 右下 = ダウンロード済み）。本棚のカードと共有する
        let (favorite_badge, downloaded_badge) = card_badges(item.book.is_favorite == 1);
        let cover_h: f32 = (card_width * 0.75).clamp(120.0, 320.0);
        let (draw_w, draw_h) = match &cover {
            Some(render) => {
                let size = render.size(0);
                fit_cover_size(
                    size.width.0.max(1) as f32,
                    size.height.0.max(1) as f32,
                    card_width,
                    cover_h,
                )
            }
            None => (card_width, cover_h),
        };
        let fits_width = draw_w >= card_width - 0.5;
        let mut cover_el = div().relative().w(px(draw_w)).h(px(draw_h)).flex_shrink_0();
        if fits_width {
            cover_el = cover_el.rounded_t_lg();
        }
        cover_el = cover_el
            .child(match &cover {
                Some(render) => {
                    let mut el = img(render.clone()).w_full().h_full().debug_selector({
                        let selector = format!("history-card-cover-img-{element_id}");
                        move || selector.clone()
                    });
                    if fits_width {
                        el = el.rounded_t_lg();
                    }
                    el.into_any_element()
                }
                None => div().w_full().h_full().bg(theme.muted).into_any_element(),
            })
            // 左上: 未読 / 読んでいる途中 / 読了（本棚のカードと同じバッジ）
            .child({
                let mut badge = div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .rounded_br_md()
                    .px_1()
                    .py_0p5()
                    .text_xs();
                if fits_width {
                    badge = badge.rounded_tl_lg();
                }
                match item.reading_state {
                    progress::ReadingState::Read => badge
                        .bg(gpui_kit::rgb(0xd1fae5))
                        .text_color(gpui_kit::rgb(0x047857))
                        .child("読了"),
                    // 読んでいる途中は青系（未読の黄 / 読了の緑と区別する）
                    progress::ReadingState::Reading => badge
                        .bg(gpui_kit::rgb(0xe0f2fe))
                        .text_color(gpui_kit::rgb(0x0369a1))
                        .child("読んでいる途中"),
                    progress::ReadingState::Unread => badge
                        .bg(gpui_kit::rgb(0xfef3c7))
                        .text_color(gpui_kit::rgb(0xb45309))
                        .child("未読"),
                }
                .into_any_element()
            })
            // 右上: お気に入りハート（本棚のカードと同じ。表紙の上に重ねる）
            .child(
                cover_badge(favorite_badge, format!("history-heart-{element_id}"))
                    .absolute()
                    .right_1()
                    .top_1()
                    .cursor_pointer()
                    .on_click({
                        let handle = handle.clone();
                        let book_id = database_id.clone();
                        move |_, _, cx| {
                            cx.stop_propagation();
                            handle.update(cx, |this, cx| this.toggle_favorite(cx, &book_id));
                        }
                    }),
            )
            // 右下: ダウンロード済みバッジ（本棚のカードと同じ。履歴の本はすべてローカル本）
            .child(
                cover_badge(
                    downloaded_badge,
                    format!("history-card-cover-downloaded-{element_id}"),
                )
                .absolute()
                .right_1()
                .bottom_1(),
            );

        let image: AnyElement = div()
            .w_full()
            .h(px(cover_h))
            .flex()
            .items_center()
            .justify_center()
            .rounded_t_lg()
            .bg(theme.secondary)
            .child(cover_el)
            .into_any_element();

        let book = &item.book;
        div()
            .id(SharedString::from(format!("history-card-{element_id}")))
            .debug_selector({
                let selector = format!("history-card-{element_id}");
                move || selector.clone()
            })
            .w(px(card_width))
            .flex()
            .flex_col()
            .rounded_lg()
            .overflow_hidden()
            .border_1()
            .border_color(if state.selected {
                if theme.is_dark() {
                    theme.primary.darken(0.35)
                } else {
                    theme.primary
                }
            } else {
                theme.border
            })
            .bg(if state.selected {
                if theme.is_dark() {
                    theme.secondary.lighten(0.12)
                } else {
                    theme.secondary
                }
            } else {
                theme.muted
            })
            .hover(|style| style.bg(theme.secondary))
            .cursor_pointer()
            .on_click({
                let handle = handle.clone();
                let book_id = database_id.clone();
                move |_, _window, cx| {
                    handle.update(cx, |this, cx| this.open_book(cx, &book_id));
                }
            })
            .child(div().child(image))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .p_3()
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(book.title.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(item.event_text.clone()),
                    )
                    .child(Self::render_entity_chips(
                        theme,
                        handle,
                        item,
                        key,
                        state.favorite_circles,
                        state.favorite_authors,
                    ))
                    .when_some(Self::progress_text(item), |this, text| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(text),
                        )
                    }),
            )
            // タグ行（本棚のカードと同じ。タグ編集の ✎ は履歴では出さない）
            .child(div().px_3().pb_3().child(Self::render_tag_row(
                window,
                theme,
                handle,
                item,
                key,
                state.tag_order,
                state.tags_expanded,
                true,
            )))
            .into_any_element()
    }

    /// リスト 1 行。**本棚の行と同じ 4 列構成**にする:
    /// 表紙(200x133 固定) / 情報列(320px 固定: タイトル・イベント名 or 購入日・サークル/作者・
    /// 進捗・状態) / タグ列(行幅の 20%) / 残り（本棚はカルーセル。履歴では空ける）。
    /// `key` は出現キー（`render_card` と同じ。同じ本が複数の日に出ても
    /// 要素 id が重複しないように前置する）。
    fn render_row(
        window: &mut Window,
        theme: &gpui_kit::component::Theme,
        handle: &Entity<Self>,
        item: &HistoryItem,
        key: &str,
        state: &ItemState<'_>,
    ) -> AnyElement {
        let database_id = item.book.id.clone();
        // 要素 id に前置する「この出現ぶん」の id（`{キー}-{本 id}`）。
        let element_id = format!("{key}-{database_id}");
        let cover = item
            .cover
            .clone()
            .or_else(|| placeholder_cover(&item.book.title, &item.book.circle_name))
            .or_else(no_image_cover);
        let info_selector = format!("history-info-{element_id}");
        let tag_area_selector = format!("history-tag-area-{element_id}");

        div()
            .id(SharedString::from(format!("history-row-{element_id}")))
            .debug_selector({
                let selector = format!("history-row-{element_id}");
                move || selector.clone()
            })
            .flex()
            .flex_row()
            .items_start()
            .gap_3()
            .p_2()
            .rounded_lg()
            .border_1()
            .border_color(if state.selected {
                if theme.is_dark() {
                    theme.primary.darken(0.35)
                } else {
                    theme.primary
                }
            } else {
                theme.border
            })
            .bg(if state.selected {
                if theme.is_dark() {
                    theme.secondary.lighten(0.12)
                } else {
                    theme.secondary
                }
            } else {
                theme.muted
            })
            .hover(|style| style.bg(theme.secondary))
            .cursor_pointer()
            .on_click({
                let handle = handle.clone();
                let book_id = database_id.clone();
                move |_, _window, cx| {
                    handle.update(cx, |this, cx| this.open_book(cx, &book_id));
                }
            })
            // 1 列目: 表紙（本棚のリストと同じ 200x133 の枠）
            .child(
                div()
                    .debug_selector({
                        let selector = format!("history-cover-{element_id}");
                        move || selector.clone()
                    })
                    .relative()
                    .w(px(LIST_COVER_W))
                    .h(px(LIST_COVER_MIN_H))
                    .flex_shrink_0()
                    .overflow_hidden()
                    // 縦長表紙で余る左右の余白を周囲と馴染ませる（本棚の行と同じ）
                    .bg(theme.muted)
                    .child(cover_fit_inside_frame(
                        cover.as_ref(),
                        format!("history-cover-img-{element_id}"),
                    )),
            )
            // 2 列目: 情報列（幅固定。タイトル / イベント名 or 購入日 / サークル・作者 / 進捗 / 状態）
            .child(
                div()
                    .debug_selector(move || info_selector.clone())
                    .flex()
                    .flex_col()
                    .gap_1()
                    .min_w_0()
                    .w(px(LIST_INFO_W))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(item.book.title.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(item.event_text.clone()),
                    )
                    .child(Self::render_entity_chips(
                        theme,
                        handle,
                        item,
                        key,
                        state.favorite_circles,
                        state.favorite_authors,
                    ))
                    .when_some(Self::progress_text(item), |this, text| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(text),
                        )
                    })
                    // 状態（読了 / 未読 + ♡）は本棚と同じくページ数の下
                    .child(Self::render_state_chips(theme, handle, item, key)),
            )
            // 3 列目: タグ列（行幅の 20%。本棚と同じ幅の取り方）
            .child(
                div()
                    .debug_selector(move || tag_area_selector.clone())
                    .w(relative(LIST_TAGS_W_RATIO))
                    .min_w_0()
                    .child(Self::render_tag_row(
                        window,
                        theme,
                        handle,
                        item,
                        key,
                        state.tag_order,
                        state.tags_expanded,
                        false,
                    )),
            )
            // 4 列目: 本棚は関連書籍カルーセル。履歴では出さないので残り幅は空ける
            .child(div().flex_1().min_w_0())
            .into_any_element()
    }

    /// タグ行（本棚と同じ見た目: チップ + 「+n」/「閉じる」）。履歴ではタグ編集（✎）は出さない。
    /// `key` は出現キー（`render_card` / `render_row` と同じ。タグ行とチップの id にも前置する）。
    #[allow(clippy::too_many_arguments)]
    fn render_tag_row(
        window: &mut Window,
        theme: &gpui_kit::component::Theme,
        handle: &Entity<Self>,
        item: &HistoryItem,
        key: &str,
        tag_order: &TagOrder,
        expanded: bool,
        card: bool,
    ) -> AnyElement {
        let database_id = item.book.id.clone();
        // タグ行 / チップの要素 id に前置する「この出現ぶん」の id（`{キー}-{本 id}`）。
        let element_id = format!("{key}-{database_id}");
        let ordered = tag_order.sorted(&item.tags);
        let visible = if expanded {
            ordered.len()
        } else if card {
            ordered.len().min(CARD_TAGS_COLLAPSED_MAX)
        } else {
            list_tags_visible_count(window, theme, &ordered)
        };
        let chips = crate::views::bookshelf::BookshelfView::render_tag_chips(
            theme,
            tag_order,
            &element_id,
            &ordered[..visible],
            {
                let handle = handle.clone();
                move |tag: &str, _window: &mut Window, cx: &mut App| {
                    let tag = tag.to_string();
                    handle.update(cx, |this, cx| this.toggle_tag(&tag, cx));
                }
            },
            {
                let handle = handle.clone();
                move |tag: &str, _window: &mut Window, cx: &mut App| {
                    let tag = tag.to_string();
                    handle.update(cx, |this, cx| this.toggle_favorite_tag(&tag, cx));
                }
            },
        );
        let hidden = ordered.len() - visible;
        let mut row = div()
            .id(SharedString::from(format!("history-tag-row-{element_id}")))
            .debug_selector({
                let selector = format!("history-tag-row-{element_id}");
                move || selector.clone()
            })
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_1()
            .items_center()
            .cursor_pointer()
            .on_click(|_, _, cx| cx.stop_propagation())
            .children(chips);
        if expanded || hidden > 0 {
            row = row.child(crate::views::bookshelf::BookshelfView::render_tag_toggle(
                theme,
                format!("history-tag-toggle-{element_id}"),
                hidden,
                expanded,
                {
                    let handle = handle.clone();
                    // 展開状態は出現ごと（同じ本が複数の日に出ても 1 枚だけ開く）
                    let element_id = element_id.clone();
                    move |_window, cx| {
                        handle.update(cx, |this, cx| this.toggle_tag_expansion(&element_id, cx));
                    }
                },
            ));
        }
        row.into_any_element()
    }

    /// サークル / 作者のチップ（本棚と同じ見た目: 値 + 右端のハートでお気に入り）。
    fn render_entity_chips(
        theme: &gpui_kit::component::Theme,
        handle: &Entity<Self>,
        item: &HistoryItem,
        key: &str,
        favorite_circles: &[String],
        favorite_authors: &[String],
    ) -> AnyElement {
        // チップ / ハートの要素 id にも出現キーを前置する（同じ本が複数出ても重複させない）。
        let target = format!("{key}-{}", item.book.id);
        div()
            .flex()
            .flex_row()
            .flex_wrap()
            .gap_1()
            .items_center()
            .children(
                [
                    (
                        db::favorites::EntityKind::Circle,
                        item.book.circle_name.as_str(),
                        favorite_circles,
                    ),
                    (
                        db::favorites::EntityKind::Author,
                        item.book.author.as_str(),
                        favorite_authors,
                    ),
                ]
                .into_iter()
                .filter(|(_, value, _)| !value.is_empty())
                .map(|(kind, value, favorites)| {
                    let is_favorite = favorites.iter().any(|f| f == value);
                    Self::render_entity_chip(
                        theme,
                        handle,
                        kind,
                        value,
                        target.clone(),
                        is_favorite,
                    )
                }),
            )
            .into_any_element()
    }

    /// サークル / 作者のチップ 1 個（値 + ハートの丸ボタン）。
    fn render_entity_chip(
        theme: &gpui_kit::component::Theme,
        handle: &Entity<Self>,
        kind: db::favorites::EntityKind,
        value: &str,
        target: String,
        is_favorite: bool,
    ) -> AnyElement {
        let kind_label = match kind {
            db::favorites::EntityKind::Circle => "circle",
            db::favorites::EntityKind::Author => "author",
        };
        let chip_selector = format!("history-{kind_label}-chip-{target}");
        let heart_selector = format!("history-{kind_label}-heart-{target}");
        div()
            .id(SharedString::from(chip_selector.clone()))
            .debug_selector(move || chip_selector.clone())
            .flex()
            .flex_row()
            .items_center()
            .gap_1()
            .px_1()
            .py_0p5()
            .rounded_full()
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .text_color(theme.muted_foreground)
            .text_xs()
            .child(value.to_string())
            .child(
                div()
                    .id(SharedString::from(heart_selector.clone()))
                    .debug_selector(move || heart_selector.clone())
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(CHIP_HEART_BUTTON))
                    .h(px(CHIP_HEART_BUTTON))
                    .rounded_full()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme.secondary))
                    .text_color(if is_favorite {
                        gpui_kit::rgb(0xf43f5e).into()
                    } else {
                        theme.muted_foreground
                    })
                    .on_click({
                        let handle = handle.clone();
                        let value = value.to_string();
                        move |_, _, cx| {
                            cx.stop_propagation();
                            handle.update(cx, |this, cx| {
                                this.toggle_favorite_entity(kind, &value, cx)
                            });
                        }
                    })
                    .child(
                        Icon::new(if is_favorite {
                            AppIcon::HeartFilled
                        } else {
                            AppIcon::Heart
                        })
                        .size(px(CHIP_HEART_ICON)),
                    ),
            )
            .into_any_element()
    }

    /// 進捗（「3 / 120ページ」）。
    fn progress_text(item: &HistoryItem) -> Option<String> {
        let (current, total) = item.progress?;
        Some(match total {
            Some(total) => format!("{} / {total}ページ", current.max(1)),
            None => format!("{current}ページ"),
        })
    }

    /// 読了 / 未読 とお気に入りハート（リストの状態列）。
    /// `key` は出現キー（`render_card` / `render_row` と同じ）。
    fn render_state_chips(
        theme: &gpui_kit::component::Theme,
        handle: &Entity<Self>,
        item: &HistoryItem,
        key: &str,
    ) -> AnyElement {
        // 状態列 / 状態チップ / ハートの要素 id に前置する「この出現ぶん」の id。
        let element_id = format!("{key}-{}", item.book.id);
        div()
            .id(SharedString::from(format!("history-status-{element_id}")))
            .debug_selector({
                let selector = format!("history-status-{element_id}");
                move || selector.clone()
            })
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(
                div()
                    .debug_selector({
                        let selector = format!(
                            "history-state-{}-{element_id}",
                            match item.reading_state {
                                progress::ReadingState::Read => "read",
                                progress::ReadingState::Reading => "reading",
                                progress::ReadingState::Unread => "unread",
                            }
                        );
                        move || selector.clone()
                    })
                    .px_1()
                    .py_0p5()
                    .rounded_md()
                    .text_xs()
                    .when(item.reading_state == progress::ReadingState::Read, |this| {
                        this.bg(gpui_kit::rgb(0xd1fae5))
                            .text_color(gpui_kit::rgb(0x047857))
                            .child("読了")
                    })
                    .when(
                        item.reading_state == progress::ReadingState::Reading,
                        |this| {
                            this.bg(gpui_kit::rgb(0xe0f2fe))
                                .text_color(gpui_kit::rgb(0x0369a1))
                                .child("読んでいる途中")
                        },
                    )
                    .when(
                        item.reading_state == progress::ReadingState::Unread,
                        |this| {
                            this.bg(gpui_kit::rgb(0xfef3c7))
                                .text_color(gpui_kit::rgb(0xb45309))
                                .child("未読")
                        },
                    ),
            )
            .child(Self::render_heart(theme, handle, item, key, 24.0))
            .into_any_element()
    }

    /// お気に入りハート（リストの状態列の丸ボタン）。行の背景の上に乗るのでテーマの色で塗る。
    /// **表紙の上に重ねるカードは `cover_badge`**（本棚のカードと共有。表紙の上で読める配色）を使う。
    fn render_heart(
        theme: &gpui_kit::component::Theme,
        handle: &Entity<Self>,
        item: &HistoryItem,
        key: &str,
        size: f32,
    ) -> AnyElement {
        let database_id = item.book.id.clone();
        // カードのハートと同じ id を使うが、出現キーを前置する（同じ本が複数出ても重複しない）。
        let element_id = format!("{key}-{database_id}");
        let is_favorite = item.book.is_favorite == 1;
        // 行の背景の上なので、本棚の行の状態アイコンと同じ配色（テーマで変わる）
        let (circle, heart) = row_heart_colors(theme, is_favorite);
        div()
            .id(SharedString::from(format!("history-heart-{element_id}")))
            .debug_selector({
                let selector = format!("history-heart-{element_id}");
                move || selector.clone()
            })
            .flex()
            .items_center()
            .justify_center()
            .w(px(size))
            .h(px(size))
            .rounded_full()
            .bg(circle)
            .text_color(heart)
            .cursor_pointer()
            .hover(|style| style.bg(theme.secondary))
            .tooltip({
                let text = if is_favorite {
                    "お気に入り（クリックで解除）"
                } else {
                    "お気に入りにする"
                }
                .to_string();
                move |window, cx| Tooltip::new(text.clone()).build(window, cx)
            })
            .on_click({
                let handle = handle.clone();
                let book_id = database_id.clone();
                move |_, _, cx| {
                    cx.stop_propagation();
                    handle.update(cx, |this, cx| this.toggle_favorite(cx, &book_id));
                }
            })
            .child(
                Icon::new(if is_favorite {
                    AppIcon::HeartFilled
                } else {
                    AppIcon::Heart
                })
                .size(px(14.0)),
            )
            .into_any_element()
    }

    /// 仮想化リストの 1 行（日付バー / その日のカード・リストの行）を構築する。
    /// `gpui_kit::list` の行クロージャから可視行ぶんだけ呼ばれる。
    ///
    /// `theme` / `tag_order` / `selected` / `card_width` は描画時の値をクロージャから
    /// 受け取る（借用を閉じるため、ここではビューの一部＝ `rows` / `days` を読む）。
    #[allow(clippy::too_many_arguments)]
    fn render_list_row(
        &self,
        window: &mut Window,
        theme: &gpui_kit::component::Theme,
        handle: &Entity<Self>,
        row_ix: usize,
        tag_order: &TagOrder,
        selected: Option<(usize, usize)>,
        view_mode: ViewMode,
        card_width: f32,
    ) -> AnyElement {
        let Some(row) = self.rows.get(row_ix) else {
            return div().into_any_element();
        };
        match *row {
            // 日付は背景色を変えた「バー」にする（カードと同じ色にしない）
            HistoryRow::Day { day_index } => {
                let Some(day) = self.days.get(day_index) else {
                    return div().into_any_element();
                };
                div()
                    .flex()
                    .flex_col()
                    // バーと本文の間隔（以前の `gap_2`）
                    .pb_2()
                    .w_full()
                    .child(
                        div()
                            .id(SharedString::from(format!("history-day-{}", day.date)))
                            // デバッグ用の id は「何番目の日か」で固定する（テストが
                            // 日付に依存せずに検証できるように）
                            .debug_selector({
                                let selector = format!("history-day-{day_index}");
                                move || selector.clone()
                            })
                            .w_full()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(day_bar_bg(theme))
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::BOLD)
                            .child(day.label.clone()),
                    )
                    .into_any_element()
            }
            HistoryRow::Items {
                day_index,
                first_item_index,
                len,
            } => {
                let Some(day) = self.days.get(day_index) else {
                    return div().into_any_element();
                };
                let Some(items) = day.items.get(first_item_index..first_item_index + len) else {
                    return div().into_any_element();
                };
                // 出現キー（`{日付}-{その日の連番}`）。同じ本が複数の日に出ても
                // 要素 id が重複しないように、カード / 行が描く id へ前置する。
                let item_state = |item_index: usize, item: &HistoryItem| {
                    // 展開状態も出現ごと（`render_tag_row` と同じ要素 id の組み立て）
                    let element_id = format!("{}-{item_index}-{}", day.date, item.book.id);
                    ItemState {
                        tag_order,
                        tags_expanded: self.expanded_tag_rows.contains(&element_id),
                        selected: selected == Some((day_index, item_index)),
                        favorite_circles: &self.favorite_circles,
                        favorite_authors: &self.favorite_authors,
                    }
                };
                let key = |offset: usize| format!("{}-{}", day.date, first_item_index + offset);
                match view_mode {
                    ViewMode::Card => div()
                        .flex()
                        .flex_row()
                        .gap_3()
                        // カードの行の間隔（以前の折り返しの `gap_3`）
                        .pb_3()
                        .w_full()
                        .children(items.iter().enumerate().map(|(offset, item)| {
                            HistoryView::render_card(
                                window,
                                theme,
                                handle,
                                item,
                                &key(offset),
                                card_width,
                                &item_state(first_item_index + offset, item),
                            )
                        }))
                        .into_any_element(),
                    ViewMode::List => div()
                        .flex()
                        .flex_col()
                        // 行の間隔（以前の `gap_2`）
                        .pb_2()
                        .w_full()
                        .children(items.iter().enumerate().map(|(offset, item)| {
                            HistoryView::render_row(
                                window,
                                theme,
                                handle,
                                item,
                                &key(offset),
                                &item_state(first_item_index + offset, item),
                            )
                        }))
                        .into_any_element(),
                }
            }
        }
    }
}

/// 履歴カードの表紙に重ねるバッジ（本棚のカードと同じ `CoverBadge` を共有する）。
/// 右上 = お気に入り（`books.is_favorite`）、右下 = ダウンロード済み（履歴の本は必ずローカル本）。
fn card_badges(is_favorite: bool) -> (CoverBadge, CoverBadge) {
    (
        CoverBadge::favorite(is_favorite),
        CoverBadge::downloaded(true),
    )
}

/// リスト行のお気に入りハートの配色（丸の地色, ハートの色）。
///
/// 表紙の上に乗るカードは [`cover_badge`] を使い、こちらは**行の背景の上**なので
/// 本棚の行の状態アイコンと共通の [`StatusIconPalette`] から色を取る（テーマで変わる）。
fn row_heart_colors(
    theme: &gpui_kit::component::Theme,
    is_favorite: bool,
) -> (gpui_kit::Hsla, gpui_kit::Hsla) {
    (
        theme.background,
        StatusIconPalette::for_theme(theme).favorite(is_favorite),
    )
}

/// 日付バーの背景色。カードの `muted` と区別できるようプライマリを薄く敷く
/// （ダークでは `muted` / `secondary` が同色なので、そこを避ける）。
fn day_bar_bg(theme: &gpui_kit::component::Theme) -> gpui_kit::Hsla {
    theme
        .primary
        .opacity(if theme.is_dark() { 0.22 } else { 0.12 })
}

/// 本棚と同じ表記の「イベント名 or 購入日」。紐づく本棚アイテムが無ければイベント不明。
pub(crate) fn shelf_event_text(shelf: Option<&db::bookshelf::BookshelfItem>) -> String {
    let Some(shelf) = shelf else {
        return "イベント不明".to_string();
    };
    let purchase_date = shelf
        .caused_at
        .as_deref()
        .map(crate::views::bookshelf::format_purchase_date);
    match (shelf.site_id.as_str(), purchase_date.as_deref()) {
        ("booth", Some(date)) | ("fanza", Some(date)) | ("dlsite", Some(date)) => {
            format!("購入日: {date}")
        }
        _ => shelf
            .event_name
            .as_deref()
            .map(crate::views::bookshelf::format_event_label)
            .unwrap_or_else(|| "イベント不明".to_string()),
    }
}

/// サイト id の表示名（本棚のサイドバーと同じ表記）。
fn site_label(site: &str) -> String {
    match site {
        "techbookfest" => "技術書典".to_string(),
        "booth" => "BOOTH".to_string(),
        "fanza" => "FANZA同人".to_string(),
        "dlsite" => "DLsite".to_string(),
        other => other.to_string(),
    }
}

impl Render for HistoryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 初回描画でフォーカスを取る（キーボード操作を効かせる）
        if !self.focus_initialized {
            self.focus_initialized = true;
            window.focus(&self.focus_handle, cx);
        }
        let theme = cx.theme().clone();
        let handle = cx.entity();
        let view_mode = self.view_mode;
        let tag_order = TagOrder::new(
            &self.favorite_tags,
            &self.selected_tags,
            self.tag_counts.clone(),
        );
        let window_width = window.bounds().size.width.as_f32();
        // カードの列数（リスト表示は 1 行 1 件なので 1 列として扱う）
        let columns = match view_mode {
            ViewMode::Card => {
                crate::views::bookshelf::BookshelfView::columns_for_width(window_width)
            }
            ViewMode::List => 1,
        };
        let content_width = window_width - SIDEBAR_W - 24.0;
        let card_width =
            ((content_width - (columns as f32 - 1.0) * 12.0) / columns as f32).max(160.0);
        // 仮想化: 行（日付バー + カード/リストの行）を組み直して、可視行だけを構築する。
        // 列数はウィンドウ幅で決まるので、幅が変われば行数も変わる
        if self.rows_stale || self.rows_columns != columns {
            self.rebuild_rows(columns, card_width);
        }

        // 選択中の出現（何日目の何冊目か）。同じ本が複数の日に出ても、選択枠はその出現だけ。
        let selected = selected_occurrence(self.selected_index, &self.days);
        let empty = self.days.is_empty();
        let list_state = self.list_state.clone();
        let row_theme = theme.clone();
        let rows = gpui_kit::list(list_state, move |row_ix, window, cx| {
            // 借用を閉じるため、可視行に必要なものは view から読む
            // （可視行のみなので、全行を組むより桁違いに軽い）
            let view = handle.read(cx);
            #[cfg(test)]
            view.built_rows
                .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            view.render_list_row(
                window,
                &row_theme,
                &handle,
                row_ix,
                &tag_order,
                selected,
                view_mode,
                card_width,
            )
        });

        div()
            .id("history-root")
            .debug_selector(|| "history-root".into())
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
            .child(self.render_header(cx))
            .child(
                // 仮想化リストの親。高さを親（`history-root` = `flex_1`）に束縛する
                // （指定しないと内容高さまで伸び、リストがスクロールできない）
                div()
                    .id("history-scroll")
                    .debug_selector(|| "history-scroll".into())
                    .flex_1()
                    .min_h_0()
                    .when(empty, |this| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("まだ閲覧履歴がありません"),
                        )
                    })
                    .when(!empty, |this| this.child(rows.h_full().w_full())),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::AppContext as _;

    /// 期間フィルタ: 全項目 / 今日 / 今週（月曜始まり） / 今月。
    #[test]
    fn period_filter_matches_dates() {
        // 2026-09-13 は日曜日 → 今週は 09-07（月）〜 09-13
        let today = "2026-09-13";
        assert_eq!(week_start(today), "2026-09-07");
        assert!(HistoryPeriod::All.contains("2020-01-01", today));
        assert!(HistoryPeriod::Today.contains("2026-09-13", today));
        assert!(!HistoryPeriod::Today.contains("2026-09-12", today));
        assert!(HistoryPeriod::Week.contains("2026-09-07", today));
        assert!(HistoryPeriod::Week.contains("2026-09-13", today));
        assert!(!HistoryPeriod::Week.contains("2026-09-06", today));
        assert!(HistoryPeriod::Month.contains("2026-09-01", today));
        assert!(!HistoryPeriod::Month.contains("2026-08-31", today));
    }

    /// 日付ラベル: `YYYY-MM-DD` → `YYYY年MM月DD日`。
    #[test]
    fn day_label_is_japanese() {
        assert_eq!(day_label("2026-09-13"), "2026年09月13日");
        assert_eq!(day_label("bad"), "bad");
    }

    /// イベント名 or 購入日（本棚のカードと同じ表記）。紐づく本棚アイテムが無ければイベント不明。
    #[test]
    fn shelf_event_text_matches_the_bookshelf() {
        assert_eq!(shelf_event_text(None), "イベント不明");
        let item = db::bookshelf::BookshelfItem {
            site_id: "techbookfest".into(),
            event_name: Some("技術書典18".into()),
            caused_at: Some("2026/01/01 19:36:23".into()),
            ..test_shelf_item("db-1")
        };
        assert_eq!(shelf_event_text(Some(&item)), "技術書典18");
        // BOOTH / FANZA / DLsite はイベント名ではなく購入日を出す（本棚と同じ）
        let item = db::bookshelf::BookshelfItem {
            site_id: "fanza".into(),
            event_name: Some("無視される".into()),
            caused_at: Some("2026/01/01 19:36:23".into()),
            ..test_shelf_item("db-2")
        };
        assert_eq!(shelf_event_text(Some(&item)), "購入日: 2026/01/01");
    }

    fn test_shelf_item(database_id: &str) -> db::bookshelf::BookshelfItem {
        db::bookshelf::BookshelfItem {
            site_id: "techbookfest".into(),
            database_id: database_id.into(),
            title: "本".into(),
            circle_name: "サークル".into(),
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
        }
    }

    /// 読書状態は 未読 / 読書中 / 読了 の 3 状態（本棚と同じ `ReadingState`）。
    /// 途中まで読んだ本が「未読」に丸められないことを確かめる。
    #[gpui_kit::test]
    async fn history_shows_reading_state(cx: &mut gpui_kit::TestAppContext) {
        use chrono::Duration;
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        for (id, title) in [("b1", "未読の本"), ("b2", "途中の本"), ("b3", "読了の本")]
        {
            seed_book(cx, id, title, "techbookfest");
        }
        cx.update(|cx| {
            let pool = &crate::app_state::AppState::global(cx).db_pool;
            let progress = |book: &str, page: i64, total: Option<i64>, finished: bool| {
                progress::upsert(
                    pool,
                    &progress::ReadingProgress {
                        book_id: book.into(),
                        content_id: String::new(),
                        current_page: page,
                        total_pages: total,
                        finished_at: finished.then(|| "2026-01-01 00:00:00".to_string()),
                        last_read_at: "2026-01-01 00:00:00".into(),
                    },
                )
                .unwrap();
            };
            // b1 は進捗なし（未読）、b2 は途中（読書中）、b3 は最終ページ（読了）
            progress("b2", 8, Some(10), false);
            progress("b3", 10, Some(10), true);
        });
        // ローカルの正午を基準にする（実行時刻で日付がずれないように）。新しい順に b1 → b2 → b3
        let now = local_noon() - Duration::hours(1);
        add_session(cx, "s1", "b1", now, 10);
        add_session(cx, "s2", "b2", now - Duration::minutes(10), 10);
        add_session(cx, "s3", "b3", now - Duration::minutes(20), 10);

        let view = cx.new(HistoryView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        // リスト形式（状態チップにデバッグ id がある）で確認する
        view.update(cx, |this, cx| {
            this.view_mode = ViewMode::List;
            cx.notify();
        });
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        for (selector, expected) in [
            (
                interned(format!(
                    "history-state-unread-{}-b1",
                    occurrence_key(now, 0)
                )),
                "未読",
            ),
            (
                interned(format!(
                    "history-state-reading-{}-b2",
                    occurrence_key(now - Duration::minutes(10), 1)
                )),
                "読んでいる途中",
            ),
            (
                interned(format!(
                    "history-state-read-{}-b3",
                    occurrence_key(now - Duration::minutes(20), 2)
                )),
                "読了",
            ),
        ] {
            assert!(
                visual.debug_bounds(selector).is_some(),
                "{expected} の状態チップが出ていない（{selector}）"
            );
        }
    }

    /// 表紙は本棚と同じ順で解決する（サイトのサムネイルキャッシュを最優先）。
    /// キャッシュに 4:1 の PNG を置き、カードの表紙画像がその比率で描かれることで確かめる
    /// （pack の表紙もプレースホルダ（3:4）も無い本なので、比率が違えば別の絵を使っている）。
    #[gpui_kit::test]
    async fn history_cover_prefers_the_site_thumbnail_cache(cx: &mut gpui_kit::TestAppContext) {
        use chrono::{Duration, Utc};
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        seed_book(cx, "b1", "本1", "techbookfest");
        // 本棚アイテムに紐づけて、サイトのサムネイルキャッシュを置く
        cx.update(|cx| {
            let state = crate::app_state::AppState::global(cx);
            let pool = &state.db_pool;
            db::bookshelf::upsert(
                pool,
                &db::bookshelf::BookshelfItem {
                    ..test_shelf_item("db-cache-1")
                },
            )
            .unwrap();
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = 'db-cache-1' WHERE id = 'b1'")
                    .execute(pool)
                    .await
            })
            .unwrap();
            // キャッシュ（448px へ縮小される前の元画像）を 4:1 で作る
            let dir = state.data_dir.join("thumbnails");
            std::fs::create_dir_all(&dir).unwrap();
            let path =
                crate::views::bookshelf::cover_cache_path(&dir, "techbookfest", "db-cache-1");
            let img = image::RgbaImage::from_pixel(160, 40, image::Rgba([10, 20, 30, 255]));
            img.save(&path).unwrap();
        });
        let started = Utc::now() - Duration::hours(1);
        add_session(cx, "s1", "b1", started, 10);

        let view = cx.new(HistoryView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
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
        let key = occurrence_key(started, 0);
        let cover = visual
            .debug_bounds(interned(format!("history-card-cover-img-{key}-b1")))
            .expect("カードの表紙画像が出ていない");
        let ratio = cover.size.width.as_f32() / cover.size.height.as_f32();
        assert!(
            (ratio - 4.0).abs() < 0.3,
            "サイトのサムネイルキャッシュが使われていない（比率 {ratio}）"
        );
        // ダウンロード済みバッジ（本棚のカードと同じ）
        assert!(
            visual
                .debug_bounds(interned(format!("history-card-cover-downloaded-{key}-b1")))
                .is_some(),
            "ダウンロード済みバッジが出ていない"
        );
        // 後始末（temp のキャッシュを残さない）
        cx.update(|cx| {
            let state = crate::app_state::AppState::global(cx);
            let path = crate::views::bookshelf::cover_cache_path(
                &state.data_dir.join("thumbnails"),
                "techbookfest",
                "db-cache-1",
            );
            let _ = std::fs::remove_file(path);
        });
    }

    /// 履歴カードの表紙オーバーレイ（右上 = お気に入り / 右下 = ダウンロード済み）は、
    /// **本棚のカードと同じ `CoverBadge`** を使う。表紙の上で読める配色（オフ = 60% 黒の
    /// スクリム + 白アイコン / オン = 不透明の色チップ + 白アイコン）と、状態で形が変わること、
    /// ホバー説明はこの共有で揃う。履歴はすべてローカル本なので、ダウンロードは常に「済み」。
    #[test]
    fn history_card_badges_reuse_the_shelf_cover_badges() {
        assert_eq!(
            card_badges(true),
            (CoverBadge::favorite(true), CoverBadge::downloaded(true))
        );
        assert_eq!(
            card_badges(false),
            (
                CoverBadge::favorite(false),
                // 「未ダウンロード」にすると、取り込み済みの本に「カードをクリックで取り込み」と
                // 出てしまう（履歴の本は必ずローカル本）
                CoverBadge::downloaded(true)
            )
        );
    }

    /// カードの表紙オーバーレイ（右上 = お気に入り / 右下 = ダウンロード済み）は
    /// **表紙の枠の内側**に乗る（本棚のカードと同じ位置。表紙の外＝隣のカードに食い込まない）。
    #[gpui_kit::test]
    async fn history_card_badges_sit_inside_the_cover(cx: &mut gpui_kit::TestAppContext) {
        use chrono::{Duration, Utc};
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        seed_book(cx, "b1", "本1", "techbookfest");
        // 本棚アイテムに紐づけて、表紙のサムネイル（3:4）を置く
        cx.update(|cx| {
            let state = crate::app_state::AppState::global(cx);
            let pool = &state.db_pool;
            db::bookshelf::upsert(
                pool,
                &db::bookshelf::BookshelfItem {
                    ..test_shelf_item("db-1")
                },
            )
            .unwrap();
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = 'db-1' WHERE id = 'b1'")
                    .execute(pool)
                    .await
            })
            .unwrap();
            let dir = state.data_dir.join("thumbnails");
            std::fs::create_dir_all(&dir).unwrap();
            let path = crate::views::bookshelf::cover_cache_path(&dir, "techbookfest", "db-1");
            let image = image::RgbImage::from_pixel(300, 400, image::Rgb([10, 20, 30]));
            image::DynamicImage::ImageRgb8(image).save(&path).unwrap();
        });
        let started = Utc::now() - Duration::hours(1);
        add_session(cx, "s1", "b1", started, 10);

        let view = cx.new(HistoryView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
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
        let key = occurrence_key(started, 0);
        let cover = visual
            .debug_bounds(interned(format!("history-card-cover-img-{key}-b1")))
            .expect("カードの表紙画像が出ていない");
        for selector in [
            interned(format!("history-heart-{key}-b1")),
            interned(format!("history-card-cover-downloaded-{key}-b1")),
        ] {
            let badge = visual
                .debug_bounds(selector)
                .unwrap_or_else(|| panic!("{selector} が出ていない"));
            let (cx0, cy0) = (cover.origin.x.as_f32(), cover.origin.y.as_f32());
            let (cw, ch) = (cover.size.width.as_f32(), cover.size.height.as_f32());
            let (bx, by) = (badge.origin.x.as_f32(), badge.origin.y.as_f32());
            let (bw, bh) = (badge.size.width.as_f32(), badge.size.height.as_f32());
            assert!(
                bx >= cx0 - 0.5
                    && by >= cy0 - 0.5
                    && bx + bw <= cx0 + cw + 0.5
                    && by + bh <= cy0 + ch + 0.5,
                "{selector} が表紙の外に出ている（表紙 {cx0},{cy0} {cw}x{ch} / \
                 バッジ {bx},{by} {bw}x{bh}）"
            );
        }
        // 後始末（temp のキャッシュを残さない）
        cx.update(|cx| {
            let state = crate::app_state::AppState::global(cx);
            let path = crate::views::bookshelf::cover_cache_path(
                &state.data_dir.join("thumbnails"),
                "techbookfest",
                "db-1",
            );
            let _ = std::fs::remove_file(path);
        });
    }

    /// リスト行のお気に入りハートは、**本棚の行の状態アイコンと同じ配色**
    /// （`StatusIconPalette` を共有）を使う。表紙の上に乗るカードは `cover_badge`。
    /// 行の背景はテーマで変わるので、両テーマで固定する。
    #[gpui_kit::test]
    async fn history_row_heart_reuses_the_shelf_status_colors(cx: &mut gpui_kit::TestAppContext) {
        cx.update(gpui_kit::component::init);
        for (name, mode) in [
            ("light", gpui_kit::component::ThemeMode::Light),
            ("dark", gpui_kit::component::ThemeMode::Dark),
        ] {
            let (on, off, palette) = cx.update(|cx| {
                if mode == gpui_kit::component::ThemeMode::Light {
                    crate::theme::apply_light_surfaces(cx);
                } else {
                    crate::theme::apply_dark_surfaces(cx);
                }
                gpui_kit::component::Theme::change(mode, None, cx);
                let theme = cx.theme();
                (
                    row_heart_colors(theme, true),
                    row_heart_colors(theme, false),
                    StatusIconPalette::for_theme(theme),
                )
            });
            assert_eq!(
                on.1,
                palette.favorite(true),
                "{name}: 登録済みの色が本棚と違う"
            );
            assert_eq!(
                off.1,
                palette.favorite(false),
                "{name}: 未登録の色が本棚と違う"
            );
            assert_ne!(on.1, off.1, "{name}: 登録 / 未登録で色が変わらない");
            // 丸の地色は行の背景（テーマ）から取る
            assert_eq!(on.0, off.0, "{name}: 丸の地色は状態で変わらない");
        }
    }

    /// 選択移動の計算（本棚と同じ規則: dx は ±1、dy は行ぶん、端で止まる）。
    #[test]
    fn next_selection_index_clamps_and_handles_empty() {
        assert_eq!(
            next_selection_index(None, 0, 1, 0, 1),
            None,
            "空なら移動しない"
        );
        // 初期状態（未選択）からは先頭
        assert_eq!(next_selection_index(None, 3, 1, 0, 1), Some(1));
        // 端で止まる
        assert_eq!(next_selection_index(Some(2), 3, 1, 0, 1), Some(2));
        assert_eq!(next_selection_index(Some(0), 3, -1, 0, 1), Some(0));
        // カードは列数ぶん上下に動く
        assert_eq!(next_selection_index(Some(1), 10, 0, 1, 4), Some(5));
        assert_eq!(next_selection_index(Some(5), 10, 0, -1, 4), Some(1));
    }

    /// 仮想化の行モデル: 日付バー + カードの行（1 行 `columns` 件）。リストは 1 行 1 件。
    #[test]
    fn rows_are_day_bars_and_item_rows() {
        let days = vec![
            test_history_day("2026-09-13", 5),
            test_history_day("2026-09-12", 1),
        ];
        // カード 2 列: 5 件は 2 + 2 + 1 の 3 行に割れる
        assert_eq!(
            build_rows(&days, ViewMode::Card, 2),
            vec![
                HistoryRow::Day { day_index: 0 },
                HistoryRow::Items {
                    day_index: 0,
                    first_item_index: 0,
                    len: 2,
                },
                HistoryRow::Items {
                    day_index: 0,
                    first_item_index: 2,
                    len: 2,
                },
                HistoryRow::Items {
                    day_index: 0,
                    first_item_index: 4,
                    len: 1,
                },
                HistoryRow::Day { day_index: 1 },
                HistoryRow::Items {
                    day_index: 1,
                    first_item_index: 0,
                    len: 1,
                },
            ],
            "日付バー + カードの行になっていない"
        );
        // 列数が増えれば行数は減る（日付バーは日の数だけ残る）
        assert_eq!(
            build_rows(&days, ViewMode::Card, 5).len(),
            2 + 1 + 1,
            "列数が変わっても行数が変わっていない"
        );
        // リスト表示は列数を無視して 1 行 1 件
        assert_eq!(
            build_rows(&days, ViewMode::List, 5),
            vec![
                HistoryRow::Day { day_index: 0 },
                HistoryRow::Items {
                    day_index: 0,
                    first_item_index: 0,
                    len: 1,
                },
                HistoryRow::Items {
                    day_index: 0,
                    first_item_index: 1,
                    len: 1,
                },
                HistoryRow::Items {
                    day_index: 0,
                    first_item_index: 2,
                    len: 1,
                },
                HistoryRow::Items {
                    day_index: 0,
                    first_item_index: 3,
                    len: 1,
                },
                HistoryRow::Items {
                    day_index: 0,
                    first_item_index: 4,
                    len: 1,
                },
                HistoryRow::Day { day_index: 1 },
                HistoryRow::Items {
                    day_index: 1,
                    first_item_index: 0,
                    len: 1,
                },
            ],
            "リスト表示が 1 行 1 件になっていない"
        );
        // 履歴が無ければ行も無い（空状態の文言だけを出す）
        assert!(build_rows(&[], ViewMode::Card, 3).is_empty());
        assert!(build_rows(&[], ViewMode::List, 1).is_empty());
    }

    /// 列数（＝ウィンドウ幅）が変わったら行を組み直し、仮想化リストの件数も追従する。
    #[gpui_kit::test]
    async fn rows_are_rebuilt_when_the_column_count_changes(cx: &mut gpui_kit::TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        let view = cx.new(HistoryView::new);
        view.update(cx, |this, _| {
            this.days = vec![test_history_day("2026-09-13", 5)];
            this.rebuild_rows(2, 200.0);
            assert_eq!(this.rows_columns, 2, "組んだときの列数を覚えていない");
            assert_eq!(this.rows.len(), 1 + 3, "2 列で 5 件が 3 行になっていない");
            assert_eq!(
                this.list_state.item_count(),
                this.rows.len(),
                "仮想化リストの件数が行数と合っていない"
            );
            // 列数が変われば行数も変わる（仮想化リストへも差し替わる）
            this.rebuild_rows(3, 200.0);
            assert_eq!(this.rows_columns, 3);
            assert_eq!(this.rows.len(), 1 + 2, "3 列で 5 件が 2 行になっていない");
            assert_eq!(this.list_state.item_count(), this.rows.len());
            // リスト表示は列数に関わらず 1 行 1 件（有効な列数は 1）
            this.view_mode = ViewMode::List;
            this.rebuild_rows(3, 200.0);
            assert_eq!(this.rows_columns, 1, "リスト表示の列数が 1 になっていない");
            assert_eq!(this.rows.len(), 1 + 5);
            assert_eq!(this.list_state.item_count(), this.rows.len());
        });
    }

    /// キーボード操作: ←→↑↓ / hjkl で選択が動き、ESC で絞り込みが解除される。
    #[gpui_kit::test]
    async fn history_keyboard_moves_selection_and_clears_filters(
        cx: &mut gpui_kit::TestAppContext,
    ) {
        use chrono::{Duration, Utc};
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        for (id, title) in [("b1", "本1"), ("b2", "本2"), ("b3", "本3")] {
            seed_book(cx, id, title, "techbookfest");
        }
        let now = Utc::now() - Duration::hours(1);
        add_session(cx, "s1", "b1", now, 10);
        add_session(cx, "s2", "b2", now - Duration::minutes(10), 10);
        add_session(cx, "s3", "b3", now - Duration::days(1), 10);

        let view = cx.new(HistoryView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
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
        // 初期は先頭を選択
        assert_eq!(view.read_with(cx, |this, _| this.selected_index), Some(0));
        // カード形式は「行ぶん」移動（1200px = 4 列）。3 件なので末尾にクランプされる
        visual.simulate_keystrokes("down");
        assert_eq!(
            view.read_with(cx, |this, _| this.selected_index),
            Some(2),
            "カードの ↓ が行ぶん移動になっていない"
        );
        visual.simulate_keystrokes("up");
        assert_eq!(
            view.read_with(cx, |this, _| this.selected_index),
            Some(0),
            "カードの ↑ で戻っていない"
        );
        // リスト表示は 1 列 = ±1
        view.update(cx, |this, cx| {
            this.view_mode = ViewMode::List;
            cx.notify();
        });
        draw(visual);
        visual.simulate_keystrokes("j");
        assert_eq!(
            view.read_with(cx, |this, _| this.selected_index),
            Some(1),
            "リストの j が ±1 になっていない"
        );
        visual.simulate_keystrokes("down");
        assert_eq!(view.read_with(cx, |this, _| this.selected_index), Some(2));
        visual.simulate_keystrokes("k");
        assert_eq!(view.read_with(cx, |this, _| this.selected_index), Some(1));
        visual.simulate_keystrokes("right");
        assert_eq!(
            view.read_with(cx, |this, _| this.selected_index),
            Some(2),
            "リストの → が ±1 になっていない"
        );
        visual.simulate_keystrokes("left");
        visual.simulate_keystrokes("h");
        assert_eq!(
            view.read_with(cx, |this, _| this.selected_index),
            Some(0),
            "リストの h で先頭に戻っていない"
        );
        // ESC: 絞り込み中だけ解除する
        view.update(cx, |this, cx| this.set_period(HistoryPeriod::Today, cx));
        draw(visual);
        assert!(view.read_with(cx, |this, _| this.period) == HistoryPeriod::Today);
        visual.simulate_keystrokes("escape");
        assert!(
            view.read_with(cx, |this, _| this.period) == HistoryPeriod::All,
            "ESC で絞り込みが解除されていない"
        );
    }

    /// タグ列は本棚と同じ見た目で出て、クリックで絞り込み・ハートでお気に入り・「+n」で展開。
    #[gpui_kit::test]
    async fn history_tag_column_filters_favorites_and_expands(cx: &mut gpui_kit::TestAppContext) {
        use chrono::Duration;
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        seed_book(cx, "b1", "本1", "techbookfest");
        seed_book(cx, "b2", "本2", "techbookfest");
        cx.update(|cx| {
            let pool = &crate::app_state::AppState::global(cx).db_pool;
            // b1 は 20 タグ（折りたたみの確認用に 1 つは識別しやすい名前にする）
            let mut tags: Vec<(String, &str)> = (1..=19)
                .map(|i| (format!("タグ{i:02}"), "manual"))
                .collect();
            tags.push(("しぼりこみ".to_string(), "manual"));
            let pairs: Vec<(&str, &str)> = tags.iter().map(|(t, s)| (t.as_str(), *s)).collect();
            db::tags::set_for_book(pool, "b1", &pairs).unwrap();
            db::tags::set_for_book(pool, "b2", &[("べつのタグ", "manual")]).unwrap();
        });
        // ローカルの正午を基準にする（実行時刻で日付がずれないように）。b1 → b2 の順
        let now = local_noon() - Duration::hours(1);
        add_session(cx, "s1", "b1", now, 10);
        add_session(cx, "s2", "b2", now - Duration::minutes(5), 10);

        let view = cx.new(HistoryView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
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
        // タグ行 / チップの要素 id には出現キー（日付 + その日の連番）が前置される
        let key = occurrence_key(now, 0);
        // カード形式（既定）にタグ列が出る
        assert!(
            visual
                .debug_bounds(interned(format!("history-tag-row-{key}-b1")))
                .is_some(),
            "タグ行が出ていない"
        );
        assert!(
            visual
                .debug_bounds(interned(format!("tag-label-{key}-b1-しぼりこみ")))
                .is_some(),
            "タグチップが出ていない（集計数の多い順で先頭のはず）"
        );
        // 「+n」で展開できる（20 タグなので折りたたまれる）
        let toggle = visual
            .debug_bounds(interned(format!("history-tag-toggle-{key}-b1")))
            .expect("「+n」トグルが出ていない");
        visual.simulate_click(toggle.center(), gpui_kit::Modifiers::default());
        draw(visual);
        assert!(
            visual
                .debug_bounds(interned(format!("tag-label-{key}-b1-タグ19")))
                .is_some(),
            "展開しても全タグが出ていない"
        );
        // タグのお気に入り（ハート）
        let heart = visual
            .debug_bounds(interned(format!("tag-heart-{key}-b1-しぼりこみ")))
            .expect("タグのハートが出ていない");
        visual.simulate_click(heart.center(), gpui_kit::Modifiers::default());
        draw(visual);
        assert!(
            view.read_with(cx, |this, _| this
                .favorite_tags
                .contains(&"しぼりこみ".to_string())),
            "ハートでタグがお気に入りになっていない"
        );
        // タグの文字クリックで絞り込み（b1 だけになる）
        let label = visual
            .debug_bounds(interned(format!("tag-label-{key}-b1-しぼりこみ")))
            .expect("タグの文字");
        visual.simulate_click(label.center(), gpui_kit::Modifiers::default());
        draw(visual);
        assert_eq!(
            view.read_with(cx, |this, _| this.flat_book_ids().len()),
            1,
            "タグで絞り込めていない"
        );
    }

    /// 履歴の本を seed する（ローカルの本 = `books`）。
    fn seed_book(cx: &mut gpui_kit::TestAppContext, id: &str, title: &str, site: &str) {
        cx.update(|cx| {
            let pool = &crate::app_state::AppState::global(cx).db_pool;
            books::insert(
                pool,
                &books::Book {
                    id: id.into(),
                    title: title.into(),
                    author: "作者".into(),
                    circle_name: "サークル".into(),
                    purchase_date: None,
                    file_name: format!("{id}.pdf"),
                    file_size: 1,
                    opfs_path: format!("{id}.opfspack"),
                    cover_thumbnail: None,
                    tbf_product_id: None,
                    site_id: Some(site.into()),
                    tags_fetched: 1,
                    pack_id: Some(id.into()),
                    is_favorite: 0,
                    is_hidden: 0,
                    created_at: "2026-08-21 00:00:00".into(),
                    updated_at: "2026-08-21 00:00:00".into(),
                    media_category: Some("comic".into()),
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

    /// テスト用の履歴 1 件（仮想化の行モデルを組むのに使う。本 id 以外は表示に使わない）。
    fn test_history_item(id: &str) -> HistoryItem {
        HistoryItem {
            book: books::Book {
                id: id.into(),
                title: format!("本{id}"),
                author: "作者".into(),
                circle_name: "サークル".into(),
                purchase_date: None,
                file_name: format!("{id}.pdf"),
                file_size: 1,
                opfs_path: format!("{id}.opfspack"),
                cover_thumbnail: None,
                tbf_product_id: None,
                site_id: Some("techbookfest".into()),
                tags_fetched: 1,
                pack_id: None,
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
            event_text: "イベント不明".into(),
            reading_state: progress::ReadingState::Unread,
            progress: None,
            tags: Vec::new(),
            cover: None,
        }
    }

    /// テスト用の履歴 1 日ぶん（`count` 件）。
    fn test_history_day(date: &str, count: usize) -> HistoryDay {
        HistoryDay {
            date: date.into(),
            label: day_label(date),
            items: (0..count)
                .map(|index| test_history_item(&format!("{date}-{index}")))
                .collect(),
        }
    }

    /// 閲覧セッションを入れる（`started_at` / `ended_at` は UTC の `YYYY-MM-DD HH:MM:SS`）。
    /// 同じ本が複数の日に出ても、表紙は 1 冊 1 回だけデコードして共有する。
    /// 以前は日ごとにデコードし直していた（同じ本を何日も読むと 1MB × 日数）。
    #[gpui_kit::test]
    async fn repeated_books_share_one_decoded_cover(cx: &mut gpui_kit::TestAppContext) {
        use chrono::{Duration, Utc};
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        seed_book(cx, "b1", "共有の本", "fanza");
        // 本棚アイテムに紐付け、サムネイルの PNG を置く（表紙が実際にデコードされるように）
        cx.update(|cx| {
            let state = crate::app_state::AppState::global(cx);
            let pool = &state.db_pool;
            db::bookshelf::upsert(pool, &test_shelf_item("db-1")).unwrap();
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = 'db-1' WHERE id = 'b1'")
                    .execute(pool)
                    .await
            })
            .unwrap();
            let thumbnails = state.data_dir.join("thumbnails");
            std::fs::create_dir_all(&thumbnails).unwrap();
            let path =
                super::super::bookshelf::cover_cache_path(&thumbnails, "techbookfest", "db-1");
            let image = image::RgbImage::from_pixel(240, 320, image::Rgb([10, 20, 30]));
            image::DynamicImage::ImageRgb8(image)
                .save(&path)
                .expect("サムネイルを書き込む");
        });
        // ローカルの正午を基準にする（実行時刻で日付がずれないように）
        let noon = chrono::Local::now()
            .date_naive()
            .and_hms_opt(12, 0, 0)
            .expect("正午")
            .and_local_timezone(chrono::Local)
            .single()
            .expect("ローカル")
            .with_timezone(&Utc);
        add_session(cx, "s1", "b1", noon, 10);
        add_session(cx, "s2", "b1", noon - Duration::days(1), 5);
        let view = cx.new(HistoryView::new);
        let (days, covers) = view.read_with(cx, |this, _| {
            (
                this.days.len(),
                this.days
                    .iter()
                    .flat_map(|day| day.items.iter())
                    .filter_map(|item| item.cover.clone())
                    .collect::<Vec<_>>(),
            )
        });
        assert_eq!(days, 2, "2 日分の履歴になっていない");
        assert_eq!(covers.len(), 2, "表紙が 2 件揃っていない");
        assert!(
            std::sync::Arc::ptr_eq(&covers[0], &covers[1]),
            "同じ本の表紙が日に何度もデコードされている"
        );
    }

    /// 同じ本が複数の日に出ても、カード / 行は日ごとに別の要素として指せる。
    /// 要素 id が本 id だけだと重複し、GPUI の要素 id は一意でなければならないため
    /// 2 つ目以降をクリックできない（クリックしても本が開かない）。
    #[gpui_kit::test]
    async fn repeated_books_get_distinct_elements(cx: &mut gpui_kit::TestAppContext) {
        use chrono::Duration;
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        seed_book(cx, "b1", "何度も読む本", "techbookfest");
        seed_book(cx, "b2", "同じ日の別の本", "techbookfest");
        // ローカルの正午を基準にする（実行時刻で日付がずれないように）
        let noon = local_noon();
        // 今日は b1（同じ日のセッションは 1 冊に集約される）と b2、昨日は b1
        add_session(cx, "s1", "b1", noon, 10);
        add_session(cx, "s2", "b1", noon - Duration::minutes(15), 10);
        add_session(cx, "s3", "b2", noon - Duration::minutes(30), 5);
        add_session(cx, "s4", "b1", noon - Duration::days(1), 10);

        let view = cx.new(HistoryView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(1200.0),
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

        // 同じ本 b1 の今日 / 昨日（同じ日の中では連番 0）と、同じ日の 2 冊目 b2（連番 1）
        let today = occurrence_key(noon, 0);
        let today_second = occurrence_key(noon - Duration::minutes(30), 1);
        let yesterday = occurrence_key(noon - Duration::days(1), 0);
        // カード形式: 同じ本でも日ごとに別の要素として指せる（重複していると 2 つ目以降が取れない）
        let today_card = visual
            .debug_bounds(interned(format!("history-card-{today}-b1")))
            .expect("今日のカードが指せない（要素 id が重複している）");
        let yesterday_card = visual
            .debug_bounds(interned(format!("history-card-{yesterday}-b1")))
            .expect("昨日のカードが指せない（要素 id が重複している）");
        assert!(
            visual
                .debug_bounds(interned(format!("history-card-{today_second}-b2")))
                .is_some(),
            "同じ日の 2 冊目のカードが指せない"
        );
        assert_ne!(
            (today_card.origin.x.as_f32(), today_card.origin.y.as_f32()),
            (
                yesterday_card.origin.x.as_f32(),
                yesterday_card.origin.y.as_f32()
            ),
            "同じ本のカードが日をまたいで同じ位置を指している"
        );
        // リスト形式も同じ（行の要素 id も本 id だけでは重複する）
        view.update(cx, |this, cx| {
            this.view_mode = ViewMode::List;
            cx.notify();
        });
        draw(visual);
        let today_row = visual
            .debug_bounds(interned(format!("history-row-{today}-b1")))
            .expect("今日の行が指せない（要素 id が重複している）");
        let yesterday_row = visual
            .debug_bounds(interned(format!("history-row-{yesterday}-b1")))
            .expect("昨日の行が指せない（要素 id が重複している）");
        assert_ne!(
            (today_row.origin.x.as_f32(), today_row.origin.y.as_f32()),
            (
                yesterday_row.origin.x.as_f32(),
                yesterday_row.origin.y.as_f32()
            ),
            "同じ本の行が日をまたいで同じ位置を指している"
        );
    }

    /// 選択枠は「出現」に付く。`selected_index` は平坦化した位置なので、同じ本が複数の日に
    /// 出ても選択中になるのはその 1 出現だけ（本 id で比べると全部が選択中になっていた）。
    #[gpui_kit::test]
    async fn selection_follows_the_occurrence_not_the_book(cx: &mut gpui_kit::TestAppContext) {
        use chrono::Duration;
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        seed_book(cx, "b1", "何度も読む本", "fanza");
        // ローカルの正午を基準にする（実行時刻で日付がずれないように）
        let noon = local_noon();
        // 同じ本を 2 日（今日 = 連番 0 / 昨日 = 連番 0）
        add_session(cx, "s1", "b1", noon, 10);
        add_session(cx, "s2", "b1", noon - Duration::days(1), 10);

        let view = cx.new(HistoryView::new);
        let (books, first, second) = view.read_with(cx, |this, _| {
            (
                this.flat_book_ids(),
                selected_occurrence(Some(0), &this.days),
                selected_occurrence(Some(1), &this.days),
            )
        });
        assert_eq!(
            books,
            vec!["b1".to_string(), "b1".to_string()],
            "同じ本が 2 日分になっていない"
        );
        assert_eq!(
            first,
            Some((0, 0)),
            "1 つ目の出現が選べていない（今日の 1 冊目）"
        );
        assert_eq!(
            second,
            Some((1, 0)),
            "2 つ目の出現が選べていない（昨日の 1 冊目）"
        );
        assert_ne!(
            first, second,
            "同じ本の 2 つの出現が同じ選択になっている（両方選択枠になる）"
        );
    }

    /// タグ列の展開（「+n」）は出現ごと。同じ本が複数の日に出ても、開いた 1 枚だけが展開される。
    #[gpui_kit::test]
    async fn tag_expansion_is_per_occurrence(cx: &mut gpui_kit::TestAppContext) {
        use chrono::Duration;
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        seed_book(cx, "b1", "同じ本", "techbookfest");
        cx.update(|cx| {
            let pool = &crate::app_state::AppState::global(cx).db_pool;
            // 折りたたまれる数 + 1（最後の 1 つが「+1」で隠れる）
            let tags: Vec<(String, &str)> = (1..=CARD_TAGS_COLLAPSED_MAX + 1)
                .map(|i| (format!("タグ{i:02}"), "manual"))
                .collect();
            let pairs: Vec<(&str, &str)> = tags.iter().map(|(t, s)| (t.as_str(), *s)).collect();
            db::tags::set_for_book(pool, "b1", &pairs).unwrap();
        });
        // ローカルの正午を基準にする（実行時刻で日付がずれないように）
        let noon = local_noon();
        add_session(cx, "s1", "b1", noon, 10);
        add_session(cx, "s2", "b1", noon - Duration::days(1), 10);

        let view = cx.new(HistoryView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(1200.0),
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
        let today = occurrence_key(noon, 0);
        let yesterday = occurrence_key(noon - Duration::days(1), 0);
        let hidden = format!("タグ{:02}", CARD_TAGS_COLLAPSED_MAX + 1);
        // 折りたたみ中は、隠れたタグがどちらの出現にも出ない
        for key in [&today, &yesterday] {
            assert!(
                visual
                    .debug_bounds(interned(format!("tag-label-{key}-b1-{hidden}")))
                    .is_none(),
                "折りたたみ中なのに {key} の隠れたタグが出ている"
            );
        }
        // 今日の出現の「+n」だけを押す
        let toggle = visual
            .debug_bounds(interned(format!("history-tag-toggle-{today}-b1")))
            .expect("「+n」トグルが出ていない");
        visual.simulate_click(toggle.center(), gpui_kit::Modifiers::default());
        draw(visual);
        assert!(
            visual
                .debug_bounds(interned(format!("tag-label-{today}-b1-{hidden}")))
                .is_some(),
            "開いた出現のタグが展開されていない"
        );
        assert!(
            visual
                .debug_bounds(interned(format!("tag-label-{yesterday}-b1-{hidden}")))
                .is_none(),
            "別の日の同じ本まで展開されている（展開が本 id で共有されている）"
        );
    }

    /// ローカルの正午（実行時刻が深夜でも「今日」がぶれない基準）。
    fn local_noon() -> chrono::DateTime<chrono::Utc> {
        chrono::Local::now()
            .date_naive()
            .and_hms_opt(12, 0, 0)
            .expect("正午")
            .and_local_timezone(chrono::Local)
            .single()
            .expect("ローカル時刻")
            .with_timezone(&chrono::Utc)
    }

    /// セッション開始時刻（UTC）が入る履歴の日付（ローカル `YYYY-MM-DD`）。
    /// `view_history::list_daily` と同じく、保存されている UTC 時刻をローカルに直して日を切る。
    fn local_day(started_utc: chrono::DateTime<chrono::Utc>) -> String {
        started_utc
            .with_timezone(&chrono::Local)
            .format("%Y-%m-%d")
            .to_string()
    }

    /// 要素 id に前置する出現キー（`{ローカル日付}-{その日の連番}`）。
    /// `started_utc` は `add_session` に渡した開始時刻、`index` はその日の何冊目か。
    fn occurrence_key(started_utc: chrono::DateTime<chrono::Utc>, index: usize) -> String {
        format!("{}-{index}", local_day(started_utc))
    }

    /// `VisualTestContext::debug_bounds` は `&'static str` しか取らないので、
    /// テストが組み立てたセレクタを static なスロットへ置いてから渡す
    /// （`Box::leak` と違って増え続けない。同じ文字列は同じスロットを共有する）。
    ///
    /// 値（セレクタ）は実行時にしか決まらず、`LazyLock` は初期化後に入れ替えられないため
    /// スロットは `OnceLock` にする（固定長の置き場。宣言時に初期化式が決まる値ではない）。
    fn interned(selector: String) -> &'static str {
        /// このテストバイナリで使うセレクタの数（並列実行の重複を見込んで余裕を持たせる）。
        const SLOTS: usize = 64;
        static POOL: [std::sync::OnceLock<String>; SLOTS] =
            [const { std::sync::OnceLock::new() }; SLOTS];
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        for slot in &POOL {
            if let Some(existing) = slot.get()
                && *existing == selector
            {
                return existing.as_str();
            }
        }
        let index = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let slot = POOL.get(index).expect("セレクタの置き場（POOL）が足りない");
        if slot.set(selector).is_err() {
            panic!("セレクタのスロットを 2 回使った");
        }
        slot.get().expect("直前に置いた").as_str()
    }

    fn add_session(
        cx: &mut gpui_kit::TestAppContext,
        id: &str,
        book_id: &str,
        started_utc: chrono::DateTime<chrono::Utc>,
        minutes: i64,
    ) {
        cx.update(|cx| {
            let pool = &crate::app_state::AppState::global(cx).db_pool;
            let ended = started_utc + chrono::Duration::minutes(minutes);
            thundoku_core::db::block_on(async {
                sqlx::query(
                    "INSERT INTO view_history (id, book_id, started_at, ended_at) \
                     VALUES (?1, ?2, ?3, ?4)",
                )
                .bind(id)
                .bind(book_id)
                .bind(started_utc.format("%Y-%m-%d %H:%M:%S").to_string())
                .bind(ended.format("%Y-%m-%d %H:%M:%S").to_string())
                .execute(pool)
                .await
            })
            .unwrap();
        });
    }

    /// 履歴画面: 日付ごとにまとまる / 期間フィルタで絞れる / カードとリストを切り替えられる。
    #[gpui_kit::test]
    async fn history_groups_by_day_and_filters(cx: &mut gpui_kit::TestAppContext) {
        use chrono::{Duration, Utc};
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        seed_book(cx, "b1", "今日の本", "fanza");
        seed_book(cx, "b2", "昨日の本", "techbookfest");
        // b1 は本棚アイテム（技術書典18）に紐づける → 本棚と同じイベント名が出るはず
        cx.update(|cx| {
            let pool = &crate::app_state::AppState::global(cx).db_pool;
            db::bookshelf::upsert(
                pool,
                &db::bookshelf::BookshelfItem {
                    event_name: Some("技術書典18".into()),
                    ..test_shelf_item("db-1")
                },
            )
            .unwrap();
            thundoku_core::db::block_on(async {
                sqlx::query("UPDATE books SET tbf_product_id = 'db-1' WHERE id = 'b1'")
                    .execute(pool)
                    .await
            })
            .unwrap();
        });
        // 「今日」と「昨日」のセッション。**ローカルの正午**を基準にする
        // （実行時刻がローカル深夜だと「1 時間前」が前日になり、テストが日付で落ちるため）
        let now = {
            chrono::Local::now()
                .date_naive()
                .and_hms_opt(12, 0, 0)
                .expect("正午")
                .and_local_timezone(chrono::Local)
                .single()
                .expect("ローカル時刻")
                .with_timezone(&Utc)
        };
        add_session(cx, "s1", "b1", now, 10);
        add_session(cx, "s2", "b1", now - Duration::minutes(30), 5);
        add_session(cx, "s3", "b2", now - Duration::days(1), 20);

        let view = cx.new(HistoryView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
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

        // 要素 id の出現キー（日付 + その日の連番）。今日は b1 の 1 冊だけ、昨日は b2
        let today = local_day(now);
        let today_key = occurrence_key(now, 0);
        let yesterday_key = occurrence_key(now - Duration::days(1), 0);
        // 日付ごとに見出しが出て、1 日 1 本に集約される
        assert!(
            visual.debug_bounds("history-day-0").is_some(),
            "日付の見出しが出ていない"
        );
        assert!(
            view.read_with(cx, |this, _| this.days.len()) == 2,
            "1 日 1 本に集約されていない"
        );
        assert!(
            view.read_with(cx, |this, _| this.flat_book_ids().len()) == 2,
            "件数が 1 日 1 本になっていない"
        );
        // 日付は背景色つきのバー（内容幅いっぱいに伸びる）
        let bar = visual.debug_bounds("history-day-0").expect("日付のバー");
        assert!(
            bar.size.width.as_f32() > 600.0,
            "日付がバーになっていない（幅 {}）",
            bar.size.width.as_f32()
        );
        // 仮想化しても並びは同じ: 今日の日付バー → 今日のカード → 昨日の日付バー
        let yesterday_bar = visual.debug_bounds("history-day-1").expect("昨日の日付バー");
        let today_card = visual
            .debug_bounds(interned(format!("history-card-{today_key}-b1")))
            .expect("今日のカード");
        assert!(
            bar.origin.y < today_card.origin.y
                && today_card.origin.y < yesterday_bar.origin.y,
            "日付バーとカードの並びが崩れている（今日 {} / カード {} / 昨日 {}）",
            bar.origin.y.as_f32(),
            today_card.origin.y.as_f32(),
            yesterday_bar.origin.y.as_f32()
        );
        // 本棚と同じ情報: 紐づく本棚アイテムのイベント名が出る
        assert_eq!(
            view.read_with(cx, |this, _| this.days[0]
                .items
                .iter()
                .find(|item| item.book.id == "b1")
                .map(|item| item.event_text.clone())),
            Some("技術書典18".to_string()),
            "本棚と同じイベント名が出ていない"
        );
        // 既定はカード形式
        assert!(
            visual
                .debug_bounds(interned(format!("history-card-{today_key}-b1")))
                .is_some(),
            "カード形式で描画されていない"
        );
        // リスト形式に切り替え
        let toggle = visual
            .debug_bounds("history-view-toggle")
            .expect("表示切替ボタン");
        visual.simulate_click(toggle.center(), gpui_kit::Modifiers::default());
        draw(visual);
        assert!(
            visual
                .debug_bounds(interned(format!("history-row-{today_key}-b1")))
                .is_some(),
            "リスト形式に切り替わっていない"
        );
        // 本棚の行と同じ 4 列構成: 表紙 → 情報列（320px 固定） → タグ列
        let cover = visual
            .debug_bounds(interned(format!("history-cover-{today_key}-b1")))
            .expect("表紙");
        let info = visual
            .debug_bounds(interned(format!("history-info-{today_key}-b1")))
            .expect("情報列");
        let tags_area = visual
            .debug_bounds(interned(format!("history-tag-area-{today_key}-b1")))
            .expect("タグ列");
        assert!(
            (info.size.width.as_f32() - LIST_INFO_W).abs() < 1.5,
            "情報列が固定幅になっていない: {} (期待 {LIST_INFO_W})",
            info.size.width.as_f32()
        );
        assert!(
            cover.origin.x < info.origin.x && info.origin.x < tags_area.origin.x,
            "列の並びが 表紙 → 情報列 → タグ列 になっていない"
        );
        // 表紙枠は本棚の行と同じ 200x133 固定
        assert!(
            (cover.size.width.as_f32() - LIST_COVER_W).abs() < 1.0
                && (cover.size.height.as_f32() - LIST_COVER_MIN_H).abs() < 1.0,
            "行の表紙枠が本棚と違う: {}x{}",
            cover.size.width.as_f32(),
            cover.size.height.as_f32()
        );
        // サークル名のチップ（本棚と同じ: 値 + ハート）
        assert!(
            visual
                .debug_bounds(interned(format!("history-circle-chip-{today_key}-b1")))
                .is_some(),
            "サークルチップが出ていない"
        );
        let heart = visual
            .debug_bounds(interned(format!("history-circle-heart-{today_key}-b1")))
            .expect("サークルのハート");
        visual.simulate_click(heart.center(), gpui_kit::Modifiers::default());
        draw(visual);
        assert!(
            view.read_with(cx, |this, _| this
                .favorite_circles
                .contains(&"サークル".to_string())),
            "ハートでサークルがお気に入りになっていない"
        );
        assert!(
            visual
                .debug_bounds(interned(format!("history-card-{today_key}-b1")))
                .is_none(),
            "カード形式が残っている"
        );
        // 期間フィルタ: 今日 → 昨日の本は出ない
        let today_button = visual
            .debug_bounds("history-period-今日")
            .expect("今日ボタン");
        visual.simulate_click(today_button.center(), gpui_kit::Modifiers::default());
        draw(visual);
        assert_eq!(
            view.read_with(cx, |this, _| this.days.len()),
            1,
            "「今日」で昨日の履歴が残っている"
        );
        assert!(
            view.read_with(cx, |this, _| this.flat_book_ids().len()) == 1,
            "「今日」の件数が合わない"
        );
        assert!(
            view.read_with(cx, |this, _| this.days[0].date == today),
            "今日以外の日が出ている"
        );
        // 全項目に戻すと昨日も出る
        let all_button = visual
            .debug_bounds("history-period-全項目")
            .expect("全項目ボタン");
        visual.simulate_click(all_button.center(), gpui_kit::Modifiers::default());
        draw(visual);
        assert_eq!(
            view.read_with(cx, |this, _| this.days.len()),
            2,
            "「全項目」で戻っていない"
        );
        // 昨日の出現（b2）も別の要素として指せる（リスト形式のまま）
        assert!(
            visual
                .debug_bounds(interned(format!("history-row-{yesterday_key}-b2")))
                .is_some(),
            "昨日の行が指せない"
        );
    }

    /// サイトフィルタ: 履歴に含まれるサイトだけ選べて、選ぶとそのサイトの本だけになる。
    #[gpui_kit::test]
    async fn history_filters_by_site(cx: &mut gpui_kit::TestAppContext) {
        use chrono::{Duration, Utc};
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        seed_book(cx, "b1", "FANZAの本", "fanza");
        seed_book(cx, "b2", "技術書典の本", "techbookfest");
        let now = Utc::now() - Duration::hours(1);
        add_session(cx, "s1", "b1", now, 10);
        add_session(cx, "s2", "b2", now - Duration::minutes(5), 10);

        let view = cx.new(HistoryView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
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
        assert_eq!(view.read_with(cx, |this, _| this.flat_book_ids().len()), 2);
        assert!(
            view.read_with(cx, |this, _| this.sites.len()) == 2,
            "サイトの候補が出ていない"
        );
        let trigger = visual
            .debug_bounds("history-site-trigger")
            .expect("サイトフィルタ");
        visual.simulate_click(trigger.center(), gpui_kit::Modifiers::default());
        draw(visual);
        let fanza = visual
            .debug_bounds("history-site-fanza")
            .expect("FANZA の選択肢");
        visual.simulate_click(fanza.center(), gpui_kit::Modifiers::default());
        draw(visual);
        assert_eq!(
            view.read_with(cx, |this, _| this.flat_book_ids().len()),
            1,
            "サイトで絞れていない"
        );
        assert!(
            view.read_with(cx, |this, _| this
                .days
                .iter()
                .flat_map(|day| day.items.iter())
                .all(|item| item.book.site_id.as_deref() == Some("fanza"))),
            "他サイトの本が残っている"
        );
    }

    /// 仮想化: 可視行だけを構築する。1 日 2 冊 × 60 日（120 件 / 120 行）を入れて、
    /// 行クロージャが構築した行数が総行数より十分少ないことを確かめる
    /// （全行を毎フレーム構築する実装に戻すと落ちる）。
    #[gpui_kit::test]
    async fn history_builds_only_visible_rows(cx: &mut gpui_kit::TestAppContext) {
        use chrono::Duration;
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        // ローカルの正午から 1 日ずつ遡る（実行時刻で日付がずれないように）
        let noon = local_noon();
        for day in 0..60 {
            for index in 0..2 {
                let id = format!("b{day}-{index}");
                seed_book(cx, &id, "本", "techbookfest");
                add_session(
                    cx,
                    &format!("s{day}-{index}"),
                    &id,
                    noon - Duration::days(day),
                    5,
                );
            }
        }

        let view = cx.new(HistoryView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
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
        let (total, built) = view.read_with(cx, |this, _| {
            (
                this.rows.len(),
                this.built_rows.load(std::sync::atomic::Ordering::Relaxed),
            )
        });
        assert!(total >= 100, "行モデルが組み上がっていない（{total} 行）");
        assert!(built > 0, "行を 1 つも構築していない（全 {total} 行）");
        assert!(
            built * 2 < total,
            "全行を構築している（4 回の描画で {built} 行 / 全 {total} 行）"
        );
    }

    /// 選択移動は「選択した本の行が見える位置」へリストを寄せる
    /// （以前は日付セクション単位でスクロール位置を計算していた）。
    #[gpui_kit::test]
    async fn moving_the_selection_scrolls_the_list(cx: &mut gpui_kit::TestAppContext) {
        use chrono::Duration;
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        // 1 日 1 冊 × 30 日（リスト表示で 1 冊ずつ下へ動かす）
        let noon = local_noon();
        for day in 0..30 {
            let id = format!("b{day}");
            seed_book(cx, &id, "本", "techbookfest");
            add_session(cx, &format!("s{day}"), &id, noon - Duration::days(day), 5);
        }

        let view = cx.new(HistoryView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(800.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        view.update(cx, |this, cx| {
            this.view_mode = ViewMode::List;
            cx.notify();
        });
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
        assert_eq!(
            view.read_with(cx, |this, _| this.list_state.logical_scroll_top().item_ix),
            0,
            "初期は先頭のはず"
        );
        // 画面に収まらない位置（20 冊目）まで選択を動かす
        for _ in 0..20 {
            visual.simulate_keystrokes("down");
        }
        assert_eq!(
            view.read_with(cx, |this, _| this.selected_index),
            Some(20),
            "選択が 20 冊目まで動いていない"
        );
        assert!(
            view.read_with(cx, |this, _| this.list_state.logical_scroll_top().item_ix) > 0,
            "選択した本の行が見える位置へスクロールしていない（先頭のまま）"
        );
    }

    /// サイト id の表示名。
    #[test]
    fn site_labels_are_japanese() {
        assert_eq!(site_label("techbookfest"), "技術書典");
        assert_eq!(site_label("fanza"), "FANZA同人");
        assert_eq!(site_label("mystery"), "mystery");
    }
}
