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
use gpui_kit::component::{ActiveTheme as _, Icon};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AnyElement, App, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement, ReadGlobal as _, Render, RenderImage, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, img, px, relative,
};

use thundoku_core::db::{self, books, bookshelf, progress};

use crate::actions::OpenReader;
use crate::app_state::AppState;
use crate::icons::AppIcon;
use crate::views::bookshelf::{
    CARD_TAGS_COLLAPSED_MAX, CHIP_HEART_BUTTON, CHIP_HEART_ICON, LIST_COVER_MIN_H, LIST_COVER_W,
    LIST_INFO_W, LIST_TAGS_W_RATIO, SIDEBAR_W, TagOrder, ViewMode, cover_fit_inside_frame,
    fit_cover_size, list_tags_visible_count, load_cached_cover, load_cover_image, no_image_cover,
    owned_book_ids, placeholder_cover,
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
    is_read: bool,
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

pub struct HistoryView {
    days: Vec<HistoryDay>,
    /// フィルタ後の件数（ヘッダの「n件」）。
    count: usize,
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
    /// タグ列を展開している本（「+n」→「閉じる」）。
    expanded_tag_rows: std::collections::HashSet<String>,
    /// キーボード操作の選択位置（日付 → 本の順に平坦化したインデックス）。
    selected_index: Option<usize>,
    focus_handle: FocusHandle,
    /// 初回描画でフォーカスを取る（本棚と同じ）。
    focus_initialized: bool,
    /// 選択に合わせたスクロール（日付セクションへ寄せる）。
    scroll_handle: ScrollHandle,
}

impl HistoryView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let view_mode = Self::read_view_mode(cx);
        let mut view = Self {
            days: Vec::new(),
            count: 0,
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
            focus_handle: cx.focus_handle(),
            focus_initialized: false,
            scroll_handle: ScrollHandle::new(),
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
        let mut count = 0;
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
            let is_read = progress::get(pool, &book.id)
                .ok()
                .flatten()
                .is_some_and(|p| p.finished_at.is_some());
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
                cover: shelf
                    .and_then(|item| load_cached_cover(&thumbnails_dir, item))
                    .or_else(|| load_cover_image(&packs_dir, book))
                    .or_else(|| placeholder_cover(&book.title, &book.circle_name)),
                book: book.clone(),
                event_text: shelf_event_text(shelf),
                is_read,
                progress: progress::get(pool, &book.id)
                    .ok()
                    .flatten()
                    .map(|p| (p.current_page, p.total_pages)),
                tags,
            };
            count += 1;
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
        self.count = count;
        self.sites = sites;
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
            // 選択した本が入る日付セクションへスクロールを寄せる
            if let Some(day_index) = self.day_index_of(next) {
                self.scroll_handle.scroll_to_item(day_index);
            }
            cx.notify();
        }
    }

    /// 平坦化した位置が何番目の日付セクションに入るか。
    fn day_index_of(&self, flat_index: usize) -> Option<usize> {
        let mut offset = 0;
        for (index, day) in self.days.iter().enumerate() {
            offset += day.items.len();
            if flat_index < offset {
                return Some(index);
            }
        }
        None
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
            let _ = db::tags::set_favorite(&state.db_pool, tag, !is_favorite);
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
            let _ = db::favorites::set_favorite(&state.db_pool, kind, value, !is_favorite);
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

    /// タグ列の折りたたみ / 展開を切り替える（本棚と同じ。行ごと）。
    fn toggle_tag_expansion(&mut self, book_id: &str, cx: &mut Context<Self>) {
        if !self.expanded_tag_rows.remove(book_id) {
            self.expanded_tag_rows.insert(book_id.to_string());
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
        let count = self.count;
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
                    .text_sm()
                    .text_color(theme.muted_foreground)
                    .child(format!("{count}件")),
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
    #[allow(clippy::too_many_arguments)]
    fn render_card(
        window: &mut Window,
        theme: &gpui_kit::component::Theme,
        handle: &Entity<Self>,
        item: &HistoryItem,
        card_width: f32,
        state: &ItemState<'_>,
    ) -> AnyElement {
        let database_id = item.book.id.clone();
        let cover = item
            .cover
            .clone()
            .or_else(|| placeholder_cover(&item.book.title, &item.book.circle_name))
            .or_else(no_image_cover);
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
                        let selector = format!("history-card-cover-img-{database_id}");
                        move || selector.clone()
                    });
                    if fits_width {
                        el = el.rounded_t_lg();
                    }
                    el.into_any_element()
                }
                None => div().w_full().h_full().bg(theme.muted).into_any_element(),
            })
            // 左上: 未読 / 既読（本棚のカードと同じバッジ）
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
                if item.is_read {
                    badge
                        .bg(gpui_kit::rgb(0xd1fae5))
                        .text_color(gpui_kit::rgb(0x047857))
                        .child("読了")
                } else {
                    badge
                        .bg(gpui_kit::rgb(0xfef3c7))
                        .text_color(gpui_kit::rgb(0xb45309))
                        .child("未読")
                }
                .into_any_element()
            })
            // 右上: お気に入りハート（本棚のカードと同じ）
            .child(Self::render_heart(theme, handle, item, 24.0))
            // 右下: ダウンロード済みバッジ（本棚のカードと同じ。履歴はすべてローカル本）
            .child(
                div()
                    .debug_selector({
                        let selector = format!("history-card-cover-downloaded-{database_id}");
                        move || selector.clone()
                    })
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
                    ),
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
            .id(SharedString::from(format!("history-card-{database_id}")))
            .debug_selector({
                let selector = format!("history-card-{database_id}");
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
                state.tag_order,
                state.tags_expanded,
                true,
            )))
            .into_any_element()
    }

    /// リスト 1 行。**本棚の行と同じ 4 列構成**にする:
    /// 表紙(200x133 固定) / 情報列(320px 固定: タイトル・イベント名 or 購入日・サークル/作者・
    /// 進捗・状態) / タグ列(行幅の 20%) / 残り（本棚はカルーセル。履歴では空ける）。
    fn render_row(
        window: &mut Window,
        theme: &gpui_kit::component::Theme,
        handle: &Entity<Self>,
        item: &HistoryItem,
        state: &ItemState<'_>,
    ) -> AnyElement {
        let database_id = item.book.id.clone();
        let cover = item
            .cover
            .clone()
            .or_else(|| placeholder_cover(&item.book.title, &item.book.circle_name))
            .or_else(no_image_cover);
        let info_selector = format!("history-info-{database_id}");
        let tag_area_selector = format!("history-tag-area-{database_id}");

        div()
            .id(SharedString::from(format!("history-row-{database_id}")))
            .debug_selector({
                let selector = format!("history-row-{database_id}");
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
                        let selector = format!("history-cover-{database_id}");
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
                        format!("history-cover-img-{database_id}"),
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
                    .child(Self::render_state_chips(theme, handle, item)),
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
    fn render_tag_row(
        window: &mut Window,
        theme: &gpui_kit::component::Theme,
        handle: &Entity<Self>,
        item: &HistoryItem,
        tag_order: &TagOrder,
        expanded: bool,
        card: bool,
    ) -> AnyElement {
        let database_id = item.book.id.clone();
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
            &database_id,
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
            .id(SharedString::from(format!("history-tag-row-{database_id}")))
            .debug_selector({
                let selector = format!("history-tag-row-{database_id}");
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
                format!("history-tag-toggle-{database_id}"),
                hidden,
                expanded,
                {
                    let handle = handle.clone();
                    let database_id = database_id.clone();
                    move |_window, cx| {
                        handle.update(cx, |this, cx| this.toggle_tag_expansion(&database_id, cx));
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
        favorite_circles: &[String],
        favorite_authors: &[String],
    ) -> AnyElement {
        let target = item.book.id.clone();
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
    fn render_state_chips(
        theme: &gpui_kit::component::Theme,
        handle: &Entity<Self>,
        item: &HistoryItem,
    ) -> AnyElement {
        let database_id = item.book.id.clone();
        div()
            .id(SharedString::from(format!("history-status-{database_id}")))
            .debug_selector({
                let selector = format!("history-status-{database_id}");
                move || selector.clone()
            })
            .flex()
            .flex_row()
            .items_center()
            .gap_2()
            .child(
                div()
                    .px_1()
                    .py_0p5()
                    .rounded_md()
                    .text_xs()
                    .when(item.is_read, |this| {
                        this.bg(gpui_kit::rgb(0xd1fae5))
                            .text_color(gpui_kit::rgb(0x047857))
                            .child("読了")
                    })
                    .when(!item.is_read, |this| {
                        this.bg(gpui_kit::rgb(0xfef3c7))
                            .text_color(gpui_kit::rgb(0xb45309))
                            .child("未読")
                    }),
            )
            .child(Self::render_heart(theme, handle, item, 24.0))
            .into_any_element()
    }

    /// お気に入りハート（本棚のカードと同じ丸ボタン）。
    fn render_heart(
        theme: &gpui_kit::component::Theme,
        handle: &Entity<Self>,
        item: &HistoryItem,
        size: f32,
    ) -> AnyElement {
        let database_id = item.book.id.clone();
        let is_favorite = item.book.is_favorite == 1;
        div()
            .id(SharedString::from(format!("history-heart-{database_id}")))
            .debug_selector({
                let selector = format!("history-heart-{database_id}");
                move || selector.clone()
            })
            .flex()
            .items_center()
            .justify_center()
            .w(px(size))
            .h(px(size))
            .rounded_full()
            .bg(theme.background)
            .text_color(if is_favorite {
                gpui_kit::rgb(0xf43f5e).into()
            } else {
                theme.muted_foreground
            })
            .cursor_pointer()
            .hover(|style| style.bg(theme.secondary))
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
}

/// 日付バーの背景色。カードの `muted` と区別できるようプライマリを薄く敷く
/// （ダークでは `muted` / `secondary` が同色なので、そこを避ける）。
fn day_bar_bg(theme: &gpui_kit::component::Theme) -> gpui_kit::Hsla {
    theme
        .primary
        .opacity(if theme.is_dark() { 0.22 } else { 0.12 })
}

/// 本棚と同じ表記の「イベント名 or 購入日」。紐づく本棚アイテムが無ければイベント不明。
fn shelf_event_text(shelf: Option<&db::bookshelf::BookshelfItem>) -> String {
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
        let columns = crate::views::bookshelf::BookshelfView::columns_for_width(window_width);
        let content_width = window_width - SIDEBAR_W - 24.0;
        let card_width =
            ((content_width - (columns as f32 - 1.0) * 12.0) / columns as f32).max(160.0);

        let days: Vec<AnyElement> = self
            .days
            .iter()
            .enumerate()
            .map(|(index, day)| {
                let selected_id = self
                    .selected_index
                    .and_then(|index| self.flat_book_ids().get(index).cloned());
                let body: AnyElement = match view_mode {
                    ViewMode::Card => div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap_3()
                        .children(day.items.iter().map(|item| {
                            HistoryView::render_card(
                                window,
                                &theme,
                                &handle,
                                item,
                                card_width,
                                &ItemState {
                                    tag_order: &tag_order,
                                    tags_expanded: self.expanded_tag_rows.contains(&item.book.id),
                                    selected: selected_id.as_deref() == Some(item.book.id.as_str()),
                                    favorite_circles: &self.favorite_circles,
                                    favorite_authors: &self.favorite_authors,
                                },
                            )
                        }))
                        .into_any_element(),
                    ViewMode::List => div()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .children(day.items.iter().map(|item| {
                            HistoryView::render_row(
                                window,
                                &theme,
                                &handle,
                                item,
                                &ItemState {
                                    tag_order: &tag_order,
                                    tags_expanded: self.expanded_tag_rows.contains(&item.book.id),
                                    selected: selected_id.as_deref() == Some(item.book.id.as_str()),
                                    favorite_circles: &self.favorite_circles,
                                    favorite_authors: &self.favorite_authors,
                                },
                            )
                        }))
                        .into_any_element(),
                };
                div()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .child(
                        // 日付は背景色を変えた「バー」にする（カードと同じ色にしない）
                        div()
                            .id(SharedString::from(format!("history-day-{}", day.date)))
                            // デバッグ用の id は「何番目の日か」で固定する（テストが
                            // 日付に依存せずに検証できるように）
                            .debug_selector({
                                let selector = format!("history-day-{index}");
                                move || selector.clone()
                            })
                            .w_full()
                            .px_3()
                            .py_2()
                            .rounded_md()
                            .bg(day_bar_bg(&theme))
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::BOLD)
                            .child(day.label.clone()),
                    )
                    .child(body)
                    .into_any_element()
            })
            .collect();

        let empty = self.days.is_empty();

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
                div()
                    .id("history-scroll")
                    .debug_selector(|| "history-scroll".into())
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap_5()
                    .track_scroll(&self.scroll_handle)
                    .overflow_y_scroll()
                    .when(empty, |this| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("まだ閲覧履歴がありません"),
                        )
                    })
                    .children(days),
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
        add_session(cx, "s1", "b1", Utc::now() - Duration::hours(1), 10);

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
        let cover = visual
            .debug_bounds("history-card-cover-img-b1")
            .expect("カードの表紙画像が出ていない");
        let ratio = cover.size.width.as_f32() / cover.size.height.as_f32();
        assert!(
            (ratio - 4.0).abs() < 0.3,
            "サイトのサムネイルキャッシュが使われていない（比率 {ratio}）"
        );
        // ダウンロード済みバッジ（本棚のカードと同じ）
        assert!(
            visual
                .debug_bounds("history-card-cover-downloaded-b1")
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
        use chrono::{Duration, Utc};
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
        // カード形式（既定）にタグ列が出る
        assert!(
            visual.debug_bounds("history-tag-row-b1").is_some(),
            "タグ行が出ていない"
        );
        assert!(
            visual.debug_bounds("tag-label-b1-しぼりこみ").is_some(),
            "タグチップが出ていない（集計数の多い順で先頭のはず）"
        );
        // 「+n」で展開できる（20 タグなので折りたたまれる）
        let toggle = visual
            .debug_bounds("history-tag-toggle-b1")
            .expect("「+n」トグルが出ていない");
        visual.simulate_click(toggle.center(), gpui_kit::Modifiers::default());
        draw(visual);
        assert!(
            visual.debug_bounds("tag-label-b1-タグ19").is_some(),
            "展開しても全タグが出ていない"
        );
        // タグのお気に入り（ハート）
        let heart = visual
            .debug_bounds("tag-heart-b1-しぼりこみ")
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
            .debug_bounds("tag-label-b1-しぼりこみ")
            .expect("タグの文字");
        visual.simulate_click(label.center(), gpui_kit::Modifiers::default());
        draw(visual);
        assert_eq!(
            view.read_with(cx, |this, _| this.count),
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

    /// 閲覧セッションを入れる（`started_at` / `ended_at` は UTC の `YYYY-MM-DD HH:MM:SS`）。
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
        // 「今日」と「昨日」のセッション（UTC の now から作る = ローカルでは今日 / 昨日）
        let now = Utc::now() - Duration::hours(1);
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

        let today = chrono::Local::now().format("%Y-%m-%d").to_string();
        let yesterday = (chrono::Local::now() - Duration::days(1))
            .format("%Y-%m-%d")
            .to_string();
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
            view.read_with(cx, |this, _| this.count) == 2,
            "件数が 1 日 1 本になっていない"
        );
        // 日付は背景色つきのバー（内容幅いっぱいに伸びる）
        let bar = visual.debug_bounds("history-day-0").expect("日付のバー");
        assert!(
            bar.size.width.as_f32() > 600.0,
            "日付がバーになっていない（幅 {}）",
            bar.size.width.as_f32()
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
            visual.debug_bounds("history-card-b1").is_some(),
            "カード形式で描画されていない"
        );
        // リスト形式に切り替え
        let toggle = visual
            .debug_bounds("history-view-toggle")
            .expect("表示切替ボタン");
        visual.simulate_click(toggle.center(), gpui_kit::Modifiers::default());
        draw(visual);
        assert!(
            visual.debug_bounds("history-row-b1").is_some(),
            "リスト形式に切り替わっていない"
        );
        // 本棚の行と同じ 4 列構成: 表紙 → 情報列（320px 固定） → タグ列
        let cover = visual.debug_bounds("history-cover-b1").expect("表紙");
        let info = visual.debug_bounds("history-info-b1").expect("情報列");
        let tags_area = visual.debug_bounds("history-tag-area-b1").expect("タグ列");
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
            visual.debug_bounds("history-circle-chip-b1").is_some(),
            "サークルチップが出ていない"
        );
        let heart = visual
            .debug_bounds("history-circle-heart-b1")
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
            visual.debug_bounds("history-card-b1").is_none(),
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
            view.read_with(cx, |this, _| this.count) == 1,
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
        let _ = yesterday;
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
        assert_eq!(view.read_with(cx, |this, _| this.count), 2);
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
            view.read_with(cx, |this, _| this.count),
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

    /// サイト id の表示名。
    #[test]
    fn site_labels_are_japanese() {
        assert_eq!(site_label("techbookfest"), "技術書典");
        assert_eq!(site_label("fanza"), "FANZA同人");
        assert_eq!(site_label("mystery"), "mystery");
    }
}
