//! 付箋画面: `page_notes` を **追加が新しい順** に並べる。
//!
//! 行は本棚のリスト形式と同じ 4 列（表紙枠 / 情報列 / タグ列 / 右端の操作）で、
//! **サムネイルは付箋を付けたページの画像**、情報に加えて**メモ**と**付箋登録日**を出す。
//! カルーセルは出さない。右端の「本を見る →」でビューアを**そのページ・その見開き側**で開く。

use std::sync::Arc;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Icon};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, Context, Entity, FocusHandle, InteractiveElement as _, IntoElement,
    KeyDownEvent, ParentElement, ReadGlobal as _, Render, RenderImage, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, px, relative,
};

use thundoku_core::db::{self, books, progress};

use crate::actions::OpenReaderAtPage;
use crate::app_state::AppState;
use crate::components::image_viewer::{PackPageLoader, PageLoader as _};
use crate::icons::AppIcon;
use crate::views::bookshelf::{
    CHIP_HEART_BUTTON, CHIP_HEART_ICON, LIST_COVER_MIN_H, LIST_COVER_W, LIST_INFO_W,
    LIST_TAGS_W_RATIO, TagOrder, count_tag_usage, cover_fit_inside_frame, list_tags_visible_count,
    owned_book_ids, placeholder_cover,
};
use crate::views::history::shelf_event_text;

/// 付箋 1 件の表示に必要な情報（DB の付箋 + 本の情報 + ページ画像）。
struct NoteRow {
    note: db::notes::PageNote,
    book: books::Book,
    /// 本棚と同じ表記のイベント名 / 購入日。
    event_text: String,
    reading_state: progress::ReadingState,
    progress: Option<(i64, Option<i64>)>,
    tags: Vec<String>,
    /// 付箋を付けたページの画像（縮小）。
    page_image: Option<Arc<RenderImage>>,
}

pub struct NotesView {
    rows: Vec<NoteRow>,
    favorite_circles: Vec<String>,
    favorite_authors: Vec<String>,
    favorite_tags: Vec<String>,
    tag_counts: Arc<std::collections::HashMap<String, usize>>,
    /// タグ絞り込み（本棚と同じ OR 条件）。空 = すべて。
    selected_tags: Vec<String>,
    /// タグ列を展開している本（「+n」→「閉じる」）。
    expanded_tag_rows: std::collections::HashSet<String>,
    focus_handle: FocusHandle,
    focus_initialized: bool,
}

impl NotesView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            rows: Vec::new(),
            favorite_circles: Vec::new(),
            favorite_authors: Vec::new(),
            favorite_tags: Vec::new(),
            tag_counts: Arc::new(std::collections::HashMap::new()),
            selected_tags: Vec::new(),
            expanded_tag_rows: std::collections::HashSet::new(),
            focus_handle: cx.focus_handle(),
            focus_initialized: false,
        };
        view.reload(cx);
        view
    }

    /// 付箋を読み直す（追加が新しい順。外した付箋は出さない）。
    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        let state = AppState::global(cx);
        let pool = &state.db_pool;
        let owned = owned_book_ids(state);
        let all_books = books::list(pool).unwrap_or_default();
        let shelf_items = db::bookshelf::list_all(pool).unwrap_or_default();

        let notes = db::notes::list_newest_first(pool).unwrap_or_default();
        let mut rows = Vec::new();
        for note in notes {
            if !owned.contains(&note.book_id) {
                continue;
            }
            let Some(book) = all_books.iter().find(|b| b.id == note.book_id) else {
                continue;
            };
            let shelf = shelf_items.iter().find(|item| {
                book.tbf_product_id.as_deref() == Some(item.database_id.as_str())
                    || item
                        .file_name
                        .as_deref()
                        .is_some_and(|name| name == book.file_name)
                    || (!item.title.is_empty() && item.title == book.title)
            });
            let tags: Vec<String> = db::tags::list_for_book(pool, &book.id)
                .unwrap_or_default()
                .into_iter()
                .map(|tag| tag.tag_name)
                .collect();
            if !self.selected_tags.is_empty()
                && !self.selected_tags.iter().any(|tag| tags.contains(tag))
            {
                continue;
            }
            rows.push(NoteRow {
                page_image: Self::load_page_image(cx, pool, book, &note),
                book: book.clone(),
                event_text: shelf_event_text(shelf),
                reading_state: progress::ReadingState::from_progress(
                    progress::get(pool, &book.id).ok().flatten().as_ref(),
                ),
                progress: progress::get(pool, &book.id)
                    .ok()
                    .flatten()
                    .map(|p| (p.current_page, p.total_pages)),
                tags,
                note,
            });
        }

        self.favorite_tags = db::tags::list_favorites(pool).unwrap_or_default();
        self.favorite_circles =
            db::favorites::list_favorites(pool, db::favorites::EntityKind::Circle)
                .unwrap_or_default();
        self.favorite_authors =
            db::favorites::list_favorites(pool, db::favorites::EntityKind::Author)
                .unwrap_or_default();
        self.tag_counts = Arc::new(count_tag_usage(rows.iter().map(|row| row.tags.as_slice())));
        self.rows = rows;
    }

    /// 付箋を付けたページの画像（ページ一覧と同じ縮小経路: `load_thumb`）。
    /// ページ画像は pack の復号が要るため、リーダーと同じ identity を渡す。
    fn load_page_image(
        cx: &Context<Self>,
        pool: &db::SqlitePool,
        book: &books::Book,
        note: &db::notes::PageNote,
    ) -> Option<Arc<RenderImage>> {
        let state = AppState::global(cx);
        let content = (!note.content_id.is_empty()).then_some(note.content_id.as_str());
        let images = db::documents::images_for_selection(pool, &book.id, content, None)
            .unwrap_or_default()
            .into_iter()
            .filter(|image| image.image_type == "page")
            .collect::<Vec<_>>();
        let loader = PackPageLoader {
            images,
            packs_dir: state.packs_dir.clone(),
            db: pool.clone(),
            identity: state
                .google_profile
                .lock()
                .as_ref()
                .map(|profile| opfspack::Identity {
                    sub: profile.sub.clone(),
                    pack_id: book.id.clone(),
                }),
            pack_bytes: std::sync::OnceLock::new(),
            pack_key: std::sync::OnceLock::new(),
        };
        let index = (note.page - 1).max(0) as usize;
        loader.load_thumb(index).ok()
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

    /// タグ列の折りたたみ / 展開を切り替える（本棚と同じ。行ごと）。
    fn toggle_tag_expansion(&mut self, book_id: &str, cx: &mut Context<Self>) {
        if !self.expanded_tag_rows.remove(book_id) {
            self.expanded_tag_rows.insert(book_id.to_string());
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

    /// 本を見る → ビューアをそのページ・その見開き側で開く。
    fn open_book(&self, cx: &mut Context<Self>, note: &db::notes::PageNote) {
        let action = OpenReaderAtPage {
            book_id: note.book_id.clone().into(),
            page: note.page,
            content_id: note.content_id.clone().into(),
            side: note.spread_side.map(|side| side.as_str().into()),
        };
        cx.defer(move |cx| cx.dispatch_action(&action));
    }

    /// タグ絞り込みを解除する（ESC / 「全項目」ボタン）。
    fn clear_filters(&mut self, cx: &mut Context<Self>) {
        self.selected_tags.clear();
        self.reload(cx);
        cx.notify();
    }

    /// メモの表示（空メモは出さない）。
    fn render_memo(theme: &gpui_kit::component::Theme, memo: &str) -> Option<AnyElement> {
        let memo = memo.trim();
        if memo.is_empty() {
            return None;
        }
        Some(
            div()
                .debug_selector(|| "notes-memo".into())
                .w_full()
                .rounded_md()
                .bg(theme.secondary)
                .px_2()
                .py_1()
                .text_sm()
                .child(memo.to_string())
                .into_any_element(),
        )
    }

    /// サークル / 作者のチップ（本棚と同じ見た目: 値 + 右端のハート）。
    fn render_entity_chips(
        &self,
        theme: &gpui_kit::component::Theme,
        handle: &Entity<Self>,
        row: &NoteRow,
    ) -> AnyElement {
        let target = row.book.id.clone();
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
                        row.book.circle_name.as_str(),
                        &row.book.id,
                    ),
                    (
                        db::favorites::EntityKind::Author,
                        row.book.author.as_str(),
                        &row.book.id,
                    ),
                ]
                .into_iter()
                .filter(|(_, value, _)| !value.is_empty())
                .map(|(kind, value, _)| {
                    self.render_entity_chip(theme, handle, kind, value, target.clone())
                }),
            )
            .into_any_element()
    }

    fn render_entity_chip(
        &self,
        theme: &gpui_kit::component::Theme,
        handle: &Entity<Self>,
        kind: db::favorites::EntityKind,
        value: &str,
        target: String,
    ) -> AnyElement {
        let is_favorite = match kind {
            db::favorites::EntityKind::Circle => self.favorite_circles.iter().any(|v| v == value),
            db::favorites::EntityKind::Author => self.favorite_authors.iter().any(|v| v == value),
        };
        let kind_label = match kind {
            db::favorites::EntityKind::Circle => "circle",
            db::favorites::EntityKind::Author => "author",
        };
        let chip_selector = format!("notes-{kind_label}-chip-{target}");
        let heart_selector = format!("notes-{kind_label}-heart-{target}");
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
                        gpui_kit::Hsla::from(gpui_kit::rgb(0xf43f5e))
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

    /// タグ行（本棚と同じ: チップ + 「+n」/「閉じる」）。
    fn render_tag_row(
        window: &mut Window,
        theme: &gpui_kit::component::Theme,
        handle: &Entity<Self>,
        row: &NoteRow,
        tag_order: &TagOrder,
        expanded: bool,
    ) -> AnyElement {
        let database_id = row.book.id.clone();
        let ordered = tag_order.sorted(&row.tags);
        let visible = if expanded {
            ordered.len()
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
        let mut tag_row = div()
            .id(SharedString::from(format!("notes-tag-row-{database_id}")))
            .debug_selector({
                let selector = format!("notes-tag-row-{database_id}");
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
            tag_row = tag_row.child(crate::views::bookshelf::BookshelfView::render_tag_toggle(
                theme,
                format!("notes-tag-toggle-{database_id}"),
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
        tag_row.into_any_element()
    }

    /// リスト 1 行（本棚のリスト形式と同じ 4 列 + メモ + 付箋登録日 + 本を見る）。
    fn render_row(
        &self,
        window: &mut Window,
        theme: &gpui_kit::component::Theme,
        handle: &Entity<Self>,
        row: &NoteRow,
        tag_order: &TagOrder,
        tags_expanded: bool,
    ) -> AnyElement {
        let database_id = row.book.id.clone();
        let note_id = format!("{database_id}-{}", row.note.page);
        let page = row.note.page;
        // 表紙枠には付箋を付けたページの画像を収める（無ければプレースホルダ）
        let page_image = row
            .page_image
            .clone()
            .or_else(|| placeholder_cover(&row.book.title, &row.book.circle_name));
        let state_selector = format!("notes-state-{note_id}");

        div()
            .id(SharedString::from(format!("notes-row-{note_id}")))
            .debug_selector({
                let selector = format!("notes-row-{note_id}");
                move || selector.clone()
            })
            .flex()
            .flex_row()
            .items_start()
            .gap_3()
            .p_2()
            .rounded_lg()
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted)
            .hover(|style| style.bg(theme.secondary))
            // 1 列目: 付箋を付けたページの画像（本棚の行と同じ枠）
            .child(
                div()
                    .debug_selector({
                        let selector = format!("notes-page-{note_id}");
                        move || selector.clone()
                    })
                    .relative()
                    .w(px(LIST_COVER_W))
                    .h(px(LIST_COVER_MIN_H))
                    .flex_shrink_0()
                    .overflow_hidden()
                    .bg(theme.muted)
                    .child(cover_fit_inside_frame(
                        page_image.as_ref(),
                        format!("notes-page-img-{note_id}"),
                    )),
            )
            // 2 列目: 情報（本棚のリストと同じ）+ メモ + 付箋登録日
            .child(
                div()
                    .debug_selector({
                        let selector = format!("notes-info-{note_id}");
                        move || selector.clone()
                    })
                    .flex()
                    .flex_col()
                    .gap_1()
                    .min_w_0()
                    .w(px(LIST_INFO_W))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .child(row.book.title.clone()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(row.event_text.clone()),
                    )
                    .child(self.render_entity_chips(theme, handle, row))
                    .when_some(Self::progress_text(row), |this, text| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(text),
                        )
                    })
                    // 状態（読了 / 読書中 / 未読 + ♡）
                    .child(
                        div()
                            .debug_selector(move || state_selector.clone())
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
                                    .when(
                                        row.reading_state == progress::ReadingState::Read,
                                        |this| {
                                            this.bg(gpui_kit::rgb(0xd1fae5))
                                                .text_color(gpui_kit::rgb(0x047857))
                                                .child("読了")
                                        },
                                    )
                                    .when(
                                        row.reading_state == progress::ReadingState::Reading,
                                        |this| {
                                            this.bg(gpui_kit::rgb(0xe0f2fe))
                                                .text_color(gpui_kit::rgb(0x0369a1))
                                                .child("読書中")
                                        },
                                    )
                                    .when(
                                        row.reading_state == progress::ReadingState::Unread,
                                        |this| {
                                            this.bg(gpui_kit::rgb(0xfef3c7))
                                                .text_color(gpui_kit::rgb(0xb45309))
                                                .child("未読")
                                        },
                                    ),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(format!("{page} ページ")),
                            ),
                    )
                    // 付箋登録日（ローカル日付）
                    .child(
                        div()
                            .debug_selector({
                                let selector = format!("notes-created-{note_id}");
                                move || selector.clone()
                            })
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "付箋登録日: {}",
                                local_date_label(&row.note.created_at)
                            )),
                    )
                    .when_some(Self::render_memo(theme, &row.note.memo), |this, memo| {
                        this.child(memo)
                    }),
            )
            // 3 列目: タグ（本棚の行と同じ幅の取り方）
            .child(
                div()
                    .debug_selector({
                        let selector = format!("notes-tag-area-{note_id}");
                        move || selector.clone()
                    })
                    .w(relative(LIST_TAGS_W_RATIO))
                    .min_w_0()
                    .child(Self::render_tag_row(
                        window,
                        theme,
                        handle,
                        row,
                        tag_order,
                        tags_expanded,
                    )),
            )
            // 4 列目: 本を見る（該当ページ・見開き側で開く）。
            // 行の高さいっぱいに広げ、押せることが分かるよう塗りボタンにする。
            .child(
                div()
                    .debug_selector({
                        let selector = format!("notes-open-{note_id}");
                        move || selector.clone()
                    })
                    .flex_shrink_0()
                    .self_stretch()
                    .flex()
                    .child(
                        div()
                            .id(SharedString::from(format!("notes-open-button-{note_id}")))
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .px_3()
                            .rounded_md()
                            .bg(theme.primary)
                            .text_color(theme.primary_foreground)
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .cursor_pointer()
                            .hover(|style| style.opacity(0.9))
                            .on_click({
                                let handle = handle.clone();
                                let note = row.note.clone();
                                move |_, _, cx| {
                                    cx.stop_propagation();
                                    handle.update(cx, |this, cx| this.open_book(cx, &note));
                                }
                            })
                            .child("本を見る"),
                    ),
            )
            .into_any_element()
    }

    fn progress_text(row: &NoteRow) -> Option<String> {
        let (current, total) = row.progress?;
        Some(match total {
            Some(total) => format!("{} / {total}ページ", current.max(1)),
            None => format!("{current}ページ"),
        })
    }
}

/// `YYYY-MM-DD HH:MM:SS`（UTC）をローカルの「YYYY年MM月DD日」にする。
fn local_date_label(utc: &str) -> String {
    use chrono::{Local, NaiveDateTime, TimeZone as _};
    let Ok(naive) = NaiveDateTime::parse_from_str(utc, "%Y-%m-%d %H:%M:%S") else {
        return utc.to_string();
    };
    let local = Local.from_utc_datetime(&naive);
    local.format("%Y年%m月%d日").to_string()
}

impl Render for NotesView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.focus_initialized {
            self.focus_initialized = true;
            window.focus(&self.focus_handle, cx);
        }
        let theme = cx.theme().clone();
        let handle = cx.entity();
        let tag_order = TagOrder::new(
            &self.favorite_tags,
            &self.selected_tags,
            self.tag_counts.clone(),
        );
        let count = self.rows.len();
        let filtering = !self.selected_tags.is_empty();

        let rows: Vec<AnyElement> = self
            .rows
            .iter()
            .map(|row| {
                self.render_row(
                    window,
                    &theme,
                    &handle,
                    row,
                    &tag_order,
                    self.expanded_tag_rows.contains(&row.book.id),
                )
            })
            .collect();

        div()
            .id("notes-root")
            .debug_selector(|| "notes-root".into())
            .track_focus(&self.focus_handle)
            .on_key_down({
                let handle = cx.entity();
                move |event: &KeyDownEvent, _window, cx| {
                    // ESC でタグ絞り込みを解除する
                    if event.keystroke.key.as_str() == "escape" {
                        handle.update(cx, |this, cx| {
                            if !this.selected_tags.is_empty() {
                                cx.stop_propagation();
                                this.clear_filters(cx);
                            }
                        });
                    }
                }
            })
            .size_full()
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .bg(cx.theme().background)
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .text_lg()
                            .font_weight(gpui_kit::FontWeight::BOLD)
                            .child("付箋"),
                    )
                    .child(
                        div()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(format!("{count}件")),
                    )
                    .when(filtering, |this| {
                        this.child(
                            div().debug_selector(|| "notes-clear-filters".into()).child(
                                Button::new("notes-clear-filters")
                                    .cursor_pointer()
                                    .primary()
                                    .label("全項目")
                                    .on_click({
                                        let handle = handle.clone();
                                        move |_, _, cx| {
                                            handle.update(cx, |this, cx| this.clear_filters(cx));
                                        }
                                    }),
                            ),
                        )
                    }),
            )
            .child(
                div()
                    .id("notes-scroll")
                    .debug_selector(|| "notes-scroll".into())
                    .flex_1()
                    .min_h_0()
                    .flex()
                    .flex_col()
                    .gap_2()
                    .overflow_y_scroll()
                    .when(rows.is_empty(), |this| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("まだ付箋がありません"),
                        )
                    })
                    .children(rows),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_kit::AppContext as _;

    /// 付箋画面: 追加が新しい順に、ページ画像・メモ・付箋登録日・「本を見る」付きで並ぶ。
    #[gpui_kit::test]
    async fn notes_screen_lists_newest_first_with_memo_and_date(cx: &mut gpui_kit::TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        cx.update(|cx| {
            let state = AppState::global(cx);
            let pool = &state.db_pool;
            for (id, title) in [("b1", "古い本"), ("b2", "新しい本")] {
                db::books::insert(
                    pool,
                    &db::books::Book {
                        id: id.into(),
                        title: title.into(),
                        author: "作者".into(),
                        circle_name: "サークル".into(),
                        ..test_book(id)
                    },
                )
                .unwrap();
            }
            db::notes::upsert(
                pool,
                &db::notes::PageNoteInput {
                    id: "n1",
                    book_id: "b1",
                    content_id: "",
                    page: 3,
                    memo: "古いメモ",
                    spread_side: None,
                },
            )
            .unwrap();
            db::notes::upsert(
                pool,
                &db::notes::PageNoteInput {
                    id: "n2",
                    book_id: "b2",
                    content_id: "",
                    page: 7,
                    memo: "新しいメモ",
                    spread_side: Some(db::notes::SpreadSide::Left),
                },
            )
            .unwrap();
        });

        let view = cx.new(NotesView::new);
        // 追加が新しい順（同時刻は後から入れたものが先）
        assert_eq!(
            view.read_with(cx, |this, _| this
                .rows
                .iter()
                .map(|row| row.note.memo.clone())
                .collect::<Vec<_>>()),
            vec!["新しいメモ".to_string(), "古いメモ".to_string()],
            "追加が新しい順に並んでいない"
        );

        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1200.0),
                height: gpui_kit::px(700.0),
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
        // 行（ページ画像 / 情報列 / タグ列 / 本を見る）が出る
        for selector in [
            "notes-page-b2-7",
            "notes-info-b2-7",
            "notes-created-b2-7",
            "notes-tag-area-b2-7",
            "notes-open-b2-7",
            "notes-memo",
        ] {
            assert!(
                visual.debug_bounds(selector).is_some(),
                "{selector} が出ていない"
            );
        }
        // 「本を見る」は矢印なしで、行の内容の高さいっぱいに広がる
        // （行の外枠は `p_2` の上下 8+8 と `border_1` の上下 2 を含むので、それを引く）
        let open = visual.debug_bounds("notes-open-b2-7").expect("本を見る");
        let row = visual.debug_bounds("notes-row-b2-7").expect("行");
        let expected = row.size.height.as_f32() - 18.0;
        assert!(
            (open.size.height.as_f32() - expected).abs() < 2.0,
            "本を見るボタンが行の高さいっぱいになっていない: {} (期待 {expected})",
            open.size.height.as_f32()
        );
        // 情報列は本棚と同じ 320px 固定
        let info = visual.debug_bounds("notes-info-b2-7").expect("情報列");
        assert!(
            (info.size.width.as_f32() - LIST_INFO_W).abs() < 1.5,
            "情報列が本棚と同じ幅になっていない: {}",
            info.size.width.as_f32()
        );
        // 「本を見る」は押せる（開いた先のページ・見開き側は workspace のテストで検証する）
        let open = visual.debug_bounds("notes-open-b2-7").expect("本を見る");
        visual.simulate_click(open.center(), gpui_kit::Modifiers::default());
        cx.run_until_parked();
        assert!(
            visual.debug_bounds("notes-open-b2-7").is_some(),
            "本を見るを押した後も画面が保たれていない"
        );
    }

    fn test_book(id: &str) -> db::books::Book {
        db::books::Book {
            id: id.into(),
            title: "t".into(),
            author: String::new(),
            circle_name: String::new(),
            purchase_date: None,
            file_name: "t.pdf".into(),
            file_size: 1,
            opfs_path: format!("{id}.opfspack"),
            cover_thumbnail: None,
            tbf_product_id: None,
            site_id: None,
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
        }
    }

    /// 付箋登録日は UTC 保存なので、ローカル日付に直して出す。
    #[test]
    fn created_date_is_shown_in_local_time() {
        // UTC 20:00 = 日本時間 翌 05:00
        let utc = "2026-09-12 20:00:00";
        let expected = {
            use chrono::{Local, NaiveDateTime, TimeZone as _};
            let naive = NaiveDateTime::parse_from_str(utc, "%Y-%m-%d %H:%M:%S").unwrap();
            Local
                .from_utc_datetime(&naive)
                .format("%Y年%m月%d日")
                .to_string()
        };
        assert_eq!(local_date_label(utc), expected);
        // パースできない値はそのまま返す（表示を壊さない）
        assert_eq!(local_date_label("bad"), "bad");
    }
}
