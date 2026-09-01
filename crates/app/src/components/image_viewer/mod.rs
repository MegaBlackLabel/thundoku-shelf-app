//! 画像ビューアー: Web 版 `thundoku-viewer` の仕様（packages-viewer.md）を
//! 参照した、アプリ内独立コンポーネント。データ取得は `PageLoader` 注入。

use std::sync::Arc;
use std::time::Duration;

use gpui::{
    AppContext as _, InteractiveElement as _, ReadGlobal as _, ScrollHandle,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Subscription,
    prelude::FluentBuilder as _,
};
use gpui::{
    Context, Entity, FocusHandle, IntoElement, KeyDownEvent, ParentElement, Render, RenderImage,
    SharedString, Window, div, img, px,
};
use gpui_base::{Transition, transition};
use gpui_component::animation::ease_out_cubic;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::input::{Input, InputState};
use gpui_component::kbd::Kbd;
use gpui_component::scroll::ScrollableElement as _;
use gpui_component::slider::{Slider, SliderState};
use gpui_component::{ActiveTheme as _, Disableable as _};
use gpui_component::{Icon, IconName};
use thundoku_core::db;

use crate::actions::CloseReader;
use thundoku_core::db::documents::DocumentImage;

pub const AUTOPLAY_MIN_MS: u64 = 3000;
pub const AUTOPLAY_MAX_MS: u64 = 30000;
pub const AUTOPLAY_DEFAULT_MS: u64 = 5000;
pub const AUTOPLAY_STEP_MS: u64 = 1000;
 const OVERLAY_HIDE_MS: u64 = 5000;

/// ウィンドウカスタムタイトルバーの高さ（px）。リーダーはタイトルバーを残すため、
/// 画像のフィット計算でウィンドウ全体の高さから差し引く（Windows のみ。Mac は 0）。
#[cfg(windows)]
const WIN_TITLE_BAR_HEIGHT: f32 = 36.0;
#[cfg(not(windows))]
const WIN_TITLE_BAR_HEIGHT: f32 = 0.0;

/// Page data source (pack entries or base64 sample pages).
pub trait PageLoader: Send + Sync + 'static {
    fn page_count(&self) -> usize;
    fn page_size(&self, index: usize) -> Option<(u32, u32)>;
    /// Blocking decode; called on the background executor.
    fn load(&self, index: usize) -> Result<Arc<RenderImage>, String>;
}

/// Loads pages from a book's `.opfspack` (via `document_images`).
pub struct PackPageLoader {
    pub images: Vec<DocumentImage>,
    pub packs_dir: std::path::PathBuf,
    pub db: thundoku_core::db::SqlitePool,
    pub identity: Option<opfspack::Identity>,
    /// pack バイト列のキャッシュ（pack_id + バイト列。初回 load 時に 1 回だけ
    /// ディスクから読み込む。ページをめくるたびに数十 MB の pack を読み直すのを防ぐ）。
    /// OnceLock なので並列アクセスでも初期化は 1 回だけ。
    pub pack_bytes: std::sync::OnceLock<Option<(String, Arc<Vec<u8>>)>>,
    /// 導出済み pack キー（identity 復号用。PBKDF2 100k 回は一度だけ実行する）。
    pub pack_key: std::sync::OnceLock<Option<[u8; 32]>>,
}

impl PageLoader for PackPageLoader {
    fn page_count(&self) -> usize {
        self.images.len()
    }

    fn page_size(&self, index: usize) -> Option<(u32, u32)> {
        self.images
            .get(index)
            .map(|image| (image.width as u32, image.height as u32))
    }

    fn load(&self, index: usize) -> Result<Arc<RenderImage>, String> {
        let load_start = std::time::Instant::now();
        let image = self.images.get(index).ok_or("page out of range")?;
        let pack_entry = image.pack_entry_path.as_deref().ok_or("no pack entry")?;
        // pack バイト列は一度だけディスクから読み込む（OnceLock で並列でも 1 回）。
        // get_or_init が返った時点でロックは解放済みなので、以降の復号
        // （PBKDF2 100k イテレーション）はロックを保持せずに実行できる。
        let cached = self.pack_bytes.get_or_init(|| {
            let db = &self.db;
            let book = db::books::get(db, &book_id_of(image, db)).ok()??;
            let pack_id = book.pack_id.as_deref().unwrap_or(&book.id).to_string();
            let path = self.packs_dir.join(format!("{pack_id}.opfspack"));
            let bytes = std::fs::read(path).ok()?;
            Some((pack_id, Arc::new(bytes)))
        });
        let (_, bytes) = cached.as_ref().ok_or("pack load failed")?;
        let reader = opfspack::PackReader::open(bytes).map_err(|e| e.to_string())?;
        // pack キーは一度だけ導出（PBKDF2 100k 回はページごとに実行しない）
        let key = self
            .pack_key
            .get_or_init(|| self.identity.as_ref().map(opfspack::derived_pack_key));
        let data = reader
            .read_entry_with_key(pack_entry, key.as_ref())
            .map_err(|e| e.to_string())?;
        let read_done = load_start.elapsed();
        let image = decode_render_image(&data)?;
        log::info!(
            "page load: index={index} bytes={} read={:.1}ms decode={:.1}ms",
            data.len(),
            read_done.as_secs_f64() * 1000.0,
            load_start.elapsed().as_secs_f64() * 1000.0
        );
        Ok(image)
    }
}

fn book_id_of(image: &DocumentImage, db: &thundoku_core::db::SqlitePool) -> String {
    db::documents::get_document(db, &image.document_id)
        .ok()
        .flatten()
        .map(|document| document.book_id)
        .unwrap_or_default()
}

/// Loads in-memory base64 sample pages (技術書典 試し読み).
pub struct Base64PageLoader {
    pub pages: Vec<(String, u32, u32)>, // (base64 data, width, height)
}

impl PageLoader for Base64PageLoader {
    fn page_count(&self) -> usize {
        self.pages.len()
    }

    fn page_size(&self, index: usize) -> Option<(u32, u32)> {
        self.pages.get(index).map(|(_, w, h)| (*w, *h))
    }

    fn load(&self, index: usize) -> Result<Arc<RenderImage>, String> {
        let (base64, _, _) = self.pages.get(index).ok_or("page out of range")?;
        use base64::Engine;
        let data = base64::engine::general_purpose::STANDARD
            .decode(base64)
            .map_err(|e| e.to_string())?;
        decode_render_image(&data)
    }
}

/// ビューアー背景: 真っ白だと本の白いページと区別できないため薄グレー。
fn viewer_bg() -> gpui::Hsla {
    gpui::rgb(0xf4f4f5).into()
}

/// Web 版の `rounded-[1.25rem]`（20px）相当の角丸を設定する。
fn rounded_web<E: gpui::Styled>(mut element: E) -> E {
    element.style().corner_radii.top_left = Some(gpui::AbsoluteLength::Pixels(px(20.0)));
    element.style().corner_radii.top_right = Some(gpui::AbsoluteLength::Pixels(px(20.0)));
    element.style().corner_radii.bottom_left = Some(gpui::AbsoluteLength::Pixels(px(20.0)));
    element.style().corner_radii.bottom_right = Some(gpui::AbsoluteLength::Pixels(px(20.0)));
    element
}

/// 上端のみ 20px 角丸（ヘッダー用: ホバーで背景が変わっても角が丸いまま）。
fn rounded_top_web<E: gpui::Styled>(mut element: E) -> E {
    element.style().corner_radii.top_left = Some(gpui::AbsoluteLength::Pixels(px(20.0)));
    element.style().corner_radii.top_right = Some(gpui::AbsoluteLength::Pixels(px(20.0)));
    element
}

/// ヘッダーの角丸: メニューが開いてる時は上端のみ（ビューと繋がる）、
/// 閉じてる時は全角丸（パネル単体で表示されるため下端も丸くする）。
fn header_radius<E: gpui::Styled>(element: E, panel_open: bool) -> E {
    if panel_open {
        rounded_top_web(element)
    } else {
        rounded_web(element)
    }
}

fn decode_render_image(data: &[u8]) -> Result<Arc<RenderImage>, String> {
    // webp は libwebp（C 実装）でデコードする。image クレートの webp デコーダーは
    // 1000px で 1 秒超かかる（実測）ため、ページ送りを高速化するにはこちらが必須。
    if data.get(8..12) == Some(b"WEBP") {
        // libwebp を直接 RGBA でデコードする（webp crate は RGB でデコードして
        // RGBA 変換に数百 ms かかるため、その変換を丸ごと省く）。
        unsafe {
            let mut width: std::os::raw::c_int = 0;
            let mut height: std::os::raw::c_int = 0;
            if libwebp_sys::WebPGetInfo(data.as_ptr(), data.len(), &mut width, &mut height) == 0
                || width <= 0
                || height <= 0
            {
                return Err("webp info failed".into());
            }
            let ptr =
                libwebp_sys::WebPDecodeRGBA(data.as_ptr(), data.len(), &mut width, &mut height);
            if ptr.is_null() {
                return Err("webp decode failed".into());
            }
            let len = (width as usize) * (height as usize) * 4;
            let bytes = std::slice::from_raw_parts(ptr, len).to_vec();
            libwebp_sys::WebPFree(ptr as *mut std::os::raw::c_void);
            let image = image::RgbaImage::from_raw(width as u32, height as u32, bytes)
                .ok_or("webp rgba conversion failed")?;
            return Ok(rgba_to_render_image(image));
        }
    }
    let decoded = image::load_from_memory(data).map_err(|e| e.to_string())?;
    Ok(rgba_to_render_image(decoded.to_rgba8()))
}

/// ズーム時の可動範囲を計算する（画像をウィンドウに contain した表示サイズの
/// 拡大後とビューポートの差の半分。画像の隅まで動けるようになる）
fn pan_max_for(scale: f32, viewport: gpui::Point<f32>, aspect: f32) -> gpui::Point<f32> {
    // contain した表示サイズ
    let (iw, ih) = if viewport.x / viewport.y > aspect {
        (viewport.y * aspect, viewport.y)
    } else {
        (viewport.x, viewport.x / aspect)
    };
    gpui::Point::new(
        ((iw * scale - viewport.x) / 2.0).max(0.0),
        ((ih * scale - viewport.y) / 2.0).max(0.0),
    )
}

/// GPUI の `RenderImage` は **BGRA** を期待する（テクスチャは
/// BGRA8Unorm、標準デコーダーは `pixel.swap(0, 2)` で変換してから
/// `RenderImage::new` に渡す）。image crate は RGBA を返すため、
/// R/B を入れ替えてから渡さないと表示で赤と青が入れ替わる。
fn rgba_to_render_image(mut rgba: image::RgbaImage) -> Arc<RenderImage> {
    for pixel in rgba.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Arc::new(RenderImage::new([image::Frame::new(rgba)]))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Single,
    Spread,
    Scroll,
}

/// Web 版のトップパネルのビュー（image-book-panel の activePanelView 相当）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PanelView {
    Menu,
    PageList,
    Shortcuts,
    AutoplaySettings,
}

pub struct ImageViewer {
    pub loader: Arc<dyn PageLoader>,
    pub title: SharedString,
    /// 本のサイト（techbookfest / booth）。サイト別ビューアー設定のキーに使う
    site_id: Option<String>,
    current_page: usize,
    mode: ViewMode,
    images: Vec<Option<Arc<RenderImage>>>,
    overlay_visible: bool,
    /// トップパネルまたはボトムドックにマウスがある間は自動非表示しない。
    hovering_ui: bool,
    hide_generation: u64,
    /// 連続ページ送りの回数（2 回以上でトップ/ボトムメニューを非表示にする）
    page_turn_count: u32,
    autoplay: bool,
    autoplay_interval_ms: u64,
    autoplay_generation: u64,
    zoomed: bool,
    zoom_scale: f32,
    /// ズーム時のパン（ドラッグ移動）オフセット（ピクセル）
    pan_offset: gpui::Point<f32>,
    /// ドラッグ中の開始位置
    drag_start: Option<gpui::Point<f32>>,
    /// 直前のクリック時刻（400ms 以内の再クリックをダブルクリックと判定）
    last_click_at: Option<std::time::Instant>,
    /// パンの速度（慣性用。px/フレーム相当）
    pan_velocity: gpui::Point<f32>,
    /// パンの可動範囲（ビューポート × (scale-1) / 2。画像の隅まで動ける）
    pan_max: gpui::Point<f32>,
    /// 慣性ループの世代（ドラッグ開始で無効化する）
    inertia_generation: u64,
    /// ズームトグルのデバウンス（ダブルクリック 1 回で複数回呼ばれるのを防ぐ）
    last_zoom_toggle: Option<std::time::Instant>,
    active_panel: Option<PanelView>,
    page_input: Option<Entity<InputState>>,
    autoplay_slider: Option<Entity<SliderState>>,
    /// ページ移動用スライダー（ボトムドック）。
    page_slider: Option<Entity<SliderState>>,
    _page_slider_subscription: Option<Subscription>,
    /// スクロールモード: 初回レイアウトの top_item は未確定（最終ページ相当を
    /// 返すことがある）ため採用しないためのフラグ。
    scroll_top_initialized: bool,
    /// ロード中のページ（二重ロード防止）。
    loading: std::collections::HashSet<usize>,
    /// 自身のエンティティハンドル（render_page は &self のため）
    self_handle: Option<gpui::Entity<ImageViewer>>,
    focus_handle: FocusHandle,
    scroll_handle: ScrollHandle,
    /// ページめくり方向（Web の pageTurnDirection 相当）。true = 右→左（日本の本）
    page_turn_right_to_left: bool,
    /// スクロールモードで最後に反映した表示中ページ（render での
    /// ポーリングの再入防止用）
    last_scroll_page: usize,
}

impl ImageViewer {
    /// サイト別設定キー（viewer.mode.{site}）。site_id がなければ従来キー。
    fn setting_key(&self, base: &str) -> String {
        match &self.site_id {
            Some(site) => format!("{base}.{site}"),
            None => base.to_string(),
        }
    }

    pub fn new(
        cx: &mut Context<Self>,
        loader: Arc<dyn PageLoader>,
        title: impl Into<SharedString>,
        initial_page: usize,
        site_id: Option<String>,
    ) -> Self {
        let page_count = loader.page_count();
        let initial = initial_page.min(page_count.saturating_sub(1));
        // サイト別設定キー（viewer.mode.{site}）。サイト設定がなければ
        // 従来のグローバルキー（viewer.mode）にフォールバックする
        let viewer_key = |base: &str| match &site_id {
            Some(site) => format!("{base}.{site}"),
            None => base.to_string(),
        };
        // 前回選択した表示モードを復元する（Web 版は localStorage 相当）
        let saved_mode = {
            let state = crate::app_state::AppState::global(cx);
            let db = &state.db_pool;
            let mode = db::settings::get(db, &viewer_key("viewer.mode"))
                .ok()
                .flatten()
                .or_else(|| db::settings::get(db, "viewer.mode").ok().flatten());
            match mode.as_deref() {
                Some("spread") => ViewMode::Spread,
                Some("scroll") => ViewMode::Scroll,
                _ => ViewMode::Single,
            }
        };
        // ページめくり方向（設定ページの「ページめくり」から復元）
        // デフォルトは左綴じ（技術書典の本は左綴じ想定）。
        // 設定で明示的に right-to-left が選ばれている場合のみ右綴じ。
        let page_turn_right_to_left = {
            let state = crate::app_state::AppState::global(cx);
            let db = &state.db_pool;
            let direction = db::settings::get(db, &viewer_key("viewer.page_turn"))
                .ok()
                .flatten()
                .or_else(|| db::settings::get(db, "viewer.page_turn").ok().flatten());
            direction.as_deref() == Some("right-to-left")
        };
        // 自動再生の間隔（サイト別キー → グローバル → デフォルト）
        let saved_autoplay_interval = {
            let state = crate::app_state::AppState::global(cx);
            let db = &state.db_pool;
            db::settings::get(db, &viewer_key("viewer.autoplay_interval"))
                .ok()
                .flatten()
                .or_else(|| {
                    db::settings::get(db, "viewer.autoplay_interval")
                        .ok()
                        .flatten()
                })
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(AUTOPLAY_DEFAULT_MS)
        };
        let mut viewer = Self {
            loader,
            title: title.into(),
            site_id,
            current_page: initial,
            mode: saved_mode,
            page_turn_right_to_left,
            autoplay_interval_ms: saved_autoplay_interval,
            last_scroll_page: initial,
            images: (0..page_count).map(|_| None).collect(),
            overlay_visible: true,
            hovering_ui: false,
            hide_generation: 0,
            page_turn_count: 0,
            autoplay: false,
            autoplay_generation: 0,
            zoomed: false,
            zoom_scale: 1.5,
            pan_offset: gpui::Point::new(0.0, 0.0),
            drag_start: None,
            last_click_at: None,
            pan_velocity: gpui::Point::new(0.0, 0.0),
            pan_max: gpui::Point::new(0.0, 0.0),
            inertia_generation: 0,
            last_zoom_toggle: None,
            active_panel: None,
            page_input: None,
            autoplay_slider: None,
            page_slider: None,
            _page_slider_subscription: None,
            loading: std::collections::HashSet::new(),
            scroll_top_initialized: false,
            self_handle: None,
            focus_handle: cx.focus_handle(),
            scroll_handle: ScrollHandle::new(),
        };
        viewer.self_handle = Some(cx.entity());
        if saved_mode == ViewMode::Scroll {
            // scroll モードで開いた場合は全ページを先にロードする
            // （表示中に順次読み込む。現在ページだけだと
            // 「読み込み中のまま」になるため）
            for index in 0..viewer.images.len() {
                viewer.ensure_loaded(cx, index);
            }
            // 途中まで読んでいた場合はそのページまでスクロールを復元する
            // （scroll_to_item は prepaint で適用される）
            viewer.scroll_handle.scroll_to_item(viewer.current_page);
        } else {
            // 現在ページと隣接ページを同時にロードする。見開きモードでは
            // 右ページも一緒に読むことで「開いた直後に片方が読込中」を防ぐ。
            viewer.ensure_loaded(cx, viewer.current_page);
            viewer.ensure_loaded(
                cx,
                (viewer.current_page + 1).min(viewer.loader.page_count().saturating_sub(1)),
            );
        }
        viewer
    }

    // -- accessors -----------------------------------------------------------

    pub fn current_page(&self) -> usize {
        self.current_page
    }

    pub fn page_count(&self) -> usize {
        self.loader.page_count()
    }

    pub fn mode(&self) -> ViewMode {
        self.mode
    }

    // -- page navigation ----------------------------------------------------

    fn clamp(&mut self) {
        let count = self.loader.page_count().max(1);
        let max = count - 1;
        if self.current_page > max {
            self.current_page = max;
        }
    }

    pub fn next_page(&mut self, cx: &mut Context<Self>) {
        self.navigate(cx, 1, false);
    }

    pub fn prev_page(&mut self, cx: &mut Context<Self>) {
        self.navigate(cx, -1, false);
    }

    /// Shift 押し: 見開きでも 1 ページだけ移動する（Web の
    /// resolveNavigationStep(shiftKey) 相当。見開きの位置調整用）。
    pub fn next_page_shift(&mut self, cx: &mut Context<Self>) {
        self.navigate(cx, 1, true);
    }

    pub fn prev_page_shift(&mut self, cx: &mut Context<Self>) {
        self.navigate(cx, -1, true);
    }

    fn navigate(&mut self, cx: &mut Context<Self>, direction: i64, shift: bool) {
        // 見開きは 2 ページ単位、Shift 押し（または単一/スクロール）は 1 ページ単位
        let step = if self.mode == ViewMode::Spread && !shift {
            2
        } else {
            1
        };
        let max = self.loader.page_count().saturating_sub(1);
        let target = (self.current_page as i64 + direction * step).clamp(0, max as i64) as usize;
        if target != self.current_page {
            self.current_page = target;
            // ページ送りが連続 2 回以上されたらトップ/ボトムメニューを非表示に
            // する（読書に集中させる。オーバーレイ表示操作でカウントはリセット）
            self.page_turn_count += 1;
            if self.page_turn_count >= 2 && self.overlay_visible {
                self.overlay_visible = false;
                self.hide_generation += 1;
            }
            self.after_page_change(cx);
        }
    }

    pub fn set_page(&mut self, cx: &mut Context<Self>, page: usize) {
        if page == self.current_page {
            return;
        }
        self.current_page = page;
        self.clamp();
        self.after_page_change(cx);
    }

    fn after_page_change(&mut self, cx: &mut Context<Self>) {
        self.clamp();
        // ページ遷移: 前のページの画像をクリアしてから現在ページをロードする。
        // 全スロットをクリアするとプリロード済みの隣接ページまで消えて
        // 読み込み待ちが増えるため、一度デコードしたページは直近 30 ページ
        // 保持して再デコードを避ける（ページを戻っても即表示される）。
        // スクロールモードは全ページを保持したままにする（クリアしない）。
        if self.mode != ViewMode::Scroll {
            for (index, slot) in self.images.iter_mut().enumerate() {
                if index.abs_diff(self.current_page) > 30 {
                    *slot = None;
                }
            }
        }
        self.ensure_loaded(cx, self.current_page);
        if self.mode != ViewMode::Scroll {
            let count = self.loader.page_count().saturating_sub(1);
            // 前後 2 ページを先読み（libwebp デコードが数十 ms になったため
            // 並列 5 ページでも CPU は飽和しない。これでページ送り時の
            // 一瞬の「読込中」表示も消える）
            for offset in 1..=2 {
                self.ensure_loaded(cx, self.current_page.saturating_sub(offset));
                self.ensure_loaded(cx, (self.current_page + offset).min(count));
            }
        }
        cx.notify();
    }

    /// The visible page index(es).
    pub fn visible_pages(&self) -> Vec<usize> {
        if self.mode == ViewMode::Scroll {
            (0..self.loader.page_count()).collect()
        } else {
            self.spread_pages()
        }
    }

    /// (left, right) page pair; right is `None` when out of range.
    pub fn spread_pages(&self) -> Vec<usize> {
        if self.mode == ViewMode::Spread {
            let mut pages = vec![self.current_page];
            if self.current_page + 1 < self.loader.page_count() {
                pages.push(self.current_page + 1);
            }
            // 右→左（日本の本）: ページ番号の小さい方が右側に来るよう反転
            if self.page_turn_right_to_left {
                pages.reverse();
            }
            pages
        } else {
            vec![self.current_page]
        }
    }

    fn ensure_loaded(&mut self, cx: &mut Context<Self>, index: usize) {
        if index >= self.images.len()
            || self.images[index].is_some()
            || self.loading.contains(&index)
        {
            return;
        }
        self.loading.insert(index);
        let loader = self.loader.clone();
        let handle = cx.entity();
        let task = cx
            .background_executor()
            .spawn(async move { loader.load(index) });
        cx.spawn(async move |_window, cx| {
            let result = task.await;
            handle.update(cx, |this, cx| {
                this.loading.remove(&index);
                if index < this.images.len() {
                    match result {
                        Ok(image) => this.images[index] = Some(image),
                        Err(_) => this.images[index] = None,
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    // -- modes --------------------------------------------------------------

    /// 見開きの綴じ方向（右綴じ = 右→左、左綴じ = 左→右）を切り替える。
    /// 設定にも永続化する（Web の setReaderBindingDirection 相当）。
    pub fn set_binding(&mut self, cx: &mut Context<Self>, right_to_left: bool) {
        self.page_turn_right_to_left = right_to_left;
        {
            let state = crate::app_state::AppState::global(cx);
            let db = &state.db_pool;
            let key = self.setting_key("viewer.page_turn");
            let _ = db::settings::set(
                db,
                &key,
                if right_to_left {
                    "right-to-left"
                } else {
                    "left-to-right"
                },
            );
        }
        cx.notify();
    }

    pub fn set_mode(&mut self, cx: &mut Context<Self>, mode: ViewMode) {
        self.mode = mode;
        // 選択した表示モードを永続化（次回起動時に復元）
        {
            let state = crate::app_state::AppState::global(cx);
            let db = &state.db_pool;
            let key_value = match mode {
                ViewMode::Single => "single",
                ViewMode::Spread => "spread",
                ViewMode::Scroll => "scroll",
            };
            let key = self.setting_key("viewer.mode");
            let _ = db::settings::set(db, &key, key_value);
        }
        if mode == ViewMode::Scroll {
            self.stop_autoplay(cx);
            let pending: Vec<usize> = self
                .images
                .iter()
                .enumerate()
                .filter(|(_, image)| image.is_none())
                .map(|(index, _)| index)
                .collect();
            for index in pending {
                self.ensure_loaded(cx, index);
            }
        } else {
            self.clamp();
            self.after_page_change(cx);
        }
        cx.notify();
    }

    // -- overlay ------------------------------------------------------------

    pub fn show_overlay(&mut self, cx: &mut Context<Self>) {
        self.overlay_visible = true;
        self.page_turn_count = 0;
        self.restart_hide_timer(cx);
        cx.notify();
    }

    pub fn hide_overlay(&mut self, cx: &mut Context<Self>) {
        self.overlay_visible = false;
        self.hide_generation += 1;
        cx.notify();
    }

    /// 中央ダブルクリックでトップ/ボトムメニューを表示・非表示切り替え
    /// （Web 版の `toggleOverlayFromMainSurface` 相当）。
    pub fn toggle_overlay(&mut self, cx: &mut Context<Self>) {
        if self.overlay_visible {
            self.hide_overlay(cx);
        } else {
            self.show_overlay(cx);
        }
    }

    fn restart_hide_timer(&mut self, cx: &mut Context<Self>) {
        self.hide_generation += 1;
        let generation = self.hide_generation;
        if self.active_panel.is_some() || self.hovering_ui {
            return;
        }
        let handle = cx.entity();
        cx.spawn(async move |_window, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(OVERLAY_HIDE_MS))
                .await;
            handle.update(cx, |this, cx| {
                if this.hide_generation == generation {
                    this.overlay_visible = false;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    // -- autoplay -----------------------------------------------------------

    pub fn toggle_autoplay(&mut self, cx: &mut Context<Self>) {
        if self.autoplay {
            self.stop_autoplay(cx);
        } else {
            self.start_autoplay(cx);
        }
    }

    fn start_autoplay(&mut self, cx: &mut Context<Self>) {
        if self.mode == ViewMode::Scroll {
            return;
        }
        self.autoplay = true;
        self.autoplay_generation += 1;
        let generation = self.autoplay_generation;
        let handle = cx.entity();
        let interval_ms = self.autoplay_interval_ms;
        cx.spawn(async move |_window, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(interval_ms))
                    .await;
                let done = handle.update(cx, |this, cx| {
                    if !this.autoplay || this.autoplay_generation != generation {
                        return true;
                    }
                    let last = this.loader.page_count().saturating_sub(1);
                    if this.current_page >= last {
                        this.autoplay = false;
                        cx.notify();
                        true
                    } else {
                        this.next_page(cx);
                        false
                    }
                });
                if done {
                    break;
                }
            }
        })
        .detach();
        cx.notify();
    }

    fn stop_autoplay(&mut self, cx: &mut Context<Self>) {
        self.autoplay = false;
        self.autoplay_generation += 1;
        cx.notify();
    }

    pub fn set_autoplay_interval(&mut self, cx: &mut Context<Self>, ms: u64) {
        self.autoplay_interval_ms = ms.clamp(AUTOPLAY_MIN_MS, AUTOPLAY_MAX_MS);
        // サイト別キーに保存する（他のビューアー設定と同じ規則。サイトが
        // 無い本はグローバルキー、既存のグローバル値も後方互換で更新）
        let key = self.setting_key("viewer.autoplay_interval");
        let state = crate::app_state::AppState::global(cx);
        let db = &state.db_pool;
        let value = self.autoplay_interval_ms.to_string();
        let _ = db::settings::set(db, &key, &value);
        let _ = db::settings::set(db, "viewer.autoplay_interval", &value);
        if self.autoplay {
            self.stop_autoplay(cx);
            self.start_autoplay(cx);
        }
    }

    // -- zoom ---------------------------------------------------------------

    /// ズーム調整（ホイール・ダブルクリック・ショートカット共用）。
    /// scale が 1.0 に戻るとズーム解除（パン位置もリセット）。
    pub fn adjust_zoom(&mut self, cx: &mut Context<Self>, delta: f32) {
        self.zoomed = true;
        self.zoom_scale = (self.zoom_scale + delta).clamp(1.0, 8.0);
        log::info!(
            "adjust_zoom: delta={delta} scale={} zoomed={}",
            self.zoom_scale,
            self.zoomed
        );
        if self.zoom_scale <= 1.0 {
            self.zoomed = false;
            self.pan_offset = gpui::Point::new(0.0, 0.0);
        }
        cx.notify();
    }

    /// ダブルクリック: 拡大トグル（1.5x で拡大 / 拡大中なら解除）
    /// ダブルクリックサイクル: 拡大 → さらに拡大 → 上限に達したら解除
    /// （1 回目: 拡大開始 / 2 回目以降: さらに拡大 / 上限で: 終了）
    /// 250ms 以内の再呼び出しは 1 回のダブルクリックとみなして無視する
    pub fn toggle_zoom(&mut self, cx: &mut Context<Self>) {
        if self
            .last_zoom_toggle
            .is_some_and(|t| t.elapsed().as_millis() < 250)
        {
            log::info!("toggle_zoom: debounced");
            return;
        }
        self.last_zoom_toggle = Some(std::time::Instant::now());
        if !self.zoomed {
            // 1 回目: 拡大開始（2 倍）
            self.zoomed = true;
            self.zoom_scale = 2.0;
        } else if self.zoom_scale < 5.9 {
            // 2 回目: さらに拡大（6 倍）
            self.zoom_scale = 6.0;
        } else {
            // 上限到達後のダブルクリックで拡大終了
            self.zoomed = false;
            self.zoom_scale = 1.5;
            self.pan_offset = gpui::Point::new(0.0, 0.0);
            self.pan_velocity = gpui::Point::new(0.0, 0.0);
        }
        log::info!(
            "toggle_zoom: zoomed={} scale={}",
            self.zoomed,
            self.zoom_scale
        );
        cx.notify();
    }

    /// ドラッグ開始（拡大中のパン用。前の慣性を止める）
    pub fn start_pan(&mut self, position: gpui::Point<f32>) {
        if self.zoomed {
            self.drag_start = Some(position);
            self.pan_velocity = gpui::Point::new(0.0, 0.0);
            self.inertia_generation += 1;
        }
    }

    /// ドラッグ移動（拡大中のパン。画像の隅まで動けるクランプ付き + 速度記録）
    pub fn update_pan(&mut self, position: gpui::Point<f32>, pan_max: gpui::Point<f32>) {
        if self.zoomed {
            // 可動範囲は呼び出し側で「拡大後の実際の画像サイズ」から計算する
            // （画像の隅まで動けるようにする）
            self.pan_max = pan_max;
            if let Some(start) = self.drag_start {
                let dx = position.x - start.x;
                let dy = position.y - start.y;
                let max = self.pan_max;
                // 目標位置へ補間して移動する（移動開始時のジャンプを滑らかに）
                let target = gpui::Point::new(
                    (self.pan_offset.x + dx).clamp(-max.x, max.x),
                    (self.pan_offset.y + dy).clamp(-max.y, max.y),
                );
                self.pan_offset = gpui::Point::new(
                    self.pan_offset.x + (target.x - self.pan_offset.x) * 0.6,
                    self.pan_offset.y + (target.y - self.pan_offset.y) * 0.6,
                );
                // 慣性用の速度（1 イベントあたりの移動量を 16ms 換算で記録）
                self.pan_velocity = gpui::Point::new(
                    (dx / 0.016).clamp(-4000.0, 4000.0),
                    (dy / 0.016).clamp(-4000.0, 4000.0),
                );
            }
            self.drag_start = Some(position);
        }
    }

    /// ドラッグ終了: 慣性で減速しながら移動を続ける（Google マップ風）
    pub fn end_pan(&mut self, cx: &mut Context<Self>) {
        self.drag_start = None;
        let velocity = self.pan_velocity;
        if velocity.x.abs() < 0.5 && velocity.y.abs() < 0.5 {
            self.pan_velocity = gpui::Point::new(0.0, 0.0);
            return;
        }
        self.inertia_generation += 1;
        let generation = self.inertia_generation;
        let handle = cx.entity();
        cx.spawn(async move |_window, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(16))
                    .await;
                let stop = handle.update(cx, |this, cx| {
                    if this.inertia_generation != generation {
                        return true;
                    }
                    let v = this.pan_velocity;
                    let max = this.pan_max;
                    this.pan_offset = gpui::Point::new(
                        (this.pan_offset.x + v.x * 0.016).clamp(-max.x, max.x),
                        (this.pan_offset.y + v.y * 0.016).clamp(-max.y, max.y),
                    );
                    this.pan_velocity = gpui::Point::new(v.x * 0.94, v.y * 0.94);
                    cx.notify();
                    this.pan_velocity.x.abs() < 0.2 && this.pan_velocity.y.abs() < 0.2
                });
                if stop {
                    break;
                }
            }
        })
        .detach();
    }

    pub fn is_dock_hidden(&self) -> bool {
        self.zoomed
    }

    // -- panels -------------------------------------------------------------

    /// ヘッダー（メニュートリガー）クリック: メニューを開く / パネルを閉じる
    /// （Web 版のトリガー: `activePanelView !== null ? closePanel() : openPanel('menu')`）。
    fn toggle_panel(&mut self, cx: &mut Context<Self>) {
        if self.active_panel.is_some() {
            self.active_panel = None;
        } else {
            self.active_panel = Some(PanelView::Menu);
        }
        self.restart_hide_timer(cx);
        cx.notify();
    }

    /// パネルビューを開く（Web 版の `openPanel(view)` 相当）。
    fn open_panel(&mut self, cx: &mut Context<Self>, view: PanelView) {
        self.active_panel = Some(view);
        if view == PanelView::PageList {
            // Web 版はサムネイルを遅延ロード。デスクトップは images キャッシュを
            // 使うため、開いた時点で全ページのサムネイル読み込みを開始する。
            let count = self.loader.page_count();
            for index in 0..count {
                self.ensure_loaded(cx, index);
            }
        }
        self.restart_hide_timer(cx);
        cx.notify();
    }

    /// パネルを閉じる（メニュービューに戻すための「戻る」用は
    /// `open_panel(Menu)` を呼ぶ）。
    pub fn close_panel(&mut self, cx: &mut Context<Self>) {
        self.active_panel = None;
        cx.notify();
    }

    /// パネルビューの「戻る」ボタン（メニュービューに戻る、Web 版の戻る相当）。
    fn panel_back(&self, handle: &Entity<ImageViewer>, cx: &mut Context<Self>) -> impl IntoElement {
        let handle = handle.clone();
        div()
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .justify_center()
            .py_2()
            .border_t_1()
            .border_color(cx.theme().muted)
            .child(
                Button::new("viewer-panel-back")
                    .cursor_pointer()
                    .label("← メニューに戻る")
                    .cursor_pointer()
                    .on_click(move |_, _window, cx| {
                        handle.update(cx, |this, cx| this.open_panel(cx, PanelView::Menu));
                    }),
            )
    }

    fn ensure_panel_states(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.page_input.is_none() {
            self.page_input = Some(cx.new(|cx| InputState::new(window, cx)));
        }
        if self.autoplay_slider.is_none() {
            // `max` はデフォルト 100 のため、先に `max` を設定してから `min` を
            // 設定する（`min` 設定時に value が clamp され、min > max で panic する）。
            let slider = cx.new(|_| {
                SliderState::new()
                    .max(AUTOPLAY_MAX_MS as f32)
                    .min(AUTOPLAY_MIN_MS as f32)
                    .step(AUTOPLAY_STEP_MS as f32)
                    .default_value(AUTOPLAY_DEFAULT_MS as f32)
            });
            self.autoplay_slider = Some(slider);
        }
        if self.page_slider.is_none() {
            // ページ移動スライダー（0..page_count-1、1 ページ刻み）
            let count = self.loader.page_count().max(1);
            let slider = cx.new(|_| {
                SliderState::new()
                    .min(0.0)
                    .max((count - 1) as f32)
                    .step(1.0)
                    .default_value(self.current_page as f32)
            });
            self._page_slider_subscription = Some(cx.observe(&slider, |this, slider, cx| {
                let value = slider.read(cx).value();
                let page = value.start().round() as usize;
                if page != this.current_page {
                    this.set_page(cx, page);
                }
            }));
            self.page_slider = Some(slider);
        } else if let Some(slider) = &self.page_slider {
            // 現在ページをスライダーに反映（スライダー操作とのループは set_page の
            // 同一ページガードで防ぐ）
            let current = self.current_page as f32;
            let value = slider.read(cx).value();
            if (value.start() - current).abs() > 0.5 {
                slider.update(cx, |state, cx| {
                    state.set_value(current, window, cx);
                });
            }
        }
    }

    fn handle_key(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        log::info!("handle_key: key={}", event.keystroke.key.as_str());
        let shift = event.keystroke.modifiers.shift;
        // 左綴じ（ページ順 →）: ←=前へ / →=次へ。右綴じでは矢印を反転する
        let rtl = self.page_turn_right_to_left;
        match event.keystroke.key.as_str() {
            // 見開き: 上下の矢印の向きは綴じ方向で入れ替える
            "right" | "l" if shift => {
                if rtl {
                    self.prev_page_shift(cx);
                } else {
                    self.next_page_shift(cx);
                }
            }
            "right" | "l" => {
                if rtl {
                    self.prev_page(cx);
                } else {
                    self.next_page(cx);
                }
            }
            "left" | "h" if shift => {
                if rtl {
                    self.next_page_shift(cx);
                } else {
                    self.prev_page_shift(cx);
                }
            }
            "left" | "h" => {
                if rtl {
                    self.next_page(cx);
                } else {
                    self.prev_page(cx);
                }
            }
            // 本棚に戻る（↑ / k / Backspace）
            "up" | "k" | "backspace" => {
                window.dispatch_action(Box::new(crate::actions::CloseReader), cx);
            }
            // ESC: パネルが開いていれば閉じる、なければメニューを表示する（トグル）
            "escape" => {
                if self.active_panel.is_some() {
                    self.active_panel = None;
                } else {
                    self.active_panel = Some(PanelView::Menu);
                }
                cx.notify();
            }
            _ => {}
        }
    }

    fn render_page(
        &self,
        _muted_foreground: gpui::Hsla,
        index: usize,
        zoom_progress: f32,
    ) -> impl IntoElement {
        let image = self.images.get(index).cloned().flatten();
        match image {
            Some(image) => {
                let (width, height) = self.loader.page_size(index).unwrap_or((0, 0));
                if self.zoomed && width > 0 && height > 0 {
                    let scale = self.zoom_scale;
                    let pan = self.pan_offset;
                    log::info!(
                        "render_page: ZOOMED branch index={index} scale={scale} w={width} h={height}"
                    );
                    let handle = self.self_handle.clone().expect("viewer handle");
                    div()
                        .debug_selector(move || format!("viewer-page-{index}"))
                        .flex()
                        .items_center()
                        .justify_center()
                        .size_full()
                        .bg(viewer_bg())
                        .overflow_hidden()
                        // ダブルクリックで拡大トグル、拡大中はドラッグでパン
                        .on_mouse_down(gpui::MouseButton::Left, {
                            let handle = handle.clone();
                            move |event, _window, cx| {
                                let position = gpui::Point::new(
                                    event.position.x.as_f32(),
                                    event.position.y.as_f32(),
                                );
                                let now = std::time::Instant::now();
                                let is_double = handle
                                    .read(cx)
                                    .last_click_at
                                    .map(|t| now.duration_since(t).as_millis() < 400)
                                    .unwrap_or(false);
                                log::info!("viewer click: is_double={is_double}");
                                handle.update(cx, |this, cx| {
                                    this.last_click_at = Some(now);
                                    if is_double {
                                        // ダブルクリック = 拡大のみ（2 倍ずつ大きく）
                                        this.toggle_zoom(cx);
                                        this.last_click_at = None;
                                    } else {
                                        this.start_pan(position);
                                    }
                                });
                            }
                        })
                        .on_mouse_move({
                            let handle = handle.clone();
                            let pan_aspect = if width > 0 && height > 0 {
                                width as f32 / height as f32
                            } else {
                                100.0 / 141.0
                            };
                            let pan_scale = scale;
                            move |event, window, cx| {
                                // ドラッグ中（start_pan 済み）ならパン位置を更新して
                                // 毎フレーム再描画する（Google マップのように滑らかに）
                                let vwp = gpui::Point::new(
                                    window.bounds().size.width.as_f32(),
                                    window.bounds().size.height.as_f32() - WIN_TITLE_BAR_HEIGHT,
                                );
                                let max = pan_max_for(pan_scale, vwp, pan_aspect);
                                handle.update(cx, |this, cx| {
                                    if this.drag_start.is_some() {
                                        this.update_pan(
                                            gpui::Point::new(
                                                event.position.x.as_f32(),
                                                event.position.y.as_f32(),
                                            ),
                                            max,
                                        );
                                    }
                                    cx.notify();
                                });
                            }
                        })
                        .on_mouse_up(gpui::MouseButton::Left, {
                            let handle = handle.clone();
                            move |_, _window, cx| {
                                handle.update(cx, |this, cx| this.end_pan(cx));
                            }
                        })
                        .on_mouse_up_out(gpui::MouseButton::Left, {
                            let handle = handle.clone();
                            move |_, _window, cx| {
                                handle.update(cx, |this, cx| this.end_pan(cx));
                            }
                        })
                        .child(
                            img(image)
                                .id(gpui::ElementId::Name(SharedString::from(format!(
                                    "viewer-page-image-{index}"
                                ))))
                                // ズーム時は指定サイズで画像を描画する
                                // （object_fit: Fill で w/h 指定がそのまま表示サイズになる。
                                //   flex の shrink で幅が潰れないよう shrink_0 を付ける）
                                .object_fit(gpui::ObjectFit::Fill)
                                .flex_shrink_0()
                                .w(px(width as f32 * (1.0 + (scale - 1.0) * zoom_progress)))
                                .h(px(height as f32 * (1.0 + (scale - 1.0) * zoom_progress)))
                                // パン: 拡大画像を余白でずらして移動させる
                                .when(pan.x != 0.0 || pan.y != 0.0, |this| {
                                    this.ml(px(pan.x)).mt(px(pan.y))
                                }),
                        )
                        .into_any_element()
                } else {
                    // 通常表示はコンテナにフィット（Web の object-contain 相当）。
                    // 背景は白（ダークモードでもページ背景は白）。
                    let handle = self.self_handle.clone().expect("viewer handle");
                    div()
                        .debug_selector(move || format!("viewer-page-{index}"))
                        .flex()
                        .items_center()
                        .justify_center()
                        .size_full()
                        .bg(viewer_bg())
                        .overflow_hidden()
                        // ダブルクリックで拡大（通常時でも効くようにここにも付ける）
                        .on_mouse_down(gpui::MouseButton::Left, {
                            let handle = handle.clone();
                            move |_event, _window, cx| {
                                let now = std::time::Instant::now();
                                let is_double = handle
                                    .read(cx)
                                    .last_click_at
                                    .map(|t| now.duration_since(t).as_millis() < 400)
                                    .unwrap_or(false);
                                handle.update(cx, |this, cx| {
                                    this.last_click_at = Some(now);
                                    if is_double {
                                        this.toggle_zoom(cx);
                                        this.last_click_at = None;
                                    }
                                });
                            }
                        })
                        .child(
                            img(image)
                                .id(gpui::ElementId::Name(SharedString::from(format!(
                                    "viewer-page-image-{index}"
                                ))))
                                .size_full()
                                .object_fit(gpui::ObjectFit::Contain),
                        )
                        .into_any_element()
                }
            }
            None => div()
                .size_full()
                .bg(viewer_bg())
                .flex()
                .items_center()
                .justify_center()
                // 開いた直後などの読み込み待ちはスピナーを表示して
                // 「止まってる感じ」を出さない（数十 ms で画像に差し替わる）
                .child(gpui_component::spinner::Spinner::new())
                .into_any_element(),
        }
    }
}

impl Render for ImageViewer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_panel_states(window, cx);
        if !self.focus_handle.is_focused(window) {
            window.focus(&self.focus_handle, cx);
        }
        let total = self.loader.page_count();
        // スクロールモード: スクロール位置から表示中のページを計算して
        // ページ番号・スライダーを更新する。ホイールイベントはオーバーレイ
        // に遮られることがあるため、描画のたびに確認する確実な方法を使う
        // （top_item = 先頭に見えている子ページ。last_scroll_page で再入防止）。
        if self.mode == ViewMode::Scroll {
            // いま表示されているページ（先頭に見えている子）を
            // レイアウト情報から取得してページ番号を更新する。
            // overflow_y_scroll + track_scroll で ScrollHandle が
            // スクロール状態を受け取るようになったため top_item が動く。
            let top = self.scroll_handle.top_item();
            if !self.scroll_top_initialized {
                // 初回レイアウト直後は top_item が未確定（最終ページ相当を返すことが
                // ある）ため採用しない。2 回目以降のスクロール位置で正しく更新される。
                self.scroll_top_initialized = true;
            } else if top < total && top != self.last_scroll_page {
                log::info!("scroll page update: top={top} (total {total})");
                self.last_scroll_page = top;
                self.current_page = top;
            }
        }
        let current = self.current_page + 1;
        let mode = self.mode;
        let page_turn_right_to_left = self.page_turn_right_to_left;
        // 右綴じの見開きは右→左に読むため、前へ/次へボタンを入れ替える
        // （Web の mirrorFooterNav 相当）
        let mirror_nav = mode == ViewMode::Spread && page_turn_right_to_left;
        let overlay = self.overlay_visible && !self.zoomed;
        // Web 版のオーバーレイ・アニメーション再現:
        // パネルは opacity + translateY(-2rem → 0) を 600ms バウンス、
        // ドックは opacity + translateY(0.75rem → 0) を 180ms。
        // GPUI に transform スタイルが無いため top/bottom をアニメーションする。
        let overlay_progress = transition(
            ("viewer-overlay", "overlay"),
            if overlay { 1.0 } else { 0.0 },
            Transition::new(Duration::from_millis(400)).ease(ease_out_cubic),
            window,
            cx,
        );
        let autoplay = self.autoplay;
        let interval_ms = self.autoplay_interval_ms;
        let panel_open = self.active_panel.is_some();
        let active_panel = self.active_panel;
        // Web 版のパネル/ドックの背景: ライト = 白 80%、ダーク = 黒 70%（パネル）/ 60%（ドック）
        let panel_bg = if cx.theme().is_dark() {
            gpui::black().alpha(0.7)
        } else {
            gpui::white().alpha(0.8)
        };
        let dock_bg = if cx.theme().is_dark() {
            gpui::black().alpha(0.6)
        } else {
            gpui::white().alpha(0.8)
        };
        // ズームのアニメーション進捗（0→1。拡大/解除を 200ms で補間）
        let zoom_progress = transition(
            ("viewer-zoom", "zoom"),
            if self.zoomed { 1.0 } else { 0.0 },
            Transition::new(Duration::from_millis(200)).ease(ease_out_cubic),
            window,
            cx,
        );
        let handle = cx.entity();
        let page_input = self.page_input.clone().expect("page input");
        let autoplay_slider = self.autoplay_slider.clone().expect("autoplay slider");
        let page_slider = self.page_slider.clone().expect("page slider");
        let title = self.title.clone();
        let pages: Vec<usize> = if mode == ViewMode::Scroll {
            (0..total).collect()
        } else {
            self.spread_pages()
        };

        let muted = cx.theme().muted_foreground;
        let scroll_handle = self.scroll_handle.clone();
        // 前へ/次へボタン（mirror_nav で左右を入れ替えるための共有定義）
        // 呼び出しのたびに生成するクロージャ（when 内で複数回使うため）
        let prev_button = || {
            let handle = handle.clone();
            Button::new("viewer-prev")
                .cursor_pointer()
                .label("前へ")
                .disabled(current <= 1)
                .cursor_pointer()
                .on_click(move |event, _window, cx| {
                    handle.update(cx, |this, cx| {
                        // Shift 押しで 1 ページ（見開きの位置調整）
                        if event.modifiers().shift {
                            this.prev_page_shift(cx);
                        } else {
                            this.prev_page(cx);
                        }
                    });
                })
        };
        let next_button = || {
            let handle = handle.clone();
            Button::new("viewer-next")
                .cursor_pointer()
                .label("次へ")
                .disabled(current >= total)
                .cursor_pointer()
                .on_click(move |event, _window, cx| {
                    handle.update(cx, |this, cx| {
                        // Shift 押しで 1 ページ（見開きの位置調整）
                        if event.modifiers().shift {
                            this.next_page_shift(cx);
                        } else {
                            this.next_page(cx);
                        }
                    });
                })
        };
        let main = if mode == ViewMode::Scroll {
            let rendered: Vec<gpui::AnyElement> = pages
                .iter()
                .map(|index| {
                    // 各ページはコンテナ幅にフィットした高さ（アスペクト比）で
                    // 縦に連続させる。size_full だと flex が縮めて重なりに見える。
                    let (width, height) = self.loader.page_size(*index).unwrap_or((0, 0));
                    let aspect = if width > 0 && height > 0 {
                        width as f32 / height as f32
                    } else {
                        100.0 / 141.0
                    };
                    // 表示幅の 80% で中央寄せ（Web 版の scroll モード相当）
                    div()
                        .w_full()
                        .flex()
                        .justify_center()
                        .child(
                            div()
                                .w(gpui::Length::Definite(gpui::DefiniteLength::Fraction(0.8)))
                                .aspect_ratio(aspect)
                                .bg(viewer_bg())
                                .overflow_hidden()
                                .child(self.render_page(muted, *index, zoom_progress)),
                        )
                        .into_any_element()
                })
                .collect();
            div()
                .id("viewer-scroll")
                .size_full()
                .flex()
                .flex_col()
                .track_scroll(&scroll_handle)
                .overflow_y_scroll()
                .children(rendered)
                .into_any_element()
        } else if mode == ViewMode::Spread {
            // 見開き: 2 枚の画像をそれぞれのアスペクト比で隙間なく並べ、
            // 全体をウィンドウの中央に配置する（Web 版の見開きと同じ）。
            // アトミック表示: 両ページが揃うまでは表示しない（片方だけ先に
            // 出て後からもう片方、という 2 段階表示を防ぐ。数十 ms で揃う）
            let spread_ready = pages
                .iter()
                .all(|&index| self.images.get(index).is_some_and(|slot| slot.is_some()));
            // 各ページのサイズを明示してペア全体を 1 枚の画像のように扱う
            // （h_full / aspect_ratio は flex のレイアウトで縮みがちなため使わない）
            let vwp = gpui::Point::new(
                window.bounds().size.width.as_f32(),
                window.bounds().size.height.as_f32() - WIN_TITLE_BAR_HEIGHT,
            );
            let ch = vwp.y;
            let animated_scale = 1.0 + (self.zoom_scale - 1.0) * zoom_progress;
            // ペア（2 枚分）の合計アスペクト比
            let total_aspect: f32 = pages
                .iter()
                .map(|index| {
                    let (w, h) = self.loader.page_size(*index).unwrap_or((0, 0));
                    if w > 0 && h > 0 {
                        w as f32 / h as f32
                    } else {
                        100.0 / 141.0
                    }
                })
                .sum();
            // ペア全体をウィンドウにフィットさせる（高さ基準で幅が出るが、
            // 横が溢れる場合は幅基準に落とす）。これが表示上の基準サイズ
            let fit = (vwp.x / (ch * total_aspect)).min(1.0);
            let base_h = ch * fit;
            let rendered: Vec<gpui::AnyElement> = pages
                .iter()
                .map(|index| {
                    let (width, height) = self.loader.page_size(*index).unwrap_or((0, 0));
                    let aspect = if width > 0 && height > 0 {
                        width as f32 / height as f32
                    } else {
                        100.0 / 141.0
                    };
                    // ページボックスで包まず画像を直接並べる（見開きが
                    // 1 枚の画像として隙間なく表示される）
                    if spread_ready {
                        self.images[*index]
                            .clone()
                            .map(|image| {
                                img(image)
                                    .object_fit(gpui::ObjectFit::Fill)
                                    .flex_shrink_0()
                                    .w(px(base_h * aspect * animated_scale))
                                    .h(px(base_h * animated_scale))
                                    .into_any_element()
                            })
                            .unwrap_or_else(|| {
                                div()
                                    .w(px(base_h * aspect * animated_scale))
                                    .h(px(base_h * animated_scale))
                                    .bg(gpui::white())
                                    .into_any_element()
                            })
                    } else {
                        div()
                            .w(px(base_h * aspect * animated_scale))
                            .h(px(base_h * animated_scale))
                            .bg(gpui::white())
                            .into_any_element()
                    }
                })
                .collect();
            // 見開き全体を 1 枚の画像と見立てて拡大する（左右それぞれが
            // バラバラに拡大しないように、ペア全体を 1 つの単位でスケールする）
            let pan = self.pan_offset;
            // 見開き全体でズーム（ダブルクリック）とドラッグパンを受け付ける
            let handle = self.self_handle.clone().expect("viewer handle");
            // justify_center + margin パンはオーバーフロー方向と喧嘩して
            // 片側が表示されないため、absolute の left/top で中央から
            // パンする（画像の端まで確実に見られる）
            let pair_w = base_h * total_aspect * animated_scale;
            let pair_h = base_h * animated_scale;
            let pair_left = (vwp.x - pair_w) / 2.0 + pan.x;
            let pair_top = (vwp.y - pair_h) / 2.0 + pan.y;
            div()
                .size_full()
                .relative()
                .overflow_hidden()
                .on_mouse_down(gpui::MouseButton::Left, {
                    let handle = handle.clone();
                    move |event, _window, cx| {
                        let position =
                            gpui::Point::new(event.position.x.as_f32(), event.position.y.as_f32());
                        let now = std::time::Instant::now();
                        let is_double = handle
                            .read(cx)
                            .last_click_at
                            .map(|t| now.duration_since(t).as_millis() < 400)
                            .unwrap_or(false);
                        handle.update(cx, |this, cx| {
                            this.last_click_at = Some(now);
                            if is_double {
                                // 見開きはペア全体を拡大（2 倍ずつ）
                                this.toggle_zoom(cx);
                                this.last_click_at = None;
                            } else {
                                this.start_pan(position);
                            }
                        });
                    }
                })
                .on_mouse_move({
                    let handle = handle.clone();
                    move |event, window, cx| {
                        // 見開きペアの表示サイズ（幅 = コンテナ高さ × 合計アスペクト比）
                        // を拡大して可動範囲を計算する
                        let vwp = gpui::Point::new(
                            window.bounds().size.width.as_f32(),
                            window.bounds().size.height.as_f32() - WIN_TITLE_BAR_HEIGHT,
                        );
                        let max = gpui::Point::new(
                            ((base_h * total_aspect * animated_scale - vwp.x) / 2.0).max(0.0),
                            ((base_h * animated_scale - vwp.y) / 2.0).max(0.0),
                        );
                        handle.update(cx, |this, cx| {
                            if this.drag_start.is_some() {
                                this.update_pan(
                                    gpui::Point::new(
                                        event.position.x.as_f32(),
                                        event.position.y.as_f32(),
                                    ),
                                    max,
                                );
                            }
                            cx.notify();
                        });
                    }
                })
                .on_mouse_up(gpui::MouseButton::Left, {
                    let handle = handle.clone();
                    move |_, _window, cx| {
                        handle.update(cx, |this, cx| this.end_pan(cx));
                    }
                })
                .on_mouse_up_out(gpui::MouseButton::Left, {
                    let handle = handle.clone();
                    move |_, _window, cx| {
                        handle.update(cx, |this, cx| this.end_pan(cx));
                    }
                })
                .child(
                    div()
                        .absolute()
                        .left(px(pair_left))
                        .top(px(pair_top))
                        .flex()
                        .flex_row()
                        .gap(px(0.0))
                        .children(rendered),
                )
                .into_any_element()
        } else {
            // 単一モード: 左右 10% のクリックナビを画像の上に重ねる
            // （画像はコンテナに contain で収まるため、表示サイズを
            // アスペクト比から計算してエッジ領域を配置する）。
            // ズーム中はナビを出さない（Web の showEdgeNav と同じ）。
            let (pw, ph) = self.loader.page_size(self.current_page).unwrap_or((0, 0));
            let aspect = if pw > 0 && ph > 0 {
                pw as f32 / ph as f32
            } else {
                100.0 / 141.0
            };
            let win = window.bounds();
            let cw = win.size.width.as_f32();
            let ch = win.size.height.as_f32() - WIN_TITLE_BAR_HEIGHT;
            let img_w = if cw / ch > aspect { ch * aspect } else { cw };
            let img_h = if img_w > 0.0 { img_w / aspect } else { 0.0 };
            let img_left = ((cw - img_w) / 2.0).max(0.0);
            let img_top = ((ch - img_h) / 2.0).max(0.0);
            let edge_w = (img_w * 0.1).max(40.0);
            div()
                .size_full()
                .relative()
                .child(self.render_page(muted, self.current_page, zoom_progress))
                .when(!self.zoomed, |this| {
                    this.child(
                        div()
                            .id("viewer-edge-left")
                            .absolute()
                            .left(px(img_left))
                            .top(px(img_top))
                            .w(px(edge_w))
                            .h(px(img_h))
                            .cursor(gpui::CursorStyle::ResizeLeft)
                            .flex()
                            .items_center()
                            .justify_center()
                            // 非ホバーは透明、ホバーで半透明オーバーレイ + アイコン表示
                            .opacity(0.0)
                            .hover(|style| style.bg(gpui::rgba(0x0000001f)).opacity(1.0))
                            .child(
                                Icon::new(IconName::ChevronLeft)
                                    .size(px(28.0))
                                    .text_color(gpui::rgba(0x00000099)),
                            )
                            .on_mouse_down(gpui::MouseButton::Left, {
                                let handle = handle.clone();
                                move |event, _window, cx| {
                                    handle.update(cx, |this, cx| {
                                        if event.modifiers.shift {
                                            this.prev_page_shift(cx);
                                        } else {
                                            this.prev_page(cx);
                                        }
                                    });
                                }
                            }),
                    )
                    .child(
                        div()
                            .id("viewer-edge-right")
                            .absolute()
                            .left(px(img_left + img_w - edge_w))
                            .top(px(img_top))
                            .w(px(edge_w))
                            .h(px(img_h))
                            .cursor(gpui::CursorStyle::ResizeRight)
                            .flex()
                            .items_center()
                            .justify_center()
                            .opacity(0.0)
                            .hover(|style| style.bg(gpui::rgba(0x0000001f)).opacity(1.0))
                            .child(
                                Icon::new(IconName::ChevronRight)
                                    .size(px(28.0))
                                    .text_color(gpui::rgba(0x00000099)),
                            )
                            .on_mouse_down(gpui::MouseButton::Left, {
                                let handle = handle.clone();
                                move |event, _window, cx| {
                                    handle.update(cx, |this, cx| {
                                        if event.modifiers.shift {
                                            this.next_page_shift(cx);
                                        } else {
                                            this.next_page(cx);
                                        }
                                    });
                                }
                            }),
                    )
                })
                .into_any_element()
        };

        // キーボードショートカットを効かせるため、ルートにフォーカスを
        // 紐付けてキーイベントを受け取る。背景は本の白と区別できる薄グレー。
        div()
            .id("viewer-root")
            .size_full()
            .track_focus(&self.focus_handle)
            .on_key_down({
                let handle = handle.clone();
                move |event, window, cx| {
                    handle.update(cx, |this, cx| this.handle_key(event, window, cx));
                }
            })
            .bg(viewer_bg())
            .relative()
            .child(main)
            .child(
                div()
                    .absolute()
                    .top_0()
                    .right_0()
                    .bottom_0()
                    .left_0()
                    .on_mouse_down(gpui::MouseButton::Left, {
                        let handle = handle.clone();
                        move |event, _window, cx| {
                            // 中央ダブルクリックでトップ/ボトムメニューを
                            // 表示・非表示切り替え（Web 版のトグル相当）。
                            if event.click_count == 2 {
                                handle.update(cx, |this, cx| this.toggle_overlay(cx));
                            }
                        }
                    })
                    .on_mouse_move({
                        let handle = handle.clone();
                        move |_, _window, cx| {
                            handle.update(cx, |this, cx| {
                                if !this.overlay_visible {
                                    this.show_overlay(cx);
                                } else {
                                    this.restart_hide_timer(cx);
                                }
                            });
                        }
                    })
                    .on_scroll_wheel({
                        let handle = handle.clone();
                        move |event, _window, cx| {
                            // Ctrl スクロール（ピンチ）のみズーム。
                            // スクロールモードのページ番号更新は render の
                            // ポーリングに加えて、ここでも確実に反映する。
                            let defer_handle = handle.clone();
                            handle.update(cx, |this, cx| {
                                if this.mode == ViewMode::Scroll {
                                    // イベント時のスクロール位置は 1 フレーム前の
                                    // ことがあるため、次フレームで読み直してから
                                    // ページ番号を更新する（確実な反映）
                                    let scroll_handle = this.scroll_handle.clone();
                                    let container_w =
                                        _window.bounds().size.width.as_f32();
                                    cx.defer(move |cx| {
                                        defer_handle.update(cx, |this, cx| {
                                            let offset_y = scroll_handle
                                                .offset()
                                                .y
                                                .as_f32()
                                                .max(0.0);
                                            let (pw, ph) = this
                                                .loader
                                                .page_size(this.current_page)
                                                .unwrap_or((0, 0));
                                            let aspect = if pw > 0 && ph > 0 {
                                                pw as f32 / ph as f32
                                            } else {
                                                100.0 / 141.0
                                            };
                                            let page_h = if aspect > 0.0 {
                                                (container_w * 0.8) / aspect
                                            } else {
                                                container_w
                                            };
                                            let page = if page_h > 0.0 {
                                                (offset_y / page_h).floor() as usize
                                            } else {
                                                0
                                            };
                                            if page < this.loader.page_count()
                                                && page != this.last_scroll_page
                                            {
                                                log::info!(
                                                    "scroll wheel page update: offset={offset_y:.0}px page={page}"
                                                );
                                                this.last_scroll_page = page;
                                                this.current_page = page;
                                                cx.notify();
                                            }
                                        });
                                    });
                                    return;
                                }
                                if !event.modifiers.control {
                                    return;
                                }
                                // トップパネル/ボトムドック上（ページ一覧の
                                // 2 本指スクロール等）ではズームしない。
                                if this.hovering_ui {
                                    return;
                                }
                                let delta = match event.delta {
                                    gpui::ScrollDelta::Pixels(point) => f32::from(point.y),
                                    gpui::ScrollDelta::Lines(point) => point.y * 20.0,
                                } * -0.001;
                                this.adjust_zoom(cx, delta);
                            });
                        }
                    }),
            )
            // 左右 10% のクリックナビ領域（見開き用。単一モードは
            // 画像の上に重ねて実装している）。左エッジ = 前へ、
            // 右エッジ = 次へ（見開きでは 2 ページ単位）。
            // カーソルは方向に合わせて左向き/右向き矢印。
            // スクロールモードでは非表示。Shift 押しで 1 ページ移動。
            .when(mode == ViewMode::Spread && !self.zoomed, |this| {
                this.child(
                    div()
                        .id("viewer-edge-left")
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(gpui::Length::Definite(gpui::DefiniteLength::Fraction(0.1)))
                        .cursor(gpui::CursorStyle::ResizeLeft)
                        .flex()
                        .items_center()
                        .justify_center()
                        .opacity(0.0)
                        .hover(|style| {
                            style.bg(gpui::rgba(0x0000001f)).opacity(1.0)
                        })
                        .child(
                            Icon::new(IconName::ChevronLeft)
                                .size(px(28.0))
                                .text_color(gpui::rgba(0x00000099)),
                        )
                        .on_mouse_down(gpui::MouseButton::Left, {
                            let handle = handle.clone();
                            move |event, _window, cx| {
                                handle.update(cx, |this, cx| {
                                    if event.modifiers.shift {
                                        this.prev_page_shift(cx);
                                    } else {
                                        this.prev_page(cx);
                                    }
                                });
                            }
                        }),
                )
                .child(
                    div()
                        .id("viewer-edge-right")
                        .absolute()
                        .right_0()
                        .top_0()
                        .bottom_0()
                        .w(gpui::Length::Definite(gpui::DefiniteLength::Fraction(0.1)))
                        .cursor(gpui::CursorStyle::ResizeRight)
                        .flex()
                        .items_center()
                        .justify_center()
                        .opacity(0.0)
                        .hover(|style| {
                            style.bg(gpui::rgba(0x0000001f)).opacity(1.0)
                        })
                        .child(
                            Icon::new(IconName::ChevronRight)
                                .size(px(28.0))
                                .text_color(gpui::rgba(0x00000099)),
                        )
                        .on_mouse_down(gpui::MouseButton::Left, {
                            let handle = handle.clone();
                            move |event, _window, cx| {
                                handle.update(cx, |this, cx| {
                                    if event.modifiers.shift {
                                        this.next_page_shift(cx);
                                    } else {
                                        this.next_page(cx);
                                    }
                                });
                            }
                        }),
                )
            })
            // Web 版のトップパネル（image-book-panel 相当）:
            // ヘッダー（メニューアイコン + タイトル）+ パネルビュー
            // （menu / page-list / shortcuts / autoplay-settings）+ 戻る。
            .child(
                rounded_web(
                    div()
                        .id("viewer-top-panel")
                        .absolute()
                        .top(px(12.0 - 32.0 * (1.0 - overlay_progress)))
                        .left_3()
                        .w(px(380.0))
                        .opacity(overlay_progress)
                        .when(overlay_progress < 0.01, |this| this.invisible())
                        .bg(panel_bg)
                        .shadow_md()
                        .overflow_hidden()
                        // パネル内のクリックは下の画像（戻る・ズーム等）に伝達しない
                        .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| {
                            cx.stop_propagation();
                        }),

                )
                .child(
                    // ヘッダー（メニュートリガー）: アイコン + タイトル。
                    // 上端に角丸を付けて、ホバーで背景が変わっても角が四角くならない。
                    header_radius(
                        div()
                            .id("viewer-panel-trigger")
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_3()
                            .px_3()
                            .h(px(60.0))
                            .hover(|style| style.bg(cx.theme().muted))
                            .cursor_pointer()
                            .on_click({
                                let handle = handle.clone();
                                move |_, _window, cx| {
                                    handle.update(cx, |this, cx| this.toggle_panel(cx));
                                }
                            })
                            .child(
                                div()
                                    .w(px(32.0))
                                    .h(px(32.0))
                                    .rounded_full()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .bg(cx.theme().muted)
                                    .text_color(cx.theme().muted_foreground)
                                    .child(if panel_open { "✕" } else { "☰" }),
                            )
                            .child(
                                div().flex_1().min_w_0().child(
                                    div()
                                        .truncate()
                                        .text_sm()
                                        .font_weight(gpui::FontWeight::BOLD)
                                        .child(title),
                                ),
                            ),
                        panel_open,
                    ),
                )
                .child(if let Some(view) = active_panel {
                    div()
                        .border_t_1()
                        .border_color(cx.theme().muted)
                        .child(match view {
                            PanelView::Menu => div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .p_3()
                                .child(
                                    div()
                                        .id("viewer-menu-page-list")
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .justify_between()
                                        .px_3()
                                        .py_2()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(cx.theme().muted)
                                        .hover(|style| style.bg(cx.theme().muted))
                                        .cursor_pointer()
                                        .on_click({
                                            let handle = handle.clone();
                                            move |_, _window, cx| {
                                                handle.update(cx, |this, cx| {
                                                    this.open_panel(cx, PanelView::PageList);
                                                });
                                            }
                                        })
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(gpui::FontWeight::MEDIUM)
                                                .child("ページ一覧"),
                                        )
                                        .child(
                                            div()
                                                .text_color(cx.theme().muted_foreground)
                                                .child("›"),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("viewer-menu-shortcuts")
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .justify_between()
                                        .px_3()
                                        .py_2()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(cx.theme().muted)
                                        .hover(|style| style.bg(cx.theme().muted))
                                        .cursor_pointer()
                                        .on_click({
                                            let handle = handle.clone();
                                            move |_, _window, cx| {
                                                handle.update(cx, |this, cx| {
                                                    this.open_panel(cx, PanelView::Shortcuts);
                                                });
                                            }
                                        })
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(gpui::FontWeight::MEDIUM)
                                                .child("ショートカット"),
                                        )
                                        .child(
                                            div()
                                                .text_color(cx.theme().muted_foreground)
                                                .child("›"),
                                        ),
                                )
                                .child(
                                    div()
                                        .id("viewer-menu-autoplay")
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .justify_between()
                                        .px_3()
                                        .py_2()
                                        .rounded_md()
                                        .border_1()
                                        .border_color(cx.theme().muted)
                                        .hover(|style| style.bg(cx.theme().muted))
                                        .cursor_pointer()
                                        .on_click({
                                            let handle = handle.clone();
                                            move |_, _window, cx| {
                                                handle.update(cx, |this, cx| {
                                                    this.open_panel(
                                                        cx,
                                                        PanelView::AutoplaySettings,
                                                    );
                                                });
                                            }
                                        })
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(gpui::FontWeight::MEDIUM)
                                                .child("自動再生設定"),
                                        )
                                        .child(
                                            div()
                                                .text_color(cx.theme().muted_foreground)
                                                .child("›"),
                                        ),
                                ),
                            PanelView::PageList => div()
                                .flex()
                                .flex_col()
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .flex_wrap()
                                        .gap_2()
                                        .p_3()
                                        .h(px(420.0))
                                        .overflow_y_scrollbar()
                                        .children((0..total).map(|index| {
                                            let handle = handle.clone();
                                            let image = self.images.get(index).cloned().flatten();
                                            let is_current = index == self.current_page;
                                            div()
                                                .id(SharedString::from(format!(
                                                    "viewer-thumb-{index}"
                                                )))
                                                .flex()
                                                .flex_col()
                                                .items_center()
                                                .gap_1()
                                                .p_1()
                                                .rounded_md()
                                                .border_1()
                                                .border_color(if is_current {
                                                    cx.theme().primary
                                                } else {
                                                    cx.theme().muted
                                                })
                                                .hover(|style| style.bg(cx.theme().muted))
                                                .cursor_pointer()
                                                .on_click(move |_, _window, cx| {
                                                    handle.update(cx, |this, cx| {
                                                        this.set_page(cx, index);
                                                    });
                                                })
                                                .child(
                                                    div()
                                                        .w(px(100.0))
                                                        .aspect_ratio(100.0 / 141.0)
                                                        .rounded_sm()
                                                        .bg(gpui::white())
                                                        .overflow_hidden()
                                                        .child(match image {
                                                            Some(image) => img(image)
                                                                .size_full()
                                                                .object_fit(
                                                                    gpui::ObjectFit::Contain,
                                                                )
                                                                .into_any_element(),
                                                            None => {
                                                                div().size_full().into_any_element()
                                                            }
                                                        }),
                                                )
                                                .child(
                                                    div()
                                                        .text_xs()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child(format!("{}", index + 1)),
                                                )
                                                .child(if is_current {
                                                    div()
                                                        .text_xs()
                                                        .text_color(cx.theme().muted_foreground)
                                                        .child("現在")
                                                        .into_any_element()
                                                } else {
                                                    div().into_any_element()
                                                })
                                        })),
                                )
                                .child(self.panel_back(&handle, cx)),
                            PanelView::Shortcuts => {
                                let kbd = |stroke: &str| Kbd::new(gpui::Keystroke::parse(stroke).unwrap());
                                let row = |label: String, strokes: Vec<&str>| {
                                    div()
                                        .flex()
                                        .flex_row()
                                        .items_center()
                                        .justify_between()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_weight(gpui::FontWeight::MEDIUM)
                                                .child(label),
                                        )
                                        .child(
                                            div().flex().flex_row().gap_1().children(
                                                strokes.into_iter().map(kbd),
                                            ),
                                        )
                                };
                                div()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .p_3()
                                    .child(row("次ページ".to_string(), vec!["→", "l"]))
                                    .child(row("前ページ".to_string(), vec!["←", "h"]))
                                    .child(row("見開き: 1 ページずらす".to_string(), vec!["shift+→", "shift+←"]))
                                    .child(row("ダブルクリック".to_string(), vec!["拡大"]))
                                    .child(row("本棚に戻る".to_string(), vec!["↑", "k", "backspace"]))
                                    .child(row("メニュー表示".to_string(), vec!["esc"]))
                                    .child(self.panel_back(&handle, cx))
                            }
                            PanelView::AutoplaySettings => div()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .p_3()
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui::FontWeight::MEDIUM)
                                        .child("ページ送りの間隔"),
                                )
                                .child(Slider::new(&autoplay_slider).horizontal().w(px(300.0)))
                                .child(
                                    div()
                                        .text_sm()
                                        .text_color(cx.theme().muted_foreground)
                                        .child(format!("{} ms", interval_ms)),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_row()
                                        .gap_2()
                                        .child(
                                            Button::new("autoplay-slower")
                                                .cursor_pointer()
                                                .label("-")
                                                .cursor_pointer()
                                                .on_click({
                                                    let handle = handle.clone();
                                                    move |_, _window, cx| {
                                                        handle.update(cx, |this, cx| {
                                                            this.set_autoplay_interval(
                                                                cx,
                                                                interval_ms.saturating_sub(1000),
                                                            );
                                                        });
                                                    }
                                                }),
                                        )
                                        .child(
                                            Button::new("autoplay-faster")
                                                .cursor_pointer()
                                                .label("+")
                                                .cursor_pointer()
                                                .on_click({
                                                    let handle = handle.clone();
                                                    move |_, _window, cx| {
                                                        handle.update(cx, |this, cx| {
                                                            this.set_autoplay_interval(
                                                                cx,
                                                                interval_ms + 1000,
                                                            );
                                                        });
                                                    }
                                                }),
                                        ),
                                )
                                .child(self.panel_back(&handle, cx)),
                        })
                        .into_any_element()
                } else {
                    div().into_any_element()
                }),
            )
            // Web 版のボトムドック: opacity + translateY(0.75rem -> 0) を
            // 180ms でアニメーション（GPUI では bottom をアニメーション）。
            // 内側パネル（背景付き）に 20px 角丸（Web の rounded-[1.25rem] 相当）。
            .child(rounded_web(
                div()
                    .id("viewer-bottom-dock")
                    .absolute()
                    .bottom(px(12.0 - 12.0 * (1.0 - overlay_progress)))
                    .left_0()
                    .right_0()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_center()
                    .opacity(overlay_progress)
                    .when(overlay_progress < 0.01, |this| this.invisible())
                    .on_hover({
                        let handle = handle.clone();
                        move |hovered, _window, cx| {
                            handle.update(cx, |this, cx| {
                                if *hovered {
                                    this.hovering_ui = true;
                                    this.hide_generation += 1;
                                } else {
                                    this.hovering_ui = false;
                                    this.restart_hide_timer(cx);
                                }
                            });
                        }
                    })
                    .child(rounded_web(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_3()
                            .px_4()
                            .py_2()
                            .bg(dock_bg)
                            .shadow_md()
                            .child(
                                Button::new("viewer-back")
                                    .cursor_pointer()
                                    .label("戻る")
                                    .cursor_pointer()
                                    .on_click(|_, _window, cx| {
                                        cx.defer(move |cx| cx.dispatch_action(&CloseReader));
                                    }),
                            )
                            // 前へ/次へ・ページ入力はスクロールモードでは非表示。
                            // スライダーとページ番号はスクロール位置に連動して更新される。
                            // 右綴じの見開きでは「次へ」を左側に置く（読む方向に合わせる）。
                            .when(mode != ViewMode::Scroll, |this| {
                                if mirror_nav {
                                    this.child(next_button())
                                } else {
                                    this.child(prev_button())
                                }
                            })
                            .child(Slider::new(&page_slider).horizontal().w(px(200.0)))
                            .when(mode != ViewMode::Scroll, |this| {
                                if mirror_nav {
                                    this.child(prev_button())
                                } else {
                                    this.child(next_button())
                                }
                            })
                            .child(div().text_sm().child(format!("{current} / {total}")))
                            .when(mode != ViewMode::Scroll, |this| {
                                this.child(Input::new(&page_input).cursor_text().w(px(72.0)))
                            })
                            .child({
                                let mut button = Button::new("viewer-mode-single")
                                    .cursor_pointer()
                                    .label("単一");
                                if mode == ViewMode::Single {
                                    button = button.primary();
                                }
                                button.cursor_pointer().on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        handle.update(cx, |this, cx| {
                                            this.set_mode(cx, ViewMode::Single);
                                        });
                                    }
                                })
                            })
                            .child({
                                let mut button = Button::new("viewer-mode-spread")
                                    .cursor_pointer()
                                    .label("見開き");
                                if mode == ViewMode::Spread {
                                    button = button.primary();
                                }
                                button.cursor_pointer().on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        handle.update(cx, |this, cx| {
                                            this.set_mode(cx, ViewMode::Spread);
                                        });
                                    }
                                })
                            })
                            .child({
                                let mut button = Button::new("viewer-mode-scroll")
                                    .cursor_pointer()
                                    .label("スクロール");
                                if mode == ViewMode::Scroll {
                                    button = button.primary();
                                }
                                button.cursor_pointer().on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        handle.update(cx, |this, cx| {
                                            this.set_mode(cx, ViewMode::Scroll);
                                        });
                                    }
                                })
                            })
                            // 綴じ方向切替（見開きのみ。Web の image-book-binding-* 相当）
                            .when(mode == ViewMode::Spread, |this| {
                                this.child(
                                    Button::new("viewer-binding-right")
                                        .cursor_pointer()
                                        .label("右綴じ")
                                        .when(page_turn_right_to_left, |b| b.primary())
                                        .cursor_pointer()
                                        .on_click({
                                            let handle = handle.clone();
                                            move |_, _window, cx| {
                                                handle.update(cx, |this, cx| {
                                                    this.set_binding(cx, true);
                                                });
                                            }
                                        }),
                                )
                                .child(
                                    Button::new("viewer-binding-left")
                                        .cursor_pointer()
                                        .label("左綴じ")
                                        .when(!page_turn_right_to_left, |b| b.primary())
                                        .cursor_pointer()
                                        .on_click({
                                            let handle = handle.clone();
                                            move |_, _window, cx| {
                                                handle.update(cx, |this, cx| {
                                                    this.set_binding(cx, false);
                                                });
                                            }
                                        }),
                                )
                            })
                            .child(
                                Button::new("viewer-autoplay")
                                    .cursor_pointer()
                                    .label(if autoplay { "停止" } else { "再生" })
                                    .disabled(mode == ViewMode::Scroll)
                                    .cursor_pointer()
                                    .on_click({
                                        let handle = handle.clone();
                                        move |_, _window, cx| {
                                            handle.update(cx, |this, cx| {
                                                this.toggle_autoplay(cx);
                                            });
                                        }
                                    }),
                            )
                            .child(
                                Button::new("viewer-fullscreen")
                                    .cursor_pointer()
                                    .label(if window.is_fullscreen() {
                                        "全画面終了"
                                    } else {
                                        "全画面"
                                    })
                                    .cursor_pointer()
                                    .on_click(|_, window, _cx| {
                                        window.toggle_fullscreen();
                                    }),
                            ),
                    )),
            ))
    }
}

#[cfg(test)]
mod tests {
    use gpui::AppContext as _;
    use gpui::TestAppContext;
    use gpui_component::slider::SliderValue;

    use super::*;

    fn make_png(width: u32, height: u32) -> Vec<u8> {
        let mut image = image::RgbImage::new(width, height);
        for pixel in image.pixels_mut() {
            *pixel = image::Rgb([200, 200, 200]);
        }
        let mut out = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut std::io::Cursor::new(&mut out), image::ImageFormat::Png)
            .unwrap();
        out
    }

    struct FakeLoader {
        count: usize,
        png: Vec<u8>,
    }

    impl PageLoader for FakeLoader {
        fn page_count(&self) -> usize {
            self.count
        }

        fn page_size(&self, _index: usize) -> Option<(u32, u32)> {
            Some((100, 140))
        }

        fn load(&self, _index: usize) -> Result<Arc<RenderImage>, String> {
            decode_render_image(&self.png)
        }
    }

    fn viewer(cx: &mut TestAppContext, count: usize) -> Entity<ImageViewer> {
        // 表示モードの永続化（app_settings）で AppState が必要。
        // 既に初期化済みなら再初期化しない（保存済み設定を消さない）。
        cx.update(|cx| {
            if cx.try_global::<crate::app_state::AppState>().is_none() {
                crate::app_state::AppState::init_test(cx);
            }
        });
        let png = make_png(100, 140);
        cx.new(|cx| ImageViewer::new(cx, Arc::new(FakeLoader { count, png }), "テスト本", 0, None))
    }

    #[gpui::test]
    async fn view_mode_persists_across_instances(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        // スクロールモードに切り替えると永続化される
        let view = viewer(cx, 3);
        cx.update(|cx| view.update(cx, |this, cx| this.set_mode(cx, ViewMode::Scroll)));
        assert_eq!(view.read_with(cx, |v, _| v.mode), ViewMode::Scroll);
        let stored = cx.update(|cx| {
            let db = &crate::app_state::AppState::global(cx).db_pool;
            thundoku_core::db::settings::get(db, "viewer.mode")
                .ok()
                .flatten()
        });
        assert_eq!(stored.as_deref(), Some("scroll"));

        // 新しいインスタンスでも復元される
        let view2 = viewer(cx, 3);
        assert_eq!(view2.read_with(cx, |v, _| v.mode), ViewMode::Scroll);
    }

    #[gpui::test]
    async fn page_navigation_clamps_at_bounds(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let view = viewer(cx, 3);
        cx.update(|cx| view.update(cx, |this, cx| this.next_page(cx)));
        assert_eq!(view.read_with(cx, |v, _| v.current_page), 1);
        cx.update(|cx| view.update(cx, |this, cx| this.next_page(cx)));
        cx.update(|cx| view.update(cx, |this, cx| this.next_page(cx)));
        assert_eq!(view.read_with(cx, |v, _| v.current_page), 2);
        cx.update(|cx| view.update(cx, |this, cx| this.prev_page(cx)));
        cx.update(|cx| view.update(cx, |this, cx| this.prev_page(cx)));
        cx.update(|cx| view.update(cx, |this, cx| this.prev_page(cx)));
        assert_eq!(view.read_with(cx, |v, _| v.current_page), 0);
    }

    #[gpui::test]
    async fn spread_mode_pairs_and_advances_by_two(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let view = viewer(cx, 5);
        cx.update(|cx| view.update(cx, |this, cx| this.set_mode(cx, ViewMode::Spread)));
        // デフォルトは左→右（技術書典の本は左綴じ想定）: 小さい番号が左
        assert_eq!(view.read_with(cx, |v, _| v.spread_pages()), vec![0, 1]);
        cx.update(|cx| view.update(cx, |this, cx| this.next_page(cx)));
        assert_eq!(view.read_with(cx, |v, _| v.spread_pages()), vec![2, 3]);
        cx.update(|cx| view.update(cx, |this, cx| this.next_page(cx)));
        assert_eq!(view.read_with(cx, |v, _| v.spread_pages()), vec![4]);
    }

    #[gpui::test]
    async fn shift_arrow_key_moves_single_page_in_spread(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let view = viewer(cx, 5);
        let window = cx.open_window(
            gpui::Size {
                width: gpui::px(800.0),
                height: gpui::px(600.0),
            },
            |window, cx| gpui_component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui::VisualTestContext::from_window(*window, cx).into_mut();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.update(|cx| view.update(cx, |this, cx| this.set_mode(cx, ViewMode::Spread)));

        // 通常の右矢印: 見開きなので 2 ページ進む
        visual.simulate_event(gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse("right").unwrap(),
            is_held: false,
            prefer_character_input: false,
        });
        assert_eq!(view.read_with(cx, |v, _| v.current_page), 2);

        // Shift + 右矢印: 1 ページだけ進む（見開きの位置調整）
        visual.simulate_event(gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse("shift-right").unwrap(),
            is_held: false,
            prefer_character_input: false,
        });
        assert_eq!(
            view.read_with(cx, |v, _| v.current_page),
            3,
            "shift+right must move a single page in spread mode"
        );

        // Shift + 左矢印: 1 ページだけ戻る
        visual.simulate_event(gpui::KeyDownEvent {
            keystroke: gpui::Keystroke::parse("shift-left").unwrap(),
            is_held: false,
            prefer_character_input: false,
        });
        assert_eq!(
            view.read_with(cx, |v, _| v.current_page),
            2,
            "shift+left must move a single page back"
        );
    }

    #[gpui::test]
    async fn scroll_mode_updates_current_page_from_position(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let view = viewer(cx, 5);
        let window = cx.open_window(
            gpui::Size {
                width: gpui::px(800.0),
                height: gpui::px(600.0),
            },
            |window, cx| gpui_component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui::VisualTestContext::from_window(*window, cx).into_mut();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.update(|cx| view.update(cx, |this, cx| this.set_mode(cx, ViewMode::Scroll)));
        // レイアウトを確定させてから 3 ページ目までスクロール
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        visual.update(|window, cx| {
            view.update(cx, |this, _| this.scroll_handle.scroll_to_item(2));
            let _ = window.draw(cx);
        });
        // scroll_to_item は prepaint で適用され、top_item は描画後に
        // 更新されるため、数フレーム描画して反映を待つ
        for _ in 0..3 {
            visual.update(|window, cx| {
                let _ = window.draw(cx);
            });
        }

        assert_eq!(
            view.read_with(cx, |v, _| v.current_page),
            2,
            "scrolling to page 3 must update current_page to 2 (0-based)"
        );
    }

    #[gpui::test]
    async fn binding_switch_persists_and_flips_spread(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(crate::app_state::AppState::init_test);
        let view = viewer(cx, 5);
        cx.update(|cx| view.update(cx, |this, cx| this.set_mode(cx, ViewMode::Spread)));
        // デフォルトは左綴じ（技術書典の本は左綴じ想定）: 小さい番号が左
        assert_eq!(
            view.read_with(cx, |v, _| v.spread_pages()),
            vec![0, 1],
            "default binding must be left-to-right"
        );
        // 右綴じに切替 → 小さい番号が右（画像の配置が入れ替わる）
        cx.update(|cx| view.update(cx, |this, cx| this.set_binding(cx, true)));
        assert_eq!(
            view.read_with(cx, |v, _| v.spread_pages()),
            vec![1, 0],
            "right binding must place the smaller page number on the right"
        );
        // 設定に永続化される
        let stored = cx.read(|cx| {
            let db = &crate::app_state::AppState::global(cx).db_pool;
            thundoku_core::db::settings::get(db, "viewer.page_turn").unwrap()
        });
        assert_eq!(stored.as_deref(), Some("right-to-left"));
        // 左綴じに戻す
        cx.update(|cx| view.update(cx, |this, cx| this.set_binding(cx, false)));
        assert_eq!(view.read_with(cx, |v, _| v.spread_pages()), vec![0, 1]);
        let stored = cx.read(|cx| {
            let db = &crate::app_state::AppState::global(cx).db_pool;
            thundoku_core::db::settings::get(db, "viewer.page_turn").unwrap()
        });
        assert_eq!(stored.as_deref(), Some("left-to-right"));
    }

    #[gpui::test]
    async fn spread_navigation_shift_moves_single_page(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let view = viewer(cx, 5);
        cx.update(|cx| view.update(cx, |this, cx| this.set_mode(cx, ViewMode::Spread)));
        // 通常の次へ: 見開きなので 2 ページ進む
        cx.update(|cx| view.update(cx, |this, cx| this.next_page(cx)));
        assert_eq!(view.read_with(cx, |v, _| v.current_page), 2);
        // Shift 押しの次へ: 1 ページだけ進む（見開きの位置調整）
        cx.update(|cx| view.update(cx, |this, cx| this.next_page_shift(cx)));
        assert_eq!(view.read_with(cx, |v, _| v.current_page), 3);
        // Shift 押しの前へ: 1 ページだけ戻る
        cx.update(|cx| view.update(cx, |this, cx| this.prev_page_shift(cx)));
        assert_eq!(view.read_with(cx, |v, _| v.current_page), 2);
        // 通常の前へ: 2 ページ戻る
        cx.update(|cx| view.update(cx, |this, cx| this.prev_page(cx)));
        assert_eq!(view.read_with(cx, |v, _| v.current_page), 0);
    }

    #[gpui::test]
    async fn page_turn_direction_flips_spread_order(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(crate::app_state::AppState::init_test);
        // 左→右（洋書）: 小さい番号が左
        cx.update(|cx| {
            let db = &crate::app_state::AppState::global(cx).db_pool;
            thundoku_core::db::settings::set(db, "viewer.page_turn", "left-to-right").unwrap();
        });
        let view = viewer(cx, 5);
        cx.update(|cx| view.update(cx, |this, cx| this.set_mode(cx, ViewMode::Spread)));
        assert_eq!(
            view.read_with(cx, |v, _| v.spread_pages()),
            vec![0, 1],
            "left-to-right keeps ascending order"
        );
        // 右→左（日本の本）: 小さい番号が右
        cx.update(|cx| {
            let db = &crate::app_state::AppState::global(cx).db_pool;
            thundoku_core::db::settings::set(db, "viewer.page_turn", "right-to-left").unwrap();
        });
        let view2 = viewer(cx, 5);
        cx.update(|cx| view2.update(cx, |this, cx| this.set_mode(cx, ViewMode::Spread)));
        assert_eq!(
            view2.read_with(cx, |v, _| v.spread_pages()),
            vec![1, 0],
            "right-to-left puts the smaller page on the right"
        );
    }

    #[gpui::test]
    async fn autoplay_starts_stops_and_advances(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let view = viewer(cx, 5);
        cx.update(|cx| view.update(cx, |this, cx| this.set_autoplay_interval(cx, 3000)));
        cx.update(|cx| view.update(cx, |this, cx| this.toggle_autoplay(cx)));
        assert!(view.read_with(cx, |v, _| v.autoplay));
        cx.run_until_parked();
        cx.background_executor.advance_clock(Duration::from_secs(4));
        cx.run_until_parked();
        let advanced = view.read_with(cx, |v, _| v.current_page) > 0;
        assert!(advanced, "autoplay should advance the page");
        cx.update(|cx| view.update(cx, |this, cx| this.toggle_autoplay(cx)));
        assert!(!view.read_with(cx, |v, _| v.autoplay));
    }

    #[gpui::test]
    async fn zoom_hides_dock_and_scroll_disables_autoplay(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let view = viewer(cx, 3);
        assert!(!view.read_with(cx, |v, _| v.is_dock_hidden()));
        cx.update(|cx| view.update(cx, |this, cx| this.adjust_zoom(cx, 0.5)));
        assert!(view.read_with(cx, |v, _| v.is_dock_hidden()));
        cx.update(|cx| view.update(cx, |this, cx| this.set_mode(cx, ViewMode::Scroll)));
        assert!(!view.read_with(cx, |v, _| v.autoplay));
    }

    #[gpui::test]
    async fn toggle_overlay_switches_visibility(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let view = viewer(cx, 3);
        // 初期状態は表示
        assert!(view.read_with(cx, |v, _| v.overlay_visible));
        // 中央ダブルクリック相当: トグルで非表示 → 表示
        cx.update(|cx| view.update(cx, |this, cx| this.toggle_overlay(cx)));
        assert!(!view.read_with(cx, |v, _| v.overlay_visible));
        cx.update(|cx| view.update(cx, |this, cx| this.toggle_overlay(cx)));
        assert!(view.read_with(cx, |v, _| v.overlay_visible));
    }

    #[gpui::test]
    async fn page_list_loads_all_thumbnails(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let view = viewer(cx, 3);
        assert!(!view.read_with(cx, |v, _| v.active_panel.is_some()));
        cx.update(|cx| {
            view.update(cx, |this, cx| this.open_panel(cx, PanelView::PageList));
        });
        assert_eq!(
            view.read_with(cx, |v, _| v.active_panel),
            Some(PanelView::PageList)
        );
        cx.run_until_parked();
        // Web 版と同様に全ページのサムネイルが読み込まれる
        assert!(
            view.read_with(cx, |v, _| v.images.iter().all(|image| image.is_some())),
            "all page thumbnails should load when the page list opens"
        );
    }

    #[gpui::test]
    async fn keyboard_shortcuts_dispatch_through_focus(cx: &mut TestAppContext) {
        // 回帰: ビューアーのルートに track_focus を付けてキーイベントを
        // 受け取れるようにした（workspace 埋め込み後もショートカットが効く）。
        cx.update(gpui_component::init);
        let view = viewer(cx, 3);
        let window = cx.open_window(
            gpui::Size {
                width: gpui::px(800.0),
                height: gpui::px(600.0),
            },
            |window, cx| gpui_component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui::VisualTestContext::from_window(*window, cx).into_mut();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });
        // 実イベント配送: 右矢印 → 次ページ
        visual.simulate_event(KeyDownEvent {
            keystroke: gpui::Keystroke::parse("right").unwrap(),
            is_held: false,
            prefer_character_input: false,
        });
        assert_eq!(
            view.read_with(cx, |v, _| v.current_page),
            1,
            "right arrow should advance the page via focus dispatch"
        );
        // 左矢印 → 前ページ
        visual.simulate_event(KeyDownEvent {
            keystroke: gpui::Keystroke::parse("left").unwrap(),
            is_held: false,
            prefer_character_input: false,
        });
        assert_eq!(
            view.read_with(cx, |v, _| v.current_page),
            0,
            "left arrow should go back via focus dispatch"
        );
    }

    #[gpui::test]
    async fn keyboard_arrows_move_pages(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let view = viewer(cx, 3);
        let window = cx.open_window(
            gpui::Size {
                width: gpui::px(800.0),
                height: gpui::px(600.0),
            },
            |window, cx| gpui_component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui::VisualTestContext::from_window(*window, cx).into_mut();
        // simulate the handler directly (key event delivery is covered by
        // the element wiring; the logic under test is the page stepping)
        visual.update(|window, cx| {
            view.update(cx, |this, cx| this.handle_key(&right_event(), window, cx));
        });
        assert_eq!(view.read_with(cx, |v, _| v.current_page), 1);
        visual.update(|window, cx| {
            view.update(cx, |this, cx| this.handle_key(&left_event(), window, cx));
        });
        assert_eq!(view.read_with(cx, |v, _| v.current_page), 0);
    }

    #[gpui::test]
    async fn autoplay_slider_initializes_with_valid_range(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let view = viewer(cx, 3);
        let window = cx.open_window(
            gpui::Size {
                width: gpui::px(800.0),
                height: gpui::px(600.0),
            },
            |window, cx| gpui_component::Root::new(view.clone(), window, cx),
        );
        cx.update_window(*window, |_root, window, cx| {
            view.update(cx, |this, cx| this.ensure_panel_states(window, cx));
        })
        .unwrap();
        // 回帰: `min` を先に設定するとデフォルト max=100 との clamp で
        // min > max panic になるため、max → min の順で設定されること。
        let slider = view
            .read_with(cx, |v, _| v.autoplay_slider.clone())
            .expect("slider created");
        let (min, max, value) =
            slider.read_with(cx, |s, _| (s.min_value(), s.max_value(), s.value()));
        assert_eq!(min, AUTOPLAY_MIN_MS as f32);
        assert_eq!(max, AUTOPLAY_MAX_MS as f32);
        assert_eq!(value, SliderValue::Single(AUTOPLAY_DEFAULT_MS as f32));
    }

    #[test]
    fn decode_render_image_outputs_bgra() {
        // 回帰: RenderImage は BGRA を期待（R/B を入れ替えないと赤が青に見える）。
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
        let render = decode_render_image(&bytes).expect("decoded");
        let frame = render.as_bytes(0).expect("frame bytes");
        // 赤 (255,0,0) は BGRA 順で (B=0, G=0, R=255, A=255) になる
        assert_eq!(&frame[0..4], &[0, 0, 255, 255]);
    }

    #[gpui::test]
    async fn page_navigation_replaces_image_element(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let view = viewer(cx, 3);
        let window = cx.open_window(
            gpui::Size {
                width: gpui::px(800.0),
                height: gpui::px(600.0),
            },
            |window, cx| gpui_component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui::VisualTestContext::from_window(*window, cx).into_mut();
        cx.run_until_parked();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        // 画像がロード済みであること（ロードされていなければ render は
        // 「読込中…」になり img 要素が存在しない）
        assert!(
            view.read_with(cx, |v, _| v.images[0].is_some()),
            "page 0 image should be loaded"
        );
        // ページ 0 のコンテナのみ要素ツリーに存在する（前のページが残らないこと）
        assert!(
            visual.debug_bounds("viewer-page-0").is_some(),
            "page 0 container should be present"
        );
        assert!(
            visual.debug_bounds("viewer-page-1").is_none(),
            "page 1 container must not exist before navigating"
        );

        visual.update(|_window, cx| view.update(cx, |this, cx| this.next_page(cx)));
        cx.run_until_parked();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        // ページ 1 のコンテナに置き換わる（ページ 0 の要素は消える）
        assert!(
            visual.debug_bounds("viewer-page-1").is_some(),
            "page 1 container should be present after navigating"
        );
        assert!(
            visual.debug_bounds("viewer-page-0").is_none(),
            "page 0 container must not remain after navigating (no overlay)"
        );
    }

    #[gpui::test]
    async fn scroll_mode_stacks_pages_vertically(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let view = viewer(cx, 3);
        let window = cx.open_window(
            gpui::Size {
                width: gpui::px(800.0),
                height: gpui::px(600.0),
            },
            |window, cx| gpui_component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui::VisualTestContext::from_window(*window, cx).into_mut();
        cx.update(|cx| view.update(cx, |this, cx| this.set_mode(cx, ViewMode::Scroll)));
        cx.run_until_parked();
        visual.update(|window, cx| {
            let _ = window.draw(cx);
        });

        // 3 ページすべてが要素ツリーに存在し、縦にずれて配置される（重ならない）
        let b0 = visual
            .debug_bounds("viewer-page-0")
            .expect("page 0 present in scroll mode");
        let b1 = visual
            .debug_bounds("viewer-page-1")
            .expect("page 1 present in scroll mode");
        let b2 = visual
            .debug_bounds("viewer-page-2")
            .expect("page 2 present in scroll mode");
        assert!(
            b1.origin.y > b0.origin.y,
            "page 1 must stack below page 0 (got {b0:?} vs {b1:?})"
        );
        assert!(
            b2.origin.y > b1.origin.y,
            "page 2 must stack below page 1 (got {b1:?} vs {b2:?})"
        );
    }

    fn right_event() -> KeyDownEvent {
        KeyDownEvent {
            keystroke: gpui::Keystroke::parse("right").unwrap(),
            is_held: false,
            prefer_character_input: false,
        }
    }

    fn left_event() -> KeyDownEvent {
        KeyDownEvent {
            keystroke: gpui::Keystroke::parse("left").unwrap(),
            is_held: false,
            prefer_character_input: false,
        }
    }
}
