//! リーダー: `ImageViewer` を本の Pack ページで起動し、`reading_progress`
//! をページ変更のたびに保存する。Workspace のメイン領域に埋め込まれる。

use std::sync::Arc;
use std::time::Instant;

use gpui_kit::Styled as _;
use gpui_kit::Subscription;
use gpui_kit::component::ActiveTheme as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement, ReadGlobal as _, Render,
    SharedString, Window, div,
};
use thundoku_core::db;

use crate::app_state::AppState;
use crate::components::image_viewer::{
    Base64PageLoader, ContentEntry, FormatEntry, ImageViewer, PackPageLoader,
};

pub struct ReaderView {
    viewer: Entity<ImageViewer>,
    /// 本棚の本の場合のみ Some（進捗保存対象）。試し読みは None。
    book_id: Option<SharedString>,
    /// 表示中のコンテンツ（フェーズ3。None = 既定表示コンテンツ）。
    content_id: Option<SharedString>,
    /// 表示中のレンディション（None = そのコンテンツの先頭レンディション）。
    format_id: Option<SharedString>,
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

    /// DB に保存するコンテンツキー（未指定 = `''`。旧データ / 単一コンテンツ）。
    fn content_key(&self) -> String {
        self.content_id
            .as_ref()
            .map(|id| id.to_string())
            .unwrap_or_default()
    }

    /// 表示中の（コンテンツ, レンディション）。None は未指定（既定表示 / 単一コンテンツ）。
    pub fn selection(&self) -> (Option<SharedString>, Option<SharedString>) {
        (self.content_id.clone(), self.format_id.clone())
    }

    /// 表示するコンテンツ／レンディションを切り替える（フェーズ4の UI から呼ぶ）。
    ///
    /// 先頭ページに戻し、ページ毎記録の起点も切り替える。進捗（`reading_progress`）は
    /// フェーズ5でコンテンツ単位にするまで本単位のまま（切り替えても保存先は同じ）。
    pub fn switch_selection(
        &mut self,
        cx: &mut Context<Self>,
        content_id: Option<String>,
        format_id: Option<String>,
    ) {
        let Some(book_id) = self.book_id.clone() else {
            return;
        };
        let state = AppState::global(cx);
        let db = state.db_pool.clone();
        let packs_dir = state.packs_dir.clone();
        let google_sub = state.google_profile.lock().as_ref().map(|p| p.sub.clone());
        let images = db::documents::images_for_selection(
            &db,
            &book_id,
            content_id.as_deref(),
            format_id.as_deref(),
        )
        .unwrap_or_default()
        .into_iter()
        .filter(|image| image.image_type == "page")
        .collect::<Vec<_>>();
        let loader = Arc::new(PackPageLoader {
            images,
            packs_dir,
            db: db.clone(),
            identity: google_sub.map(|sub| opfspack::Identity {
                sub,
                pack_id: book_id.to_string(),
            }),
            pack_bytes: std::sync::OnceLock::new(),
            pack_key: std::sync::OnceLock::new(),
        });
        // レンディション未指定なら、そのコンテンツの先頭を実際の選択として記録する
        let resolved_format = match &format_id {
            Some(id) => Some(id.clone()),
            None => content_id.as_deref().and_then(|content_id| {
                db::contents::formats_for_content(&db, content_id)
                    .ok()
                    .and_then(|formats| formats.into_iter().next())
                    .map(|format| format.format_id)
            }),
        };
        self.content_id = content_id.map(SharedString::from);
        self.format_id = resolved_format.map(SharedString::from);
        // そのコンテンツの保存位置から再開する（コンテンツごとに独立した進捗）
        let content_str = self.content_key();
        let initial_page = db::progress::get_for(&db, &book_id, &content_str)
            .ok()
            .flatten()
            .map(|progress| progress.current_page.max(1).saturating_sub(1) as usize)
            .unwrap_or(0);
        self.last_saved_page = initial_page as i64 + 1;
        self.last_pages = vec![initial_page];
        self.last_page_at = Some(Instant::now());
        let viewer = self.viewer.clone();
        viewer.update(cx, |viewer, cx| viewer.set_loader(cx, loader, initial_page));
        // ページ一覧の「現在表示中」マークを更新する
        self.refresh_contents(&viewer, cx);
        cx.notify();
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
                let content_str = self.content_key();
                let state = AppState::global(cx);
                for page in &self.last_pages {
                    let _ = db::page_views::add_dwell(
                        &state.db_pool,
                        &book_str,
                        &content_str,
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

        let (title, images, progress, site_id, selection) = {
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
            // 既定表示コンテンツとその先頭レンディション（旧データは None = 未指定）
            let selection = db::contents::primary_for_book(&db, &book_id)
                .ok()
                .flatten()
                .map(|content| {
                    let format_id = db::contents::formats_for_content(&db, &content.content_id)
                        .ok()
                        .and_then(|formats| formats.into_iter().next())
                        .map(|format| format.format_id);
                    (content.content_id, format_id)
                });
            (title, images, progress, site_id, selection)
        };

        let identity = google_sub.map(|sub| opfspack::Identity {
            sub,
            pack_id: book_id.clone(),
        });
        let contents = load_content_entries(&db, &book_id);
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
        // ページ一覧で使うコンテンツ一覧を渡す（複数コンテンツ / レンディションの切替用）
        viewer.update(cx, |viewer, cx| {
            viewer.set_contents(
                cx,
                contents,
                selection.as_ref().map(|(content_id, _)| content_id.clone()),
                selection
                    .as_ref()
                    .and_then(|(_, format_id)| format_id.clone()),
            )
        });
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
            let content_str = selection
                .as_ref()
                .map(|(content_id, _)| content_id.clone())
                .unwrap_or_default();
            let state = AppState::global(cx);
            for page in &initial_pages {
                let _ = db::page_views::record_view(
                    &state.db_pool,
                    &book_id,
                    &content_str,
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
            content_id: selection
                .as_ref()
                .map(|(content_id, _)| SharedString::from(content_id.clone())),
            format_id: selection
                .as_ref()
                .and_then(|(_, format_id)| format_id.clone())
                .map(SharedString::from),
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
            content_id: None,
            format_id: None,
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
        // メニューの切替を反映する（選択したコンテンツが既定表示になる）
        if let Some((content_id, format_id)) = viewer.update(cx, |viewer, _| viewer.take_action()) {
            self.apply_switch(content_id, format_id, cx);
        }
        // コンテンツ名のカスタムを DB と pack に反映する（フェーズ7）
        if let Some((content_id, display_name)) =
            viewer.update(cx, |viewer, _| viewer.take_rename())
        {
            self.apply_rename(content_id, display_name, cx);
        }
        self.record_page_view(viewer.clone(), cx);
        self.save_progress(viewer, cx);
    }

    /// メニューで選ばれたコンテンツ／レンディションに切り替える。
    /// 選択したコンテンツはその本の既定表示（`is_primary`）にする。
    fn apply_switch(
        &mut self,
        content_id: String,
        format_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(book_id) = self.book_id.clone() {
            let state = AppState::global(cx);
            let _ = db::contents::set_primary(&state.db_pool, &book_id, &content_id);
        }
        self.switch_selection(cx, Some(content_id), format_id);
    }

    /// コンテンツ名のカスタムを DB と pack の両方へ反映する。
    ///
    /// DB を正とし（空名・同名は [`db::contents::rename`] が弾く）、pack は
    /// Drive から復元したときに取り込み時の名前へ戻らないよう書き換える。
    fn apply_rename(&mut self, content_id: String, display_name: String, cx: &mut Context<Self>) {
        let Some(book_id) = self.book_id.clone() else {
            return;
        };
        let changed = {
            let state = AppState::global(cx);
            db::contents::rename(&state.db_pool, &book_id, &content_id, &display_name)
                .unwrap_or(false)
        };
        if changed {
            self.rewrite_pack_content_name(&book_id, &content_id, &display_name, cx);
        }
        // メニューの表示名を保存後の名前（トリム済み）に更新する
        let viewer = self.viewer.clone();
        self.refresh_contents(&viewer, cx);
    }

    /// pack の `metadata.json` に記録された表示名を書き換える。
    /// 全エントリを復号 → 再圧縮して組み直すため重い（背景スレッドで実行する）。
    /// pack が無い本（未ダウンロード）や読み取りに失敗した場合は何もしない。
    fn rewrite_pack_content_name(
        &self,
        book_id: &str,
        content_id: &str,
        display_name: &str,
        cx: &mut Context<Self>,
    ) {
        let state = AppState::global(cx);
        let db = state.db_pool.clone();
        let packs_dir = state.packs_dir.clone();
        let sub = state
            .google_profile
            .lock()
            .as_ref()
            .map(|profile| profile.sub.clone());
        let book_id = book_id.to_string();
        let content_id = content_id.to_string();
        let display_name = display_name.to_string();
        // 背景（ページ画像のデコードと同じ executor）で実行する — 全エントリの
        // 復号 + 再圧縮は重く、UI スレッドを止められないため。
        cx.background_executor()
            .spawn(async move {
                rewrite_pack_content_name_sync(
                    &db,
                    &packs_dir,
                    sub,
                    &book_id,
                    &content_id,
                    &display_name,
                );
            })
            .detach();
    }

    /// ページ一覧用のコンテンツ一覧を DB から読み直してビューアに渡す。
    fn refresh_contents(&self, viewer: &Entity<ImageViewer>, cx: &mut Context<Self>) {
        let Some(book_id) = self.book_id.clone() else {
            return;
        };
        let state = AppState::global(cx);
        let contents = load_content_entries(&state.db_pool, &book_id);
        let current_content = self.content_id.as_ref().map(|id| id.to_string());
        let current_format = self.format_id.as_ref().map(|id| id.to_string());
        viewer.update(cx, |viewer, cx| {
            viewer.set_contents(cx, contents, current_content, current_format)
        });
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
                let content_str = self.content_key();
                for page in &self.last_pages {
                    let _ = db::page_views::add_dwell(
                        db,
                        &book_str,
                        &content_str,
                        *page as i64 + 1,
                        secs,
                    );
                }
            }
        }
        // 新しい表示ページ集合を計上し、滞在計測を開始する
        self.last_pages = current_pages.clone();
        self.last_page_at = Some(now);
        let content_str = self.content_key();
        for page in &current_pages {
            let _ = db::page_views::record_view(db, &book_str, &content_str, *page as i64 + 1);
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
                content_id: self.content_key(),
                current_page,
                total_pages: Some(total_pages),
                finished_at,
                last_read_at: timestamp,
                scroll_position: 0.0,
            },
        );
    }
}

/// pack の `metadata.json` に記録された表示名を書き換える（同期・重い処理の本体）。
///
/// 全エントリを復号 → 再圧縮して組み直すため、アプリからは背景 executor 経由で呼ぶ。
/// テストが決定的に検証できるよう、I/O まで含めて関数として切り出している。
fn rewrite_pack_content_name_sync(
    db: &thundoku_core::db::SqlitePool,
    packs_dir: &std::path::Path,
    sub: Option<String>,
    book_id: &str,
    content_id: &str,
    display_name: &str,
) {
    let Ok(Some(book)) = db::books::get(db, book_id) else {
        return;
    };
    let pack_id = book.pack_id.clone().unwrap_or_else(|| book.id.clone());
    let path = packs_dir.join(format!("{pack_id}.opfspack"));
    // 未ダウンロードの本は pack が無い（DB の名前だけが正）
    let Ok(bytes) = std::fs::read(&path) else {
        return;
    };
    // 読み出しと同じ identity（`PackPageLoader` と同じ pack_id = book_id）で復号する
    let identity = sub.map(|sub| opfspack::Identity {
        sub,
        pack_id: book_id.to_string(),
    });
    match thundoku_core::import::rename_content_in_pack(
        &bytes,
        content_id,
        display_name,
        identity.as_ref(),
    ) {
        Ok(Some(rewritten)) => {
            if let Err(error) = std::fs::write(&path, &rewritten) {
                log::warn!("pack の名前書き換えに失敗: {path:?}: {error}");
            }
        }
        Ok(None) => {}
        Err(error) => log::warn!("pack の名前書き換えに失敗: {path:?}: {error}"),
    }
}

/// ページ一覧メニュー用のコンテンツ一覧（レンディション付き）を DB から読む。
///
/// フェーズ2以前に取り込んだ本は `book_contents` を持たないため、取り込み済み
/// ドキュメントから 1 行だけ合成する（メニューは「アイコン + 一覧 ›」の同じ形）。
fn load_content_entries(db: &thundoku_core::db::SqlitePool, book_id: &str) -> Vec<ContentEntry> {
    let stored = db::contents::list_with_formats(db, book_id).unwrap_or_default();
    if !stored.is_empty() {
        return stored
            .into_iter()
            .map(|(content, formats)| ContentEntry {
                content_id: content.content_id,
                display_name: content.display_name,
                media_kind: content.media_kind,
                is_primary: content.is_primary != 0,
                formats: formats
                    .into_iter()
                    .map(|format| FormatEntry {
                        format_id: format.format_id,
                        label: format.label,
                        page_count: format.page_count,
                        format_kind: format.format_kind,
                    })
                    .collect(),
            })
            .collect();
    }
    let Some(document) = db::documents::get_document_by_book_id(db, book_id)
        .ok()
        .flatten()
    else {
        return Vec::new();
    };
    let (label, kind) = match document.source_type.as_str() {
        "pdf" => ("PDF", "pdf"),
        "epub" => ("EPUB", "epub"),
        _ => ("画像", "image"),
    };
    vec![ContentEntry {
        content_id: String::new(),
        display_name: String::new(),
        media_kind: kind.to_string(),
        is_primary: true,
        formats: vec![FormatEntry {
            format_id: String::new(),
            label: label.to_string(),
            page_count: document.total_pages,
            format_kind: kind.to_string(),
        }],
    }]
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
    use gpui_kit::TestAppContext;
    use thundoku_core::db::documents;
    use thundoku_core::db::page_views;

    fn book_row(id: &str, title: &str, file_name: &str) -> db::books::Book {
        db::books::Book {
            id: id.into(),
            title: title.into(),
            author: String::new(),
            circle_name: String::new(),
            purchase_date: None,
            file_name: file_name.to_string(),
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
        }
    }

    fn seed_book_with_pages(cx: &mut TestAppContext, id: &str, title: &str, page_count: i64) {
        cx.update(|cx| {
            let state = crate::app_state::AppState::global(cx);
            let db = &state.db_pool;
            db::books::insert(db, &book_row(id, title, &format!("{id}.pdf"))).unwrap();
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
                        content_id: None,
                        format_id: None,
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

    /// 2 コンテンツ（本編 3 ページ / 別冊 1 ページ）の本を入れる（フェーズ3 の切り替え用）。
    fn seed_book_with_two_contents(cx: &mut TestAppContext, id: &str) {
        cx.update(|cx| {
            let state = crate::app_state::AppState::global(cx);
            let db = &state.db_pool;
            db::books::insert(db, &book_row(id, "複数コンテンツ本", &format!("{id}.zip"))).unwrap();
            documents::insert_document(
                db,
                &documents::ImportedDocument {
                    id: format!("{id}-doc"),
                    book_id: id.into(),
                    source_type: "image-set".into(),
                    file_hash: "h".into(),
                    total_pages: 3,
                    metadata: None,
                    status: "done".into(),
                    created_at: "2026-08-21 00:00:00".into(),
                    updated_at: "2026-08-21 00:00:00".into(),
                },
            )
            .unwrap();
            let stamp = "2026-08-21 00:00:00";
            db::contents::insert_batch(
                db,
                &[
                    db::contents::BookContent {
                        content_id: "c-main".into(),
                        book_id: id.into(),
                        display_name: "本編".into(),
                        media_kind: "image".into(),
                        is_primary: 1,
                        sort_order: 0,
                        created_at: stamp.into(),
                    },
                    db::contents::BookContent {
                        content_id: "c-sub".into(),
                        book_id: id.into(),
                        display_name: "別冊".into(),
                        media_kind: "image".into(),
                        is_primary: 0,
                        sort_order: 1,
                        created_at: stamp.into(),
                    },
                ],
                &[
                    db::contents::ContentFormat {
                        format_id: "f-main".into(),
                        content_id: "c-main".into(),
                        label: "画像".into(),
                        format_kind: "image".into(),
                        page_count: 3,
                        pack_entry_prefix: Some("pages".into()),
                        sort_order: 0,
                        created_at: stamp.into(),
                    },
                    db::contents::ContentFormat {
                        format_id: "f-sub".into(),
                        content_id: "c-sub".into(),
                        label: "画像".into(),
                        format_kind: "image".into(),
                        page_count: 1,
                        pack_entry_prefix: Some("contents/1/r0".into()),
                        sort_order: 0,
                        created_at: stamp.into(),
                    },
                ],
            )
            .unwrap();
            let insert_page = |content: &str, format: &str, page: i64, index: usize| {
                documents::insert_image(
                    db,
                    &documents::DocumentImage {
                        id: format!("{id}-img{index}"),
                        document_id: format!("{id}-doc"),
                        content_id: Some(content.into()),
                        format_id: Some(format.into()),
                        page_number: page,
                        image_type: "page".into(),
                        opfs_path: format!("{id}/p{index}").into(),
                        width: 1,
                        height: 1,
                        mime_type: "image/webp".into(),
                        file_size: 1,
                        extracted_text: None,
                        pack_entry_path: None,
                        created_at: stamp.into(),
                    },
                )
                .unwrap();
            };
            for page in 1..=3 {
                insert_page("c-main", "f-main", page, page as usize);
            }
            insert_page("c-sub", "f-sub", 1, 4);
        });
    }

    #[gpui_kit::test]
    async fn switching_selection_reloads_pages(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        seed_book_with_two_contents(cx, "b3");

        let reader = cx.new(|cx| ReaderView::for_book(cx, "b3".to_string()));
        let viewer = reader.read_with(cx, |r, _| r.viewer.clone());
        // 既定表示 = 本編（3 ページ）
        assert_eq!(viewer.read_with(cx, |v, _| v.page_count()), 3);
        assert_eq!(
            reader.read_with(cx, |r, _| r.selection().0.map(|s| s.to_string())),
            Some("c-main".to_string())
        );

        // 別冊へ切り替え（1 ページ・先頭に戻る）
        cx.update(|cx| {
            reader.update(cx, |r, cx| {
                r.switch_selection(cx, Some("c-sub".into()), None)
            });
        });
        assert_eq!(viewer.read_with(cx, |v, _| v.page_count()), 1);
        assert_eq!(viewer.read_with(cx, |v, _| v.current_page()), 0);
        assert_eq!(
            reader.read_with(cx, |r, _| r.selection().0.map(|s| s.to_string())),
            Some("c-sub".to_string())
        );

        // 本編へ戻す（3 ページに戻る）
        cx.update(|cx| {
            reader.update(cx, |r, cx| {
                r.switch_selection(cx, Some("c-main".into()), None)
            });
        });
        assert_eq!(viewer.read_with(cx, |v, _| v.page_count()), 3);
    }

    /// 1 コンテンツ + 1 レンディション（PDF 2 ページ）の本。
    fn seed_book_with_single_rendition(cx: &mut TestAppContext, id: &str, display_name: &str) {
        cx.update(|cx| {
            let state = crate::app_state::AppState::global(cx);
            let db = &state.db_pool;
            db::books::insert(db, &book_row(id, "PDFだけの本", &format!("{id}.pdf"))).unwrap();
            documents::insert_document(
                db,
                &documents::ImportedDocument {
                    id: format!("{id}-doc"),
                    book_id: id.into(),
                    source_type: "pdf".into(),
                    file_hash: "h".into(),
                    total_pages: 2,
                    metadata: None,
                    status: "done".into(),
                    created_at: "2026-08-21 00:00:00".into(),
                    updated_at: "2026-08-21 00:00:00".into(),
                },
            )
            .unwrap();
            let stamp = "2026-08-21 00:00:00";
            db::contents::insert_batch(
                db,
                &[db::contents::BookContent {
                    content_id: format!("{id}-c"),
                    book_id: id.into(),
                    display_name: display_name.into(),
                    media_kind: "pdf".into(),
                    is_primary: 1,
                    sort_order: 0,
                    created_at: stamp.into(),
                }],
                &[db::contents::ContentFormat {
                    format_id: format!("{id}-f"),
                    content_id: format!("{id}-c"),
                    label: "PDF".into(),
                    format_kind: "pdf".into(),
                    page_count: 2,
                    pack_entry_prefix: Some("pages".into()),
                    sort_order: 0,
                    created_at: stamp.into(),
                }],
            )
            .unwrap();
            for page in 1..=2 {
                documents::insert_image(
                    db,
                    &documents::DocumentImage {
                        id: format!("{id}-img{page}"),
                        document_id: format!("{id}-doc"),
                        content_id: Some(format!("{id}-c")),
                        format_id: Some(format!("{id}-f")),
                        page_number: page,
                        image_type: "page".into(),
                        opfs_path: format!("{id}/p{page}"),
                        width: 1,
                        height: 1,
                        mime_type: "image/webp".into(),
                        file_size: 1,
                        extracted_text: None,
                        pack_entry_path: None,
                        created_at: stamp.into(),
                    },
                )
                .unwrap();
            }
        });
    }

    #[gpui_kit::test]
    async fn menu_shows_row_even_for_single_format(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        seed_book_with_single_rendition(cx, "b7", "本文");

        let reader = cx.new(|cx| ReaderView::for_book(cx, "b7".to_string()));
        let viewer = reader.read_with(cx, |r, _| r.viewer.clone());
        // 形式が 1 つだけでもメニューに 1 行出す（「ページ一覧」にフォールバックしない）
        assert_eq!(
            viewer.read_with(cx, |v, _| v.menu_row_titles()),
            vec!["PDF".to_string()]
        );

        // 行を選ぶとその形式でページ一覧が開く
        cx.update(|cx| viewer.update(cx, |v, cx| v.page_list_select_format(cx, 0, 0)));
        cx.run_until_parked();
        assert_eq!(viewer.read_with(cx, |v, _| v.page_count()), 2);
        assert_eq!(
            reader.read_with(cx, |r, _| r.selection().1.map(|s| s.to_string())),
            Some("b7-f".to_string())
        );

        // フォルダ名があるコンテンツはそのフォルダ名を出す（合成名 `本文` のときだけ形式名）
        seed_book_with_single_rendition(cx, "b8", "1.尻穴便女");
        let reader = cx.new(|cx| ReaderView::for_book(cx, "b8".to_string()));
        let viewer = reader.read_with(cx, |r, _| r.viewer.clone());
        assert_eq!(
            viewer.read_with(cx, |v, _| v.menu_row_titles()),
            vec!["1.尻穴便女".to_string()]
        );

        // 旧データ（`book_contents` なし）も同じ「アイコン + 一覧 ›」の行を出す
        seed_book_with_pages(cx, "b9", "旧データ本", 2);
        let reader = cx.new(|cx| ReaderView::for_book(cx, "b9".to_string()));
        let viewer = reader.read_with(cx, |r, _| r.viewer.clone());
        assert_eq!(
            viewer.read_with(cx, |v, _| v.menu_row_titles()),
            vec!["PDF".to_string()]
        );
        assert_eq!(
            viewer.read_with(cx, |v, _| v.menu_row_subtitles()),
            vec!["PDF 2ページ".to_string()]
        );
        // 切替は起きず、ページ一覧だけが開く
        cx.update(|cx| viewer.update(cx, |v, cx| v.page_list_select_format(cx, 0, 0)));
        assert_eq!(viewer.read_with(cx, |v, _| v.page_count()), 2);
    }

    #[gpui_kit::test]
    async fn renaming_rewrites_the_pack_metadata(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        seed_book_with_two_contents(cx, "b13");
        // 実 pack（metadata.json + ページ 1 枚）を置く
        let packs_dir = cx.update(|cx| crate::app_state::AppState::global(cx).packs_dir.clone());
        let mut builder = opfspack::PackBuilder::new(1);
        builder.add_entry(
            "metadata.json",
            serde_json::to_vec(&serde_json::json!({
                "schemaVersion": 1,
                "title": "複数コンテンツ本",
                "contents": [
                    { "contentId": "c-main", "displayName": "本編", "formats": [] },
                    { "contentId": "c-sub", "displayName": "別冊", "formats": [] }
                ]
            }))
            .unwrap(),
            "application/json",
            false,
        );
        builder.add_entry(
            "pages/page_0001.webp",
            b"page-1".to_vec(),
            "image/webp",
            false,
        );
        let pack = builder.build(None, false).unwrap();
        std::fs::create_dir_all(&packs_dir).unwrap();
        std::fs::write(packs_dir.join("b13.opfspack"), &pack).unwrap();

        let reader = cx.new(|cx| ReaderView::for_book(cx, "b13".to_string()));
        let viewer = reader.read_with(cx, |r, _| r.viewer.clone());
        cx.update(|cx| viewer.update(cx, |v, cx| v.rename_content(cx, "c-sub", "続編")));
        cx.run_until_parked();
        // DB の更新を確認したうえで、pack の書き換え本体を同期で実行する
        // （アプリでは背景 executor から同じ関数を呼ぶ。待ち合わせで揺れないように）
        cx.update(|cx| {
            let state = crate::app_state::AppState::global(cx);
            rewrite_pack_content_name_sync(
                &state.db_pool,
                &state.packs_dir,
                None,
                "b13",
                "c-sub",
                "続編",
            );
        });

        // pack の metadata.json も書き換わっている（Drive 復元で名前が戻らない）
        let bytes = std::fs::read(packs_dir.join("b13.opfspack")).unwrap();
        let reader = opfspack::PackReader::open(&bytes).unwrap();
        let meta: serde_json::Value =
            serde_json::from_slice(&reader.read_entry("metadata.json", None).unwrap()).unwrap();
        assert_eq!(meta["contents"][1]["displayName"], "続編");
        assert_eq!(meta["contents"][0]["displayName"], "本編");
        // ページは壊れていない
        assert_eq!(
            reader.read_entry("pages/page_0001.webp", None).unwrap(),
            b"page-1"
        );
    }

    #[gpui_kit::test]
    async fn renaming_a_content_updates_the_menu_and_the_db(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        seed_book_with_two_contents(cx, "b12");

        let reader = cx.new(|cx| ReaderView::for_book(cx, "b12".to_string()));
        let viewer = reader.read_with(cx, |r, _| r.viewer.clone());
        assert_eq!(
            viewer.read_with(cx, |v, _| v.menu_row_titles()),
            vec!["本編".to_string(), "別冊".to_string()]
        );

        // 名前を変更すると（＝入力の確定と同じ経路）、親が DB に保存して一覧を更新する
        cx.update(|cx| viewer.update(cx, |v, cx| v.rename_content(cx, "c-sub", " 続編 ")));
        cx.run_until_parked();

        assert_eq!(
            viewer.read_with(cx, |v, _| v.menu_row_titles()),
            vec!["本編".to_string(), "続編".to_string()]
        );
        let stored = cx.update(|cx| {
            let state = crate::app_state::AppState::global(cx);
            db::contents::list_for_book(&state.db_pool, "b12").unwrap()
        });
        assert_eq!(stored[1].display_name, "続編");

        // 空白だけの名前では何も変わらない（既存の名前を壊さない）
        cx.update(|cx| viewer.update(cx, |v, cx| v.rename_content(cx, "c-sub", "   ")));
        cx.run_until_parked();
        assert_eq!(
            viewer.read_with(cx, |v, _| v.menu_row_titles()),
            vec!["本編".to_string(), "続編".to_string()]
        );
    }

    #[gpui_kit::test]
    async fn page_list_lists_contents_and_switches(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        seed_book_with_two_contents(cx, "b4");

        let reader = cx.new(|cx| ReaderView::for_book(cx, "b4".to_string()));
        let viewer = reader.read_with(cx, |r, _| r.viewer.clone());

        // 一覧はコンテンツ 2 件（表示名つき）。既定表示は本編。
        assert_eq!(viewer.read_with(cx, |v, _| v.contents().len()), 2);
        assert_eq!(
            viewer.read_with(cx, |v, _| v.contents()[0].display_name.clone()),
            "本編"
        );
        assert!(viewer.read_with(cx, |v, _| v.contents()[0].is_primary));

        // メニューの「ページ一覧」位置に切替行が出る（コンテンツ名）
        assert_eq!(
            viewer.read_with(cx, |v, _| v.menu_row_titles()),
            vec!["本編".to_string(), "別冊".to_string()]
        );

        // 別冊の行を選ぶ → ReaderView が反映してページ数が変わる
        cx.update(|cx| viewer.update(cx, |v, cx| v.page_list_select_format(cx, 1, 0)));
        cx.run_until_parked();
        assert_eq!(viewer.read_with(cx, |v, _| v.page_count()), 1);
        assert_eq!(
            reader.read_with(cx, |r, _| r.selection().0.map(|s| s.to_string())),
            Some("c-sub".to_string())
        );
    }

    #[gpui_kit::test]
    async fn switching_content_resumes_its_own_position(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        seed_book_with_two_contents(cx, "b11");
        // 本編（3 ページ）を最後まで読んだ状態にしておく
        cx.update(|cx| {
            let state = crate::app_state::AppState::global(cx);
            db::progress::upsert(
                &state.db_pool,
                &db::progress::ReadingProgress {
                    book_id: "b11".into(),
                    content_id: "c-main".into(),
                    current_page: 3,
                    total_pages: Some(3),
                    finished_at: None,
                    last_read_at: "2026-01-01 00:00:00".into(),
                    scroll_position: 0.0,
                },
            )
            .unwrap();
        });

        let reader = cx.new(|cx| ReaderView::for_book(cx, "b11".to_string()));
        let viewer = reader.read_with(cx, |r, _| r.viewer.clone());
        // 保存位置（3 ページ目 = index 2）から再開する
        assert_eq!(viewer.read_with(cx, |v, _| v.current_page()), 2);

        // 別冊（進捗なし）へ切り替えると先頭から
        cx.update(|cx| viewer.update(cx, |v, cx| v.page_list_select_format(cx, 1, 0)));
        cx.run_until_parked();
        assert_eq!(viewer.read_with(cx, |v, _| v.current_page()), 0);

        // 本編に戻ると保存位置へ
        cx.update(|cx| viewer.update(cx, |v, cx| v.page_list_select_format(cx, 0, 0)));
        cx.run_until_parked();
        assert_eq!(viewer.read_with(cx, |v, _| v.current_page()), 2);

        // 別冊を見ても本編の進捗は壊れない（コンテンツ単位）
        let main = cx.update(|cx| {
            let state = crate::app_state::AppState::global(cx);
            db::progress::get_for(&state.db_pool, "b11", "c-main")
                .unwrap()
                .unwrap()
        });
        assert_eq!(main.current_page, 3, "別冊の閲覧で本編の進捗が壊れない");
    }

    #[gpui_kit::test]
    async fn selecting_content_makes_it_the_default(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        seed_book_with_two_contents(cx, "b5");

        let reader = cx.new(|cx| ReaderView::for_book(cx, "b5".to_string()));
        let viewer = reader.read_with(cx, |r, _| r.viewer.clone());
        // 初期状態は本編（3 ページ）が既定表示
        assert!(viewer.read_with(cx, |v, _| v.contents()[0].is_primary));

        // 別冊を選ぶ → 表示が切り替わり、そのコンテンツが既定表示になる
        cx.update(|cx| viewer.update(cx, |v, cx| v.page_list_select_format(cx, 1, 0)));
        cx.run_until_parked();

        let primary = cx.update(|cx| {
            let state = crate::app_state::AppState::global(cx);
            db::contents::primary_for_book(&state.db_pool, "b5")
                .unwrap()
                .unwrap()
                .content_id
        });
        assert_eq!(primary, "c-sub", "選択したコンテンツが既定表示になる");
        assert!(viewer.read_with(cx, |v, _| v.contents()[1].is_primary));
        assert!(!viewer.read_with(cx, |v, _| v.contents()[0].is_primary));
        assert_eq!(viewer.read_with(cx, |v, _| v.page_count()), 1);
        assert_eq!(
            reader.read_with(cx, |r, _| r.selection().0.map(|s| s.to_string())),
            Some("c-sub".to_string())
        );
    }

    /// 1 コンテンツ + 2 レンディション（画像 3 ページ / PDF 2 ページ）の本。
    /// 実データの「PDF版 + 画像版」と同じ形（`姉とアナルセックスする話` 相当）。
    fn seed_book_with_two_renditions(cx: &mut TestAppContext, id: &str, display_name: &str) {
        cx.update(|cx| {
            let state = crate::app_state::AppState::global(cx);
            let db = &state.db_pool;
            db::books::insert(db, &book_row(id, "画像とPDFの本", &format!("{id}.zip"))).unwrap();
            documents::insert_document(
                db,
                &documents::ImportedDocument {
                    id: format!("{id}-doc"),
                    book_id: id.into(),
                    source_type: "image-set".into(),
                    file_hash: "h".into(),
                    total_pages: 3,
                    metadata: None,
                    status: "done".into(),
                    created_at: "2026-08-21 00:00:00".into(),
                    updated_at: "2026-08-21 00:00:00".into(),
                },
            )
            .unwrap();
            let stamp = "2026-08-21 00:00:00";
            db::contents::insert_batch(
                db,
                &[db::contents::BookContent {
                    content_id: format!("{id}-c"),
                    book_id: id.into(),
                    display_name: display_name.into(),
                    media_kind: "image".into(),
                    is_primary: 1,
                    sort_order: 0,
                    created_at: stamp.into(),
                }],
                &[
                    db::contents::ContentFormat {
                        format_id: format!("{id}-f-img"),
                        content_id: format!("{id}-c"),
                        label: "画像".into(),
                        format_kind: "image".into(),
                        page_count: 3,
                        pack_entry_prefix: Some("pages".into()),
                        sort_order: 0,
                        created_at: stamp.into(),
                    },
                    db::contents::ContentFormat {
                        format_id: format!("{id}-f-pdf"),
                        content_id: format!("{id}-c"),
                        label: "PDF".into(),
                        format_kind: "pdf".into(),
                        page_count: 2,
                        pack_entry_prefix: Some("contents/0/r1".into()),
                        sort_order: 1,
                        created_at: stamp.into(),
                    },
                ],
            )
            .unwrap();
            let insert_page = |format: &str, page: i64, index: usize| {
                documents::insert_image(
                    db,
                    &documents::DocumentImage {
                        id: format!("{id}-img{index}"),
                        document_id: format!("{id}-doc"),
                        content_id: Some(format!("{id}-c")),
                        format_id: Some(format.into()),
                        page_number: page,
                        image_type: "page".into(),
                        opfs_path: format!("{id}/p{index}"),
                        width: 1,
                        height: 1,
                        mime_type: "image/webp".into(),
                        file_size: 1,
                        extracted_text: None,
                        pack_entry_path: None,
                        created_at: stamp.into(),
                    },
                )
                .unwrap();
            };
            for page in 1..=3 {
                insert_page(&format!("{id}-f-img"), page, page as usize);
            }
            for page in 1..=2 {
                insert_page(&format!("{id}-f-pdf"), page, 3 + page as usize);
            }
        });
    }

    #[gpui_kit::test]
    async fn page_list_shows_rendition_switch_for_single_content(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        seed_book_with_two_renditions(cx, "b6", "本文");

        let reader = cx.new(|cx| ReaderView::for_book(cx, "b6".to_string()));
        let viewer = reader.read_with(cx, |r, _| r.viewer.clone());
        // 1 コンテンツ + 2 レンディション
        assert_eq!(viewer.read_with(cx, |v, _| v.contents().len()), 1);
        assert_eq!(
            viewer.read_with(cx, |v, _| v.contents()[0].formats.len()),
            2
        );

        // メニューの「ページ一覧」位置に、形式の行が並ぶ
        assert_eq!(
            viewer.read_with(cx, |v, _| v.menu_row_titles()),
            vec!["画像".to_string(), "PDF".to_string()]
        );
        // 補足は「画像 Nファイル」「PDF Nページ」
        assert_eq!(
            viewer.read_with(cx, |v, _| v.menu_row_subtitles()),
            vec!["画像 3ファイル".to_string(), "PDF 2ページ".to_string()]
        );

        // PDF 版に切り替えるとページ数が変わる
        cx.update(|cx| viewer.update(cx, |v, cx| v.page_list_select_format(cx, 0, 1)));
        cx.run_until_parked();
        assert_eq!(viewer.read_with(cx, |v, _| v.page_count()), 2);
        assert_eq!(
            reader.read_with(cx, |r, _| r.selection().1.map(|s| s.to_string())),
            Some("b6-f-pdf".to_string())
        );

        // 内容名があるときは行タイトルに形式名を付けない（アイコンと補足で分かる）
        seed_book_with_two_renditions(cx, "b10", "姉とアナルセックスする話");
        let reader = cx.new(|cx| ReaderView::for_book(cx, "b10".to_string()));
        let viewer = reader.read_with(cx, |r, _| r.viewer.clone());
        assert_eq!(
            viewer.read_with(cx, |v, _| v.menu_row_titles()),
            vec![
                "姉とアナルセックスする話".to_string(),
                "姉とアナルセックスする話".to_string()
            ]
        );
        assert_eq!(
            viewer.read_with(cx, |v, _| v.menu_row_subtitles()),
            vec!["画像 3ファイル".to_string(), "PDF 2ページ".to_string()]
        );
    }

    #[gpui_kit::test]
    async fn reading_records_per_page_view_and_dwell(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
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
        assert!(
            rows[2].total_seconds >= 0.0,
            "closing finalizes last page dwell"
        );
    }

    #[gpui_kit::test]
    async fn spread_mode_records_both_pages_of_each_spread(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
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
        assert!(
            pages.contains(&2),
            "right-hand page of first spread recorded"
        );
        assert!(
            pages.contains(&4),
            "right-hand page of second spread recorded"
        );
    }
}
