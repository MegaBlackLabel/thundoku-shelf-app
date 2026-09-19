# 05. ビューアーとドメインロジック（読書状態 / 付箋 / タグ / 検索 / 並び替え / 統計）

> `docs/spec/README.md`（索引）から参照される設計仕様書の一部。
> 本章は **ビューアーの内部仕様と、本棚まわりのドメイン規則**。読み手は AI（別言語での再実装・Web 版への移植を想定）。
> 事実にはアンカー付き。断定できない事項は章末の「不明点 / 推測」に分離してある。
> 情報源: crates/app/src/components/image_viewer/**, crates/app/src/views/{reader,bookshelf}.rs, crates/core/src/{db,tags}

情報源（すべて読み取りのみ。行番号は 2026-09-20 時点のリポジトリ状態）:

- `crates/app/src/components/image_viewer/mod.rs`
- `crates/app/src/views/reader.rs`
- `crates/app/src/views/bookshelf.rs`
- `crates/core/src/db/progress.rs` / `db/notes.rs` / `db/view_history.rs` / `db/page_views.rs` / `db/tags.rs`
- `crates/core/src/tags.rs`
- `crates/core/src/import/mod.rs`（タグ生成の呼び出し元）
- `crates/app/src/workspace.rs`（未読バッジ・リーダー終了処理）
- docs: `docs/features.md` / `docs/database.md` / `docs/import-patterns.md`

---

## 1. 画像ビューアー（`crates/app/src/components/image_viewer/mod.rs`）

### 1.1 定数（数値・単位付き）

| 定数 | 値 | 単位 | 用途 / 意味 | アンカー |
|---|---|---|---|---|
| `NOTE_ICON_SIZE` | `28.0` | px | 付箋アイコンの一辺。ボタン / アイコン / 単一表示の位置計算で共有（`pub`） | `crates/app/src/components/image_viewer/mod.rs:32` |
| `AUTOPLAY_MIN_MS` | `3000` | ms | 自動再生間隔の下限（`pub`） | `crates/app/src/components/image_viewer/mod.rs:34` |
| `AUTOPLAY_MAX_MS` | `30000` | ms | 自動再生間隔の上限（`pub`） | `crates/app/src/components/image_viewer/mod.rs:35` |
| `AUTOPLAY_DEFAULT_MS` | `5000` | ms | 自動再生間隔の既定値（`pub`） | `crates/app/src/components/image_viewer/mod.rs:36` |
| `AUTOPLAY_STEP_MS` | `1000` | ms | スライダーの刻み / `+` `-` ボタンの増減量（`pub`） | `crates/app/src/components/image_viewer/mod.rs:37` |
| `OVERLAY_HIDE_MS` | `5000` | ms | 無操作でトップパネル / ボトムドックを自動非表示にするまでの時間 | `crates/app/src/components/image_viewer/mod.rs:38` |
| `WIN_TITLE_BAR_HEIGHT` | `workspace::TITLE_BAR_HEIGHT` を参照（`36.0` Windows / `34.0` 非 Windows） | px | タイトルバーの高さ。フィット計算でウィンドウ高から差し引く（リーダーはタイトルバーの下に描く） | `crates/app/src/components/image_viewer/mod.rs:44` |
| `PAGE_THUMB_WIDTH` | `200.0` | px | ページ一覧サムネイルの幅（`load_thumb` の縮小先） | `crates/app/src/components/image_viewer/mod.rs:48` |
| `MAX_PAGE_THUMBS` | `120` | 枚 | ページ一覧サムネイルの同時保持上限（超えたら古い順に捨てる。1 枚 ≒ 0.2MB → 24MB 程度） | `crates/app/src/components/image_viewer/mod.rs:51` |
| `PAGE_TILE_WIDTH` | `108.0` | px | ページ一覧タイル幅 + gap（列数の計算に使う） | `crates/app/src/components/image_viewer/mod.rs:53` |
| `PAGE_LIST_HEIGHT` | `420.0` | px | ページ一覧パネルの高さ | `crates/app/src/components/image_viewer/mod.rs:55` |
| `PAGE_LIST_ROW_HEIGHT` | `196.0` | px | ページ一覧 1 行の高さ（固定。`measure_all` を使わない理由は §1.7） | `crates/app/src/components/image_viewer/mod.rs:58` |
| `SCROLL_CACHE_BUDGET_BYTES` | `384 * 1024 * 1024` = 402,653,184 = **384 MiB** | バイト | スクロールモードで保持するデコード済みページの総バイト予算（`pub`） | `crates/app/src/components/image_viewer/mod.rs:439` |
| `page_cache_budget`（初期値） | `SCROLL_CACHE_BUDGET_BYTES` | バイト | 上記の実行時値。テストから小さくできる | `crates/app/src/components/image_viewer/mod.rs:577` |
| 背景色（`viewer_bg()`） | `0xf4f4f5` | RGB | ビューアー背景（真っ白だと本の白ページと区別できないため薄グレー） | `crates/app/src/components/image_viewer/mod.rs:238-241` |
| 角丸（`rounded_web`） | `20.0` | px | Web 版 `rounded-[1.25rem]` 相当。全 4 隅 | `crates/app/src/components/image_viewer/mod.rs:243-250` |
| 角丸（`rounded_top_web`） | `20.0` | px | 上端のみ（ホバーで背景が変わっても角が丸いまま） | `crates/app/src/components/image_viewer/mod.rs:252-257` |

### 1.2 表示モードと設定の永続化

- `ViewMode` = `Single` / `Spread` / `Scroll`（`crates/app/src/components/image_viewer/mod.rs:327-331`）。
- パネル種別 `PanelView` = `Menu` / `PageList` / `Shortcuts` / `AutoplaySettings`（`crates/app/src/components/image_viewer/mod.rs:335-339`）。
- サイト別設定キー `setting_key(base)` = `"{base}.{site}"`、サイトが無ければ `base`（`crates/app/src/components/image_viewer/mod.rs:483-489`）。
- ビューアは対象の本を `BookScope { id, site_id }` で持つ（試し読みなど本に紐づかない表示は `None`）（`crates/app/src/components/image_viewer/mod.rs:342-352`）。

| 設定 | キー | 保存値 | 復元の優先順位 | 既定 | アンカー |
|---|---|---|---|---|---|
| 表示モード | `viewer.mode.{site}` → `viewer.mode` | `"spread"` / `"scroll"` / それ以外 = `"single"` | サイト別 → グローバル → サイト既定 | `site_id` が `fanza` / `dlsite` なら `Spread`、他は `Single` | `crates/app/src/components/image_viewer/mod.rs:491-510` |
| 綴じ方向 | `books.page_turn`（本ごと）→ `viewer.page_turn.{site}` → `viewer.page_turn` | `"right-to-left"` = 右綴じ / `"left-to-right"` = 左綴じ | **本ごと → サイト別 → グローバル → サイト既定** | `site_id` が `fanza` / `dlsite` なら右綴じ、他は左綴じ（技術書典は左綴じ想定） | `crates/app/src/components/image_viewer/mod.rs:465-480`, `:511-526` |
| 自動再生間隔 | `viewer.autoplay_interval.{site}` → `viewer.autoplay_interval` | ミリ秒の文字列（`u64` parse 失敗時は既定） | 同上 | `AUTOPLAY_DEFAULT_MS` = 5000 ms | `crates/app/src/components/image_viewer/mod.rs:528-541` |
| ホイール方向 | `viewer.wheel_direction` | `"down-to-next"` = 下スクロールで次へ / `"up-to-next"` = 上スクロールで次へ（未知の値・未設定は既定に倒す） | サイト別キー無し（グローバルのみ） | `down-to-next` | `crates/app/src/views/settings.rs:103-105`、`crates/app/src/components/image_viewer/mod.rs:571-580` |

- モード変更は `set_mode()` が `viewer.mode.{site}` に書き込む（`crates/app/src/components/image_viewer/mod.rs:1619-1631`）。
- 綴じ方向は `set_binding()` が **その本の行**（`books.page_turn`）に書き込む。本ごとの指定はサイト別設定より優先され、サイト別設定（他の本の既定）は書き換えない。本に紐づかない表示では保存しない（`crates/app/src/components/image_viewer/mod.rs:1587-1604`、`crates/core/src/db/books.rs:465-493`）。
- 自動再生間隔は `set_autoplay_interval(ms)` が `ms.clamp(AUTOPLAY_MIN_MS, AUTOPLAY_MAX_MS)` したうえで **サイト別キーとグローバルキーの両方**に書き込む（後方互換）（`crates/app/src/components/image_viewer/mod.rs:1751-1768`）。

### 1.3 ページ決定式（受入基準の核心）

用語: `current_page` は **0-indexed**（フィールド定義 `crates/app/src/components/image_viewer/mod.rs:347`）。`N` = `loader.page_count()`。

| モード | 表示ページ集合 | ページ送り量 | アンカー |
|---|---|---|---|
| 単一 (Single) | `[current_page]` | ±1 | `crates/app/src/components/image_viewer/mod.rs:1382-1394`、`crates/app/src/components/image_viewer/mod.rs:1258-1266` |
| 見開き (Spread) | `pages = [current_page]`、`current_page + 1 < N` なら `pages.push(current_page + 1)`。その後 `page_turn_right_to_left == true` なら `pages.reverse()` | 通常 ±2 / Shift 押し ±1 | 同上 |
| スクロール (Scroll) | 全ページ `0..N` を縦に並べる（`render` の `pages`）。`visible_pages()` も `(0..N)` を返す | —（スクロール位置から算出） | `crates/app/src/components/image_viewer/mod.rs:2295-2299`、`crates/app/src/components/image_viewer/mod.rs:1337-1343` |

- ページ送り式: `target = (current_page as i64 + direction * step).clamp(0, max as i64) as usize`、`max = page_count().saturating_sub(1)`（`crates/app/src/components/image_viewer/mod.rs:1258-1272`）。
- `step` の式: `if mode == ViewMode::Spread && !shift { 2 } else { 1 }`（`crates/app/src/components/image_viewer/mod.rs:1259-1263`）。
- クランプ（`clamp()`）: `count = page_count().max(1)`、`max = count - 1`、`current_page > max` なら `max` に切り詰め（`crates/app/src/components/image_viewer/mod.rs:1232-1237`）。
- 初期ページ: `initial = initial_page.min(page_count.saturating_sub(1))`（保存値が範囲外でも安全）（`crates/app/src/components/image_viewer/mod.rs:465`）。
- 見開きの「2 ページ揃うまで表示しない」= アトミック表示: `spread_ready = pages.iter().all(|&index| self.images.get(index).is_some_and(|slot| slot.is_some()))` が false の間は白い箱（`gpui_kit::white()`）を出す（`crates/app/src/components/image_viewer/mod.rs:2386-2389`、`crates/app/src/components/image_viewer/mod.rs:2416-2440`）。

**スクロールモードの `current_page` 決定式（2 経路。両方とも `last_scroll_page` で再入防止）**

1. 描画ポーリング（常時）: `top = scroll_handle.top_item()`。初回フレームは不採用（`scroll_top_initialized = true` を立てるだけ）。2 フレーム目以降、`top < total && top != last_scroll_page` のとき `last_scroll_page = top; current_page = top`（`crates/app/src/components/image_viewer/mod.rs:2235-2246`）。
2. ホイールイベント（次フレームで再評価）: ページ高 `page_h = (container_w * 0.8) / aspect`（`aspect <= 0` なら `container_w`）、`page = floor(offset_y / page_h)`。`page < page_count() && page != last_scroll_page` のとき更新（`crates/app/src/components/image_viewer/mod.rs:2737-2765`）。

**表示サイズの式**

| 対象 | 式 | アンカー |
|---|---|---|
| ビューポート | `vwp = (window.width, window.height - WIN_TITLE_BAR_HEIGHT)` | `crates/app/src/components/image_viewer/mod.rs:2391-2393` |
| スクロールの 1 ページ枠 | 幅 = コンテナ幅 × `0.8`（`DefiniteLength::Fraction(0.8)`）、高さ = `aspect_ratio(aspect)` で幅から決まる | `crates/app/src/components/image_viewer/mod.rs:2361-2364` |
| 見開きの基準高 | `animated_scale = 1.0 + (zoom_scale - 1.0) * zoom_progress`、`total_aspect = Σ(aspect_i)`、`fit = (vwp.x / (ch * total_aspect)).min(1.0)`、`base_h = ch * fit`（`ch = vwp.y`） | `crates/app/src/components/image_viewer/mod.rs:2396-2412` |
| 見開きの各ページ箱 | 幅 `base_h * aspect * animated_scale`、高さ `base_h * animated_scale` | `crates/app/src/components/image_viewer/mod.rs:2416-2434` |
| 見開きペア配置 | `pair_w = base_h * total_aspect * animated_scale`、`pair_h = base_h * animated_scale`、`pair_left = (vwp.x - pair_w)/2 + pan.x`、`pair_top = (vwp.y - pair_h)/2 + pan.y` | `crates/app/src/components/image_viewer/mod.rs:2472-2475` |
| 単一の contain | `img_w = if cw/ch > aspect { ch*aspect } else { cw }`、`img_h = img_w / aspect`、`img_left = max((cw-img_w)/2, 0)`、`img_top = max((ch-img_h)/2, 0)` | `crates/app/src/components/image_viewer/mod.rs:2562-2574` |
| アスペクト不明時の既定 | `100.0 / 141.0`（≒ 0.7092） | `crates/app/src/components/image_viewer/mod.rs:2352`、`crates/app/src/components/image_viewer/mod.rs:2405`、`crates/app/src/components/image_viewer/mod.rs:2420`、`crates/app/src/components/image_viewer/mod.rs:2566`、`crates/app/src/components/image_viewer/mod.rs:2741` |

### 1.4 見開きの左右判定・綴じ方向・付箋の側

- `spread_pages()` の戻り値の**先頭が左、2 番目が右**（`Vec<usize>` の順序が画面の左右に対応する）。
  - 左綴じ（`page_turn_right_to_left == false`）: `[current_page, current_page + 1]` → ページ番号の小さい方が左。
  - 右綴じ（true）: `reverse()` するので `[current_page + 1, current_page]` → **ページ番号の小さい方が右**。
  - テストで固定: 左綴じ `spread_pages() == vec![0, 1]`（`crates/app/src/components/image_viewer/mod.rs:3485-3505`）/ 右綴じ `vec![1, 0]`（`crates/app/src/components/image_viewer/mod.rs:3585-3615`）。
- 付箋の `spread_side` 判定式: `pair = spread_pages()` の長さが 2 のとき、`pair.first() == Some(&index)` なら `Left`、それ以外は `Right`。長さ 1（単一表示 / 最終ページ単独）は `None`（`crates/app/src/components/image_viewer/mod.rs:1419-1427`）。
- 付箋から開くときの復元（`ReaderView::for_book_at`）: `side.is_some()` なら **Spread モードに切替** → `set_page(target)`（`target = (page - 1).max(0)`）→ `pair = spread_pages()`、`on_left = pair.first() == Some(&target)`、`want_left = side == SpreadSide::Left`。`on_left != want_left && target > 0` なら `set_page(target - 1)`（狙った側に来るまで **1 ページだけ** 戻す）（`crates/app/src/views/reader.rs:480-511`、`crates/app/src/views/reader.rs:505-509`）。
- キーボードの左右は綴じ方向で反転: `rtl == true` のとき `right`/`l` = 前へ、`left`/`h` = 次へ（Shift 付きは 1 ページ版）（`crates/app/src/components/image_viewer/mod.rs:1953-1993`）。
- ボトムドックの「前へ / 次へ」ボタンも `mirror_nav = mode == Spread && page_turn_right_to_left` のとき入れ替える（`crates/app/src/components/image_viewer/mod.rs:2254`、`crates/app/src/components/image_viewer/mod.rs:2930-2950`）。
- ナビ帯: 単一表示は画像矩形上に左右 `edge_w = max(img_w * 0.1, 40.0)` px（`crates/app/src/components/image_viewer/mod.rs:2575`）。見開きはウィンドウ左右端の幅 10%（`DefiniteLength::Fraction(0.1)`）で、`mode == Spread && !zoomed` のときだけ出す（`crates/app/src/components/image_viewer/mod.rs:2789-2849`）。スクロールでは非表示。

### 1.5 ズームとパン

| 操作 | 式 / 値 | アンカー |
|---|---|---|
| ホイールズーム / ページ送り | `delta = (Pixels(y) or Lines(y) * 20.0) * -0.001`、Ctrl 押下時のみ。`hovering_ui` 中とスクロールモードでは無効。**通常ホイール（Ctrl なし）は Single / Spread で 1 ノッチ（`WHEEL_TURN_LINES = 3.0` 行の累積）= 1 ページ送り**（方向は `viewer.wheel_direction`）、Scroll では不適用（ページ番号の更新のみで送りはしない） | `crates/app/src/components/image_viewer/mod.rs:2773-2872`、`crates/app/src/components/image_viewer/mod.rs:46` |
| `adjust_zoom(delta)` | `zoomed = true`、`zoom_scale = (zoom_scale + delta).clamp(1.0, 8.0)`。`zoom_scale <= 1.0` なら `zoomed = false` + `pan_offset = (0,0)` | `crates/app/src/components/image_viewer/mod.rs:1720-1734` |
| ダブルクリック（サイクル） | 未ズーム → `zoom_scale = 2.0` / `zoom_scale < 5.9` → `6.0` / それ以上 → 解除（`zoomed = false`, `zoom_scale = 1.5`, `pan_offset = (0,0)`, `pan_velocity = (0,0)`） | `crates/app/src/components/image_viewer/mod.rs:1739-1762` |
| ダブルクリックのデバウンス | 直前の呼び出しから **250 ms 未満**なら無視 | `crates/app/src/components/image_viewer/mod.rs:1741-1745` |
| ダブルクリック判定 | 直前クリックから **400 ms 未満**の再クリック（単一 / 見開き / ズーム中の 3 経路とも同値） | `crates/app/src/components/image_viewer/mod.rs:2043-2045`、`crates/app/src/components/image_viewer/mod.rs:2142-2144`、`crates/app/src/components/image_viewer/mod.rs:2490-2492` |
| `zoom_scale` 初期値 / 解除時 | `1.5`（`new` のフィールド初期化と `set_loader` の両方） | `crates/app/src/components/image_viewer/mod.rs:548`、`crates/app/src/components/image_viewer/mod.rs:643` |
| ズームアニメ | `transition(("viewer-zoom","zoom"), zoomed ? 1.0 : 0.0, Transition::new(200ms).ease(ease_out_cubic))` | `crates/app/src/components/image_viewer/mod.rs:2283-2289` |
| ズーム中の描画サイズ | `w = width_px * (1 + (scale - 1) * zoom_progress)`、`h = height_px * (1 + (scale - 1) * zoom_progress)`（`object_fit: Fill` + `flex_shrink_0`） | `crates/app/src/components/image_viewer/mod.rs:2113-2114` |
| ズーム解除条件（ドック非表示） | `is_dock_hidden() == zoomed`。オーバーレイ表示は `overlay = overlay_visible && !zoomed` | `crates/app/src/components/image_viewer/mod.rs:1846-1848`、`crates/app/src/components/image_viewer/mod.rs:2255` |
| パン可動域（単一 / 汎用） | `pan_max_for(scale, viewport, aspect)`: contain サイズ `(iw, ih)` を `viewport.x / viewport.y > aspect` で分岐（`(v.y*aspect, v.y)` か `(v.x, v.x/aspect)`）、返り値 `max(0, (iw*scale - viewport.x)/2)`、`max(0, (ih*scale - viewport.y)/2)` | `crates/app/src/components/image_viewer/mod.rs:302-317` |
| パン可動域（見開き） | `max(0, (base_h*total_aspect*animated_scale - vwp.x)/2)`、`max(0, (base_h*animated_scale - vwp.y)/2)` | `crates/app/src/components/image_viewer/mod.rs:2512-2520` |
| パン移動 | 目標 `target = clamp(pan_offset + drag_delta, ±pan_max)`、実際は `pan_offset + (target - pan_offset) * 0.6` で補間 | `crates/app/src/components/image_viewer/mod.rs:1786-1797` |
| パン速度 | `(dx / 0.016).clamp(-4000.0, 4000.0)` px/秒相当（1 イベントの移動量を 16 ms 換算で記録） | `crates/app/src/components/image_viewer/mod.rs:1798-1802` |
| 慣性 | 16 ms ごとに `pan_offset += v * 0.016`、`v *= 0.94`。`|v.x| < 0.2 && |v.y| < 0.2` で停止。開始時に `|v| < 0.5` なら慣性なし | `crates/app/src/components/image_viewer/mod.rs:1812-1843`（`0.5` 判定は `crates/app/src/components/image_viewer/mod.rs:1812`） |

### 1.6 ページキャッシュ（バイト予算・追い出し・GPU 解放）

| 項目 | 規則 | アンカー |
|---|---|---|
| 1 ページのデコード後バイト数 | `decoded_page_bytes = w * h * 4`（RGBA）。寸法が取れない場合は `None` | `crates/app/src/components/image_viewer/mod.rs:442-446` |
| スクロールの保持半径 | `per_page = decoded_page_bytes(current_page).unwrap_or(4 * 1024 * 1024).max(1)`、`radius = (page_cache_budget / per_page / 2).max(1)` | `crates/app/src/components/image_viewer/mod.rs:1346-1351` |
| スクロールの保持範囲 | `first = current_page.saturating_sub(radius)`、`last = min(current_page + radius, total - 1)`。`[first, last]` 外の `images[i]` を `take()` し、`window.drop_image(image)` で **GPU(sprite atlas) からも解放**。残した範囲は `ensure_loaded` で読み込む | `crates/app/src/components/image_viewer/mod.rs:1357-1378`（`drop_image` は `crates/app/src/components/image_viewer/mod.rs:1371`） |
| スクロールの保持数（テスト） | 4,000×4,000 px = 64 MiB/ページ相当と申告する 20 ページの本で「3 枚以上 13 枚以下」を検証（予算 384 MiB ÷ 64 MiB ÷ 2 = 3） | `crates/app/src/components/image_viewer/mod.rs:3360-3400` |
| 単一 / 見開きの追い出し | ページ送り時、`index.abs_diff(current_page) > 30` のスロットを `None` にする（**直近 30 ページ保持**）。スクロールモードではこのクリアを行わない | `crates/app/src/components/image_viewer/mod.rs:1310-1320`（条件は `crates/app/src/components/image_viewer/mod.rs:1317`） |
| 単一 / 見開きのプリロード | ページ変更時に `offset in 1..=2` の前後（`current_page - offset` と `min(current_page + offset, count)`）を `ensure_loaded` | `crates/app/src/components/image_viewer/mod.rs:1322-1331` |
| スクロールモード開始時のプリロード | `new()` は半径 `scroll_keep_radius()` 分だけ先読みし、`scroll_handle.scroll_to_item(current_page)` で位置復元。`set_mode(Scroll)` は「まだ `None` のスロット全部」を読み込む | `crates/app/src/components/image_viewer/mod.rs:586-596`、`crates/app/src/components/image_viewer/mod.rs:1585-1596` |
| 二重ロード防止 | `loading: HashSet<usize>` に登録済み / `images[index].is_some()` なら即 return | `crates/app/src/components/image_viewer/mod.rs:1511-1519` |
| ロード失敗 | `images[index] = None` とし、`load_error` が未設定なら最初のエラー文字列を保持。ページ一覧の上部に赤字で「ページ画像を読み込めませんでした: {error}（再ダウンロードしてください）」を出し、通常表示でも説明 + エラー文字列を出す（白画面にしない） | `crates/app/src/components/image_viewer/mod.rs:1531-1537`、`crates/app/src/components/image_viewer/mod.rs:1211-1225`、`crates/app/src/components/image_viewer/mod.rs:2167-2190` |
| pack バイト列キャッシュ | `pack_bytes: OnceLock<Option<(String, Arc<Vec<u8>>)>>`。初回 `load` 時に 1 回だけディスクから読む（`get_or_init` は復号前にロックを解放） | `crates/app/src/components/image_viewer/mod.rs:150-154`、`crates/app/src/components/image_viewer/mod.rs:186-203` |
| pack 鍵キャッシュ | `pack_key: OnceLock<Option<[u8;32]>>`。PBKDF2 100k 回は `get_or_init` で 1 回だけ | `crates/app/src/components/image_viewer/mod.rs:155-156`、`crates/app/src/components/image_viewer/mod.rs:208-210` |
| 自己参照 | `self_handle: WeakEntity<ImageViewer>`（強参照の自己参照は解放漏れになる。回帰テストあり） | `crates/app/src/components/image_viewer/mod.rs:407-410`、`crates/app/src/components/image_viewer/mod.rs:3432-3445` |

### 1.7 サムネイルキャッシュ（ページ一覧）

| 項目 | 規則 | アンカー |
|---|---|---|
| 1 枚の生成 | `PageLoader::load_thumb` の既定実装 = `load(index)` → `downscale_render_image(&image, PAGE_THUMB_WIDTH = 200px)`。縮小は `image::imageops::resize` + `FilterType::Triangle`。すでに幅 ≤ 200px なら元画像を clone（縮小しない） | `crates/app/src/components/image_viewer/mod.rs:66-77`、`crates/app/src/components/image_viewer/mod.rs:79-101` |
| 保持上限 | `thumbs: Vec<Option<Arc<RenderImage>>>` + `thumb_order: VecDeque<usize>`（読み込み完了順）。`evict_thumbs()` が `while thumb_order.len() > MAX_PAGE_THUMBS (120)` で最古を `take()` し `pending_image_drops` に積む | `crates/app/src/components/image_viewer/mod.rs:841-854` |
| GPU 解放 | `render()` 冒頭で `release_evicted_images(window)` → `window.drop_image(image)` | `crates/app/src/components/image_viewer/mod.rs:856-860`、`crates/app/src/components/image_viewer/mod.rs:2221` |
| 読み込み範囲 | ページ一覧は **行単位の仮想化**（`gpui_kit::list`）。可視行のタイル（`start..end`）だけが `ensure_thumb_loaded` を呼ぶ | `crates/app/src/components/image_viewer/mod.rs:1108-1135` |
| 列数 / 行数 | `columns = (((panel_width - 24.0) / PAGE_TILE_WIDTH).floor() as usize).max(1)`、`rows = total.div_ceil(columns)`（`panel_width = window.bounds().size.width`） | `crates/app/src/components/image_viewer/mod.rs:1115-1117`、`crates/app/src/components/image_viewer/mod.rs:2997` |
| 行の高さ | `PAGE_LIST_ROW_HEIGHT = 196.0` px（`ListState::new(0, ListAlignment::Top, px(196.0))`）。`measure_all` は使わない（全行構築 → 全ページ読み込みになるため） | `crates/app/src/components/image_viewer/mod.rs:58`、`crates/app/src/components/image_viewer/mod.rs:566-570` |
| タイル内訳 | サムネ枠 `w(100px)` + `aspect_ratio(100.0 / 141.0)` + ページ番号（`index + 1`）。現在ページは primary 枠 + 「現在」ラベル | `crates/app/src/components/image_viewer/mod.rs:1168-1210`（`aspect_ratio` は `crates/app/src/components/image_viewer/mod.rs:1176`） |
| 孤立サムネの扱い | 読み込み失敗は `log::warn!` のみで枠だけ出す（壊れた 1 枚で一覧全体を止めない） | `crates/app/src/components/image_viewer/mod.rs:826-833` |
| メモリ削減の根拠（コメント） | 旧実装は一覧を開いた時点で全ページのフル解像度を読んでおり、実測 192 ページ = 142 秒 / 8.7 GB、3,321 ページ = 41 分 / 146 GB 相当だった（R6） | `crates/app/src/components/image_viewer/mod.rs:795-803` |

### 1.8 ローダー（ページ供給）

| 実装 | 供給元 | ページ数 | 寸法 | 備考 | アンカー |
|---|---|---|---|---|---|
| `PackPageLoader` | `.opfspack`（`document_images.pack_entry_path`） | `images.len()` | `(image.width, image.height)` | `book_id_of()` は `documents::get_document(document_id).book_id`（解決できなければ空文字） | `crates/app/src/components/image_viewer/mod.rs:145-156`、`crates/app/src/components/image_viewer/mod.rs:158-203`、`crates/app/src/components/image_viewer/mod.rs:205-211` |
| `Base64PageLoader` | メモリ上の base64（技術書典 試し読み `pages: Vec<(String, u32, u32)>`） | `pages.len()` | 各行の `(w, h)` | `base64::engine::general_purpose::STANDARD` でデコード | `crates/app/src/components/image_viewer/mod.rs:214-236` |

- デコード: webp（`data.get(8..12) == Some(b"WEBP")`）は **libwebp で直接 RGBA デコード**（`WebPGetInfo` → `WebPDecodeRGBA` → `WebPFree`）。その他は `image::load_from_memory` → `to_rgba8()`。理由コメント: image クレートの webp デコーダーは 1000px で 1 秒超（実測）（`crates/app/src/components/image_viewer/mod.rs:268-300`）。
- 色順: `RenderImage` は **BGRA** を期待するため `rgba_to_render_image` が `pixel.swap(0, 2)` する（`crates/app/src/components/image_viewer/mod.rs:319-325`、回帰テスト `crates/app/src/components/image_viewer/mod.rs:4239-4259`）。
- ページロード時に `log::info!("page load: index={index} bytes={} read={:.1}ms decode={:.1}ms", ...)` を出す（`crates/app/src/components/image_viewer/mod.rs:214-222`）。

### 1.9 オーバーレイ・自動再生・パネル・レンディション切替

| 項目 | 規則 | アンカー |
|---|---|---|
| オーバーレイ表示条件 | `overlay = overlay_visible && !zoomed`。パネル / ドックのアニメは `transition(("viewer-overlay","overlay"), ..., Transition::new(400ms).ease(ease_out_cubic))` | `crates/app/src/components/image_viewer/mod.rs:2255`、`crates/app/src/components/image_viewer/mod.rs:2260-2266` |
| パネル / ドックの背景 | ライト = 白 80%（`white().alpha(0.8)`。パネル・ドック共通）/ ダーク = 黒 70%（パネル）/ 黒 60%（ドック） | `crates/app/src/components/image_viewer/mod.rs:2272-2281` |
| 自動非表示 | `OVERLAY_HIDE_MS`(5000 ms) 後、`hide_generation` が一致するときだけ `overlay_visible = false`。`active_panel.is_some() || hovering_ui` 中はタイマーを張らない | `crates/app/src/components/image_viewer/mod.rs:1626-1644` |
| 連続ページ送りでの自動非表示 | `page_turn_count += 1` が **2 回以上**かつ表示中なら `overlay_visible = false` + `hide_generation += 1`。`show_overlay()` で `page_turn_count = 0`。オーバーレイ表示操作でカウントはリセット | `crates/app/src/components/image_viewer/mod.rs:1267-1276`、`crates/app/src/components/image_viewer/mod.rs:1603-1608` |
| ホバー挙動 | 画像面ホバーで非表示なら表示 / 表示中ならタイマー再起動。ドック / パネル内は `hovering_ui` を立てて自動非表示を止め、外れたら再起動 | `crates/app/src/components/image_viewer/mod.rs:2688-2706`、`crates/app/src/components/image_viewer/mod.rs:2925-2943` |
| 中央ダブルクリック | `event.click_count == 2` で `toggle_overlay()` | `crates/app/src/components/image_viewer/mod.rs:2695-2698` |
| 自動再生開始条件 | `mode == Scroll` なら `start_autoplay` は何もしない。ループは `interval_ms` ごとに `next_page()`。最終ページで `autoplay = false` にして停止 | `crates/app/src/components/image_viewer/mod.rs:1657-1692` |
| スライダー（自動再生） | `SliderState::new().max(30000).min(3000).step(1000).default_value(5000)`（**max を先に設定**。min を先にすると min > max で panic する） | `crates/app/src/components/image_viewer/mod.rs:1908-1918` |
| スライダー（ページ） | `.min(0.0).max((count - 1) as f32).step(1.0).default_value(current_page)`。observer が `value.start().round() as usize` を `set_page`（同一ページガードでループ防止）。逆方向の同期は差 > 0.5 のときだけ | `crates/app/src/components/image_viewer/mod.rs:1920-1950` |
| ページ番号表示 | `format!("{current} / {total}")`（`current = current_page + 1`）。ページ番号入力欄は幅 72 px | `crates/app/src/components/image_viewer/mod.rs:2959-2961` |
| フォーカス維持 | 入力（rename / page）フォーカス中、または付箋ダイアログ表示中は `window.focus(&self.focus_handle)` を呼ばない（毎フレーム奪うと入力できない） | `crates/app/src/components/image_viewer/mod.rs:2215-2224` |
| レンディション切替 | `page_list_select_format` が `pending_action = Some((content_id, Some(format_id)))`。`format_id` が空（旧データ）なら切替せずページ一覧を開くだけ。親が `take_action()` で拾う | `crates/app/src/components/image_viewer/mod.rs:776-797`、`crates/app/src/components/image_viewer/mod.rs:747` |
| 表示中マーク | `is_current = current_format_id.as_deref() == Some(format_id.as_str())`。行は緑チェック + 「表示中」 | `crates/app/src/components/image_viewer/mod.rs:1016-1030` |
| 切替行のラベル | `named = !content.display_name.is_empty() && content.display_name != "本文"`。`named` なら `display_name`、でなければ `format.label` | `crates/app/src/components/image_viewer/mod.rs:868-894`（`named` は `crates/app/src/components/image_viewer/mod.rs:873`） |
| 形式の補足文 `format_subtitle` | `image` → `画像 {n}ファイル` / `pdf` → `PDF {n}ページ` / `epub` → `EPUB ドキュメント` / `audio` → `音声 {n}ファイル` / `video` → `動画 {n}ファイル` / その他 → `{n} ページ` | `crates/app/src/components/image_viewer/mod.rs:133-143` |
| コンテンツ改名 | `rename_content` が `pending_rename = Some((content_id, display_name))` を立てる。親（`ReaderView::apply_rename`）が DB（`contents::rename`）と pack（`import::rename_content_in_pack`）に反映 | `crates/app/src/components/image_viewer/mod.rs:684-688`、`crates/app/src/views/reader.rs:409-427` |

### 1.10 キーボード（ビューアー内）

| キー | 動作 | アンカー |
|---|---|---|
| `right` / `l` | 左綴じ: 次ページ / 右綴じ: 前ページ。Shift 付きは 1 ページ版（`next_page_shift` / `prev_page_shift`） | `crates/app/src/components/image_viewer/mod.rs:1959-1978` |
| `left` / `h` | 左綴じ: 前ページ / 右綴じ: 次ページ。Shift 付きは 1 ページ版 | `crates/app/src/components/image_viewer/mod.rs:1979-1990` |
| `up` / `k` / `backspace` | `CloseReader` アクションを dispatch（本棚に戻る） | `crates/app/src/components/image_viewer/mod.rs:1981-1984` |
| `escape` | パネルが開いていれば閉じる、無ければメニューを開く（トグル） | `crates/app/src/components/image_viewer/mod.rs:1985-1993` |

- ルート要素は `track_focus(&self.focus_handle)` + `on_key_down` でキーを受ける（`crates/app/src/components/image_viewer/mod.rs:2670-2678`）。
- ショートカットパネル（`PanelView::Shortcuts`）の表示行: `次ページ →, l` / `前ページ ←, h` / `見開き: 1 ページずらす shift+→, shift+←` / `ダブルクリック 拡大` / `本棚に戻る ↑, k, backspace` / `メニュー表示 esc`（`crates/app/src/components/image_viewer/mod.rs:3028-3036`）。

---

## 2. 読書状態と進捗の記録

### 2.1 `ReadingState::from_progress`（唯一の判定）

定義: `crates/core/src/db/progress.rs:47-53`（enum `ReadingState` は `crates/core/src/db/progress.rs:30-35`、`is_finished` は `crates/core/src/db/progress.rs:21-26`）。

```rust
pub fn from_progress(progress: Option<&ReadingProgress>) -> Self {
    match progress {
        Some(p) if p.finished_at.is_some() || p.is_finished() => ReadingState::Read,
        Some(p) if p.current_page > 0 => ReadingState::Reading,
        _ => ReadingState::Unread,
    }
}
```

| 状態 | 判定式（優先順位どおり） | アンカー |
|---|---|---|
| `Read`（読了） | `finished_at.is_some()` **または** `is_finished()`（= `total_pages.is_some_and(|t| current_page >= t)`） | `crates/core/src/db/progress.rs:49`、`crates/core/src/db/progress.rs:21-26` |
| `Reading`（表示は「読んでいる途中」） | 上記以外で `current_page > 0`（1-indexed なので本を開いただけでは進捗行が作られず未読のまま） | `crates/core/src/db/progress.rs:50` |
| `Unread`（未読） | 進捗なし、または `current_page == 0` | `crates/core/src/db/progress.rs:51` |

境界の固定（テスト）: `None` → Unread / `(0, Some(10), false)` → Unread / `(1, Some(10), false)` → Reading / `(9, Some(10), false)` → Reading（最終ページ 1 つ手前は読了ではない）/ `(10, Some(10), false)` → Read / `(1, Some(10), true)` → Read（`finished_at` 優先）/ `(3, None, false)` → Reading（`crates/core/src/db/progress.rs:151-194`）。


**バッジ / ステータスの表示（表示と判定は別経路）**

| 表示 | 内容 | アンカー |
|---|---|---|
| カードのバッジ | `card.local` が無ければ `unwrap_or(ReadingState::Unread)`。`Read` → 「読了」（背景 `rgb(0xd1fae5)` / 文字 `rgb(0x047857)`）、`Reading` → 「読んでいる途中」（背景 `rgb(0xe0f2fe)` / 文字 `rgb(0x0369a1)`）、`Unread` → 「未読」（背景 `rgb(0xfef3c7)` / 文字 `rgb(0xb45309)`）を表紙左上に重ねる | `crates/app/src/views/bookshelf.rs:4043-4059` |
| リストのステータス | ローカル本のみ `Read` → `status_tag(database_id, "read", "既読", TagVariant::Success)` / `Reading` → `("reading", "読んでいる途中", TagVariant::Info)` / `Unread` → `("unread", "未読", TagVariant::Secondary)`。未ダウンロード本は「未読」ではなく `"not-downloaded"` のアイコンを出す | `crates/app/src/views/bookshelf.rs:4974`、`crates/app/src/views/bookshelf.rs:5017` |
| 表示とフィルタの差 | 未ダウンロード本はカード上「未読」バッジだが、`ReadFilter::Unread` は `card.local.map(|e| e.reading_state) == Some(Unread)` なので **未読フィルタに一致しない** | `crates/app/src/views/bookshelf.rs:3966`、`crates/app/src/views/bookshelf.rs:2195-2208` |

使用者（判定が 1 か所であることの根拠）: 本棚カード（`crates/app/src/views/bookshelf.rs:1355`）、フィルタ（`crates/app/src/views/bookshelf.rs:2216-2228`）、設定の冊数集計（`crates/app/src/views/settings.rs:256-259`）、履歴画面（`crates/app/src/views/history.rs:265-267`）、付箋画面（`crates/app/src/views/notes.rs:167-169`）、サイドバー未読バッジ（`crates/app/src/workspace.rs:483-484`）。

### 2.2 `ReadingProgress` / `reading_progress`

| フィールド | 型 | 単位・意味 | アンカー |
|---|---|---|---|
| `book_id` | `String` | 本 ID | `crates/core/src/db/progress.rs:7` |
| `content_id` | `String` | コンテンツ ID。`''` = 未指定（旧データ / 単一コンテンツ） | `crates/core/src/db/progress.rs:9` |
| `current_page` | `i64` | **1-indexed**。最終ページ = `total_pages` | `crates/core/src/db/progress.rs:10` |
| `total_pages` | `Option<i64>` | 総ページ数 | `crates/core/src/db/progress.rs:11` |
| `finished_at` | `Option<String>` | 読了日時。**一度セットすると消えない** | `crates/core/src/db/progress.rs:12-13` |
| `last_read_at` | `String` | 最終閲覧日時（`chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")`） | `crates/core/src/db/progress.rs:14`、`crates/app/src/views/reader.rs:670` |
| `scroll_position` | `f64` | スクロール位置。**現行コードは常に `0.0` を保存**（読書位置の復元には未使用） | `crates/core/src/db/progress.rs:15`、`crates/app/src/views/reader.rs:686` |

- 取得: `get(pool, book_id)` は **優先（primary）コンテンツ**の進捗を返す（`contents::primary_for_book` の ID、無ければ `''`）。`get_for(pool, book_id, content_id)` は指定コンテンツ（`crates/core/src/db/progress.rs:58-64` / `crates/core/src/db/progress.rs:66-77`）。
- `upsert` の SQL（逐語）:

```sql
INSERT INTO reading_progress (book_id, content_id, current_page, total_pages,
  finished_at, last_read_at, scroll_position) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
ON CONFLICT(book_id, content_id) DO UPDATE SET
  current_page = excluded.current_page,
  total_pages = excluded.total_pages,
  finished_at = CASE
    WHEN reading_progress.finished_at IS NOT NULL THEN reading_progress.finished_at
    ELSE excluded.finished_at
  END,
  last_read_at = excluded.last_read_at,
  scroll_position = excluded.scroll_position
```
（`crates/core/src/db/progress.rs:83-113`。`finished_at` が `None` でも `is_finished()` が真なら `last_read_at` を入れて保存する前処理がある: `crates/core/src/db/progress.rs:85-91`）
- `delete(pool, book_id)` = 全コンテンツ分削除 / `delete_for_content(pool, book_id, content_id)` = 指定コンテンツのみ（`crates/core/src/db/progress.rs:120-129` / `crates/core/src/db/progress.rs:131-146`）。
- ページ数表示用の行は取り込み時に **無いときだけ**作る（`seed_progress_if_absent`: `current_page: 0`, `total_pages: Some(n)`, `last_read_at: "2026-01-01 00:00:00"`）。再取得で読書位置を消さないため（`crates/app/src/views/bookshelf.rs:6936-6963`）。

### 2.3 `view_history`（セッション単位）

| 関数 | 内容 | アンカー |
|---|---|---|
| `start(pool, book_id)` | `INSERT INTO view_history (id, book_id, started_at, ended_at) SELECT hex(randomblob(16)), ?, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP RETURNING ...`（**id = 16 バイト乱数の hex 32 文字**。`ended_at` は開始時刻で初期化） | `crates/core/src/db/view_history.rs:23-35` |
| `end(pool, session_id)` | `UPDATE view_history SET ended_at = CURRENT_TIMESTAMP WHERE id = ?` | `crates/core/src/db/view_history.rs:37-45` |
| `view_count(pool, book_id)` | `SELECT COUNT(*) FROM view_history WHERE book_id = ?` | `crates/core/src/db/view_history.rs:59-67` |
| `total_duration_secs(pool, book_id)` | 下記 `view_stats` と同じ `SUM(julianday(...) * 86400)` を 1 冊に絞って `CAST(... AS INTEGER)` | `crates/core/src/db/view_history.rs:110-124` |
| `list_daily(pool)` | 全行を読み、`CURRENT_TIMESTAMP`（UTC）を `Local` に直して `(book_id, day)` で集約。`duration = (ended - started).num_seconds().max(0)`、`sessions += 1`、`last_started_at` は最大値。並びは `last_started_at` 降順 → `book_id` 昇順 | `crates/core/src/db/view_history.rs:142-198` |

**重要（コード上の事実）**: `view_history::touch` はアプリ本体から呼ばれていなかったため削除済み。`ended_at` を更新する直接の UPDATE はテスト専用ヘルパ（`crates/core/src/db/view_history.rs:195-199`）にのみ残る。

リーダー側の呼び出し:

- 開始: `ReaderView::for_book` が `db::view_history::start(&db, &book_id)`（**本を開くたびに 1 行**）（`crates/app/src/views/reader.rs:280-286`）。
- 終了: `ReaderView::end_session(cx)` が `db::view_history::end(...)`。呼び出し元は `WorkspaceView::close_reader`（`crates/app/src/workspace.rs:660-662`、`crates/app/src/views/reader.rs:146-151`）。
- 試し読み（`for_sample`）は `book_id` が `None` のため `view_session_id` も `None`（記録しない）（`crates/app/src/views/reader.rs:353-364`）。

### 2.4 `page_views`（ページ単位）

| 列 | 型 | 意味 | アンカー |
|---|---|---|---|
| `book_id` / `content_id` / `page_number` | TEXT / TEXT / INTEGER | 複合キー。`page_number` は **1-indexed** | `crates/core/src/db/page_views.rs:10-20` |
| `view_count` | `i64` | そのページが現在ページになった回数 | `crates/core/src/db/page_views.rs:17` |
| `total_seconds` | `f64` | 滞在合計（秒。小数可） | `crates/core/src/db/page_views.rs:18` |
| `last_viewed_at` | `String` | `CURRENT_TIMESTAMP`（UTC） | `crates/core/src/db/page_views.rs:19` |

```sql
-- record_view（+1 回）
INSERT INTO page_views (book_id, content_id, page_number, view_count, total_seconds,
  last_viewed_at) VALUES (?1, ?2, ?3, 1, 0, CURRENT_TIMESTAMP)
ON CONFLICT(book_id, content_id, page_number) DO UPDATE SET
  view_count = view_count + 1, last_viewed_at = CURRENT_TIMESTAMP

-- add_dwell（秒加算）
INSERT INTO page_views (... ) VALUES (?1, ?2, ?3, 0, ?4, CURRENT_TIMESTAMP)
ON CONFLICT(book_id, content_id, page_number) DO UPDATE SET
  total_seconds = total_seconds + excluded.total_seconds, last_viewed_at = CURRENT_TIMESTAMP
```
（`crates/core/src/db/page_views.rs:23-45`、`crates/core/src/db/page_views.rs:47-69`）
`for_book(pool, book_id)` は `ORDER BY content_id, page_number ASC`（`crates/core/src/db/page_views.rs:73-88`）。

**リーダーの記録タイミング（`ReaderView`）**

| タイミング | 内容 | アンカー |
|---|---|---|
| 本を開いた直後 | `spread_pages()` の各ページに `record_view`（見開きは左右 2 ページ） | `crates/app/src/views/reader.rs:286-300` |
| 表示ページ集合が変わったとき | `record_page_view`: `current_pages == last_pages` なら何もしない（画像ロード等の notify を弾く）。変わったら **離れる前の集合**の各ページに `add_dwell(secs / ページ数)`（`secs = now - last_page_at`、秒・小数。集合へ均等配分）→ `last_pages` を更新 → **新しい集合**の各ページに `record_view` → `last_page_at = now` | `crates/app/src/views/reader.rs:632-664` |
| リーダーを閉じるとき | `end_session`: 最後の集合に `add_dwell(started.elapsed().as_secs_f64() / ページ数)`（`secs > 0.0` のときのみ。均等配分） | `crates/app/src/views/reader.rs:153-170` |
| 試し読み | `book_id == None` のため `record_page_view` / `end_session` は何もしない | `crates/app/src/views/reader.rs:618-620` |

**単位の注意（コード上の事実）**: 見開きは **表示中のページ集合へ均等配分**する（`share = secs / self.last_pages.len().max(1)` を各ページに渡すので、合計が実時間になる）（`crates/app/src/views/reader.rs:161-169`、`crates/app/src/views/reader.rs:649-657`）。

### 2.5 進捗の保存（リーダー）

- `save_progress` はビューアーの notify ごとに呼ばれるが、`current_page + 1 == last_saved_page` なら**書き込まない**（`crates/app/src/views/reader.rs:655-665`）。
- 保存値: `current_page = ビューアーの current_page(0-indexed) + 1`、`total_pages = Some(page_count)`、`last_read_at = Utc::now()`（`"%Y-%m-%d %H:%M:%S"`）、`scroll_position = 0.0`（`crates/app/src/views/reader.rs:660-689`）。
- 読了判定: `total_pages > 0 && current_page >= total_pages` のとき `finished_at = Some(timestamp)`（`crates/app/src/views/reader.rs:672-676`）。
- 再開: `for_book` は `progress.current_page.max(1).saturating_sub(1) as usize` を初期ページにする（0 や負値は 1 ページ目に落とす。usize アンダーフローで最終ページに飛ぶ事故の防止）（`crates/app/src/views/reader.rs:219-226`）。コンテンツ切替時は `progress::get_for(book_id, content_key)` から同じ式で復元（`crates/app/src/views/reader.rs:130-137`）。
- リーダーを閉じると `close_reader` が `end_session` → 付箋画面 `reload` → 本棚 `reload` + `restore_selection` → `refresh_unread_count`（`crates/app/src/workspace.rs:659-679`）。

---

## 3. 付箋（`page_notes`）

### 3.1 モデル

| フィールド | 型 | 意味 | アンカー |
|---|---|---|---|
| `id` | `String` | 安定 ID。リーダーが作る式は `format!("note-{book_id}-{content}-{page}")`（`content` は `content_id`、空なら空文字、`page` は 1-indexed） | `crates/core/src/db/notes.rs:51`、`crates/app/src/views/reader.rs:598-599` |
| `book_id` / `content_id` / `page` | `String` / `String` / `i64` | `book_id + content_id + page` で **一意（1 ページ 1 件）**。`page` は **1-indexed** | `crates/core/src/db/notes.rs:52-56`、`crates/core/src/db/notes.rs:89-94` |
| `memo` | `String` | メモ本文（空でも付箋としては存在する） | `crates/core/src/db/notes.rs:57` |
| `spread_side` | `Option<SpreadSide>` | 付けたときの見開きの左右。単一表示で付けたら `None` | `crates/core/src/db/notes.rs:59` |
| `is_active` | `bool` | 付箋の ON / OFF。`false` = 外した状態（**メモは残る**） | `crates/core/src/db/notes.rs:61`、`crates/core/src/db/notes.rs:1-6` |
| `created_at` / `updated_at` | `String` | DB が `CURRENT_TIMESTAMP`（UTC）で入れる | `crates/core/src/db/notes.rs:62-63`、`crates/core/src/db/notes.rs:93-99` |

- `SpreadSide` = `Left` / `Right`。DB 文字列は `"left"` / `"right"`、未知の値は `parse` が `None`（`crates/core/src/db/notes.rs:16-45`）。
- **`is_active` の意味**: 「付箋が付いているか」。`noted_pages` と一覧（`list_newest_first`）は `is_active = 1` のみ返す。外してもメモは残り、`upsert` し直すと `is_active = 1` に戻りメモも復帰する（誤操作でメモを失わない）（`crates/core/src/db/notes.rs:1-6`、`crates/core/src/db/notes.rs:89-94`、`crates/core/src/db/notes.rs:158-166`）。

### 3.2 主要クエリ

| 関数 | SQL / 挙動 | アンカー |
|---|---|---|
| `upsert(note)` | `INSERT INTO page_notes (id, book_id, content_id, page, memo, spread_side, is_active) VALUES (?1,...,?6, 1) ON CONFLICT(book_id, content_id, page) DO UPDATE SET memo = excluded.memo, spread_side = excluded.spread_side, is_active = 1, updated_at = CURRENT_TIMESTAMP`（`created_at` は初回のまま） | `crates/core/src/db/notes.rs:87-105` |
| `set_active(book_id, content_id, page, active)` | `UPDATE page_notes SET is_active = ?4, updated_at = CURRENT_TIMESTAMP WHERE book_id = ?1 AND content_id = ?2 AND page = ?3`（`i64::from(active)`） | `crates/core/src/db/notes.rs:113-133` |
| `get_for_page` | `WHERE book_id = ?1 AND content_id = ?2 AND page = ?3`。**外した付箋も返す**（メモ復帰用） | `crates/core/src/db/notes.rs:136-155` |
| `noted_pages` | `SELECT page FROM page_notes WHERE book_id = ?1 AND content_id = ?2 AND is_active = 1 ORDER BY page` → `page > 0` を `(page - 1) as usize` にして **0-indexed の `HashSet<usize>`** で返す | `crates/core/src/db/notes.rs:158-178` |
| `list_newest_first` | `WHERE is_active = 1 ORDER BY created_at DESC, rowid DESC` | `crates/core/src/db/notes.rs:181-193` |
| `delete(id)` | `DELETE FROM page_notes WHERE id = ?1` | `crates/core/src/db/notes.rs:195-203` |
| 行→モデル | `spread_side` は `as_deref().and_then(SpreadSide::parse)`、`is_active = row.get::<i64,_>("is_active") != 0` | `crates/core/src/db/notes.rs:205-218` |

### 3.3 リーダーでの操作フロー

| 操作 | 挙動 | アンカー |
|---|---|---|
| 付箋アイコンのクリック | ビューアーが `crate::actions::NotePageRequest { page: index as i64 + 1, side }` を dispatch（`side` は `spread_pages()` の左右から `"left"` / `"right"` の `SharedString`。単一表示は `None`） | `crates/app/src/components/image_viewer/mod.rs:1429-1500` |
| 受信 | `for_book` が登録した `App::on_action` リスナー（**弱参照** `cx.entity().downgrade()`）が `note_request = Some((page, side))` にする | `crates/app/src/views/reader.rs:258-279` |
| ダイアログ生成 | `ensure_note_draft`（render から呼ぶ）: 既存付箋の `spread_side` を優先（`note.spread_side.or(requested_side)`）、メモを既存値で初期化、`InputState::new(..).placeholder("メモ")`、入力欄へフォーカス、`set_note_dialog_open(true)` | `crates/app/src/views/reader.rs:528-581` |
| **ON のアイコンを押すと解除** | `existing.is_active` が真なら `set_active(..., false)` を実行し、**ダイアログを出さない**（メモは残る） | `crates/app/src/views/reader.rs:544-555` |
| 保存（OK / Enter） | `save_note_draft`: `id = note-{book_id}-{content}-{page}` で `PageNoteInput` を作り `upsert`、`set_note_dialog_open(false)`、`refresh_notes` | `crates/app/src/views/reader.rs:584-615` |
| Enter の購読 | `cx.subscribe(&input, ... InputEvent::PressEnter ...)` で OK と同じ経路（`crates/app/src/views/reader.rs:567-577`） |
| アイコン表示条件 | `notes_enabled && overlay_visible && !zoomed`（トップバー / ボトムドックと同じ）。付箋ありは青 `0x2563eb`・不透明度 1.0、未付箋は灰 `0x64748b`・0.7（ホバーで 1.0）。塗りは同形アイコンを不透明度 0.35 で重ねる | `crates/app/src/components/image_viewer/mod.rs:1408-1414`、`crates/app/src/components/image_viewer/mod.rs:1495-1530` |
| 配置 | 単一表示は「実際に描かれている画像」の上端・右端（`anchor = Some((img_w - NOTE_ICON_SIZE, 0.0))`）。見開き / スクロールはページ枠の `top_0().right_0()`。`deferred` で最前面に描く | `crates/app/src/components/image_viewer/mod.rs:1450-1470`、`crates/app/src/components/image_viewer/mod.rs:2578-2592` |
| 付箋ページの印 | `set_notes(enabled, noted: HashSet<usize>, ...)` を `ReaderView` が `noted_pages` から設定（本の閲覧のみ `true`） | `crates/app/src/components/image_viewer/mod.rs:1282-1291`、`crates/app/src/views/reader.rs:245-254` |
| 付箋画面からの復元 | `for_book_at(book_id, page, content_id, side)`（§1.4） | `crates/app/src/views/reader.rs:479-511` |

---

## 4. タグ

### 4.1 生成元（`book_tags.source` の値）

| `source` | 生成元 | 実装 | アンカー |
|---|---|---|---|
| `"generated"` | 取り込み時の形態素解析（lindera ipadic）→ 名詞抽出 → Zenn タグ照合 | `import::finish_import` が `tags_repo::set_for_book(pool, &book_id, &tag_pairs)`（全件置換） | `crates/core/src/import/mod.rs:1138-1146` |
| `"manual"` | 手動編集（本棚のインラインエディタ / タグ編集画面） | `set_for_book(local_id, tags.iter().map(|t| (t, "manual")))` | `crates/app/src/views/bookshelf.rs:3700-3702`、`crates/app/src/views/tag_edit.rs:139-141` |
| `"fanza_genre"` | FANZA のジャンルタグ（取り込み時） | `set_for_book(imported.book.id, genre_tags.map(|t| (t, "fanza_genre")))` + `bookshelf::update_tags` で `tags_json` にも書く | `crates/app/src/views/bookshelf.rs:3081-3088` |
| `"dlsite_genre"` | DLsite のカスタムジャンル（`item.tags_json`） | `set_for_book(imported.book.id, tags.map(|t| (t, "dlsite_genre")))` | `crates/app/src/views/bookshelf.rs:3108-3114` |

- タグ行 ID は `uuid::Uuid::new_v4().to_string()`（UUID v4 の文字列）。`set_for_book` は **DELETE + INSERT の全置換**を 1 トランザクションで行う（`crates/core/src/db/tags.rs:18-42`）。
- 編集 UI が読み込むのは `source` が `"manual"` / `"fanza_genre"` / `"dlsite_genre"` の行のみ（`"generated"` は編集対象に出さない）（`crates/app/src/views/bookshelf.rs:3552-3562`）。
- `delete_generated(book_id)` は `DELETE FROM book_tags WHERE book_id = ?1 AND source = 'generated'`（タグ取得 OFF 時に自動生成タグだけ消す。手動タグは残す）（`crates/core/src/db/tags.rs:47-55`）。

### 4.2 形態素解析〜タグ生成（`crates/core/src/tags.rs`）

| 定数 / 処理 | 値・式 | アンカー |
|---|---|---|
| `MAX_TAGS` | `10`（生成タグの最大件数） | `crates/core/src/tags.rs:11` |
| `MAX_WORD_CHARS` | `20`（これを超える文字数の語は除外。`word.chars().count() > 20`） | `crates/core/src/tags.rs:12`、`crates/core/src/tags.rs:74-76` |
| `ZENN_TAGS_URL` | `"https://zenn.dev/api/tags"` | `crates/core/src/tags.rs:13` |
| 辞書 | lindera `TokenizerBuilder::new()` + `set_segmenter_dictionary_kind(&DictionaryKind::IPADIC)` | `crates/core/src/tags.rs:23-27` |
| 名詞判定 | `details.first()` が `"名詞"` で始まるトークンのみ（`pos.starts_with("名詞")`） | `crates/core/src/tags.rs:36-39` |
| 原形（`base_form`） | `details[6]`。空 or `"*"` なら表層形（`surface`）を使う | `crates/core/src/tags.rs:40-46` |
| 読み（`reading`） | `details[7]`。`"*"` なら空文字 | `crates/core/src/tags.rs:47-52` |
| 頻度集計 | 語 = `base_form`（空なら表層形）。`excluded` に完全一致する語と 20 文字超を除外し、`HashMap<String, usize>` に加算 | `crates/core/src/tags.rs:64-82` |
| 並び | `sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)))` = **頻度降順 → 語の昇順** | `crates/core/src/tags.rs:84-87`、`crates/core/src/tags.rs:158-160` |
| Zenn 取得 | プロセス内 1 回キャッシュ（`static CACHE: Mutex<Option<Vec<String>>>`）。ureq Agent: `timeout_connect = 5s`, `timeout_read = 10s`、`User-Agent: thundoku-shelf/0.1`。HTTP ステータス ≠ 200 / JSON 配列が空なら `Err(ZennError)` | `crates/core/src/tags.rs:93-145` |
| Zenn タグ抽出 | 配列要素の `name`、無ければ `id` を文字列として集める | `crates/core/src/tags.rs:122-136` |
| `generate_tags` | 全ページの `extract_nouns` を合算 → 頻度順にソート → `zenn_tags` が非空なら **Zenn に含まれる語だけに絞る** → `.take(MAX_TAGS)` = 最大 10 件 | `crates/core/src/tags.rs:148-166` |
| 起動時の先読み | アプリ起動時に `std::thread::spawn(|| { let _ = thundoku_core::tags::fetch_zenn_tags(); })` | `crates/app/src/app_state.rs:260-262` |
| 取り込み時の除外語 | `extract_nouns(text, &[&title])`（タイトルそのものを除外）、生成タグの除外語も `&[&title]` | `crates/core/src/import/mod.rs:1117-1118`、`crates/core/src/import/mod.rs:1141` |
| トークン行の保存 | `token_analysis` に `pos = "名詞"`, `base_form = Some(word)`, `reading = None`, `frequency = count as i64` | `crates/core/src/import/mod.rs:1117-1132` |

### 4.3 表示順 `TagOrder`

定義は本棚モジュール（`pub(crate)`、履歴・付箋画面と共用）: `crates/app/src/views/bookshelf.rs:509-558`。

| フィールド | 型 | 意味 | アンカー |
|---|---|---|---|
| `selected` | `HashSet<String>` | 絞り込みで選択中のタグ | `crates/app/src/views/bookshelf.rs:511-513` |
| `favorites` | `HashSet<String>` | お気に入りタグ（`favorite_tags` テーブル） | `crates/app/src/views/bookshelf.rs:514` |
| `counts` | `Arc<HashMap<String, usize>>` | タグ名 → そのタグを持つ**カード数** | `crates/app/src/views/bookshelf.rs:515-516` |
| `is_selected` / `is_favorite` / `count` | — | チップの色分けにも使う。`count` は未登録なら `0` | `crates/app/src/views/bookshelf.rs:536-546` |

比較式（逐語。`sorted()`）:

```rust
sorted.sort_by(|a, b| {
    self.is_selected(b).cmp(&self.is_selected(a))            // 選択中が先（true > false）
        .then_with(|| self.is_favorite(b).cmp(&self.is_favorite(a))) // お気に入りが先
        .then_with(|| self.count(b).cmp(&self.count(a)))      // 集計数の多い順
        .then_with(|| a.cmp(b))                               // 名前順（String の Ord = バイト順）
});
```
（`crates/app/src/views/bookshelf.rs:548-558`）

| 項目 | 事実 | アンカー |
|---|---|---|
| 集計の作り方 | `count_tag_usage(shelf_cards.iter().map(|c| c.tags.as_slice()))`。**同じカード内の重複タグは 1 冊として数える**（`HashSet` で重複排除）。追加 SQL は無し | `crates/app/src/views/bookshelf.rs:707-717`、`crates/app/src/views/bookshelf.rs:1615-1618` |
| 集計の再計算タイミング | `reload` のたび（タグの追加・削除・取り込みで並びが変わる）。お気に入りの付け外しは `TagOrder` を作り直すだけで並び替わる（reload 不要） | `docs/features.md:139-145`、`crates/app/src/views/bookshelf.rs:5761-5766` |
| `site_favorite_tags` | サイト絞り込み中に使う「お気に入りタグのうち、そのサイトに存在するもの」のキャッシュ。絞り込みなしのときは `favorite_tags` のコピー。サイトのタグ集合は本棚アイテム（`tags_json`）とローカル本（`book_tags`）の両方から作る。お気に入りタグの一覧（ポップオーバー）もこの並びで出す | `crates/app/src/views/bookshelf.rs:1051-1052`、`crates/app/src/views/bookshelf.rs:2357-2390` |
| `site_favorite_tags` の再計算タイミング | 画面に入ったとき（`reload` 後の描画）とフィルタ変更時（`rebuild_filtered`）。以前は `render` の中で本 1 冊ずつ `book_tags` を読んでいたため、スクロールのたびにその冊数ぶんクエリが走っていた（実機で 1 ノッチあたり約 90 件） | `crates/app/src/views/bookshelf.rs:2347-2348`、`crates/app/src/views/bookshelf.rs:2356` |
| `TagOrder::new` の生成箇所 | 本棚の描画（1 描画につき 1 個）、履歴画面、付箋画面 | `crates/app/src/views/bookshelf.rs:5761-5766`、`crates/app/src/views/history.rs:1417-1421`、`crates/app/src/views/notes.rs:1018-1021` |
| 性能（コメント実測） | 集計 1000 冊 × 30 タグ（30,000 件）= 33.7 ms / 1 フレーム分（可視 25 冊 × 30 タグ）の並び替え = 1.06 ms（debug ビルド） | `crates/app/src/views/bookshelf.rs:9882-9905`、`docs/features.md:158-161` |

### 4.4 折りたたみ個数の計算式（受入基準）

| 表示 | 表示個数の式 | アンカー |
|---|---|---|
| カード（グリッド） | `visible = if expanded { ordered.len() } else { ordered.len().min(CARD_TAGS_COLLAPSED_MAX) }`（`CARD_TAGS_COLLAPSED_MAX = 6`） | `crates/app/src/views/bookshelf.rs:4339-4342`、`crates/app/src/views/bookshelf.rs:6740` |
| リスト（行） | `visible = if expanded { ordered.len() } else { list_tags_visible_count(window, theme, &ordered) }` | `crates/app/src/views/bookshelf.rs:5545-5551` |

**リストの計算式（幅から個数を求める）**

```
list_tag_area_width(window)
  = ((window.width - SIDEBAR_W - 24.0) - 16.0) * LIST_TAGS_W_RATIO
  = ((window.width - 255.0 - 24.0) - 16.0) * 0.20            [px]
```
（`crates/app/src/views/bookshelf.rs:564-570`。`SIDEBAR_W = 255.0`、`LIST_TAGS_W_RATIO = 0.20`、`24.0` = ビューの左右パディング `p_3` の 12+12、`16.0` = 行の左右パディング `p_2` の 8+8）

```
widths[i]   = tag_chip_width(window, theme, tag_i)
            = tag_text_width(window, theme, tag_i) + CHIP_CHROME_W     # CHIP_CHROME_W = 34.0
tag_text_width(text) = window.text_system().layout_line(text, rem_size * 0.75, run, None).width
trailing    = TAG_EDIT_BUTTON_W + TAG_CHIP_GAP + tag_text_width("+99") + TAG_TOGGLE_CHROME_W
            = 24.0 + 4.0 + (「+99」の実測幅) + 10.0
visible     = packed_tag_count(widths, area_w, LIST_TAG_MAX_ROWS /* = 4 */, trailing)
if visible == ordered.len() {            # 全部入るなら「+n」は不要
    visible = packed_tag_count(widths, area_w, 4, TAG_EDIT_BUTTON_W /* = 24.0 */)
}
```
（`crates/app/src/views/bookshelf.rs:571-593`、`crates/app/src/views/bookshelf.rs:618-648`）

| 関数 / 定数 | 定義 | アンカー |
|---|---|---|
| `packed_rows(widths, row_width, max_rows)` | 幅の広い順（= 表示順）に詰める。`used > 0.0 && used + TAG_CHIP_GAP + width > row_width` なら改行（ただし `rows >= max_rows` なら打ち切り）。戻り値 `(count, used)` | `crates/app/src/views/bookshelf.rs:650-671` |
| `packed_tag_count(widths, row_width, max_rows, trailing)` | `widths` が空 / `max_rows == 0` / `row_width <= 0.0` なら `0`。`packed_rows` で出した個数から、最後の行の使用幅 `used` が `used + TAG_CHIP_GAP + trailing <= row_width` を満たすまで **1 個ずつ減らす**。`count > 1` の間だけ減らすので **最低 1 個は必ず出す**（「+n」だけの行を作らない） | `crates/app/src/views/bookshelf.rs:677-696` |
| `LIST_TAG_MAX_ROWS` | `4`（表紙の高さ `LIST_COVER_MIN_H = 133.0` px ÷ チップ 1 行 ≈ 29 px） | `crates/app/src/views/bookshelf.rs:6721` |
| `CHIP_CHROME_W` | `34.0` px（左右パディング 4+4・文字とハートの間隔 6・ハートの丸ボタン 18・枠線 1+1） | `crates/app/src/views/bookshelf.rs:6724` |
| `TAG_TOGGLE_CHROME_W` | `10.0` px（「+n」チップの左右パディング 4+4・枠線 1+1） | `crates/app/src/views/bookshelf.rs:6726` |
| `TAG_CHIP_GAP` | `4.0` px（`gap_1`） | `crates/app/src/views/bookshelf.rs:6728` |
| `TAG_EDIT_BUTTON_W` | `24.0` px（✎ ボタン） | `crates/app/src/views/bookshelf.rs:6730` |
| `CARD_TAGS_COLLAPSED_MAX` | `6`（カードの上限。幅 190px のカードでの実測: タグ無し 296.5px / 6 件 = チップ 3 行 +90.0px / 20 件 = 10 行 +295.0px） | `crates/app/src/views/bookshelf.rs:6731-6740` |
| 「+n」ラベル | `tag_toggle_label(hidden, expanded) = if expanded { "閉じる" } else { format!("+{hidden}") }`。`hidden = ordered.len() - visible` | `crates/app/src/views/bookshelf.rs:697-703`、`crates/app/src/views/bookshelf.rs:4364-4378` |
| 展開状態の保持 | `expanded_tag_rows: HashSet<String>`（キーは `database_id`）。`toggle_tag_expansion` が insert/remove をトグル | `crates/app/src/views/bookshelf.rs:777-778`、`crates/app/src/views/bookshelf.rs:3682-3687` |
| トグルの要素 ID | `tag-toggle-{database_id}`（カード / リスト共通）。クリックは `stop_propagation` する | `crates/app/src/views/bookshelf.rs:4377-4378`、`crates/app/src/views/bookshelf.rs:5142-5177` |
| トグルの tooltip | 折りたたみ中 = `残り {hidden} 件のタグを表示` / 展開中 = `タグを閉じる` | `crates/app/src/views/bookshelf.rs:5149-5156` |
| 選択中 / お気に入りの特例 | `tag_order.sorted()` で先頭に来るが、**上限からの除外特例は無い**（選択中・お気に入りが 6 個を超えれば 7 個目以降は折りたたまれる） | `crates/app/src/views/bookshelf.rs:4335`、`crates/app/src/views/bookshelf.rs:4347` |
| トグルの表示条件 | 折りたたみ中は `hidden > 0` のときだけトグルを出す。展開中は `hidden == 0` でも「閉じる」を出す | `crates/app/src/views/bookshelf.rs:4374`、`crates/app/src/views/bookshelf.rs:4380`、`crates/app/src/views/bookshelf.rs:5587`、`crates/app/src/views/bookshelf.rs:5593` |
| 「+n」の `n` | `hidden = ordered.len() - visible`（カード `crates/app/src/views/bookshelf.rs:4363` / リスト `crates/app/src/views/bookshelf.rs:5574`） | `crates/app/src/views/bookshelf.rs:4363` |
| 実測（docs） | 「タグ01」形式 20 件のリスト表示: 窓 700px = 1 件 / 1000px = 7 件 / 1400px = 11 件 / 1920px = 15 件（行高はタグ無しと同じ 150px のまま） | `docs/features.md:129-134` |

### 4.5 チップの識別子・配色・クリック挙動

| 項目 | 値 | アンカー |
|---|---|---|
| タグ行 / トグル / 編集ボタンの要素 ID | `tag-row-{database_id}`（カード `crates/app/src/views/bookshelf.rs:4364` / リスト `crates/app/src/views/bookshelf.rs:5575`）、`tag-toggle-{database_id}`（`:4378` / `:5591`）、`tag-edit-{database_id}`（`crates/app/src/views/bookshelf.rs:4937`） | `crates/app/src/views/bookshelf.rs:4364` |
| タグ編集の削除ボタン ID | `edit-tag-x-{tag_id}`（`tag_id` はタグ名そのもの） | `crates/app/src/views/bookshelf.rs:4764` |
| お気に入りタグポップオーバーの選択肢 ID | `tag-option-{tag_id}`（タグ名そのもの） | `crates/app/src/views/bookshelf.rs:6220-6224` |
| お気に入りの保存先 | `db::tags::set_favorite(db, tag, !is_favorite)`。実体は `favorite_tags(tag_name TEXT PRIMARY KEY, created_at)` の**行の有無**（boolean 列ではない） | `crates/app/src/views/bookshelf.rs:3509-3525`、`crates/core/migrations/0001_init.sql:199-201` |
| タグ文字の要素 ID | `tag-label-{database_id}-{tag}` | `crates/app/src/views/bookshelf.rs:4601-4608` |
| ハートの要素 ID | `tag-heart-{database_id}-{tag}` / アイコンは `{heart}-icon` | `crates/app/src/views/bookshelf.rs:4602-4605` |
| ハートのサイズ | 丸ボタン `CHIP_HEART_BUTTON = 18.0` px、アイコン `CHIP_HEART_ICON = 12.0` px | `crates/app/src/views/bookshelf.rs:115-118`、`crates/app/src/views/bookshelf.rs:4547-4585` |
| 文字クリック | `on_click_tag` → `toggle_tag(tag)`（絞り込みの選択トグル）。`cx.stop_propagation()` でカード / 行クリックに伝播させない | `crates/app/src/views/bookshelf.rs:4613-4624`、`crates/app/src/views/bookshelf.rs:4337-4340` |
| ハートクリック | `on_heart` → `toggle_favorite_tag(tag)`（`db::tags::set_favorite` に書き込み + メモリの `favorite_tags` を更新） | `crates/app/src/views/bookshelf.rs:3509-3525`、`crates/app/src/views/bookshelf.rs:4353-4356` |
| 配色（優先度） | **絞り込み選択（青） > お気に入り（ピンク） > 既定（`muted`）**。ハートの色はお気に入り専用で選択中でも変わらない | `crates/app/src/views/bookshelf.rs:132-145`、`crates/app/src/views/bookshelf.rs:189-231` |
| ライト配色 | お気に入り背景 `rgb(0xfce7f3)`（`:149`）/ 文字 `rgb(0x9d174d)`（`:151`）/ ハート `rgb(0xdb2777)`（`:152`）、選択文字 `rgb(0x1e40af)`（`:153`）、既定枠 `hsla(0,0,0,0.12)`（`:155`）、お気に入り枠 `hsla(0.925,0.71,0.51,0.35)`（`:156`）、選択枠 `hsla(0.6028,0.91,0.60,0.40)`（`:157`）、ハート地色 `hsla(0,0,0,0.06)`/hover `0.14`（`:158`） | `crates/app/src/views/bookshelf.rs:149-163` |
| ダーク配色 | お気に入り背景 `hsla(0.925,0.55,0.50,0.24)`（`:167`）/ 文字 `rgb(0xf9a8d4)`（`:169`）/ ハート `rgb(0xf472b6)`（`:170`）、選択文字 `rgb(0x93c5fd)`（`:171`）、既定枠 `hsla(0,0,1,0.22)`（`:173`）、お気に入り枠 `hsla(0.925,0.80,0.72,0.45)`（`:174`）、選択枠 `hsla(0.6028,0.90,0.72,0.45)`（`:175`）、ハート地色 `hsla(0,0,1,0.14)`/hover `0.24`（`:176`） | `crates/app/src/views/bookshelf.rs:165-178` |
| 選択中背景の定数 | `const CHIP_SELECTED_BG: Hsla = gpui_kit::hsla(0.6028, 0.85, 0.55, 0.30)`（`h` は 0..1 の正規化値。度を渡すと clamp されて別の色になる） | `crates/app/src/views/bookshelf.rs:120-123` |
| アイコン | お気に入り = `AppIcon::HeartFilled`、未登録 = `AppIcon::Heart` | `crates/app/src/views/bookshelf.rs:4571-4580` |
| カード / リストのタグ取得元 | `ordered = tag_order.sorted(&card.tags)`。`card.tags` は FANZA / DLsite なら `bookshelf_items.tags_json`、それ以外はローカル本（`book_tags`）を優先し、無ければ `tags_json` | `crates/app/src/views/bookshelf.rs:1505-1513`、`crates/app/src/views/bookshelf.rs:4336`、`crates/app/src/views/bookshelf.rs:5545` |

---

## 5. 検索 / 絞り込み

### 5.1 検索

| 項目 | 事実 | アンカー |
|---|---|---|
| 入力状態 | `search_state: Option<Entity<InputState>>`（本棚ビューのフィールド） | `crates/app/src/views/bookshelf.rs:751` |
| プレースホルダ | `"検索（タイトル・サークル・著者）"` | `crates/app/src/views/bookshelf.rs:2218-2222` |
| クエリ取得 | `current_search(cx)` = `input.value().to_string()` を `!value.is_empty()` でフィルタ（**trim しない・小文字化もしない**） | `crates/app/src/views/bookshelf.rs:2211-2216` |
| 検索対象フィールド（完全な一覧） | `shelf.title` + `shelf.circle_name` + `shelf.author` の 3 つを **半角スペース 1 個で連結**した 1 本の文字列 | `crates/app/src/views/bookshelf.rs:2151-2156` |
| 正規化 | クエリ・ハイスタック双方を `str::to_lowercase()`（Unicode 小文字化）。それ以外の正規化（trim / 全角半角 / カタカナ / NFKC）は**無し** | `crates/app/src/views/bookshelf.rs:2151-2157` |
| 一致方式 | `haystack.contains(&query)` = **部分一致（case-insensitive）**。語分割や AND/OR は無し（空白入りクエリはそのまま部分文字列として検索される） | `crates/app/src/views/bookshelf.rs:2157-2159` |
| 検索の解除 | `clear_filters` が `state.set_value("", window, cx)` | `crates/app/src/views/bookshelf.rs:3501-3503` |
| 変更検出 → 再フィルタ | 描画時に `search.as_deref() != Some(self.last_search.as_str())` なら `last_search = search.unwrap_or_default(); filtered_dirty = true;` とし、直後に `if self.filtered_dirty { self.rebuild_filtered(cx); }` | `crates/app/src/views/bookshelf.rs:5732-5740` |
| キーボード `ESC` | `"escape" if self.is_filtering(cx)` のとき `clear_filters(window, cx)` を呼び `cx.stop_propagation()`。**絞り込みが無いときは何もせず親（ワークスペース）へ流す** | `crates/app/src/views/bookshelf.rs:1178-1182` |
| 検索対象外 | haystack は 3 項目だけなので、タグ・イベント・サイト・日付・説明・シリーズ名・ファイル名・ID は**検索されない** | `crates/app/src/views/bookshelf.rs:2153-2158` |
| docs との差 | `docs/features.md:190` は「検索（タイトル・サークル名）」と書くが、実装は **author も含む**（プレースホルダも「著者」を含む） | `docs/features.md:190` vs `crates/app/src/views/bookshelf.rs:2151-2156` |

### 5.2 フィルタ状態（型定義）

```rust
pub enum ReadFilter { All, Unread, Reading, Read, Favorite }   // crates/app/src/views/bookshelf.rs:41-49
```
（`Reading` は「途中まで読んだ（最終ページには達していない）」）

フィルタ関連フィールド（`crates/app/src/views/bookshelf.rs:748-782`）:

| フィールド | 型 | 既定 | 意味 |
|---|---|---|---|
| `search_state` | `Option<Entity<InputState>>` | `None` | 検索入力 |
| `selected_tags` | `Vec<String>` | `vec![]` | 選択中タグ（**OR**、複数可） |
| `all_tags` | `Vec<String>` | `vec![]` | タグ候補（`reload` で再構築） |
| `selected_events` | `Vec<String>` | `vec![]` | 選択中イベント（OR） |
| `available_events` | `Vec<String>` | `vec![]` | イベント候補（技術書典のみ表示） |
| `site_filter` | `Option<String>` | 設定 `bookshelf.site_filter` から復元（`"all"` = None） | サイトスコープ |
| `circle_filter` | `Option<String>` | `None` | サークル絞り込み（単一値・完全一致） |
| `author_filter` | `Option<String>` | `None` | 作者絞り込み（単一値・完全一致） |
| `read_filter` | `ReadFilter` | `All` | 読書状態 / お気に入り |
| `expanded_tag_rows` | `HashSet<String>` | 空 | タグ列を展開中の本（`database_id` キー） |

### 5.3 絞り込みの合成規則（`matches_filter`）

`matches_filter(cx, card) -> bool` は **すべて AND**（いずれかが false なら除外）。カテゴリ内の複数値（タグ / イベント）だけが OR（`any`）（`crates/app/src/views/bookshelf.rs:2135-2209`）。

| # | 条件 | 判定式 | 合成 | アンカー |
|---|---|---|---|---|
| 1 | 非表示 | `shelf.is_hidden == 1` なら除外（無条件） | AND | `crates/app/src/views/bookshelf.rs:2138` |
| 2 | 表紙未取得のリモート本 | `card.local.is_none() && card.cover.is_none() && card.shelf.thumbnail_url.is_some()` なら除外（表紙取得完了まで出さない） | AND | `crates/app/src/views/bookshelf.rs:2145` |
| 3 | サイト | `site_filter` が `Some(site)` のとき `shelf.site_id != *site` なら除外（完全一致） | AND | `crates/app/src/views/bookshelf.rs:2148-2151` |
| 4 | 検索 | `current_search` が `Some(q)` のとき `format!("{} {} {}", title, circle_name, author).to_lowercase().contains(&q.to_lowercase())` が false なら除外 | AND | `crates/app/src/views/bookshelf.rs:2153-2159` |
| 5 | イベント | `selected_events` が非空のとき `any(|e| shelf.event_name.as_deref() == Some(e.as_str()))` が false なら除外（**完全一致・OR**） | カテゴリ内 OR / 他とは AND | `crates/app/src/views/bookshelf.rs:2161-2168` |
| 6 | タグ | `selected_tags` が非空のとき `any(|t| card_tags.contains(t))` が false なら除外（**OR**）。`card_tags` = `card.local.map(|e| e.tags)` に **`bookshelf_items.tags_json` も結合**（`bookshelf::tags_of(&card.shelf)`。未ダウンロード本もチップが出ているタグで一致する） | カテゴリ内 OR / 他とは AND | `crates/app/src/views/bookshelf.rs:2672-2691` |
| 7 | サークル | `circle_filter == Some(c)` のとき `shelf.circle_name != *c` なら除外（完全一致・大小文字区別あり） | AND | `crates/app/src/views/bookshelf.rs:2183-2187` |
| 8 | 作者 | `author_filter == Some(a)` のとき `shelf.author != *a` なら除外（完全一致） | AND | `crates/app/src/views/bookshelf.rs:2188-2192` |
| 9 | 読書状態 / お気に入り | `ReadFilter::All` → 通す / `Unread` / `Reading` / `Read` → `card.local.map(|e| e.reading_state) == Some(該当状態)`（ローカル本のみ該当。リモート本は不一致）/ `Favorite` → `card.shelf.is_favorite == 1` | AND | `crates/app/src/views/bookshelf.rs:2195-2208` |

適用箇所は 2 系統あり、どちらも同じ `matches_filter` を通る:

- `visible_shelf_cards(cx)`: フィルタ → `sort_by(compare_cards)`（`crates/app/src/views/bookshelf.rs:1920-1933`）。**テスト専用**（`#[cfg(test)]`）。描画は Card / List とも `filtered`（同じ順序のインデックス）を使う（`crates/app/src/views/bookshelf.rs:6536-6581`）。
- `rebuild_filtered(cx)`: 仮想化リスト用のインデックスキャッシュ `filtered`（`filtered_dirty` で再構築）。**Card / List とも**こちらを使う（`crates/app/src/views/bookshelf.rs:1941-1948`、`crates/app/src/views/bookshelf.rs:6417-6440`、`crates/app/src/views/bookshelf.rs:6536-6581`）。

フィルタは **SQL の `WHERE` ではなく Rust 側**（`books::list(db)` / `bookshelf::list_all(db)` で全件取ってから判定）。所有者スコープ（`owned_book_ids`）だけは `entries` 構築時に適用される（`crates/app/src/views/bookshelf.rs:1341-1346`、`crates/app/src/views/bookshelf.rs:598-603`）。

### 5.4 絞り込みの解除規則

| 操作 | 挙動 | アンカー |
|---|---|---|
| タグチップの再クリック | `toggle_tag`: 既に選択済みなら `retain(|t| t != tag)` で外す、未選択なら末尾に push（複数選択可） | `crates/app/src/views/bookshelf.rs:3440-3447` |
| サークルリンクの再クリック | `toggle_circle_filter`: 同じ値なら `None`、違う値なら置換（単一値なので上書き） | `crates/app/src/views/bookshelf.rs:3451-3459` |
| 作者リンクの再クリック | `toggle_author_filter`: 同上 | `crates/app/src/views/bookshelf.rs:3462-3470` |
| イベントの個別トグル | 専用関数は無くクリッククロージャ内で `retain(|e| e != &event_id)` / `push(event_id)` | `crates/app/src/views/bookshelf.rs:6064-6081` |
| 読書状態の切替 | `set_read_filter(filter)` が値を代入（解除値は `ReadFilter::All`） | `crates/app/src/views/bookshelf.rs:3798-3802` |
| 「全項目」ボタン | `clear_filters`: `circle_filter = None`、`author_filter = None`、`selected_tags.clear()`、`selected_events.clear()`、`read_filter = All`、検索入力に `""` を設定。**サイト（サイドバーのスコープ）は変更しない** | `crates/app/src/views/bookshelf.rs:3495-3508` |
| 絞り込み中の表示 | `is_filtering(cx)` = 検索 || events || tags || circle || author || `read_filter != All`（**サイトは数えない**）。ラベルは「絞込中 ✕」/「全項目」 | `crates/app/src/views/bookshelf.rs:3475-3491` |
| サイト切替時 | `set_site_filter`: 別サイトへ切り替えたとき **タグ / イベント / サークル / 作者** を解除（検索・既読モードは維持）。「すべての本」へ戻すときは何も解除しない。選択は `bookshelf.site_filter` に保存 | `crates/app/src/views/bookshelf.rs:1114-1132` |
| 存在しなくなったタグ | `reload` の最後で `selected_tags.retain(|tag| all_tags.contains(tag))` | `crates/app/src/views/bookshelf.rs:1633` |
| タグ候補の作り方 | `db::tags::all_tags(db)`（`SELECT DISTINCT tag_name FROM book_tags ORDER BY tag_name`）+ `bookshelf_items` の `tags_json` 由来タグを末尾に追加（重複は `contains` で除外、ソートはしない） | `crates/core/src/db/tags.rs:70-82`、`crates/app/src/views/bookshelf.rs:1449-1457` |

---

## 6. 並び替え（受入基準）

### 6.1 ソートキー（逐語）

```rust
pub(crate) enum SortField {
    PurchaseDate,   // 購入日
    ReleaseDate,    // 発売日
    LastViewedAt,   // 最終閲覧日
    ViewCount,      // 閲覧回数
    ViewSeconds,    // 閲覧時間
    FileSize,       // サイズ
    Title,          // タイトル
}
```
（`crates/app/src/views/bookshelf.rs:258-266`）

| 項目 | 値 | アンカー |
|---|---|---|
| メニュー表示順 `SortField::ALL` | `[PurchaseDate, ReleaseDate, Title, LastViewedAt, ViewCount, ViewSeconds, FileSize]`（7 項目） | `crates/app/src/views/bookshelf.rs:268-277` |
| ラベル | 購入日 / 発売日 / 最終閲覧日 / 閲覧回数 / 閲覧時間 / サイズ / タイトル | `crates/app/src/views/bookshelf.rs:279-290` |
| 方向ラベル | 日付系 = 「古い順 / 新しい順」、`ViewCount` = 「少ない順 / 多い順」、`ViewSeconds` = 「短い順 / 長い順」、`FileSize` = 「小さい順 / 大きい順」、`Title` = 「A→Z / Z→A」 | `crates/app/src/views/bookshelf.rs:292-330` |
| `default_ascending()` | `matches!(self, SortField::Title)` → **タイトルだけ昇順（A→Z）**、他は降順（新しい順 / 多い順） | `crates/app/src/views/bookshelf.rs:332-335` |
| `is_available(cards)` | `ReleaseDate` は発売日を持つカードが 1 つも無ければメニューに出さない / `FileSize` は `card.local.is_some()` のカードが無ければ出さない / 他は常に true | `crates/app/src/views/bookshelf.rs:338-345` |
| メニュー行のセレクタ | `purchase-date` / `release-date` / `last-viewed-at` / `view-count` / `view-seconds` / `file-size` / `title` | `crates/app/src/views/bookshelf.rs:347-359` |
| ボタンラベル | `sort_label_for(field, asc)` = `"並び替え: {label} {direction_label}"`（例 `並び替え: 閲覧回数 多い順`） | `crates/app/src/views/bookshelf.rs:383-389` |

### 6.2 状態と既定値

| フィールド / 操作 | 値 | アンカー |
|---|---|---|
| `sort_field` 既定 | `SortField::PurchaseDate` | `crates/app/src/views/bookshelf.rs:1073` |
| `sort_ascending` 既定 | `false`（= 購入日の新しい順。従来の `causedAt DESC` と同じ） | `crates/app/src/views/bookshelf.rs:1074`、`crates/app/src/views/bookshelf.rs:257` |
| 永続化 | `bookshelf.sort_field`（slug 文字列）/ `bookshelf.sort_ascending`（`"1"` / `"0"`）に保存（`db::settings`）。起動時に `load_sort` で復元し、保存値が無ければ既定 | `crates/app/src/views/bookshelf.rs:2392-2394`、`crates/app/src/views/bookshelf.rs:2416-2444`、`crates/app/src/views/bookshelf.rs:1411-1413` |
| `set_sort_field(field)` | 同じ項目なら**何もしない**（方向も変えない）。違う項目なら `sort_ascending = field.default_ascending()` にリセット | `crates/app/src/views/bookshelf.rs:1952-1960` |
| `set_sort_direction(ascending)` | 方向だけ変更 | `crates/app/src/views/bookshelf.rs:1963-1967` |
| `reset_sort()` | `PurchaseDate` + 降順に戻す（メニューの「既定に戻す」） | `crates/app/src/views/bookshelf.rs:1970-1975` |
| サイト切替時の補正 | `if !self.sort_field.is_available(&shelf_cards) { sort_field = PurchaseDate; sort_ascending = false; }`（`reload` 内） | `crates/app/src/views/bookshelf.rs:1621-1625` |
| UI | `available_sort_fields()` を `Radio` で並べる（項目・方向の 2 セクション + 「既定に戻す」）。メニューは幅 224 px | `crates/app/src/views/bookshelf.rs:1978-1984`、`crates/app/src/views/bookshelf.rs:2103-2116` |

### 6.3 比較関数（正規化・値なし・タイブレーク）

比較値 `SortValue` = `Date(Option<String>)` / `Number(Option<i64>)` / `Text(String)`（`crates/app/src/views/bookshelf.rs:401-427`）。

| ソートキー | 比較値の式 | 型 | アンカー |
|---|---|---|---|
| `PurchaseDate` | `SortValue::date(purchase_date())`、`purchase_date() = non_empty(shelf.caused_at) ?? non_empty(local.book.purchase_date)` | Date | `crates/app/src/views/bookshelf.rs:452-458`、`crates/app/src/views/bookshelf.rs:472` |
| `ReleaseDate` | `SortValue::date(release_date())`、`release_date() = non_empty(shelf.release_date) ?? non_empty(local.book.release_date)` | Date | `crates/app/src/views/bookshelf.rs:461-467`、`crates/app/src/views/bookshelf.rs:473` |
| `LastViewedAt` | `SortValue::date(self.last_viewed_at)`（`view_history::view_stats` の `MAX(COALESCE(ended_at, started_at))`。未閲覧は `None`） | Date | `crates/app/src/views/bookshelf.rs:474` |
| `ViewCount` | `SortValue::Number(Some(self.view_count))`（`view_stats` の `COUNT(*)`。**0 回も値として並ぶ**＝末尾固定にしない） | Number | `crates/app/src/views/bookshelf.rs:476-477` |
| `ViewSeconds` | `SortValue::Number(Some(self.view_seconds))`（`view_stats` の `SUM`。0 も値） | Number | `crates/app/src/views/bookshelf.rs:478` |
| `FileSize` | `SortValue::Number(self.local.as_ref().map(|e| e.book.file_size))`（未ダウンロードは `None` = 値なし） | Number | `crates/app/src/views/bookshelf.rs:480-483` |
| `Title` | `SortValue::Text(self.shelf.title.to_lowercase())` | Text | `crates/app/src/views/bookshelf.rs:484` |

**日付の正規化式（`date_sort_key`）**（`crates/app/src/views/bookshelf.rs:366-387`）

```
text = raw.trim()
if text.is_empty() -> None
date_part = text.split(['T', ' ']).next()                  # "2026-04-12T09:16:36.410Z" と "2026/05/04 17:03" の両方に対応
normalized = date_part.replace(['年','月'], "/").replace('日', "")
parts = normalized.split(['/', '-'])
if parts.len() < 3 -> None
year, month, day = parts[0..3].trim().parse::<u32>()?       # 解析できなければ None
-> Some(format!("{year:04}{month:02}{day:02}"))             # 例: "20260903"
```

| 入力例 | 結果 | 出典（テスト） |
|---|---|---|
| `"2026-04-12T09:16:36.410Z"`（技術書典 RFC3339） | `"20260412"` | `crates/app/src/views/bookshelf.rs:7466-7471` |
| `"2026/05/04 17:03"`（BOOTH / DLsite） | `"20260504"` | `crates/app/src/views/bookshelf.rs:7472-7475` |
| `"2026年09月03日"`（FANZA） | `"20260903"` | `crates/app/src/views/bookshelf.rs:7476` |
| `"2025-06-17 16:00:00"` | `"20250617"` | `crates/app/src/views/bookshelf.rs:7477-7480` |
| `"2026/9/3"`（0 埋めなし） | `"20260903"`（0 埋めして比較可能に） | `crates/app/src/views/bookshelf.rs:7482` |
| `""` / `"   "` / `"日付なし"` | `None` | `crates/app/src/views/bookshelf.rs:7484-7486` |

**比較の全体式（`compare_cards`、逐語の意味）**（`crates/app/src/views/bookshelf.rs:1916-1938`）

| 状況 | 結果 | アンカー |
|---|---|---|
| 両方とも値なし | `Equal`（タイブレークへ） | `crates/app/src/views/bookshelf.rs:1918-1919` |
| a のみ値なし | `Greater`（a が後ろ = **値なしは方向に関係なく常に末尾**） | `crates/app/src/views/bookshelf.rs:1920-1921` |
| b のみ値なし | `Less`（b が後ろ） | `crates/app/src/views/bookshelf.rs:1922` |
| 両方に値あり | `cmp_same_kind` の結果を、`sort_ascending` なら そのまま / 降順なら `order.reverse()` | `crates/app/src/views/bookshelf.rs:1923-1931` |
| 同順位のタイブレーク | `a.shelf.title.to_lowercase().cmp(&b.shelf.title.to_lowercase())`（**常にタイトル昇順**。降順設定でもタイブレークは昇順） | `crates/app/src/views/bookshelf.rs:1932-1937` |
| それも同一のとき | `Vec::sort_by` は安定ソート（std の仕様）なので元の並び（`shelf_cards` = `bookshelf_items` の取得順）を保つ | `crates/app/src/views/bookshelf.rs:1910`、`crates/app/src/views/bookshelf.rs:1945` |
| `cmp_same_kind` | `Date(Some,Some)` → 文字列比較 / `Number(Some,Some)` → 数値比較 / `Text` → 文字列比較 / 型が混ざる場合は `Equal`（項目が同じなら型も揃う前提） | `crates/app/src/views/bookshelf.rs:420-427` |

- 並び替えは **Rust 側の `sort_by`**（`ORDER BY` は使わない）。ソート対象は `visible_shelf_cards`（フィルタ後）または `rebuild_filtered` のインデックス列（`crates/app/src/views/bookshelf.rs:1910`、`crates/app/src/views/bookshelf.rs:1945`）。
- 日付の検証は**範囲チェック無し**: 月 `1..=12` / 日が暦上有効かの検証はせず、成立条件は「`['/', '-']` 分割が 3 要素以上」かつ「先頭 3 要素が `u32` に parse できる」ことのみ（`crates/app/src/views/bookshelf.rs:375-381`）。
- `Text`（タイトル）には**値なしの概念が無い**（`Text(String)` であり `Option` ではない）。trim・全角半角・かな正規化はせず `to_lowercase()` のみ（`crates/app/src/views/bookshelf.rs:422-423`、`crates/app/src/views/bookshelf.rs:482`）。
- カード / リストの切替は `toggle_view_mode()` が `ViewMode::Card ⇄ List` を切り替え、`persist_view_mode()` で `bookshelf.view_mode`（`"card"` / `"list"`）に保存し（起動時に復元）、`cx.notify()` をする。**絞り込み・ソート状態は変更しない**（切替時の既定リセットは無い）（`crates/app/src/views/bookshelf.rs:4024-4032`、`crates/app/src/views/bookshelf.rs:2398-2414`）。
- `docs/features.md:205-211` に対応する記述（日付の正規化・値なしは末尾・データが無い項目は非表示・同順位はタイトル昇順）がある。

---

## 7. 統計（`view_stats`）

### 7.1 SQL（逐語）

```sql
SELECT book_id, COUNT(*),
CAST(COALESCE(SUM(CAST(julianday(COALESCE(ended_at, started_at)) -
  julianday(started_at) AS REAL) * 86400), 0) AS INTEGER),
MAX(COALESCE(ended_at, started_at))
FROM view_history GROUP BY book_id
```
（`crates/core/src/db/view_history.rs:79-95`）

| 出力 | 型 | 単位 | 式 |
|---|---|---|---|
| `count` | `i64` | 回（セッション数） | `COUNT(*)`（`view_history` の行数 = 「その本を開いた回数」） |
| `total_seconds` | `i64` | 秒 | `COALESCE(SUM(julianday(COALESCE(ended_at, started_at)) - julianday(started_at)) * 86400, 0)` を `CAST(... AS INTEGER)`（1 日 = 86400 秒。小数は切り捨て） |
| `last_viewed_at` | `Option<String>` | UTC `YYYY-MM-DD HH:MM:SS` | `MAX(COALESCE(ended_at, started_at))`（文字列比較 = 時刻順） |

- 戻り値は `HashMap<String /*book_id*/, ViewStats>`。**閲覧 0 回の本は行が存在しない**（`crates/core/src/db/view_history.rs:68-107`）。
- 全書籍分を **1 クエリ**で取得し、本棚の `reload` で 1 回だけ呼ぶ（`crates/app/src/views/bookshelf.rs:1339-1340`）。
- 1 冊分の同等式が `total_duration_secs`（`crates/core/src/db/view_history.rs:110-124`）、回数だけなら `view_count`（`crates/core/src/db/view_history.rs:59-67`）。

### 7.2 利用箇所

| 用途 | 式 | アンカー |
|---|---|---|
| カードのソート値 | `view_count: stats.map_or(0, |s| s.count)`、`view_seconds: stats.map_or(0, |s| s.total_seconds)`、`last_viewed_at: stats.and_then(|s| s.last_viewed_at.clone())`。`stats = local.and_then(|entry| view_stats.get(&entry.book.id))`（**ローカル本にしか紐付かない**。未取り込みのリモート本は 0 / None） | `crates/app/src/views/bookshelf.rs:1514-1519`、`crates/app/src/views/bookshelf.rs:1586-1595` |
| 件数表示 | 画面上部の件数のみフィルタ連動: `visible_count = visible.len()` を `format!("{visible_count}件")` で表示。**未読 / 読んでいる途中 / 読了の冊数は本棚に出さない**（設定画面が集計する） | `crates/app/src/views/bookshelf.rs:5753-5754`、`crates/app/src/views/bookshelf.rs:5886`、`crates/app/src/views/settings.rs:250-262` |
| 右クリックメニュー | `format!("閲覧回数: {count} 回")`（`db::view_history::view_count(db, book_id)` を都度クエリ、無効項目として表示） | `crates/app/src/views/bookshelf.rs:4411-4416`、`crates/app/src/views/bookshelf.rs:5668-5673` |
| 履歴画面 | `view_history::list_daily(pool)`（1 日 1 本に集約。`duration_secs` / `sessions` は同じ 86400 秒換算・`num_seconds().max(0)`） | `crates/app/src/views/history.rs:222`、`crates/core/src/db/view_history.rs:142-198` |
| 設定画面の冊数 | `ReadingState::from_progress` で `read / reading / unread` をカウント | `crates/app/src/views/settings.rs:250-262` |
| サイドバー未読バッジ | `ReadingState::from_progress(progress) == Unread` の件数（`books` に紐付かない本棚アイテムは無条件で未読に数える） | `crates/app/src/workspace.rs:465-490` |

---

## 不明点

1. `reading_progress.scroll_position` の用途: 列とフィールドは存在するが、**書き込みは常に `0.0`**（`crates/app/src/views/reader.rs:686`）、読み出しも `for_book` / `switch_selection` のどこからも参照されない。スクロール位置の復元は `current_page` 経由でしか行われない（`crates/app/src/views/reader.rs:219-226`）。将来の用途は不明（`docs/database.md:110` は「スクロールモードの位置」と説明）。
2. 並び替えの永続化: `sort_field` / `sort_ascending` は `bookshelf.sort_field` / `bookshelf.sort_ascending` に保存し、起動時に `load_sort` で復元する（`crates/app/src/views/bookshelf.rs:2392-2394`、`crates/app/src/views/bookshelf.rs:1411-1413`）。サイト切替時に使えない項目だった場合の補正（`crates/app/src/views/bookshelf.rs:2016-2020`）は保存しない（意図かは不明）。サイトフィルタ（`bookshelf.site_filter`）と表示モード（`bookshelf.view_mode`）も保存される。
3. 検索の「複数語 AND」検討の有無: 実装は単純な部分文字列一致のみ。仕様書・コメントに複数語対応の記述は無い（`docs/features.md:190` は「検索（タイトル・サークル名）」のみ）。
4. タグ `all_tags` の並び: `book_tags` 側は `ORDER BY tag_name` だが、`shelf_items.tags_json` 由来タグは末尾に **追記のみ**（ソートされない）。フィルタ候補リストの最終的な並びが「厳密なソート」を意図しているかは不明（`crates/app/src/views/bookshelf.rs:1449-1457`）。
5. `note_request` の `side` はアクションの `SharedString` を `SpreadSide::parse` した結果で、不正値は `None` になる（= 単一表示扱い）。不正値が実際に送られる経路があるかは不明（`crates/app/src/views/reader.rs:265-277`）。
6. カード / リストのタグ折りたたみで「+n」の `n` は `ordered.len() - visible` だが、カードは `CARD_TAGS_COLLAPSED_MAX` による固定、リストは幅計算。両者が混在する画面（カード表示とリスト表示の切替）で展開状態 `expanded_tag_rows` が共用される（`crates/app/src/views/bookshelf.rs:4335-4342`、`crates/app/src/views/bookshelf.rs:5545-5551`）。切替時の展開状態のリセット有無は不明。
7. タグの `confidence` 列（`book_tags.confidence: Option<f64>`）は読み出しのみで、書き込みは `set_for_book` の INSERT に含まれない（= 常に NULL）（`crates/core/src/db/tags.rs:6-13`、`crates/core/src/db/tags.rs:30-38`）。

## 推測

1. **推測**: `view_history::touch`（削除済み）は `view_stats` の `MAX(COALESCE(ended_at, started_at))` が「最後に読んでいた時刻」になるようにするための heartbeat だったと考えられる。現行は `end` が閉じるときに一度だけ `ended_at` を更新するため、強制終了時はセッション開始時刻のままになる（`crates/core/src/db/view_history.rs:23-35` の「ended_at は開始時刻で初期化」と `crates/app/src/views/reader.rs:146-151`）。
2. **推測**: スクロール位置の復元は「`current_page` 経由で `scroll_to_item`」で代替されている（`crates/app/src/components/image_viewer/mod.rs:593-596`）ため、`scroll_position` は未使用の残置カラムと考えられる（根拠: `scroll_position: 0.0` 固定の保存と、`ReaderView` 内に読み出しが無いこと）。
3. **推測**: 見開きの左右ナビ帯がウィンドウ端（画像端ではない）なのは、見開きペアが中央寄せで幅が可変のため、固定のクリック領域を端に置いたほうが押しやすいからだと思われる（根拠: 単一表示だけ画像矩形に合わせている `crates/app/src/components/image_viewer/mod.rs:2578-2600` と、見開きの実装 `crates/app/src/components/image_viewer/mod.rs:2789-2849` の差）。
4. **推測**: `docs/features.md:190` の検索対象の記述が古い（author を含む実装に対し「タイトル・サークル名」）のは、author 追加時（プレースホルダは「著者」に更新済み）に docs を更新し忘れたため（根拠: `crates/app/src/views/bookshelf.rs:2152-2156` のコメント「placeholder（タイトル・サークル・著者）に合わせて author も検索対象にする」）。
5. **推測**: `card.tags`（表示）と絞り込み判定が別経路なのは、FANZA / DLsite の `tags_json` スナップショットが「ローカル取り込み前でもカードにタグを出す」ための表示用データであり、絞り込み側は `book_tags` に加えて `bookshelf_items.tags_json` も結合して判定するから（根拠: `crates/app/src/views/bookshelf.rs:1900-1908` のコメントと `crates/app/src/views/bookshelf.rs:2672-2691`）。未ダウンロードの FANZA / DLsite 本も、チップが出ているタグ（`tags_json` 由来）でタグ絞り込みにヒットする。

---
