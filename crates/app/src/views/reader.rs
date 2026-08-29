//! リーダー: `ImageViewer` を本の Pack ページで起動し、`reading_progress`
//! をページ変更のたびに保存する。Workspace のメイン領域に埋め込まれる。

use std::sync::Arc;

use gpui::Styled as _;
use gpui::Subscription;
use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement, ReadGlobal as _, Render,
    SharedString, Window, div,
};
use gpui_component::ActiveTheme as _;
use thundoku_core::db;

use crate::app_state::AppState;
use crate::components::image_viewer::{Base64PageLoader, ImageViewer, PackPageLoader};

pub struct ReaderView {
    viewer: Entity<ImageViewer>,
    /// 本棚の本の場合のみ Some（進捗保存対象）。試し読みは None。
    book_id: Option<SharedString>,
    _subscription: Option<Subscription>,
    /// 最後に保存したページ（1-indexed）。同じページの再保存を防ぐ。
    last_saved_page: i64,
    /// 閲覧履歴セッション ID（開いたときに開始、閉じるときに終了）
    view_session_id: Option<String>,
}

impl ReaderView {
    /// 閲覧履歴セッションを終了する（ビューアーを閉じる際に呼ぶ）。
    pub(crate) fn end_session(&mut self, cx: &App) {
        if let Some(session_id) = self.view_session_id.take() {
            let state = AppState::global(cx);
            let _ = db::view_history::end(&state.db_pool, &session_id);
        }
    }

    /// 本棚の本を開く（Pack ページ + 進捗保存）。
    pub fn for_book(cx: &mut Context<Self>, book_id: String) -> Self {
        let state = AppState::global(cx);
        let db = state.db_pool.clone();
        let packs_dir = state.packs_dir.clone();
        let google_sub = state.google_profile.lock().as_ref().map(|p| p.sub.clone());

        let (title, images, progress, site_id) = {
            let book = db::books::get(&db, &book_id).ok().flatten();
            let title = book
                .as_ref()
                .map(|book| book.title.clone())
                .unwrap_or_else(|| book_id.clone());
            let site_id = book.as_ref().and_then(|book| book.site_id.clone());
            let images = db::documents::images_for_book(&db, &book_id)
                .unwrap_or_default()
                .into_iter()
                .filter(|image| image.image_type == "page")
                .collect::<Vec<_>>();
            let progress = db::progress::get(&db, &book_id).ok().flatten();
            (title, images, progress, site_id)
        };

        let identity = google_sub.map(|sub| opfspack::Identity {
            sub,
            pack_id: book_id.clone(),
        });
        let loader = Arc::new(PackPageLoader {
            images,
            packs_dir,
            db,
            identity,
            pack_bytes: std::sync::OnceLock::new(),
            pack_key: std::sync::OnceLock::new(),
        });
        let initial_page = progress
            .as_ref()
            // 保存は 1-indexed（Web と同じ）。表示 index は 0 始まりなので -1 する。
            // current_page が 0 や負の値の場合は最初のページに落とす（usize の
            // アンダーフローで最終ページに飛ぶ事故を防ぐ）
            .map(|p| p.current_page.max(1).saturating_sub(1) as usize)
            .unwrap_or(0);
        log::info!(
            "open reader: book={book_id} current_page={} initial_page={initial_page}",
            progress.as_ref().map(|p| p.current_page).unwrap_or(0)
        );

        let viewer =
            cx.new(|cx| ImageViewer::new(cx, loader, title.clone(), initial_page, site_id));
        // 閲覧履歴のセッションを開始する（途中で落ちた場合に備え ended_at は
        // 開始時刻で初期化された状態で作成される）
        let view_session_id = {
            let state = AppState::global(cx);
            db::view_history::start(&state.db_pool, &book_id)
                .ok()
                .map(|session| session.id)
        };
        let subscription = cx.observe(&viewer, |this, viewer, cx| this.save_progress(viewer, cx));
        Self {
            viewer,
            book_id: Some(book_id.into()),
            _subscription: Some(subscription),
            // 初期ページ（1-indexed）を保存済みとしてマークし、開いた直後の
            // 不要な保存をスキップする（ページを移動してから保存される）
            last_saved_page: initial_page as i64 + 1,
            view_session_id,
        }
    }

    /// チェックリストの試し読み画像を開く（DB に保存済みの base64 ページ）。
    pub fn for_sample(cx: &mut Context<Self>, item_id: String) -> Self {
        let state = AppState::global(cx);
        let db = state.db_pool.clone();
        let (title, pages) = {
            let title = db::checklist::get_item(&db, &item_id)
                .ok()
                .flatten()
                .map(|item| item.product_title)
                .unwrap_or_else(|| "試し読み".to_string());
            let pages: Vec<(String, u32, u32)> = db::samples::list_for_item(&db, &item_id)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|row| {
                    let data = row.image_data.clone()?;
                    Some((
                        data,
                        row.width.unwrap_or(0) as u32,
                        row.height.unwrap_or(0) as u32,
                    ))
                })
                .collect();
            (title, pages)
        };
        let loader = Arc::new(Base64PageLoader { pages });
        let viewer = cx.new(|cx| ImageViewer::new(cx, loader, title, 0, None));
        Self {
            viewer,
            book_id: None,
            _subscription: None,
            last_saved_page: -1,
            view_session_id: None,
        }
    }

    fn save_progress(&mut self, viewer: Entity<ImageViewer>, cx: &mut Context<Self>) {
        let Some(book_id) = self.book_id.clone() else {
            return;
        };
        // Web と同じ 1-indexed で保存（最終ページ = total_pages になり読了判定が成立する）
        let current_page = viewer.read(cx).current_page() as i64 + 1;
        let total_pages = viewer.read(cx).page_count() as i64;
        // 保存済みのページと同じなら保存しない（スライダー同期などの
        // 不要な再保存・DB 書き込みを避ける）
        if current_page == self.last_saved_page {
            return;
        }
        self.last_saved_page = current_page;
        let state = AppState::global(cx);
        let db = &state.db_pool;
        let timestamp = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
        // 最終ページまで読んだら読了日時をセット（一度セットすると維持される）
        let finished_at = if total_pages > 0 && current_page >= total_pages {
            Some(timestamp.clone())
        } else {
            None
        };
        let _ = db::progress::upsert(
            db,
            &db::progress::ReadingProgress {
                book_id: book_id.to_string(),
                current_page,
                total_pages: Some(total_pages),
                finished_at,
                last_read_at: timestamp,
                scroll_position: 0.0,
            },
        );
    }
}

impl Render for ReaderView {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(cx.theme().background)
            .child(self.viewer.clone())
    }
}
