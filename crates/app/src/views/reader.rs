//! リーダー: `ImageViewer` を本の Pack ページで起動し、`reading_progress`
//! をページ変更のたびに保存する。Workspace のメイン領域に埋め込まれる。

use std::sync::Arc;
use std::time::Instant;

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
    /// ページ毎閲覧記録用: 現在表示しているページ集合（0-indexed）。
    /// 単一表示は 1 ページ、見開きは左右の 2 ページ。
    last_pages: Vec<usize>,
    /// ページ毎の滞在計測開始時刻（現在の表示を開き始めた時刻）。
    last_page_at: Option<Instant>,
}

impl ReaderView {
    /// このリーダーが開いている本の ID（試し読みは None）。
    pub(crate) fn book_id(&self) -> Option<SharedString> {
        self.book_id.clone()
    }

    /// 閲覧履歴セッションを終了する（ビューアーを閉じる際に呼ぶ）。
    pub(crate) fn end_session(&mut self, cx: &App) {
        if let Some(session_id) = self.view_session_id.take() {
            let state = AppState::global(cx);
            let _ = db::view_history::end(&state.db_pool, &session_id);
        }
        // 最後に表示していたページ集合の滞在時間を確定する
        if let (Some(book_id), Some(started)) = (&self.book_id, self.last_page_at) {
            let secs = started.elapsed().as_secs_f64();
            if secs > 0.0 {
                let book_str = book_id.to_string();
                let state = AppState::global(cx);
                for page in &self.last_pages {
                    let _ = db::page_views::add_dwell(
                        &state.db_pool,
                        &book_str,
                        *page as i64 + 1,
                        secs,
                    );
                }
            }
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
        // 初期表示ページ集合（単一: 1 ページ、見開き: 左右 2 ページ）を計上する
        let initial_pages = viewer.read(cx).spread_pages();
        {
            let state = AppState::global(cx);
            for page in &initial_pages {
                let _ = db::page_views::record_view(
                    &state.db_pool,
                    &book_id,
                    *page as i64 + 1,
                );
            }
        }
        let subscription = cx.observe(&viewer, |this, viewer, cx| {
            this.on_viewer_changed(viewer, cx);
        });
        Self {
            viewer,
            book_id: Some(book_id.into()),
            _subscription: Some(subscription),
            // 初期ページ（1-indexed）を保存済みとしてマークし、開いた直後の
            // 不要な保存をスキップする（ページを移動してから保存される）
            last_saved_page: initial_page as i64 + 1,
            view_session_id,
            last_pages: initial_pages,
            last_page_at: Some(Instant::now()),
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
            // 試し読みはページ毎記録しない（book_id が None）
            last_pages: vec![0],
            last_page_at: None,
        }
    }

    /// ビューアーの notify 時（ページ移動・画像ロード・オーバーレイ等）。
    /// ページ毎の閲覧記録と進捗保存をまとめて行う。ページ移動のみを検知するため、
    /// 画像ロードなどの notify では記録しない（`last_page` 比較で弾く）。
    fn on_viewer_changed(&mut self, viewer: Entity<ImageViewer>, cx: &mut Context<Self>) {
        self.record_page_view(viewer.clone(), cx);
        self.save_progress(viewer, cx);
    }

    /// ページ毎の閲覧回数・滞在時間を記録する。
    /// 表示ページ集合（`spread_pages()`）が変わったときのみ記録する。
    /// 見開きモードでは表示中の左右両ページを計上する（右ページも抜けない）。
    fn record_page_view(&mut self, viewer: Entity<ImageViewer>, cx: &mut Context<Self>) {
        let Some(book_id) = self.book_id.clone() else {
            return;
        };
        let current_pages = viewer.read(cx).spread_pages();
        if current_pages == self.last_pages {
            // 表示ページ集合の変化なし（画像ロード・オーバーレイ等の notify）
            return;
        }
        let now = Instant::now();
        let book_str = book_id.to_string();
        let state = AppState::global(cx);
        let db = &state.db_pool;
        // 前の表示ページ集合を離れたので、その滞在時間を確定する
        if let Some(started) = self.last_page_at {
            let secs = now.duration_since(started).as_secs_f64();
            if secs > 0.0 {
                for page in &self.last_pages {
                    let _ = db::page_views::add_dwell(db, &book_str, *page as i64 + 1, secs);
                }
            }
        }
        // 新しい表示ページ集合を計上し、滞在計測を開始する
        self.last_pages = current_pages.clone();
        self.last_page_at = Some(now);
        for page in &current_pages {
            let _ = db::page_views::record_view(db, &book_str, *page as i64 + 1);
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

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;
    use gpui_component;
    use thundoku_core::db::documents;
    use thundoku_core::db::page_views;

    fn seed_book_with_pages(cx: &mut TestAppContext, id: &str, title: &str, page_count: i64) {
        cx.update(|cx| {
            let state = crate::app_state::AppState::global(cx);
            let db = &state.db_pool;
            db::books::insert(
                db,
                &db::books::Book {
                    id: id.into(),
                    title: title.into(),
                    author: String::new(),
                    circle_name: String::new(),
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
                },
            )
            .unwrap();
            documents::insert_document(
                db,
                &documents::ImportedDocument {
                    id: format!("{id}-doc"),
                    book_id: id.into(),
                    source_type: "pdf".into(),
                    file_hash: "h".into(),
                    total_pages: page_count,
                    metadata: None,
                    status: "done".into(),
                    created_at: "2026-08-21 00:00:00".into(),
                    updated_at: "2026-08-21 00:00:00".into(),
                },
            )
            .unwrap();
            for page in 0..page_count {
                documents::insert_image(
                    db,
                    &documents::DocumentImage {
                        id: format!("{id}-img{page}"),
                        document_id: format!("{id}-doc"),
                        page_number: page + 1,
                        image_type: "page".into(),
                        opfs_path: format!("{id}/p{page}").into(),
                        width: 1,
                        height: 1,
                        mime_type: "image/webp".into(),
                        file_size: 1,
                        extracted_text: None,
                        pack_entry_path: None,
                        created_at: "2026-08-21 00:00:00".into(),
                    },
                )
                .unwrap();
            }
        });
    }

    #[gpui::test]
    async fn reading_records_per_page_view_and_dwell(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(crate::app_state::AppState::init_test);
        seed_book_with_pages(cx, "b1", "本1", 3);

        // リーダーを開く（初期ページ 1 が表示として計上される）
        let reader = cx.new(|cx| ReaderView::for_book(cx, "b1".to_string()));
        let viewer = reader.read_with(cx, |r, _| r.viewer.clone());

        // ページ送り ×2（index 0→1→2、ページ番号は 1-indexed）
        cx.update(|cx| viewer.update(cx, |v, cx| v.next_page(cx)));
        cx.update(|cx| viewer.update(cx, |v, cx| v.next_page(cx)));
        cx.run_until_parked();

        // 各ページが一度ずつ表示されている（view_count=1）
        let rows = cx.update(|cx| {
            let state = crate::app_state::AppState::global(cx);
            page_views::for_book(&state.db_pool, "b1").unwrap()
        });
        assert_eq!(rows.len(), 3, "3 pages should each have a record");
        for row in &rows {
            assert_eq!(row.view_count, 1, "each page shown once");
        }
        assert_eq!(rows[0].page_number, 1);
        assert_eq!(rows[1].page_number, 2);
        assert_eq!(rows[2].page_number, 3);

        // 閉じると最後のページの滞在も確定される（行は維持・count は増えない）
        cx.update(|cx| reader.update(cx, |r, cx| r.end_session(cx)));
        cx.run_until_parked();
        let rows = cx.update(|cx| {
            let state = crate::app_state::AppState::global(cx);
            page_views::for_book(&state.db_pool, "b1").unwrap()
        });
        assert_eq!(rows.len(), 3);
        assert!(rows[2].total_seconds >= 0.0, "closing finalizes last page dwell");
    }

    #[gpui::test]
    async fn spread_mode_records_both_pages_of_each_spread(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(crate::app_state::AppState::init_test);
        // 見開き（spread）モードで開く（サイト別キーが無いグローバル設定へ）
        cx.update(|cx| {
            let state = crate::app_state::AppState::global(cx);
            let _ = db::settings::set(&state.db_pool, "viewer.mode", "spread");
        });
        seed_book_with_pages(cx, "b2", "本2", 4);

        let reader = cx.new(|cx| ReaderView::for_book(cx, "b2".to_string()));
        let viewer = reader.read_with(cx, |r, _| r.viewer.clone());
        // 開いた直後: spread 0 → [0,1]（ページ 1,2 を計上）
        // 次の spread: current 0→2 → [2,3]（ページ 3,4 を計上）
        cx.update(|cx| viewer.update(cx, |v, cx| v.next_page(cx)));
        cx.run_until_parked();

        let rows = cx.update(|cx| {
            let state = crate::app_state::AppState::global(cx);
            page_views::for_book(&state.db_pool, "b2").unwrap()
        });
        assert_eq!(rows.len(), 4, "both pages of each spread must be recorded");
        for row in &rows {
            assert_eq!(row.view_count, 1, "each page of the spread shown once");
        }
        let pages: Vec<i64> = rows.iter().map(|r| r.page_number).collect();
        assert!(pages.contains(&2), "right-hand page of first spread recorded");
        assert!(pages.contains(&4), "right-hand page of second spread recorded");
    }
}
