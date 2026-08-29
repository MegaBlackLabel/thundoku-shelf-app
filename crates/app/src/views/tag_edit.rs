//! タグ編集ダイアログ: カンマ区切りでマニュアルタグを保存する。

use gpui::{
    App, Context, Entity, IntoElement, ParentElement, Render, SharedString, Window, div, px,
};
use gpui::{
    AppContext as _, InteractiveElement as _, ReadGlobal as _, StatefulInteractiveElement as _,
    Styled as _,
};
use gpui_component::ActiveTheme as _;
use gpui_component::Sizable as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::dialog::Dialog;
use gpui_component::input::{Input, InputState};
use thundoku_core::db;
use thundoku_core::db::{books, bookshelf};
use thundoku_core::tbf;

use crate::app_state::AppState;

pub struct TagEditDialog {
    book_id: String,
    input: Option<Entity<InputState>>,
    /// 編集中のタグ一覧（Web の TagsInput 相当）。
    tags: Vec<String>,
    /// サジェスチョン（お気に入りタグ + 「後で読む」）。
    suggestions: Vec<String>,
}

impl TagEditDialog {
    pub fn new(_cx: &mut Context<Self>, book_id: String) -> Self {
        Self {
            book_id,
            input: None,
            tags: Vec::new(),
            suggestions: Vec::new(),
        }
    }

    pub fn open(cx: &mut App, book_id: String) {
        cx.spawn(async move |cx| {
            cx.open_window(gpui_component::TitleBar::window_options(), |window, cx| {
                let dialog = cx.new(|cx| TagEditDialog::new(cx, book_id.clone()));
                cx.new(|cx| gpui_component::Root::new(dialog, window, cx))
            })
            .expect("failed to open tag edit window");
        })
        .detach();
    }

    /// ローカル book（id または tbf_product_id）を解決する。無ければ
    /// bookshelf_items の database_id として扱う（Web の updateBookTags と同一）。
    fn resolve_local_book_id(&self, db: &thundoku_core::db::SqlitePool) -> Option<String> {
        books::list(db)
            .unwrap_or_default()
            .into_iter()
            .find(|b| {
                b.id == self.book_id || b.tbf_product_id.as_deref() == Some(self.book_id.as_str())
            })
            .map(|b| b.id)
    }

    fn ensure_input(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.input.is_none() {
            let (existing, suggestions) = {
                let state = AppState::global(cx);
                let db = &state.db_pool;
                let tags = if let Some(book_id) = self.resolve_local_book_id(db) {
                    db::tags::list_for_book(db, &book_id)
                        .unwrap_or_default()
                        .into_iter()
                        .filter(|tag| tag.source == "manual")
                        .map(|tag| tag.tag_name)
                        .collect::<Vec<_>>()
                } else {
                    bookshelf::list(db, tbf::SITE_ID_TECHBOOKFEST)
                        .unwrap_or_default()
                        .iter()
                        .find(|item| item.database_id == self.book_id)
                        .map(bookshelf::tags_of)
                        .unwrap_or_default()
                };
                // Web の tagSuggestions 相当: 後で読む + お気に入りタグ
                let mut suggestions = vec!["後で読む".to_string()];
                for tag in db::tags::list_favorites(db).unwrap_or_default() {
                    if !suggestions.contains(&tag) {
                        suggestions.push(tag);
                    }
                }
                (tags, suggestions)
            };
            self.tags = existing;
            self.suggestions = suggestions;
            self.input =
                Some(cx.new(|cx| InputState::new(window, cx).placeholder("タグを追加...")));
        }
    }

    /// タグを追加（重複は無視、Web の `addTag` 相当）。
    fn add_tag(&mut self, cx: &mut Context<Self>, tag: &str) {
        let tag = tag.trim();
        if tag.is_empty() || tag.len() > 20 {
            return;
        }
        if !self.tags.iter().any(|t| t == tag) {
            self.tags.push(tag.to_string());
        }
        cx.notify();
    }

    /// タグを削除（Web の `removeTag` 相当）。
    fn remove_tag(&mut self, cx: &mut Context<Self>, tag: &str) {
        self.tags.retain(|t| t != tag);
        cx.notify();
    }

    /// 入力からタグを追加（カンマ区切りにも対応）。
    fn add_from_input(&mut self, cx: &mut Context<Self>) {
        let value = self
            .input
            .as_ref()
            .map(|state| state.read(cx).value().to_string())
            .unwrap_or_default();
        for part in value.split(',') {
            self.add_tag(cx, part);
        }
    }

    fn save(&mut self, cx: &mut Context<Self>) {
        self.add_from_input(cx);
        let tags = self.tags.clone();
        {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            if let Some(book_id) = self.resolve_local_book_id(db) {
                let tag_pairs: Vec<(&str, &str)> =
                    tags.iter().map(|tag| (tag.as_str(), "manual")).collect();
                let _ = db::tags::set_for_book(db, &book_id, &tag_pairs);
            } else {
                let _ = bookshelf::update_tags(db, tbf::SITE_ID_TECHBOOKFEST, &self.book_id, &tags);
            }
        }
    }
}

impl Render for TagEditDialog {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_input(window, cx);
        let input = self.input.clone().expect("input state");
        let handle = cx.entity();
        let content_handle = handle.clone();
        let footer_handle = handle.clone();
        // クロージャは 'static なので self を借用できない。必要な値をコピーする
        let tags = self.tags.clone();
        let suggestions = self.suggestions.clone();
        let theme_muted = cx.theme().muted;
        let theme_muted_foreground = cx.theme().muted_foreground;

        div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(cx.theme().background)
            .child(
                Dialog::new(cx)
                    .title(
                        div()
                            .font_weight(gpui::FontWeight::SEMIBOLD)
                            .child("タグ編集"),
                    )
                    .content(move |content, _window, _cx| {
                        // Web の TagsInput 相当: チップ + 入力 + サジェスチョン
                        content.child(
                            div()
                                .flex()
                                .flex_row()
                                .flex_wrap()
                                .gap_1()
                                .child(
                                    // チップ表示（✕ で削除）
                                    div().flex().flex_row().flex_wrap().gap_1().children(
                                        tags.clone().into_iter().map({
                                            let handle = content_handle.clone();
                                            move |tag| {
                                                let handle = handle.clone();
                                                let tag_id = tag.clone();
                                                div()
                                                    .id(SharedString::from(format!(
                                                        "tag-chip-{tag_id}"
                                                    )))
                                                    .flex()
                                                    .flex_row()
                                                    .items_center()
                                                    .gap_1()
                                                    .px_2()
                                                    .py_0p5()
                                                    .rounded_full()
                                                    .bg(theme_muted)
                                                    .text_xs()
                                                    .child(
                                                        div()
                                                            .max_w(px(120.0))
                                                            .truncate()
                                                            .child(tag.clone()),
                                                    )
                                                    .child(
                                                        div()
                                                            .id(SharedString::from(format!(
                                                                "tag-chip-x-{tag_id}"
                                                            )))
                                                            .rounded_full()
                                                            .px_0p5()
                                                            .text_color(theme_muted_foreground)
                                                            .hover(|style| {
                                                                style.bg(theme_muted_foreground
                                                                    .alpha(0.2))
                                                            })
                                                            .cursor_pointer()
                                                            .on_click(move |_, _window, cx| {
                                                                handle.update(cx, |this, cx| {
                                                                    this.remove_tag(cx, &tag_id);
                                                                });
                                                            })
                                                            .child("✕"),
                                                    )
                                            }
                                        }),
                                    ),
                                )
                                .child(
                                    div()
                                        .on_key_down({
                                            let handle = handle.clone();
                                            move |event, _window, cx| {
                                                if event.keystroke.key == "enter"
                                                    || event.keystroke.key == ","
                                                {
                                                    handle.update(cx, |this, cx| {
                                                        this.add_from_input(cx);
                                                    });
                                                }
                                            }
                                        })
                                        .child(Input::new(&input).cursor_text().w(px(280.0))),
                                )
                                .child(
                                    // サジェスチョン（お気に入りタグ + 後で読む）
                                    div().flex().flex_row().flex_wrap().gap_1().children(
                                        suggestions.clone().into_iter().map({
                                            let handle = content_handle.clone();
                                            move |suggestion| {
                                                let handle = handle.clone();
                                                Button::new(format!("tag-suggestion-{suggestion}"))
                                                    .cursor_pointer()
                                                    .label(suggestion.clone())
                                                    .outline()
                                                    .small()
                                                    .cursor_pointer()
                                                    .on_click(move |_, _window, cx| {
                                                        handle.update(cx, |this, cx| {
                                                            this.add_tag(cx, &suggestion);
                                                        });
                                                    })
                                            }
                                        }),
                                    ),
                                ),
                        )
                    })
                    .footer(
                        div()
                            .flex()
                            .flex_row()
                            .gap_2()
                            .child(
                                Button::new("tag-edit-cancel")
                                    .cursor_pointer()
                                    .label("キャンセル")
                                    .cursor_pointer()
                                    .on_click(|_, window, _cx| {
                                        window.remove_window();
                                    }),
                            )
                            .child(
                                Button::new("tag-edit-save")
                                    .cursor_pointer()
                                    .primary()
                                    .label("保存")
                                    .cursor_pointer()
                                    .on_click({
                                        let handle = footer_handle.clone();
                                        move |_, window, cx| {
                                            handle.update(cx, |this, cx| this.save(cx));
                                            window.remove_window();
                                        }
                                    }),
                            ),
                    ),
            )
    }
}
