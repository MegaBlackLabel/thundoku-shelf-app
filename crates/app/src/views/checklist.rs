//! チェックリストビュー: イベント一覧 → チェック項目、同期、試し読み、
//! お気に入り取り込み。

use std::collections::{HashMap, HashSet, VecDeque};
use std::io::Read as _;
use std::sync::Arc;

use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, IconName};
use gpui_kit::{
    Context, FontWeight, IntoElement, ParentElement, Render, RenderImage, SharedString,
    StyledImage, Window, div, img, px,
};
use gpui_kit::{
    InteractiveElement as _, ReadGlobal as _, StatefulInteractiveElement as _, Styled as _,
};
use thundoku_core::db;
use thundoku_core::db::checklist::{CheckedItem, TbfEvent};
use thundoku_core::tbf;

use crate::app_state::AppState;
use crate::icons::AppIcon;
use image::GenericImageView as _;

/// デコード済みサムネイルの保持上限（1 ページ = 50 件 + 余裕を持たせた値）。
/// これを超えたら古い順に追い出してメモリに残さない。
const MAX_THUMB_CACHE: usize = 64;

/// 並び替えフィールド（Web の SortField 相当）。
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SortField {
    SortOrder,
    CircleName,
    SpaceNumber,
}

pub struct ChecklistView {
    events: Vec<TbfEvent>,
    selected_slug: Option<String>,
    items: Vec<CheckedItem>,
    busy: bool,
    error: Option<String>,
    toast: Option<String>,
    last_sync: Option<String>,
    /// 並び替え（Web の sort 相当）
    sort_field: SortField,
    /// true = 昇順（Web の sort.direction === "asc"）
    sort_ascending: bool,
    /// ページネーション（Web の PAGE_SIZE = 50）
    page: usize,
    /// 一覧の「これより以前のイベントを表示」で表示済みの件数
    visible_older_count: usize,
    /// デコード済みサムネイルのキャッシュ（render で毎回デコードしない）。
    /// 保持数は MAX_THUMB_CACHE まで（超過分は古い順に追い出す）
    thumbnail_images: HashMap<String, Arc<RenderImage>>,
    /// thumbnail_images の挿入順（先頭が最古）。上限超過時の追い出しに使う
    thumbnail_order: VecDeque<String>,
    /// 追い出し済みサムネイルの GPU テクスチャ解放待ち（`release_evicted_thumbs` が
    /// 描画時に drop_image する）。キャッシュ操作は window を持たない
    /// バックグラウンドタスクから呼ばれるため、いったんここに預けてから解放する
    evicted_thumb_images: Vec<Arc<RenderImage>>,
    /// 取得中のサムネイル（重複ダウンロード防止）
    thumbnail_fetching: HashSet<String>,
    /// ダウンロード失敗したサムネイル（reload 連鎖の無限ループ防止。
    /// 失敗はアプリ再起動でリセットされ、次回起動時に再試行される）
    fetch_failed: HashSet<String>,
}

impl ChecklistView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let mut view = Self {
            events: Vec::new(),
            selected_slug: None,
            items: Vec::new(),
            busy: false,
            error: None,
            toast: None,
            last_sync: None,
            sort_field: SortField::SortOrder,
            sort_ascending: true,
            page: 0,
            visible_older_count: 0,
            thumbnail_images: HashMap::new(),
            thumbnail_order: VecDeque::new(),
            evicted_thumb_images: Vec::new(),
            thumbnail_fetching: HashSet::new(),
            fetch_failed: HashSet::new(),
        };
        view.reload(cx);
        view
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let (events, items, last_sync, selected) = {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            let events = db::checklist::list_events(db).unwrap_or_default();
            let selected = self.selected_slug.clone();
            let items = match &selected {
                Some(slug) => db::checklist::list_items(db, slug).unwrap_or_default(),
                None => Vec::new(),
            };
            let last_sync = db::settings::get(db, "api.last_sync_at").ok().flatten();
            (events, items, last_sync, selected)
        };
        self.selected_slug = selected;
        self.events = events;
        self.items = items;
        self.last_sync = last_sync;
        self.fetch_missing_thumbnails(cx);
        self.decode_cached_thumbnails(cx);
        cx.notify();
    }

    /// 現在の並び替え・ページ設定で表示中の項目（Web の PAGE_SIZE = 50 件）。
    /// サムネイルのデコード対象と、追い出しから守る範囲を揃えるために使う。
    fn page_items(&self) -> Vec<CheckedItem> {
        let mut sorted = self.items.clone();
        sorted.sort_by(|a, b| {
            let ordering = match self.sort_field {
                SortField::CircleName => a.circle_name.cmp(&b.circle_name),
                SortField::SpaceNumber => a.space_number.cmp(&b.space_number),
                SortField::SortOrder => a.sort_order.cmp(&b.sort_order),
            };
            if self.sort_ascending {
                ordering
            } else {
                ordering.reverse()
            }
        });
        const PAGE_SIZE: usize = 50;
        let start = self.page * PAGE_SIZE;
        sorted.into_iter().skip(start).take(PAGE_SIZE).collect()
    }

    /// DB に保存済みの thumbnail_data（base64）をバックグラウンドで
    /// 縮小デコードしてキャッシュする（render で毎回デコードしない）。
    /// デコード対象（thumbnail_data あり・未キャッシュ）を現在表示中の
    /// ページ（50 件）に限定して返す。全件を一括デコードしない。
    fn decode_pending(&self) -> Vec<(String, String)> {
        self.page_items()
            .into_iter()
            .filter(|item| {
                item.thumbnail_data.is_some()
                    && !self.thumbnail_images.contains_key(&item.id)
                    && !self.thumbnail_fetching.contains(&item.id)
                    && !self.fetch_failed.contains(&item.id)
            })
            .filter_map(|item| {
                item.thumbnail_data
                    .clone()
                    .map(|data| (item.id.clone(), data))
            })
            .take(20)
            .collect()
    }

    /// デコード済みサムネイルをキャッシュへ追加する（上限つき LRU）。
    /// 上限を超えた分は古い順に追い出し、`evicted_thumb_images` に積む。
    /// ここでは window を持たない（バックグラウンドタスクから呼ばれる）ため、
    /// GPU テクスチャの解放は `release_evicted_thumbs` が描画時に行う。
    fn cache_thumb(&mut self, key: String, image: Arc<RenderImage>) {
        if let Some(previous) = self.thumbnail_images.insert(key.clone(), image) {
            // 同じキーの再挿入（取得とデコードの競合）は古い画像を解放待ちにして、
            // 挿入位置を最新へ移す
            self.evicted_thumb_images.push(previous);
            if let Some(index) = self.thumbnail_order.iter().position(|k| k == &key) {
                self.thumbnail_order.remove(index);
            }
        }
        self.thumbnail_order.push_back(key);
        self.trim_thumb_cache();
    }

    /// キャッシュの上限を超えたサムネイルを古い順に追い出す。
    /// 現在表示中のページの項目は、追い出すと次のデコードまで表紙が消えるため残す。
    fn trim_thumb_cache(&mut self) {
        if self.thumbnail_images.len() <= MAX_THUMB_CACHE {
            return;
        }
        let visible: Vec<String> = self.page_items().into_iter().map(|item| item.id).collect();
        let mut index = 0;
        while self.thumbnail_images.len() > MAX_THUMB_CACHE && index < self.thumbnail_order.len() {
            if visible.contains(&self.thumbnail_order[index]) {
                // 表示中の行は飛ばして、次に古いものを追い出す
                index += 1;
                continue;
            }
            let key = self.thumbnail_order.remove(index).expect("index < len");
            self.evict_thumb(&key);
        }
        // 表示中だけで上限を超えた場合は、古い順に落として上限を守る
        while self.thumbnail_images.len() > MAX_THUMB_CACHE {
            let Some(key) = self.thumbnail_order.pop_front() else {
                break;
            };
            self.evict_thumb(&key);
        }
    }

    /// キャッシュから外し、GPU テクスチャ解放待ちに積む。
    fn evict_thumb(&mut self, key: &str) {
        if let Some(image) = self.thumbnail_images.remove(key) {
            self.evicted_thumb_images.push(image);
        }
    }

    /// 追い出し済みサムネイルの GPU テクスチャを解放する（render から呼ぶ）。
    fn release_evicted_thumbs(&mut self, window: &mut Window) {
        for image in std::mem::take(&mut self.evicted_thumb_images) {
            // GPU 側のテクスチャも解放する（atlas は明示的に消すまで残る）
            let _ = window.drop_image(image);
        }
    }

    fn decode_cached_thumbnails(&mut self, cx: &mut Context<Self>) {
        let pending = self.decode_pending();
        if pending.is_empty() {
            return;
        }
        log::info!("decode_cached_thumbnails: {} pending", pending.len());
        for (id, _) in &pending {
            self.thumbnail_fetching.insert(id.clone());
        }
        let handle = cx.entity();
        let task = cx.background_executor().spawn(async move {
            let mut images: Vec<(String, Arc<RenderImage>)> = Vec::new();
            let mut failed: Vec<String> = Vec::new();
            for (id, data) in pending {
                let bytes = data
                    .strip_prefix("data:image/")
                    .and_then(|rest| rest.split_once(','))
                    .map(|(_, b64)| b64)
                    .unwrap_or(&data);
                let ok = (|| {
                    let decoded =
                        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, bytes)
                            .map_err(|e| e.to_string())?;
                    let image = decode_thumbnail(&decoded).ok_or("decode failed")?;
                    images.push((id.clone(), image));
                    Ok::<(), String>(())
                })();
                if ok.is_err() {
                    failed.push(id);
                }
            }
            (images, failed)
        });
        cx.spawn(async move |_window, cx| {
            let (images, failed) = task.await;
            let count = images.len();
            handle.update(cx, |this, cx| {
                for (id, image) in images {
                    this.thumbnail_fetching.remove(&id);
                    this.cache_thumb(id, image);
                }
                // デコード失敗も記録して毎回の再試行を防ぐ（連鎖の停止）
                for id in failed {
                    this.thumbnail_fetching.remove(&id);
                    this.fetch_failed.insert(id);
                }
                log::info!("decode_cached_thumbnails done: {count} images");
                // 残りは直接続行（reload() は全体再ロードを伴い連鎖を生むので
                // 呼ばない。pending が空なら即 return して終了する）
                this.decode_cached_thumbnails(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// サムネイルが未取得の項目について、thumbnail_url から画像を
    /// ダウンロードして thumbnail_data（base64）に保存する。
    /// メモリ上に持つのは保存用の縮小 JPEG のみで、表示用のデコードは
    /// 描画中のページに限り `decode_cached_thumbnails` が行う。
    fn fetch_missing_thumbnails(&mut self, cx: &mut Context<Self>) {
        // 取得済み/取得中の項目はスキップし、1 度の実行では 20 件まで
        let pending: Vec<(String, String)> = self
            .items
            .iter()
            .filter(|item| {
                item.thumbnail_data.is_none()
                    && item.thumbnail_url.is_some()
                    && !self.thumbnail_fetching.contains(&item.id)
                    && !self.fetch_failed.contains(&item.id)
            })
            .filter_map(|item| item.thumbnail_url.clone().map(|url| (item.id.clone(), url)))
            .take(20)
            .collect();
        if pending.is_empty() {
            return;
        }
        for (id, _) in &pending {
            self.thumbnail_fetching.insert(id.clone());
        }
        let handle = cx.entity();
        let state = AppState::global(cx);
        let db = state.db_pool.clone();
        let task = cx.background_executor().spawn(async move {
            let agent = ureq::AgentBuilder::new()
                .timeout_connect(std::time::Duration::from_secs(15))
                .timeout_read(std::time::Duration::from_secs(30))
                .build();
            // ダウンロード → 縮小 JPEG にして DB へ保存する。
            // 表示用のデコードは描画中のページだけ `decode_cached_thumbnails` が行う
            // （全件分の RenderImage をメモリに作らない）
            let mut stored: Vec<(String, String)> = Vec::new();
            let mut failed: Vec<String> = Vec::new();
            for (id, url) in pending {
                let ok = (|| {
                    let resp = agent.get(&url).call().map_err(|e| e.to_string())?;
                    let mut body = Vec::new();
                    resp.into_reader()
                        .read_to_end(&mut body)
                        .map_err(|e| e.to_string())?;
                    // 永続化は縮小 JPEG のみ（元画像は保存しない）
                    let jpeg_b64 = thumbnail_jpeg_base64(&body).ok_or("not an image")?;
                    stored.push((id.clone(), jpeg_b64));
                    Ok::<(), String>(())
                })();
                if ok.is_err() {
                    failed.push(id);
                }
            }
            for (id, data) in &stored {
                let _ = db::checklist::update_thumbnail_data(&db, id, data);
            }
            (failed, stored)
        });
        cx.spawn(async move |_window, cx| {
            let (failed, stored) = task.await;
            handle.update(cx, |this, cx| {
                this.apply_fetched_thumbnails(cx, stored, failed);
                // 残りは直接続行（reload() は全体再ロードを伴い連鎖を生むので
                // 呼ばない。pending が空なら即 return して終了する）
                this.fetch_missing_thumbnails(cx);
                cx.notify();
            });
        })
        .detach();
    }

    /// 取得結果（保存済み base64 / 失敗した項目 ID）を items へ反映し、
    /// 描画中のページのサムネイルだけをデコードし直す。
    /// 非表示ページ分の RenderImage は作らない（メモリに残さない）。
    fn apply_fetched_thumbnails(
        &mut self,
        cx: &mut Context<Self>,
        stored: Vec<(String, String)>,
        failed: Vec<String>,
    ) {
        // 保存済みデータをメモリ上の items にも反映して、
        // 次の reload まで再取得対象にならないようにする
        for (id, data) in stored {
            self.thumbnail_fetching.remove(&id);
            if let Some(item) = self.items.iter_mut().find(|item| item.id == *id) {
                item.thumbnail_data = Some(data);
            }
        }
        // 失敗した項目は記録して再試行しない（無限連鎖の防止）
        for id in failed {
            self.thumbnail_fetching.remove(&id);
            self.fetch_failed.insert(id);
        }
        // 表示中のページのサムネイルをデコードしてキャッシュする
        self.decode_cached_thumbnails(cx);
    }

    fn select_event(&mut self, cx: &mut Context<Self>, slug: &str) {
        self.selected_slug = Some(slug.to_string());
        self.page = 0;
        self.reload(cx);
    }

    /// イベント一覧に戻る（Web の「← イベント一覧に戻る」相当）。
    fn back_to_list(&mut self, cx: &mut Context<Self>) {
        self.selected_slug = None;
        self.reload(cx);
    }

    /// 並び替え（Web の handleSortChange 相当: 同じフィールドなら向きを反転）。
    fn toggle_sort(&mut self, cx: &mut Context<Self>, field: SortField) {
        if self.sort_field == field {
            self.sort_ascending = !self.sort_ascending;
        } else {
            self.sort_field = field;
            self.sort_ascending = true;
        }
        self.page = 0;
        cx.notify();
    }

    /// 選択中イベントのチェックリストを同期する。
    pub fn sync(&mut self, cx: &mut Context<Self>) {
        let logged_in = *AppState::global(cx).tbf_logged_in.lock();
        if !logged_in {
            self.toast = Some("ログインしてから同期してください".into());
            cx.defer(move |cx| cx.dispatch_action(&crate::actions::OpenAuth));
            cx.notify();
            return;
        }
        let Some(slug) = self.selected_slug.clone() else {
            return;
        };
        self.busy = true;
        self.error = None;
        let handle = cx.entity();
        let state = AppState::global(cx);
        let tbf_client = state.tbf.clone();
        let db = state.db_pool.clone();
        let slug_for_task = slug.clone();
        let owner = crate::app_state::owner_token(state);
        let task: gpui_kit::Task<Result<usize, String>> =
            cx.background_executor().spawn(async move {
                let mut client = tbf_client.lock();
                let outcome = tbf::sync::refresh_checklist(
                    &db,
                    &mut client,
                    &slug_for_task,
                    owner.as_deref(),
                )
                .map_err(|e| e.to_string())?;
                Ok(outcome.count)
            });
        cx.spawn(async move |_window, cx| {
            let result = task.await;
            handle.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(count) => {
                        log::info!("sync done: {count} entries");
                        this.toast = Some(format!("チェックリストを同期しました（{count} 件）"));
                        this.reload(cx);
                    }
                    Err(message) => {
                        this.error = Some(message.clone());
                        if message.contains("session expired") {
                            cx.defer(move |cx| cx.dispatch_action(&crate::actions::OpenAuth));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// お気に入り（checkedProductInfos）をチェックリストへ取り込む。
    pub fn import_favorites(&mut self, cx: &mut Context<Self>) {
        let logged_in = *AppState::global(cx).tbf_logged_in.lock();
        if !logged_in {
            self.toast = Some("ログインしてから実行してください".into());
            cx.defer(move |cx| cx.dispatch_action(&crate::actions::OpenAuth));
            cx.notify();
            return;
        }
        let Some(slug) = self.selected_slug.clone() else {
            return;
        };
        self.busy = true;
        self.error = None;
        let handle = cx.entity();
        let state = AppState::global(cx);
        let tbf_client = state.tbf.clone();
        let db = state.db_pool.clone();
        let slug_for_task = slug.clone();
        let owner = crate::app_state::owner_token(state);
        let task: gpui_kit::Task<Result<usize, String>> =
            cx.background_executor().spawn(async move {
                let mut client = tbf_client.lock();
                let entries = client
                    .favorites(&slug_for_task)
                    .map_err(|e| e.to_string())?;
                tbf::sync::save_checklist(&db, &slug_for_task, &entries, owner.as_deref())
                    .map_err(|e| e.to_string())?;
                Ok(entries.len())
            });
        cx.spawn(async move |_window, cx| {
            let result = task.await;
            handle.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(count) => {
                        this.toast = Some(format!("お気に入りを {count} 件取り込みました"));
                        this.reload(cx);
                    }
                    Err(message) => {
                        this.error = Some(message.clone());
                        if message.contains("session expired") {
                            cx.defer(move |cx| cx.dispatch_action(&crate::actions::OpenAuth));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// チェック状態をトグルする。
    pub fn toggle_item(&mut self, cx: &mut Context<Self>, item_id: &str) {
        let (next, slug) = {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            let current = db::checklist::get_item(db, item_id)
                .ok()
                .flatten()
                .map(|item| item.is_checked != 0)
                .unwrap_or(false);
            let slug = self.selected_slug.clone().unwrap_or_default();
            let _ = db::checklist::set_checked(db, item_id, !current);
            (!current, slug)
        };
        let _ = slug;
        let _ = next;
        self.reload(cx);
    }

    /// 試し読みページを取得してビューアーで開く。
    pub fn fetch_sample(&mut self, cx: &mut Context<Self>, item: CheckedItem) {
        log::info!("fetch_sample: {}", item.id);
        let Some(product_id) = item.product_id.clone() else {
            return;
        };
        self.busy = true;
        self.error = None;
        let handle = cx.entity();
        let state = AppState::global(cx);
        let tbf_client = state.tbf.clone();
        let db = state.db_pool.clone();
        let item_id = item.id.clone();
        let open_item_id = item_id.clone();
        type SamplePages = Vec<(String, u32, u32)>;
        let task: gpui_kit::Task<Result<SamplePages, String>> =
            cx.background_executor().spawn(async move {
                let mut client = tbf_client.lock();
                let pages = client
                    .product_sample_pages(&product_id)
                    .map_err(|e| e.to_string())?;
                let mut stored: Vec<(String, u32, u32)> = Vec::new();
                // 旧ページを先に 1 回だけ削除してから全ページを保存する
                {
                    let _ = db::samples::delete_for_item(&db, &item_id);
                }
                for page in pages {
                    let bytes = client.download(&page.url).map_err(|e| e.to_string())?;
                    use base64::Engine;
                    let data = base64::engine::general_purpose::STANDARD.encode(&bytes);
                    let (width, height) = match (page.width, page.height) {
                        (Some(w), Some(h)) => (w, h),
                        _ => decode_size(&bytes).unwrap_or((0, 0)),
                    };
                    {
                        let _ = db::samples::insert_sample_page(
                            &db,
                            &db::samples::SamplePageRow {
                                id: uuid::Uuid::new_v4().to_string(),
                                checklist_item_id: item_id.clone(),
                                product_id: Some(product_id.clone()),
                                page_number: page.page_number,
                                image_url: Some(page.url.clone()),
                                image_data: Some(data.clone()),
                                mime_type: "image/jpeg".to_string(),
                                width: Some(width),
                                height: Some(height),
                                file_size: Some(bytes.len() as i64),
                                fetched_at: chrono::Utc::now().to_rfc3339(),
                            },
                        );
                    }
                    stored.push((data, width as u32, height as u32));
                }
                Ok(stored)
            });
        cx.spawn(async move |_window, cx| {
            let result = task.await;
            handle.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(pages) if !pages.is_empty() => {
                        // 同じウィンドウのリーダーで開く（workspace が処理）
                        let action = crate::actions::OpenSampleReader {
                            item_id: open_item_id.clone().into(),
                        };
                        cx.defer(move |cx| cx.dispatch_action(&action));
                    }
                    Ok(_) => this.error = Some("試し読み画像がありません".into()),
                    Err(message) => {
                        this.error = Some(message.clone());
                        if message.contains("session expired") {
                            cx.defer(move |cx| cx.dispatch_action(&crate::actions::OpenAuth));
                        }
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }
}

fn decode_size(bytes: &[u8]) -> Option<(i64, i64)> {
    let image = image::load_from_memory(bytes).ok()?;
    Some((image.width() as i64, image.height() as i64))
}

/// 試し読み画像を同じウィンドウのリーダーで開く（workspace が処理）。
impl Render for ChecklistView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // 追い出し済みサムネイルの GPU テクスチャを解放する（window を持つここで行う）
        self.release_evicted_thumbs(window);
        // Web と同じ 2 画面構成: 選択中イベントがなければ一覧、あれば詳細
        if self.selected_slug.is_none() {
            self.render_event_list(window, cx).into_any_element()
        } else {
            self.render_event_detail(window, cx).into_any_element()
        }
    }
}

impl ChecklistView {
    /// イベントの日付を「YYYY/MM/DD」形式に整形（Web の toLocaleDateString("ja-JP") 相当）。
    fn format_date_ja(&self, date: &str) -> String {
        let digits: Vec<&str> = date.split('-').collect();
        if digits.len() == 3 {
            format!("{}/{}/{}", digits[0], digits[1], digits[2])
        } else {
            date.to_string()
        }
    }

    /// Web の formatEventDate 相当。
    fn format_event_date(&self, event: &TbfEvent) -> String {
        if event.is_cancelled != 0 {
            return event
                .event_date
                .as_deref()
                .map(|d| format!("{} (中止)", self.format_date_ja(d)))
                .unwrap_or_else(|| "日付未設定".to_string());
        }
        if event.event_format == "online"
            && let (Some(start), Some(end)) = (
                event.event_start_date.as_deref(),
                event.event_end_date.as_deref(),
            )
        {
            return format!(
                "{} 〜 {}",
                self.format_date_ja(start),
                self.format_date_ja(end)
            );
        }
        event
            .event_date
            .as_deref()
            .map(|d| self.format_date_ja(d))
            .unwrap_or_else(|| "日付未設定".to_string())
    }

    /// イベントの日付値（ソート用。日付なしは負の無限大相当）。
    fn event_date_value(&self, event: &TbfEvent) -> (i32, i32, i32) {
        event
            .event_date
            .as_deref()
            .and_then(|d| {
                let parts: Vec<&str> = d.split('-').collect();
                if parts.len() == 3 {
                    Some((
                        parts[0].parse().unwrap_or(0),
                        parts[1].parse().unwrap_or(0),
                        parts[2].parse().unwrap_or(0),
                    ))
                } else {
                    None
                }
            })
            .unwrap_or((0, 0, 0))
    }

    /// イベント一覧（Web の ChecklistEventList 相当）。
    fn render_event_list(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let handle = cx.entity();
        let events = self.events.clone();
        let visible_older_count = self.visible_older_count;
        let border = cx.theme().border;
        let muted = cx.theme().muted;
        let muted_fg = cx.theme().muted_foreground;
        let primary = cx.theme().primary;
        let danger = cx.theme().danger;

        // ソート: featured 先頭、非 featured は日付降順（Web と同じ）
        let mut featured: Vec<TbfEvent> = Vec::new();
        let mut non_featured: Vec<TbfEvent> = Vec::new();
        for event in &events {
            if event.is_featured != 0 {
                featured.push(event.clone());
            } else {
                non_featured.push(event.clone());
            }
        }
        non_featured.sort_by_key(|e| std::cmp::Reverse(self.event_date_value(e)));
        // tbf17 より前は「これより以前のイベントを表示」で段階表示
        let boundary = non_featured
            .iter()
            .position(|e| e.slug.as_deref() == Some("tbf17"))
            .unwrap_or(2.min(non_featured.len()));
        let recent = non_featured[..boundary].to_vec();
        let older = non_featured[boundary..].to_vec();
        let visible_older = older[..visible_older_count.min(older.len())].to_vec();
        let has_more_older = older.len() > visible_older_count;

        let card = |event: &TbfEvent| {
            let handle = handle.clone();
            let event = event.clone();
            let name = event.event_name.clone();
            let date_text = self.format_event_date(&event);
            let icon = if event.is_cancelled != 0 {
                Icon::new(IconName::TriangleAlert)
                    .size(px(24.0))
                    .text_color(danger)
                    .into_any_element()
            } else if event.event_format == "online" {
                Icon::new(IconName::Globe)
                    .size(px(24.0))
                    .text_color(primary)
                    .into_any_element()
            } else if event.event_format == "hybrid" {
                Icon::new(IconName::Building2)
                    .size(px(24.0))
                    .text_color(primary)
                    .into_any_element()
            } else {
                Icon::new(IconName::Calendar)
                    .size(px(24.0))
                    .text_color(primary)
                    .into_any_element()
            };
            let slug = event.slug.clone().unwrap_or_else(|| event.id.clone());
            div()
                .id(SharedString::from(format!("event-card-{slug}")))
                .flex()
                .flex_row()
                .items_center()
                .gap_4()
                .rounded_lg()
                .border_1()
                .border_color(border)
                .bg(cx.theme().background)
                .p_4()
                .hover(|style| style.bg(muted.opacity(0.3)))
                .cursor_pointer()
                .on_click(move |_, _window, cx| {
                    handle.update(cx, |this, cx| this.select_event(cx, &slug));
                })
                .child(
                    div()
                        .flex()
                        .items_center()
                        .justify_center()
                        .w(px(48.0))
                        .h(px(48.0))
                        .rounded_full()
                        .bg(primary.opacity(0.1))
                        .child(icon),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .min_w_0()
                        .flex_1()
                        .child(
                            div()
                                .text_sm()
                                .font_weight(FontWeight::SEMIBOLD)
                                .truncate()
                                .child(name),
                        )
                        .child(
                            div()
                                .flex()
                                .flex_row()
                                .items_center()
                                .gap_1()
                                .text_sm()
                                .text_color(muted_fg)
                                .child(
                                    Icon::new(IconName::Calendar)
                                        .size(px(14.0))
                                        .text_color(muted_fg),
                                )
                                .child(date_text),
                        ),
                )
                .child(
                    Icon::new(IconName::ChevronRight)
                        .size(px(20.0))
                        .text_color(muted_fg),
                )
        };

        div()
            .size_full()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(
                div()
                    .mx_auto()
                    .w(px(720.0))
                    .py_8()
                    .px_4()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .child(
                        div()
                            .text_2xl()
                            .font_weight(FontWeight::SEMIBOLD)
                            .child("チェックリスト(イベント一覧)"),
                    )
                    .child(if events.is_empty() {
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .rounded_lg()
                            .border_1()
                            .border_color(border)
                            .bg(muted.opacity(0.3))
                            .px_4()
                            .py_12()
                            .text_center()
                            .child(
                                div().mb_4().rounded_full().bg(muted).p_4().child(
                                    Icon::new(IconName::Calendar)
                                        .size(px(32.0))
                                        .text_color(muted_fg),
                                ),
                            )
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("イベントがありません"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(muted_fg)
                                    .child("まだチェックリスト用のイベントがありません。"),
                            )
                            .into_any_element()
                    } else {
                        div()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .children(
                                featured
                                    .iter()
                                    .chain(recent.iter())
                                    .chain(visible_older.iter())
                                    .map(|event| card(event).into_any_element()),
                            )
                            .child(if has_more_older {
                                div()
                                    .id("load-more-older-events")
                                    .w_full()
                                    .rounded_lg()
                                    .border_1()
                                    .border_color(border)
                                    .px_4()
                                    .py_3()
                                    .text_sm()
                                    .text_color(muted_fg)
                                    .text_center()
                                    .hover(|style| style.bg(muted.opacity(0.3)))
                                    .cursor_pointer()
                                    .on_click({
                                        let handle = handle.clone();
                                        move |_, _window, cx| {
                                            handle.update(cx, |this, cx| {
                                                this.visible_older_count += 5;
                                                cx.notify();
                                            });
                                        }
                                    })
                                    .child("これより以前のイベントを表示")
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            })
                            .into_any_element()
                    }),
            )
    }

    /// イベント詳細（Web の Checklist 相当: テーブル + 並び替え + ページネーション）。
    fn render_event_detail(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let busy = self.busy;
        let error = self.error.clone();
        let toast = self.toast.clone();
        let last_sync = self.last_sync.clone();
        let selected_slug = self.selected_slug.clone().unwrap_or_default();
        let handle = cx.entity();
        let events = self.events.clone();
        let items = self.items.clone();
        let sort_field = self.sort_field;
        let sort_ascending = self.sort_ascending;
        let page = self.page;
        let border = cx.theme().border;
        let muted = cx.theme().muted;
        let muted_fg = cx.theme().muted_foreground;
        let danger = cx.theme().danger;

        let event = events
            .iter()
            .find(|e| e.slug.as_deref() == Some(selected_slug.as_str()))
            .cloned();

        // 並び替え（Web の sortedItems 相当）
        let mut sorted = items.clone();
        sorted.sort_by(|a, b| {
            let ordering = match sort_field {
                SortField::CircleName => a.circle_name.cmp(&b.circle_name),
                SortField::SpaceNumber => a.space_number.cmp(&b.space_number),
                SortField::SortOrder => a.sort_order.cmp(&b.sort_order),
            };
            if sort_ascending {
                ordering
            } else {
                ordering.reverse()
            }
        });

        // ページネーション（Web の PAGE_SIZE = 50）
        const PAGE_SIZE: usize = 50;
        let total_pages = sorted.len().div_ceil(PAGE_SIZE).max(1);
        let start = page * PAGE_SIZE;
        let paginated: Vec<CheckedItem> = sorted[start.min(sorted.len())..]
            .iter()
            .take(PAGE_SIZE)
            .cloned()
            .collect();

        // テーブル行（Web の ChecklistItemRow 相当）
        let item_row = |item: &CheckedItem| {
            let handle = handle.clone();
            let circle = item.circle_name.clone();
            let space = item.space_number.clone();
            let title = item.product_title.clone();
            let price = item
                .price
                .map(|p| format!("¥{p}"))
                .unwrap_or_else(|| "¥-".to_string());
            let purchased = item.is_purchased != 0;
            let has_product = item.product_id.is_some();
            let sample_fetched = item.sample_fetch_attempted_at.is_some();
            let item_for_sample = item.clone();
            // render では毎回デコードせず、バックグラウンドで用意した
            // キャッシュ（thumbnail_images）だけを参照する
            let thumbnail = self.thumbnail_images.get(&item.id).cloned();

            div()
                .id(SharedString::from(format!("checklist-item-{}", item.id)))
                .flex()
                .flex_row()
                .items_center()
                .gap_3()
                .p_3()
                .border_b_1()
                .border_color(border)
                .hover(|style| style.bg(muted.opacity(0.5)))
                // 表紙（Web の aspect-[3/4] w-40 相当）
                .child(
                    div()
                        .relative()
                        .w(px(80.0))
                        .h(px(107.0))
                        .rounded_md()
                        .bg(muted)
                        .overflow_hidden()
                        .child(if let Some(image) = thumbnail {
                            img(image)
                                .w_full()
                                .h_full()
                                .object_fit(gpui_kit::ObjectFit::Cover)
                                .into_any_element()
                        } else {
                            div()
                                .w_full()
                                .h_full()
                                .flex()
                                .items_center()
                                .justify_center()
                                .text_xs()
                                .text_color(muted_fg)
                                .child(truncate_text(&circle, 8))
                                .into_any_element()
                        })
                        // 購入済/未購入バッジ
                        .child(
                            div()
                                .absolute()
                                .top_0()
                                .left_0()
                                .right_0()
                                .text_center()
                                .text_size(px(10.0))
                                .font_weight(FontWeight::BOLD)
                                .text_color(gpui_kit::white())
                                .py_0p5()
                                .bg(if purchased {
                                    gpui_kit::rgb(0x10b981).opacity(0.8)
                                } else {
                                    gpui_kit::rgb(0xec4899).opacity(0.8)
                                })
                                .child(if purchased { "購入済" } else { "未購入" }),
                        )
                        // サンプルバッジ
                        .child(if has_product {
                            div()
                                .absolute()
                                .bottom_0()
                                .left_0()
                                .right_0()
                                .text_center()
                                .text_size(px(10.0))
                                .font_weight(FontWeight::BOLD)
                                .text_color(gpui_kit::white())
                                .py_0p5()
                                .bg(gpui_kit::rgba(0x00000099))
                                .child(if sample_fetched {
                                    "サンプルを読む"
                                } else {
                                    "サンプル未取得"
                                })
                                .into_any_element()
                        } else {
                            div().into_any_element()
                        }),
                )
                // スペース名/サークル名
                .child(
                    div()
                        .w(px(140.0))
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_sm().child(space))
                        .child(
                            div()
                                .text_xs()
                                .text_color(muted_fg)
                                .truncate()
                                .child(circle),
                        ),
                )
                // 書籍名/価格
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .gap_1()
                        .child(div().text_sm().truncate().child(if title.is_empty() {
                            "-".to_string()
                        } else {
                            title
                        }))
                        .child(div().text_xs().text_color(muted_fg).child(price)),
                )
                // 試し読み
                .child(
                    Button::new(format!("sample-{}", item.id))
                        .cursor_pointer()
                        .label(if sample_fetched {
                            "開く"
                        } else {
                            "試し読み"
                        })
                        .disabled(!has_product || busy)
                        .cursor_pointer()
                        .on_click({
                            let handle = handle.clone();
                            move |_, _window, cx| {
                                handle.update(cx, |this, cx| {
                                    this.fetch_sample(cx, item_for_sample.clone());
                                });
                            }
                        }),
                )
        };

        div()
            .size_full()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            .child(
                div()
                    .mx_auto()
                    .w(px(960.0))
                    .py_8()
                    .px_4()
                    .flex()
                    .flex_col()
                    .gap_4()
                    // 一覧に戻る（最上部・左寄せ）
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .child(
                                Button::new("back-to-list").cursor_pointer()
                                    .label("← イベント一覧に戻る")
                                    .ghost()
                                    .small().cursor_pointer().on_click({
                                let handle = handle.clone();
                                move |_, _window, cx| {
                                    handle.update(cx, |this, cx| {
                                        this.back_to_list(cx);
                                    });
                                }
                                                                }),
                            ),
                    )
                    // ヘッダー: イベント名 + 日付
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(event.as_ref().map(|e| e.event_name.clone()).unwrap_or_default()),
                            )
                            .child(if let Some(event) = &event {
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_1()
                                    .text_sm()
                                    .text_color(muted_fg)
                                    .child(
                                        Icon::new(IconName::Calendar)
                                            .size(px(14.0))
                                            .text_color(muted_fg),
                                    )
                                    .child(self.format_event_date(event))
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            })
                            // ポーリング ON/OFF
                            .child(if let Some(event) = &event {
                                let poll_slug = event.slug.clone().unwrap_or_default();
                                let poll_enabled = event.poll_sync_enabled != 0;
                                let poll_handle = handle.clone();
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_2()
                                    .child(
                                        Switch::new("event-poll-sync-toggle")
                                            .checked(poll_enabled)
                                            .cursor_pointer()
                                            .on_click(move |checked, _window, cx| {
                                                let slug = poll_slug.clone();
                                                poll_handle.update(cx, |this, cx| {
                                                    let state = AppState::global(cx);
                                                    let db = &state.db_pool;
                                                    let _ = db::checklist::set_poll_enabled(db, &slug, *checked);
                                                    this.reload(cx);
                                                });
                                            }),
                                    )
                                    .child(
                                        div()
                                            .text_sm()
                                            .text_color(muted_fg)
                                            .child("このイベントについて、技術書典手から最新状況を同期する"),
                                    )
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            }),
                    )
                    // ツールバー
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .flex_wrap()
                            .child(if !sorted.is_empty() {
                                div()
                                    .text_sm()
                                    .text_color(muted_fg)
                                    .child(format!(
                                        "{}件{}",
                                        sorted.len(),
                                        last_sync
                                            .map(|t| format!(" ({t})"))
                                            .unwrap_or_default()
                                    ))
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            })
                            .child(
                                Button::new("checklist-sync").cursor_pointer()
                                    .icon(AppIcon::RefreshCw)
                                    .label("同期")
                                    .disabled(busy).cursor_pointer().on_click({
                                let handle = handle.clone();
                                move |_, _window, cx| {
                                    handle.update(cx, |this, cx| this.sync(cx));
                                }
                                                                }),
                            )
                            .child(div().text_sm().text_color(muted_fg).child("並び替え:"))
                            .children(
                                [
                                    (SortField::SortOrder, "順序"),
                                    (SortField::CircleName, "サークル名"),
                                    (SortField::SpaceNumber, "スペース"),
                                ]
                                .into_iter()
                                .map(|(field, label)| {
                                    let active = sort_field == field;
                                    let handle = handle.clone();
                                    let mut button = Button::new(format!("sort-{label}")).cursor_pointer()
                                        .label(format!(
                                            "{}{}",
                                            label,
                                            if active {
                                                if sort_ascending { " ↑" } else { " ↓" }
                                            } else {
                                                ""
                                            }
                                        ))
                                        .small();
                                    if active {
                                        button = button.secondary();
                                    }
                                    button.cursor_pointer().on_click(move |_, _window, cx| {
                                        handle.update(cx, |this, cx| {
                                            this.toggle_sort(cx, field);
                                        });
                                    })
                                }),
                            ),
                    )
                    // テーブル
                    .child(if sorted.is_empty() {
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .justify_center()
                            .rounded_lg()
                            .border_1()
                            .border_color(border)
                            .bg(muted.opacity(0.3))
                            .px_4()
                            .py_12()
                            .text_center()
                            .child(
                                div()
                                    .mb_4()
                                    .rounded_full()
                                    .bg(muted)
                                    .p_4()
                                    .child(
                                        Icon::new(IconName::CircleCheck)
                                            .size(px(32.0))
                                            .text_color(muted_fg),
                                    ),
                            )
                            .child(
                                div()
                                    .text_lg()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("チェックリストは空です"),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(muted_fg)
                                    .child(
                                        "まだアイテムが登録されていません。上の同期ボタンからチェックリストを同期できます。",
                                    ),
                            )
                            .into_any_element()
                    } else {
                        div()
                            .rounded_lg()
                            .border_1()
                            .border_color(border)
                            .overflow_hidden()
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_3()
                                    .p_3()
                                    .bg(muted.opacity(0.5))
                                    .child(div().w(px(80.0)).text_sm().font_weight(FontWeight::MEDIUM).child("表紙"))
                                    .child(div().w(px(140.0)).text_sm().font_weight(FontWeight::MEDIUM).child("スペース名/サークル名"))
                                    .child(div().flex_1().text_sm().font_weight(FontWeight::MEDIUM).child("書籍名/価格"))
                                    .child(div().w(px(80.0))),
                            )
                            .children(paginated.iter().map(|item| item_row(item).into_any_element()))
                            .into_any_element()
                    })
                    // ページネーション
                    .child(if total_pages > 1 {
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .justify_center()
                            .gap_4()
                            .py_2()
                            .child(
                                Button::new("page-prev").cursor_pointer()
                                    .label("前へ")
                                    .disabled(page == 0).cursor_pointer().on_click({
                                let handle = handle.clone();
                                move |_, _window, cx| {
                                    handle.update(cx, |this, cx| {
                                        this.page = this.page.saturating_sub(1);
                                        cx.notify();
                                    });
                                }
                                                                }),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(muted_fg)
                                    .child(format!("{} / {}", page + 1, total_pages)),
                            )
                            .child(
                                Button::new("page-next").cursor_pointer()
                                    .label("次へ")
                                    .disabled(page + 1 >= total_pages).cursor_pointer().on_click({
                                let handle = handle.clone();
                                move |_, _window, cx| {
                                    handle.update(cx, |this, cx| {
                                        this.page = (this.page + 1).min(total_pages - 1);
                                        cx.notify();
                                    });
                                }
                                                                }),
                            )
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    })
                    // ステータス（トースト・エラー）
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_3()
                            .child(if let Some(toast) = toast {
                                div()
                                    .text_sm()
                                    .text_color(muted_fg)
                                    .child(toast)
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            })
                            .child(if let Some(message) = error {
                                div()
                                    .text_sm()
                                    .text_color(danger)
                                    .child(message)
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            }),
                    ),
            )
    }
}

/// チェックリストのサムネイル（base64 データ URI）を RenderImage に変換する。
/// 画像バイト列をデコードし、サムネイル表示用に縮小した RenderImage を返す。
/// 表紙は最大 144x192 の表示なので、幅 256px に縮小してメモリ使用量を抑える。
fn decode_thumbnail(bytes: &[u8]) -> Option<Arc<RenderImage>> {
    let rgba = shrink_rgba(bytes)?;
    let (width, height) = rgba.dimensions();
    let mut flat = rgba.into_raw();
    // BGRA に変換（gpui の RenderImage は BGRA 前提）
    for pixel in flat.chunks_exact_mut(4) {
        pixel.swap(0, 2);
    }
    Some(Arc::new(RenderImage::new([image::Frame::new(
        image::RgbaImage::from_raw(width, height, flat)?,
    )])))
}

/// 画像バイト列を幅 256px 上限まで縮小した RGBA 画像にする。
fn shrink_rgba(bytes: &[u8]) -> Option<image::RgbaImage> {
    let format = image::guess_format(bytes).ok()?;
    let img = image::load_from_memory_with_format(bytes, format).ok()?;
    let (w, h) = img.dimensions();
    let img = if w > 256 {
        let nh = ((h as f32) * (256.0 / w as f32)).round().max(1.0) as u32;
        img.resize(256, nh, image::imageops::FilterType::Lanczos3)
    } else {
        img
    };
    Some(img.to_rgba8())
}

/// ダウンロードした表紙画像を縮小 JPEG の base64 に変換する（DB 保存用）。
/// 元画像（最大 2MB 級）をそのまま保存せず、表示に十分なサイズで永続化する。
fn thumbnail_jpeg_base64(bytes: &[u8]) -> Option<String> {
    let rgba = shrink_rgba(bytes)?;
    let (width, height) = rgba.dimensions();
    // JPEG は RGB のみ（アルファなし）なので変換してからエンコードする
    let rgb = image::DynamicImage::ImageRgba8(rgba).to_rgb8();
    let mut jpeg = Vec::new();
    let mut encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 80);
    encoder
        .encode(&rgb, width, height, image::ExtendedColorType::Rgb8)
        .ok()?;
    drop(encoder);
    Some(base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        &jpeg,
    ))
}

/// Web の truncateText 相当。
fn truncate_text(text: &str, max_length: usize) -> String {
    if text.chars().count() <= max_length {
        text.to_string()
    } else {
        let truncated: String = text.chars().take(max_length).collect();
        format!("{truncated}...")
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::AppContext as _;
    use gpui_kit::TestAppContext;

    use thundoku_core::db::checklist as checklist_db;

    use super::*;

    fn seed_event(cx: &mut TestAppContext, slug: &str, name: &str) {
        cx.update(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            checklist_db::upsert_event(
                db,
                &checklist_db::TbfEvent {
                    id: slug.into(),
                    site_id: "techbookfest".into(),
                    slug: Some(slug.into()),
                    tbf_event_id: Some(format!("Event:{slug}")),
                    event_name: name.into(),
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
    }

    fn seed_item(cx: &mut TestAppContext, id: &str, slug: &str, circle: &str) {
        cx.update(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            checklist_db::upsert_item(
                db,
                &checklist_db::CheckedItem {
                    id: id.into(),
                    event_id: slug.into(),
                    circle_name: circle.into(),
                    space_number: "あ-01".into(),
                    memo: String::new(),
                    is_checked: 0,
                    sort_order: 0,
                    tbf_circle_id: None,
                    product_id: Some("p1".into()),
                    product_title: "本".into(),
                    thumbnail_url: None,
                    thumbnail_data: None,
                    price: Some(1000),
                    is_purchased: 0,
                    sample_fetch_attempted_at: None,
                    created_at: "2026-08-21 00:00:00".into(),
                },
            )
            .unwrap();
        });
    }

    #[gpui_kit::test]
    async fn loads_events_and_items_for_selection(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_event(cx, "tbf20", "技術書典20");
        seed_event(cx, "tbf19", "技術書典19");
        seed_item(cx, "item-1", "tbf20", "サークルA");
        seed_item(cx, "item-2", "tbf19", "サークルB");
        let view = cx.new(ChecklistView::new);
        assert_eq!(view.read_with(cx, |v, _| v.events.len()), 2);
        // Web と同じ: 最初はイベント一覧表示（選択なし）
        assert!(view.read_with(cx, |v, _| v.selected_slug.is_none()));
        assert!(view.read_with(cx, |v, _| v.items.is_empty()));
        // イベントを選択 → 詳細（アイテム）が読み込まれる
        cx.update(|cx| view.update(cx, |this, cx| this.select_event(cx, "tbf20")));
        assert_eq!(view.read_with(cx, |v, _| v.items.len()), 1);
        assert_eq!(
            view.read_with(cx, |v, _| v.items[0].circle_name.clone()),
            "サークルA".to_string()
        );
        // 一覧に戻る（Web の「← イベント一覧に戻る」相当）
        cx.update(|cx| view.update(cx, |this, cx| this.back_to_list(cx)));
        assert!(view.read_with(cx, |v, _| v.selected_slug.is_none()));
    }

    #[gpui_kit::test]
    async fn toggle_sort_changes_field_and_direction(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_event(cx, "tbf20", "技術書典20");
        let view = cx.new(ChecklistView::new);
        assert_eq!(
            view.read_with(cx, |v, _| v.sort_field),
            SortField::SortOrder
        );
        assert!(view.read_with(cx, |v, _| v.sort_ascending));
        // 別フィールド → 昇順で切り替え
        cx.update(|cx| view.update(cx, |this, cx| this.toggle_sort(cx, SortField::CircleName)));
        assert_eq!(
            view.read_with(cx, |v, _| v.sort_field),
            SortField::CircleName
        );
        assert!(view.read_with(cx, |v, _| v.sort_ascending));
        // 同じフィールド → 降順に反転
        cx.update(|cx| view.update(cx, |this, cx| this.toggle_sort(cx, SortField::CircleName)));
        assert!(!view.read_with(cx, |v, _| v.sort_ascending));
    }

    #[gpui_kit::test]
    async fn toggle_updates_checked_state(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_event(cx, "tbf20", "技術書典20");
        seed_item(cx, "item-1", "tbf20", "サークルA");
        let view = cx.new(ChecklistView::new);
        cx.update(|cx| view.update(cx, |this, cx| this.toggle_item(cx, "item-1")));
        let checked = cx.read(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            checklist_db::get_item(db, "item-1")
                .unwrap()
                .unwrap()
                .is_checked
        });
        assert_eq!(checked, 1);
    }

    #[test]
    fn decode_thumbnail_scales_down_large_images() {
        // 1024x1024 の画像を作ってデコード → 256px 幅に縮小される
        let img = image::RgbaImage::from_pixel(1024, 1024, image::Rgba([200, 100, 50, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let image = decode_thumbnail(bytes.get_ref()).expect("must decode");
        let data = image.as_bytes(0).expect("pixels");
        // 256x256x4 = 262144
        assert_eq!(data.len(), 256 * 256 * 4, "must be scaled to 256px width");
    }

    #[test]
    fn thumbnail_jpeg_base64_produces_small_jpeg() {
        // 1024x1024 の PNG → 縮小 JPEG base64（幅 256px に縮小）
        let img = image::RgbaImage::from_pixel(1024, 1024, image::Rgba([200, 100, 50, 255]));
        let mut bytes = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgba8(img)
            .write_to(&mut bytes, image::ImageFormat::Png)
            .unwrap();
        let b64 = thumbnail_jpeg_base64(bytes.get_ref()).expect("must convert");
        let decoded =
            base64::Engine::decode(&base64::engine::general_purpose::STANDARD, &b64).unwrap();
        assert!(
            matches!(image::guess_format(&decoded), Ok(image::ImageFormat::Jpeg)),
            "persisted thumbnail must be JPEG"
        );
        let loaded = image::load_from_memory(&decoded).unwrap();
        assert_eq!(loaded.width(), 256, "must be scaled to 256px width");
    }

    #[test]
    fn base64_thumbnail_decodes_to_image() {
        // 1x1 の透明 PNG（base64）
        let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        let decoded = base64::Engine::decode(&base64::engine::general_purpose::STANDARD, png_b64)
            .expect("base64 decodes");
        let image = decode_thumbnail(&decoded);
        assert!(
            image.is_some(),
            "valid base64 PNG must decode to a RenderImage"
        );
        let image = image.expect("decoded");
        assert!(
            image.as_bytes(0).is_some(),
            "render image must contain pixel data"
        );
    }

    /// テスト用の項目（id と並び順だけ指定）。
    fn test_item(id: &str, sort_order: i64) -> CheckedItem {
        CheckedItem {
            id: id.into(),
            event_id: "tbf20".into(),
            circle_name: "サークルA".into(),
            space_number: "あ-01".into(),
            memo: String::new(),
            is_checked: 0,
            sort_order,
            tbf_circle_id: None,
            product_id: Some("p1".into()),
            product_title: "本".into(),
            thumbnail_url: None,
            thumbnail_data: Some("eA==".into()),
            price: Some(1000),
            is_purchased: 0,
            sample_fetch_attempted_at: None,
            created_at: "2026-08-21 00:00:00".into(),
        }
    }

    /// テスト用のビュー（非同期タスクを伴わず決定的に検証する）。
    fn view_with_items(items: Vec<CheckedItem>) -> ChecklistView {
        ChecklistView {
            events: Vec::new(),
            selected_slug: Some("tbf20".into()),
            items,
            busy: false,
            error: None,
            toast: None,
            last_sync: None,
            sort_field: SortField::SortOrder,
            sort_ascending: true,
            page: 0,
            visible_older_count: 0,
            thumbnail_images: HashMap::new(),
            thumbnail_order: VecDeque::new(),
            evicted_thumb_images: Vec::new(),
            thumbnail_fetching: HashSet::new(),
            fetch_failed: HashSet::new(),
        }
    }

    /// テスト用の 1x1 RenderImage（デコード不要で軽い）。
    fn dummy_render_image() -> Arc<RenderImage> {
        Arc::new(RenderImage::new([image::Frame::new(
            image::RgbaImage::from_pixel(1, 1, image::Rgba([0, 0, 0, 255])),
        )]))
    }

    #[test]
    fn decode_pending_limits_to_current_page() {
        // 60 件の項目を直接構築（非同期タスクなしで決定的に検証する）
        let items: Vec<CheckedItem> = (0..60)
            .map(|i| test_item(&format!("item-{i:02}"), i as i64))
            .collect();
        let view = view_with_items(items);
        // ページ 0 では先頭 50 件が対象（1 度の実行は 20 件まで）
        let pending = view.decode_pending();
        assert_eq!(pending.len(), 20, "page 0: at most 20 per batch");
        // ページ 1 では残り 10 件だけが対象（全 60 件を一括デコードしない）
        let mut page1 = view;
        page1.page = 1;
        let pending = page1.decode_pending();
        assert_eq!(pending.len(), 10, "page 1: only the last 10 items");
        // fetch_failed に記録された項目は再試行されない
        page1.fetch_failed.insert("item-59".into());
        let pending = page1.decode_pending();
        assert_eq!(pending.len(), 9, "failed item must be skipped");
    }

    #[test]
    fn cache_thumb_keeps_cache_within_limit() {
        let mut view = view_with_items(Vec::new());
        // 上限を超えてキャッシュしても、保持数と挿入順は上限で止まる
        for i in 0..(MAX_THUMB_CACHE + 10) {
            view.cache_thumb(format!("item-{i:02}"), dummy_render_image());
        }
        assert_eq!(
            view.thumbnail_images.len(),
            MAX_THUMB_CACHE,
            "保持数が上限を超えている"
        );
        assert_eq!(
            view.thumbnail_order.len(),
            MAX_THUMB_CACHE,
            "挿入順の管理数が上限を超えている"
        );
        // 最古の 10 件は追い出され、GPU テクスチャ解放待ちに積まれる
        for i in 0..10 {
            assert!(
                !view.thumbnail_images.contains_key(&format!("item-{i:02}")),
                "最古のサムネイルが残っている"
            );
        }
        assert!(
            view.thumbnail_images
                .contains_key(&format!("item-{:02}", MAX_THUMB_CACHE + 9)),
            "最新のサムネイルが追い出されている"
        );
        assert_eq!(
            view.evicted_thumb_images.len(),
            10,
            "追い出した画像が解放待ちに積まれていない"
        );
    }

    #[test]
    fn cache_thumb_keeps_thumbnails_of_visible_page() {
        // 表示中のページ（先頭 50 件）のサムネイルは、他ページのサムネイルを
        // 上限以上に足しても追い出されない（表示中に表紙が消えない）
        let mut view = view_with_items(
            (0..50)
                .map(|i| test_item(&format!("item-{i:02}"), i as i64))
                .collect(),
        );
        for i in 0..50 {
            view.cache_thumb(format!("item-{i:02}"), dummy_render_image());
        }
        for i in 0..MAX_THUMB_CACHE {
            view.cache_thumb(format!("other-{i:02}"), dummy_render_image());
        }
        assert!(
            view.thumbnail_images.len() <= MAX_THUMB_CACHE,
            "保持数が上限を超えている"
        );
        for i in 0..50 {
            assert!(
                view.thumbnail_images.contains_key(&format!("item-{i:02}")),
                "表示中のサムネイルが追い出された"
            );
        }
    }

    #[gpui_kit::test]
    async fn fetched_thumbnails_are_decoded_only_for_current_page(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        // 1x1 の透明 PNG（base64）。取得済みサムネイルとして渡す
        let png_b64 = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==";
        let view = cx.new(ChecklistView::new);
        cx.update(|cx| {
            view.update(cx, |this, cx| {
                // 60 件（ページ 0 = 先頭 50 件、ページ 1 = 残り 10 件）
                this.items = (0..60)
                    .map(|i| test_item(&format!("item-{i:02}"), i as i64))
                    .collect();
                // ページ 0 とページ 1 の項目の取得結果をまとめて反映する
                this.apply_fetched_thumbnails(
                    cx,
                    vec![
                        ("item-01".into(), png_b64.into()),
                        ("item-59".into(), png_b64.into()),
                    ],
                    vec!["item-02".into()],
                );
            });
        });
        cx.run_until_parked();
        view.read_with(cx, |this, _| {
            // 取得結果は items に反映され、取得中フラグも解除される
            let item = this.items.iter().find(|item| item.id == "item-01").unwrap();
            assert!(
                item.thumbnail_data.is_some(),
                "取得データが反映されていない"
            );
            assert!(
                !this.thumbnail_fetching.contains("item-01"),
                "取得中フラグが残っている"
            );
            // 失敗した項目は再取得しない
            assert!(
                this.fetch_failed.contains("item-02"),
                "失敗が記録されていない"
            );
            // 描画中のページ（ページ 0）だけデコードされる
            assert!(
                this.thumbnail_images.contains_key("item-01"),
                "表示中のサムネイルがデコードされていない"
            );
            assert!(
                !this.thumbnail_images.contains_key("item-59"),
                "非表示ページのサムネイルまでデコードしている"
            );
        });
    }

    #[gpui_kit::test]
    async fn sample_store_writes_base64_rows(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        seed_event(cx, "tbf20", "技術書典20");
        seed_item(cx, "item-1", "tbf20", "サークルA");
        cx.update(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            db::samples::insert_sample_page(
                db,
                &db::samples::SamplePageRow {
                    id: "sp-1".into(),
                    checklist_item_id: "item-1".into(),
                    product_id: Some("p1".into()),
                    page_number: 1,
                    image_url: Some("https://example.com/1.jpg".into()),
                    image_data: Some("aGVsbG8=".into()),
                    mime_type: "image/jpeg".into(),
                    width: Some(100),
                    height: Some(140),
                    file_size: Some(5),
                    fetched_at: "2026-08-21T00:00:00Z".into(),
                },
            )
            .unwrap();
        });
        let rows = cx.read(|cx| {
            let state = AppState::global(cx);
            let db = &state.db_pool;
            db::samples::list_for_item(db, "item-1").unwrap()
        });
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].image_data.as_deref(), Some("aGVsbG8="));
    }
}
