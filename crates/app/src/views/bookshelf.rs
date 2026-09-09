//! 本棚ビュー: ローカル本のグリッド + 検索/タグ/既読フィルタ + 技術書典同期と
//! ダウンロード導線 + インポート。

use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

use gpui_kit::StyledImage as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Anchor, AppContext as _, InteractiveElement as _, ReadGlobal as _,
    StatefulInteractiveElement as _, Styled as _,
};
use gpui_kit::{
    App, Context, Entity, IntoElement, KeyDownEvent, ParentElement, Render, RenderImage,
    SharedString, Window, div, img, px,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState};
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::theme::Colorize as _;
use gpui_kit::component::{ActiveTheme as _, Icon, IconName};
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

/// 本棚の表示モード（Web の viewMode 相当）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum ViewMode {
    Card,
    List,
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
    search_state: Option<Entity<InputState>>,
    selected_tags: Vec<String>,
    all_tags: Vec<String>,
    selected_events: Vec<String>,
    available_events: Vec<String>,
    /// サイトフィルタ（None = すべての本、Some(site_id) = そのサイトのみ）
    site_filter: Option<String>,
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
    read_filter: ReadFilter,
    view_mode: ViewMode,
    tag_fetch_enabled: bool,
    /// 実行中の同期タスク数（TBF / BOOTH が並列に走るためカウンタで管理）
    sync_busy: usize,
    /// 表紙取得（fetch_remote_covers）の実行中フラグ（二重実行防止）
    fetching_covers: bool,
    /// 表紙取得の再実行済みフラグ（同期 reload で後から増えたカード分を 1 回だけ再取得）
    cover_fetch_retried: bool,
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
    /// キーバインド用フォーカス
    focus_handle: gpui_kit::FocusHandle,
    /// フォーカス初回付与済みフラグ
    focus_initialized: bool,
    /// 選択中のカード（filtered 内のインデックス）
    selected_index: Option<usize>,
    error: Option<String>,
    toast: Option<String>,
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
            search_state: None,
            selected_tags: Vec::new(),
            all_tags: Vec::new(),
            selected_events: Vec::new(),
            site_filter: Self::read_site_filter(cx),
            available_events: Vec::new(),
            editing_book_id: None,
            editing_site_id: None,
            editing_tags: Vec::new(),
            editing_suggestions: Vec::new(),
            editing_input: None,
            pending_tag_edit: None,
            read_filter: ReadFilter::All,
            view_mode: ViewMode::Card,
            tag_fetch_enabled: true,
            sync_busy: 0,
            fetching_covers: false,
            cover_fetch_retried: false,
            auto_download_started: false,
            filtered: Vec::new(),
            filtered_dirty: true,
            last_search: String::new(),
            list_state: gpui_kit::ListState::new(0, gpui_kit::ListAlignment::Top, gpui_kit::px(100.0))
                .measure_all(),
            scroll_handle: gpui_kit::ScrollHandle::new(),
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
    fn columns_for_width(window_width: f32) -> usize {
        if window_width >= 2560.0 {
            // 4K 等の超広幅では、タイル幅を理想値に近づけるよう列数を増やす
            // （固定 5 列だとタイルが横に間延びするため）。
            let content = window_width - 255.0 - 24.0; // サイドバー + パディング
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

    /// キーボード操作: Enter で開く、矢印 / hjkl で選択移動
    fn handle_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        match event.keystroke.key.as_str() {
            "enter" => self.activate_selected(cx),
            "right" | "l" => self.shift_selection(1, 0, window, cx),
            "left" | "h" => self.shift_selection(-1, 0, window, cx),
            "down" | "j" => self.shift_selection(0, 1, window, cx),
            "up" | "k" => self.shift_selection(0, -1, window, cx),
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
        self.entries
            .iter()
            .find(|e| e.book.id == book_id)
            .map(|e| {
                let (current, total) = e.progress.unwrap_or((0, None));
                (current, total, e.is_read)
            })
    }

    pub(crate) fn reload(&mut self, cx: &mut Context<Self>) {
        log::info!("reload: 開始");
        let reload_start = std::time::Instant::now();
        let (entries, shelf_items, all_tags, favorite_tags, available_events, shelf_cards) = {
            let state = Self::app_state(cx);
            let db = &state.db_pool;
            let packs_dir = state.packs_dir.clone();
            let thumbnails_dir = state.data_dir.join("thumbnails");
            // 所有者フィルタ：ログイン中は現在 sub の本、未ログインは未所属(NULL)の本だけ表示。
            let google_sub = state.google_profile.lock().as_ref().map(|p| p.sub.clone());
            let owned = {
                let key = state.secrets.db_key().ok();
                match (google_sub.clone(), key) {
                    (Some(sub), Some(key)) => {
                        db::books::owned_book_ids(db, &key, Some(&sub)).unwrap_or_default()
                    }
                    // ログイン中だが key が無い → 復号不能なので表示しない。
                    (Some(_), None) => std::collections::HashSet::new(),
                    // 未ログイン → 未所属(NULL)。NULL 判定は key を使わないのでダミーで良い。
                    (None, key) => db::books::owned_book_ids(
                        db,
                        key.as_ref().unwrap_or(&[0u8; 32]),
                        None,
                    )
                    .unwrap_or_default(),
                }
            };
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
                let cover = if shelf.site_id == "fanza" || shelf.site_id == "dlsite" {
                    // FANZA / DLsite はダウンロード後のパック表紙に差し替えず、同期時の
                    // サムネイル（thumbnail_url 由来のキャッシュ）を維持する。
                    load_cached_cover(&thumbnails_dir, shelf)
                        .or_else(|| local.and_then(|entry| entry.cover.clone()))
                        .or_else(|| placeholder_cover(&shelf.title, &shelf.circle_name))
                } else {
                    local
                        .and_then(|entry| entry.cover.clone())
                        .or_else(|| load_cached_cover(&thumbnails_dir, shelf))
                        .or_else(|| placeholder_cover(&shelf.title, &shelf.circle_name))
                };
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
                    });
                }
            }
            (
                entries,
                shelf_items,
                all_tags,
                favorite_tags,
                available_events,
                shelf_cards,
            )
        };
        log::info!(
            "reload: データ取得 + カード生成（{} 件）（{:?}）",
            shelf_cards.len(),
            reload_start.elapsed()
        );
        self.entries = entries;
        self.shelf_items = shelf_items;
        self.shelf_cards = shelf_cards;
        self.all_tags = all_tags;
        self.favorite_tags = favorite_tags;
        self.available_events = available_events;
        self.selected_tags.retain(|tag| self.all_tags.contains(tag));
        self.filtered_dirty = true;
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
                            let bytes = if site_id == "booth" {
                                // BOOTH の表紙は公開画像（booth.pximg.net — Cookie 不要）。
                                // 商品ページの共有画像（オリジナル・高解像度）を優先し、
                                // ライブラリのサムネイル（thumbnail_url）はフォールバック。
                                let primary = booth_session.as_ref().and_then(|session| {
                                    let item_id: u64 = database_id.parse().ok()?;
                                    let detail =
                                        BoothClient::new(session).item_detail(item_id).ok()?;
                                    detail.images.into_iter().next()
                                });
                                match primary {
                                    Some(image_url) => fetch_bytes(agent, &image_url)
                                        .or_else(|| fetch_bytes(agent, &resolved)),
                                    None => fetch_bytes(agent, &resolved),
                                }
                            } else if site_id == "dlsite" {
                                // DLsite の表紙は公開 CDN（img.dlsite.jp、Cookie 不要）。
                                // プロトコル相対 URL は上で絶対化済み。TBF クライアントには
                                // フォールバックしない（誤った経路で失敗するため）。
                                fetch_bytes(agent, &resolved)
                            } else {
                                // TBF の表紙も公開 URL なら直接取得する（4 並列が機能する）。
                                // 失敗した場合のみセッション付きクライアントにフォールバック
                                // （クライアントは Mutex のため直列になるが、まれなケース）。
                                match fetch_bytes(agent, &resolved) {
                                    Some(bytes) => Some(bytes),
                                    None => {
                                        log::warn!(
                                            "TBF 表紙を直接取得できずフォールバック: {resolved}"
                                        );
                                        tbf_client.lock().download(&resolved).ok()
                                    }
                                }
                            };
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
                            let _ext = match &bytes[..] {
                                _ if bytes.len() >= 2 && bytes[0] == 0xff && bytes[1] == 0xd8 => {
                                    "jpg"
                                }
                                _ if bytes.len() >= 8 && &bytes[0..8] == b"\x89PNG\r\n\x1a\n" => {
                                    "png"
                                }
                                _ if bytes.len() >= 12
                                    && &bytes[0..4] == b"RIFF"
                                    && &bytes[8..12] == b"WEBP" =>
                                {
                                    "webp"
                                }
                                _ => "jpg",
                            };
                            // 縮小済みサムネイルを PNG で保存する（reload 時のキャッシュ
                            // 読み込みがオリジナル（1MB 超）だと 300 件で 100 秒超かかるため）
                            let cache_path =
                                thumbnails_dir.join(format!("{site_id}_{database_id}.png"));
                            if let Some(cached) = resize_for_cache(&bytes, 288) {
                                let _ = std::fs::write(&cache_path, &cached);
                            }
                            // デコード + 縮小はこのスレッド（4 並列）で行い、UI には
                            // デコード済みサムネイルだけ送る（UI スレッドで 307 枚
                            // デコードすると固まるため）
                            let Some(image) = decode_and_resize(&bytes, 288) else {
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
                let session =
                    session.ok_or_else(|| "FANZA セッションがありません".to_string())?;
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
                        if message.contains("not logged in") || message.contains("セッション") {
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
                let session =
                    session.ok_or_else(|| "DLsite セッションがありません".to_string())?;
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
                        if message.contains("not logged in") || message.contains("セッション") {
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
        // ダウンロード + インポート（レンダリング含む）は GPUI のワーカーを
        // 数分ブロックすると他の処理（表紙取得など）が止まってビジーになるため、
        // 専用スレッドで実行して結果をチャネルで受け取る。
        let (result_tx, result_rx) = std::sync::mpsc::channel::<Result<String, String>>();
        std::thread::spawn(move || {
            let result = (|| -> Result<String, String> {
                // ダウンロード（サイトで分岐）:
                // - BOOTH: セッション Cookie で downloadables/{id} を GET → 302 の
                //   Location（署名付き S3 URL）を自動追跡してファイル本体を取得
                // - 技術書典: GraphQL の downloadURL を resolve して取得
                let mut genre_tags: Vec<String> = Vec::new();
                let bytes = if site_id == "booth" {
                    let session =
                        booth_session.ok_or_else(|| "BOOTH セッションがありません".to_string())?;
                    let client = BoothClient::new(&session);
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
                        return Err("DRM 付き作品は取り込めません".to_string());
                    }
                    let url = detail
                        .download_link
                        .ok_or_else(|| "FANZA ダウンロード URL がありません".to_string())?;
                    // ジャンルタグ（作品ページから。取得失敗してもダウンロードは続行）
                    genre_tags = client
                        .product_page(&product_id)
                        .map(|p| p.genre_tags)
                        .unwrap_or_default();
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
                                .ok_or_else(|| {
                                    "DLsite ダウンロード URL がありません".to_string()
                                })?
                        }
                    };
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
                if site_id == "fanza" || site_id == "dlsite" {
                    if let Some(ext) = sniff_extension(&bytes) {
                        let stem = file_name
                            .rsplit_once('.')
                            .map(|(s, _)| s)
                            .unwrap_or(&file_name);
                        file_name = format!("{stem}.{ext}");
                    }
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
                    let reuse_id = books::resolve_reuse_id(
                        &db,
                        key,
                        &site_id,
                        &product_id,
                        Some(sub),
                    )
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
                        _ => books::resolve_reuse_id(
                            &db,
                            &[0u8; 32],
                            &site_id,
                            &product_id,
                            None,
                        )
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
                    )
                    .map_err(|e| e.to_string())?;
                    // ダウンロード元のサイトを記録（ビューアー設定のサイト別キー用）
                    if !site_id.is_empty() {
                        let _ = books::set_site_id(&db, &imported.book.id, &site_id);
                    }
                    // 本棚の bookshelf_items.database_id と対応付け、カードを
                    // 「ダウンロード済み」として表示・ビューアーで開けるようにする。
                    let _ = books::set_tbf_product_id(&db, &imported.book.id, &product_id);
                    // ダウンロード直後からページ数を表示できるように
                    // reading_progress（未読・総ページ数）を作成する。
                    if imported.document.total_pages > 0 {
                        let _ = progress::upsert(
                            &db,
                            &progress::ReadingProgress {
                                book_id: imported.book.id.clone(),
                                current_page: 0,
                                total_pages: Some(imported.document.total_pages),
                                finished_at: None,
                                last_read_at: "2026-01-01 00:00:00".to_string(),
                                scroll_position: 0.0,
                            },
                        );
                    }
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
                    Ok::<_, String>(imported)
                } else {
                    let imported = match extension.as_str() {
                        "epub" => thundoku_core::import::import_epub_bytes(
                            &db, &file_name, &bytes, &packs_dir, identity.as_ref(),
                        ),
                        "zip" => thundoku_core::import::import_zip_bytes(
                            &db,
                            &file_name,
                            &bytes,
                            &packs_dir,
                            identity.as_ref(),
                            &mut on_import,
                            reuse_book_id.as_deref(),
                        ),
                        // BOOTH は PDF だけでなく画像ファイル（イラスト等）もある
                        "jpg" | "jpeg" | "png" | "webp" | "gif" => {
                            thundoku_core::import::import_image_bytes(
                                &db, &file_name, &bytes, &packs_dir, identity.as_ref(),
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
                        // FANZA: インポート後にリモートメタ（タイトル/サークル/購入日）を補完する
                        //（インポートはファイル名由来で作るため空になる）
                        if site_id == "fanza" {
                            let _ = books::set_metadata(
                                &db,
                                &imported.book.id,
                                &item.title,
                                "",
                                &item.circle_name,
                                item.caused_at.clone(),
                            );
                            // サイトから取得したジャンルタグを book_tags に保存する
                            if !genre_tags.is_empty() {
                                let pairs: Vec<(&str, &str)> = genre_tags
                                    .iter()
                                    .map(|t| (t.as_str(), "fanza_genre"))
                                    .collect();
                                let _ = db::tags::set_for_book(&db, &imported.book.id, &pairs);
                                // 本棚アイテムの tags_json にも書く（owned フィルタで
                                // local が外れてもカードに表示できるように）
                                let _ = bookshelf::update_tags(
                                    &db,
                                    "fanza",
                                    &product_id,
                                    &genre_tags,
                                );
                            }
                        }
                        // DLsite: インポート後に共有メタ列（media_category / ai_type / is_drm /
                        // release_date / maker_id / age_rating / series_name）とカスタムジャンル
                        // （tags_json）を books / book_tags へ反映する。
                        if site_id == "dlsite" {
                            let _ = books::set_metadata(
                                &db,
                                &imported.book.id,
                                &item.title,
                                "",
                                &item.circle_name,
                                item.caused_at.clone(),
                            );
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
                            if let Some(tags_json) = &item.tags_json {
                                if let Ok(tags) = serde_json::from_str::<Vec<String>>(tags_json) {
                                    if !tags.is_empty() {
                                        let pairs: Vec<(&str, &str)> = tags
                                            .iter()
                                            .map(|t| (t.as_str(), "dlsite_genre"))
                                            .collect();
                                        let _ = db::tags::set_for_book(
                                            &db,
                                            &imported.book.id,
                                            &pairs,
                                        );
                                    }
                                }
                            }
                        }
                        if imported.document.total_pages > 0 {
                            let _ = progress::upsert(
                                &db,
                                &progress::ReadingProgress {
                                    book_id: imported.book.id.clone(),
                                    current_page: 0,
                                    total_pages: Some(imported.document.total_pages),
                                    finished_at: None,
                                    last_read_at: "2026-01-01 00:00:00".to_string(),
                                    scroll_position: 0.0,
                                },
                            );
                        }
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
                    imported.map_err(|e| e.to_string())
                };
                imported
                    .map(|imported| imported.book.title)
                    .map_err(|e| e.to_string())
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
            let result = result_rx
                .try_recv()
                .unwrap_or_else(|_| Err("ダウンロード処理が結果を返しませんでした".into()));
            log::info!("download_item: スレッド完了、UI 反映開始");
            let complete_start = std::time::Instant::now();
            handle.update(cx, |this, cx| {
                this.download_states.remove(&database_id);
                match result {
                    Ok(title) => {
                        this.toast = Some(format!("「{title}」をダウンロードしました"));
                        this.reload(cx);
                    }
                    Err(error) => {
                        log::warn!("download_item: 失敗しました: {error}");
                        this.error = Some(error);
                    }
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
    /// 既にローカルに持っている場合は pack ごと削除してから再取得する。
    pub(crate) fn redownload_item(&mut self, cx: &mut Context<Self>, card: &ShelfCard) {
        let state = Self::app_state(cx);
        if let Some(local) = &card.local {
            let db = &state.db_pool;
            let _ = progress::delete(db, &local.book.id);
            let _ = books::delete(db, &local.book.id);
            let path = state.packs_dir.join(format!("{}.opfspack", local.book.id));
            let _ = std::fs::remove_file(path);
        }
        // 表紙キャッシュも破棄して再取得させる（reload の fetch_remote_covers が
        // thumbnail_url から取り直す）。キャッシュファイル + カードの cover をクリア。
        if !card.shelf.site_id.is_empty() {
            let cache_path = state.data_dir.join("thumbnails").join(format!(
                "{}_{}.png",
                card.shelf.site_id, card.shelf.database_id
            ));
            let _ = std::fs::remove_file(&cache_path);
            if let Some(slot) = self.shelf_cards.iter_mut().find(|c| {
                c.shelf.site_id == card.shelf.site_id
                    && c.shelf.database_id == card.shelf.database_id
            }) {
                slot.cover = None;
                slot.cover_fetch_failed = false;
            }
        }
        self.download_item(cx, card.shelf.clone());
        self.toast = Some("再取得を開始しました".into());
        cx.notify();
    }

    /// タグ取得 ON/OFF トグル（Web 版の `handleToggleTagFetch` 相当）。
    /// OFF の間はダウンロード時にタグを自動生成しない。
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

    /// インラインタグ編集を開始（Web の TagList 編集モード相当）。
    fn start_tag_edit(&mut self, window: &mut Window, cx: &mut Context<Self>, book_id: &str) {
        let (tags, suggestions) = {
            let state = Self::app_state(cx);
            let db = &state.db_pool;
            let tags = if let Some(local_id) = Self::resolve_local_book_id(db, book_id) {
                db::tags::list_for_book(db, &local_id)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|tag| tag.source == "manual" || tag.source == "fanza_genre" || tag.source == "dlsite_genre")
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
            let book_site: Option<String> = if let Some(local_id) =
                Self::resolve_local_book_id(db, book_id)
            {
                books::get(db, &local_id).ok().flatten().and_then(|b| b.site_id)
            } else {
                bookshelf::list_all(db)
                    .ok()
                    .unwrap_or_default()
                    .into_iter()
                    .find(|i| i.database_id == book_id)
                    .map(|i| i.site_id)
            };
            let allowed: Option<std::collections::HashSet<String>> = book_site.as_deref().map(|site| {
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
                if allowed.as_ref().map_or(true, |s| s.contains(&tag))
                    && !suggestions.contains(&tag)
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
                if matches!(self.editing_site_id.as_deref(), Some("fanza") | Some("dlsite")) {
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
        favorite_tags: &[String],
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
        let purchase_date = shelf
            .caused_at
            .as_deref()
            .map(format_purchase_date);
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

        // -- 表紙: 画像 + 未読/既読バッジ + ダウンロード状態アイコン + 進捗リング --
        // 枠の高さを画像のアスペクト比に合わせる（自然比率を保つ＝クロップ/レターボックスなし）。
        // バッジ（未読/♡/↓）は枠=画像に乗る。FANZA は横長（4:3）サムネ等のため枠高さが変わる。
        let cover_h: f32 = match &cover {
            Some(render) => {
                let size = render.size(0);
                let w = size.width.0 as f32;
                let h = size.height.0 as f32;
                (144.0 * (h / w.max(1.0))).clamp(80.0, 420.0)
            }
            None => 192.0,
        };
        let image: gpui_kit::AnyElement = match &cover {
            Some(render) => div()
                .w(px(144.0))
                .h(px(cover_h))
                .overflow_hidden()
                .child(
                    img(render.clone())
                        .w_full()
                        .h_full()
                        .object_fit(gpui_kit::ObjectFit::Cover),
                )
                .into_any_element(),
            None => div()
                .w(px(144.0))
                .h(px(cover_h))
                .bg(theme.muted)
                .into_any_element(),
        };

        let mut cover_el = div()
            .relative()
            .w(px(144.0))
            .h(px(cover_h))
            .child(image)
            // 左上: 未読/既読バッジ（Web の statusText と同じ）
            .child(if is_read {
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .rounded_br_md()
                    .px_1()
                    .py_0p5()
                    .bg(gpui_kit::rgb(0xd1fae5))
                    .text_color(gpui_kit::rgb(0x047857))
                    .text_xs()
                    .child("読了")
                    .into_any_element()
            } else {
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .rounded_br_md()
                    .px_1()
                    .py_0p5()
                    .bg(gpui_kit::rgb(0xfef3c7))
                    .text_color(gpui_kit::rgb(0xb45309))
                    .text_xs()
                    .child("未読")
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

        let mut card_el = div()
            .id(SharedString::from(format!(
                "book-card-{}",
                shelf.database_id
            )))
            .w(px(card_width))
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_lg()
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
            // 表紙は中央寄せ（Web の mx-auto 相当）
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if is_read {
                        div().opacity(0.75).child(cover_el)
                    } else {
                        div().child(cover_el)
                    }),
            )
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
                        ("booth", Some(date)) | ("fanza", Some(date)) | ("dlsite", Some(date)) => format!("購入日: {date}"),
                        _ => event_text.clone(),
                    }),
            )
            // サークル名（非空のときだけ表示）
            .child(if !circle_name.is_empty() {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("サークル: {circle_name}"))
                    .into_any_element()
            } else {
                div().into_any_element()
            })
            // 作者名（BOOTH 等。空でなければ表示。技術書典は author が空なので出ない）
            .child(if !author.is_empty() {
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format!("作者: {author}"))
                    .into_any_element()
            } else {
                div().into_any_element()
            })
            .child(match progress_text.as_deref() {
                Some(text) => div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(text.to_string())
                    .into_any_element(),
                None => div().into_any_element(),
            });

        // タグ行: 編集中なら Web の TagsInput 風エディタを表示
        card_el = card_el.child(if editing {
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
            div()
                .id(SharedString::from(format!("tag-row-{database_id}")))
                .flex()
                .flex_row()
                .flex_wrap()
                .gap_1()
                .items_center()
                .cursor_pointer()
                .on_click(|_, _, cx| cx.stop_propagation())
                .children(BookshelfView::render_tag_chips(
                    theme,
                    &handle,
                    favorite_tags,
                    card,
                ))
                .child(BookshelfView::render_tag_edit_button(
                    window,
                    theme,
                    &handle,
                    &database_id,
                ))
                .into_any_element()
        });

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

    /// タグチップ行（Web の TagList 相当: お気に入りは ♥ + ピンク）。
    fn render_tag_chips(
        theme: &gpui_kit::component::Theme,
        handle: &gpui_kit::Entity<BookshelfView>,
        favorite_tags: &[String],
        card: &ShelfCard,
    ) -> Vec<gpui_kit::AnyElement> {
        let shelf = &card.shelf;
        let tags = card.tags.clone();
        let favorite_tags = favorite_tags.to_vec();
        let handle = handle.clone();
        tags.into_iter()
            .map(|tag| {
                let is_favorite = favorite_tags.contains(&tag);
                let handle = handle.clone();
                let tag_id = tag.clone();
                let chip_selector = format!("tag-chip-{}-{}", shelf.database_id, tag);
                div()
                    .id(SharedString::from(format!(
                        "tag-chip-{}-{}",
                        shelf.database_id, tag
                    )))
                    .debug_selector(move || chip_selector.clone())
                    .flex()
                    .flex_row()
                    .items_center()
                    .gap_0p5()
                    .px_1()
                    .py_0p5()
                    .rounded_full()
                    .bg(if is_favorite {
                        gpui_kit::rgb(0xfbcfe8).into()
                    } else {
                        theme.muted
                    })
                    .text_color(if is_favorite {
                        gpui_kit::rgb(0x9d174d).into()
                    } else {
                        theme.muted_foreground
                    })
                    .text_xs()
                    .child(
                        div()
                            .text_color(if is_favorite {
                                gpui_kit::rgb(0xbe185d).into()
                            } else {
                                theme.muted_foreground
                            })
                            .child(if is_favorite { "♥" } else { "♡" }),
                    )
                    .child(div().child(tag))
                    .cursor_pointer()
                    .on_click(move |_event, _window, cx| {
                        cx.stop_propagation();
                        handle.update(cx, |this, cx| {
                            this.toggle_favorite_tag(cx, &tag_id);
                        });
                    })
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
                                    div().debug_selector(|| "tag-edit-refetch-btn".into()).child(
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
                    )
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
                            let suggestion_selector =
                                format!("edit-suggestion-{suggestion_id}");
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

    /// リスト表示の 1 行（Web の table 行相当: サムネイル + タイトル + イベント + 進捗 + タグ）。
    fn render_list_row(
        &self,
        window: &mut Window,
        cx: &mut Context<Self>,
        card: &ShelfCard,
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
        let purchase_date = shelf
            .caused_at
            .as_deref()
            .map(format_purchase_date);
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
        let _is_favorite = shelf.is_favorite == 1;
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

        // 表紙（Web の h-24 w-20 = 80x96 相当）
        let image: gpui_kit::AnyElement = match &cover {
            Some(render) => div()
                .w(px(64.0))
                .h(px(90.0))
                .overflow_hidden()
                .child(
                    img(render.clone())
                        .w_full()
                        .h_full()
                        .object_fit(gpui_kit::ObjectFit::Cover),
                )
                .into_any_element(),
            None => div()
                .w(px(64.0))
                .h(px(90.0))
                .bg(cx.theme().muted)
                .into_any_element(),
        };

        let mut cover_el = div()
            .relative()
            .w(px(64.0))
            .h(px(90.0))
            .child(image)
            .child(if is_read {
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .rounded_br_md()
                    .px_1()
                    .py_0p5()
                    .bg(gpui_kit::rgb(0xd1fae5))
                    .text_color(gpui_kit::rgb(0x047857))
                    .text_xs()
                    .child("読了")
                    .into_any_element()
            } else {
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .rounded_br_md()
                    .px_1()
                    .py_0p5()
                    .bg(gpui_kit::rgb(0xfef3c7))
                    .text_color(gpui_kit::rgb(0xb45309))
                    .text_xs()
                    .child("未読")
                    .into_any_element()
            })
            .child(if is_downloaded {
                div()
                    .absolute()
                    .right_0p5()
                    .bottom_0p5()
                    .rounded_full()
                    .w(px(18.0))
                    .h(px(18.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui_kit::rgba(0x05966933))
                    .child(
                        div()
                            .text_color(gpui_kit::rgb(0x059669))
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::BOLD)
                            .child("✓"),
                    )
                    .into_any_element()
            } else {
                div()
                    .absolute()
                    .right_0p5()
                    .bottom_0p5()
                    .rounded_full()
                    .w(px(18.0))
                    .h(px(18.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(gpui_kit::rgba(0x00000033))
                    .child(
                        div()
                            .text_color(gpui_kit::white())
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::BOLD)
                            .child("↓"),
                    )
                    .into_any_element()
            });

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

        let mut row = div()
            .id(SharedString::from(format!("book-list-{database_id}")))
            .flex()
            .flex_row()
            .gap_3()
            .p_2()
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

        row = row.child(cover_el).child(
            div()
                .flex()
                .flex_col()
                .gap_1()
                .min_w_0()
                .flex_1()
                .child(
                    div()
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
                // サークル名（非空のときだけ表示）
                .child(if !circle_name.is_empty() {
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("サークル: {circle_name}"))
                        .into_any_element()
                } else {
                    div().into_any_element()
                })
                // 作者名（BOOTH 等。空でなければ表示。技術書典は author が空なので出ない）
                .child(if !author.is_empty() {
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(format!("作者: {author}"))
                        .into_any_element()
                } else {
                    div().into_any_element()
                })
                .child(match progress_text.as_deref() {
                    Some(text) => div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(text.to_string())
                        .into_any_element(),
                    None => div().into_any_element(),
                })
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
                        div()
                            .id(SharedString::from(format!("tag-row-{database_id}")))
                            .flex()
                            .flex_row()
                            .flex_wrap()
                            .gap_1()
                            .items_center()
                            .cursor_pointer()
                            .on_click(|_, _, cx| cx.stop_propagation())
                            .children(BookshelfView::render_tag_chips(
                                cx.theme(),
                                &handle,
                                &self.favorite_tags,
                                card,
                            ))
                            .child(BookshelfView::render_tag_edit_button(
                                window,
                                cx.theme(),
                                &handle,
                                &database_id,
                            ))
                            .into_any_element()
                    },
                ),
        );

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
        let favorite_tags = {
            let favs = self.favorite_tags.clone();
            match self.site_filter.as_deref() {
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
            }
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
        let handle = cx.entity();

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
                                {
                                    let mut button = Button::new("filter-all").cursor_pointer().label("全項目");
                                    if read_filter == ReadFilter::All {
                                        button = button.primary();
                                    }
                                    button
                                }.cursor_pointer().on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        handle.update(cx, |this, cx| {
                                            this.set_read_filter(cx, ReadFilter::All)
                                        });
                                    }
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
                            let content_width = window_width - 255.0 - 24.0;
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
                                                let (cards, favorite_tags, editing_tags, editing_suggestions, editing_input) = {
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
                                                            (card, ds, editing, selected)
                                                        })
                                                        .collect::<Vec<_>>();
                                                    (
                                                        cards,
                                                        view.favorite_tags.clone(),
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
                                                        |(card, ds, editing, selected)| {
                                                            BookshelfView::render_card(
                                                                window,
                                                                &theme,
                                                                &handle,
                                                                card,
                                                                card_width,
                                                                *ds,
                                                                *editing,
                                                                &favorite_tags,
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
                              .flex()
                              .flex_col()
                              .rounded_lg()
                              .border_1()
                              .border_color(cx.theme().border)
                              .overflow_hidden()
                              .overflow_y_scroll()
                              .track_scroll(&self.scroll_handle)
                              .children(visible.iter().enumerate().map(|(idx, entry)| {
                                let selected = self.selected_index == Some(idx);
                                self.render_list_row(window, cx, entry, selected).into_any_element()
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
    }
}

/// Resolve a remote/cached cover for a bookshelf item to a RenderImage.
/// GPUI の RenderImage は BGRA を期待するため、RGBA から R/B を入れ替える。
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
        img.resize(nw, nh, image::imageops::FilterType::Triangle)
    } else {
        img
    };
    let mut out = std::io::Cursor::new(Vec::new());
    resized.write_to(&mut out, image::ImageFormat::Png).ok()?;
    Some(out.into_inner())
}

/// 画像を縮小（最大幅 max_width px）して BGRA の RenderImage に変換する。
/// 元画像のアスペクト比を保つ（クロップしない）。カード側で object_fit(Contain) により
/// はみ出さず比率のまま表示する（FANZA 等はサムネの比率がバラバラのため）。
fn decode_and_resize(data: &[u8], max_width: u32) -> Option<Arc<RenderImage>> {
    let decoded = image::load_from_memory(data).ok()?;
    let (w, h) = (decoded.width(), decoded.height());
    let resized = if w > max_width {
        let scale = max_width as f32 / w as f32;
        let nw = (w as f32 * scale).max(1.0) as u32;
        let nh = (h as f32 * scale).max(1.0) as u32;
        decoded.resize(nw, nh, image::imageops::FilterType::Triangle)
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
    decode_and_resize(data, 288)
}

/// `thumbnails/{site_id}_{database_id}.{ext}` cache file -> RenderImage.
fn load_cached_cover(
    thumbnails_dir: &std::path::Path,
    shelf: &bookshelf::BookshelfItem,
) -> Option<Arc<RenderImage>> {
    for ext in ["png", "jpg", "jpeg", "webp"] {
        let path = thumbnails_dir.join(format!("{}_{}.{ext}", shelf.site_id, shelf.database_id));
        if let Ok(data) = std::fs::read(&path)
            && let Some(image) = decode_bytes_to_render_image(&data)
        {
            return Some(image);
        }
    }
    None
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
        Some(Arc::new(gpui_kit::RenderImage::new([image::Frame::new(rgba)])))
    });
    LOGO.clone()
}

/// 表紙画像が取得できなかったカード用の NoImage ダミー（グレー背景 + NoImage 表記）。
fn no_image_cover() -> Option<Arc<RenderImage>> {
    let svg = "<svg xmlns='http://www.w3.org/2000/svg' viewBox='0 0 240 320' width='240' height='320'><rect width='240' height='320' fill='#e5e7eb'/><rect x='60' y='90' width='120' height='90' fill='none' stroke='#9ca3af' stroke-width='6'/><path d='M70 165 L100 135 L125 155 L150 130 L170 165 Z' fill='#9ca3af'/><text x='120' y='200' text-anchor='middle' font-family='sans-serif' font-size='16' font-weight='bold' fill='#6b7280'>NoImage</text></svg>";
    decode_bytes_to_render_image(svg.as_bytes())
}

fn placeholder_cover(title: &str, circle: &str) -> Option<Arc<RenderImage>> {
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
fn format_event_label(event_name: &str) -> String {
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

fn load_cover_image(packs_dir: &std::path::Path, book: &books::Book) -> Option<Arc<RenderImage>> {
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
fn format_purchase_date(raw: &str) -> String {
    let s = raw.trim();
    if s.contains('年') {
        let y = s.split('年').next().unwrap_or("");
        let m = s.split('年').nth(1).and_then(|p| p.split('月').next()).unwrap_or("");
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
    use gpui_kit::AppContext as _;
    use gpui_kit::TestAppContext;

    use thundoku_core::db::{books, progress};

    use super::*;

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
        // タグチップの ♥ をクリック → お気に入り登録される
        let chip = visual
            .debug_bounds("tag-chip-db-1-後で読む")
            .expect("tag chip rendered");
        visual.simulate_click(chip.center(), gpui_kit::Modifiers::default());
        assert!(
            view.read_with(cx, |this, _| this
                .favorite_tags
                .contains(&"後で読む".to_string())),
            "heart click must add the tag to favorites"
        );
        // DB にも保存される
        let stored = cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            db::tags::list_favorites(db).unwrap_or_default()
        });
        assert_eq!(stored, vec!["後で読む".to_string()]);
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
