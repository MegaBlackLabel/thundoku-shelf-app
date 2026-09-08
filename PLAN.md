# FANZA同人取り込み（PLAN）

## Context

Thundoku Shelf Desktop に、FANZA同人ストアで購入した作品の**画像系のみ**を取り込み、既存ビューアー（.opfspack 画像ビューアー）で表示できるようにする。

- 対象ストア: FANZA同人（`www.dmm.co.jp/dc/doujin/...`）。ログイン必須の販売作品一覧/詳細 + 一般公開の商品ページ。
- 認証: アプリ内 WebView でログイン → `www.dmm.co.jp` / `accounts.dmm.co.jp` のセッション Cookie を捕捉・永続化（BOOTH 方式を踏襲）。
- 取得 API / ページ（実測済み）:
  - 一覧 `GET https://www.dmm.co.jp/dc/doujin/api/mylibraries/?page={n}&sort=purchasedate_desc&genre=all&limit=20` → `data.items`（購入日 key）+ `data.total` + `data.hasNext`。Cookie のみで OK（CSRF 不要）。
  - 詳細 `GET https://www.dmm.co.jp/dc/doujin/api/mylibraries/details/{contentId}/` → `downloadLinks`、`fileSize`、`drm{dmmBooks,softDenchi}`、`genre`、`makerName`、`makerId`、`deliveryDate` 等。
  - 商品ページ `GET https://www.dmm.co.jp/dc/doujin/-/detail/=/cid={contentId}/`（**SSR HTML、一般公開**）→ `配信開始日`、`作品コメント`、`題材`、`ページ数`、`ジャンルタグ`（複数）、`価格`、`作者`、`ファイル容量`、`利用期限` 等。
  - ダウンロード proxy URL は 302 → 実 ZIP（`doujin{nn}.contents.doujin.dmm.co.jp/bb/dm_comic/{id}.zip`）。
- **分類（2 軸）**:
  - 軸1 メディア種別: `image_src` パス `/digital/{comic,cg,voice,game}/`（優先・安定）or `genre` の `・` 前。
  - 軸2 AI 生成: `genre` サフィックス `〜・一部AI`=partial / `〜・AI`=full / なし=none。
  - **取り込み対象**: comic / cg（`コミック`、`コミック・一部AI`、`CG`、`CG・AI`）。DRM 無しのみ。
  - **除外**: voice / game / video（ボイス・ゲーム・動画）。DRM 付き（`drm.dmmBooks|softDenchi` または `isSdrm != "0"`）はビューアー不可。

## Approach

### Slice 1 — メディア分類（純関数・TDD）
- 新規 `crates/core/src/fanza/mod.rs`:
  - `enum MediaCategory { Comic, Cg, Voice, Game, Video }`
  - `enum AiType { None, PartialAi, FullAi }`
  - `struct FanzaMeta { media: MediaCategory, ai: AiType }`
  - `fn classify(image_src: &str, genre: &str) -> FanzaMeta`
  - `fn is_viewable_included(meta: &FanzaMeta, drm_ok: bool) -> bool`
- 判定: パス `/digital/(comic|cg|voice|game|video)/` をセグメント完全一致で、無ければ genre の `・` 前を `コミック|CG|ボイス|ゲーム|動画` にマップ。AI は末尾 `・一部AI`→partial、`・AI`→full。部分マッチで広がらない。
- テスト: 表形式。`/digital/comic/`+`コミック`、`コミック・一部AI`、`CG`、`CG・AI` → include; `/digital/voice/`+`ボイス`、`ゲーム`、`動画`、`ボイス・AI` → exclude（AI でも種別で除外）。

### Slice 2 — FanzaClient（Transport 抽象・モック可能）
- `crates/core/src/fanza/client.rs`:
  - `struct FanzaSession { cookies: HashMap<String,String> }`（`logged_in()`、`cookie_header()`）
  - `struct FanzaPurchase { content_id, title, genre, image_src, maker_name, delivery_date, is_streaming, is_unavailable }`
  - `struct FanzaDetail { content_id, title, genre, maker_name, maker_id, delivery_date, file_size, download_link, drm_dmm_books, drm_soft_denchi, is_sdrm }`
  - `struct FanzaProductPage { release_date, description, theme, page_count, genre_tags: Vec<String>, price, author, maker_id }`
  - `struct FanzaClient { transport: Box<dyn Transport>, session: FanzaSession }`
  - `pub fn with_transport(transport: Box<dyn Transport>) -> Self`
  - `pub fn purchased(&self) -> Result<Vec<FanzaPurchase>, FanzaError>`（`hasNext` で page を回し `total` で打ち切り）
  - `pub fn detail(&self, content_id: &str) -> Result<FanzaDetail, FanzaError>`
  - `pub fn product_page(&self, cid: &str) -> Result<FanzaProductPage, FanzaError>`（SSR HTML パース。一般公開だが age-check Cookie も付与）
  - `pub fn download_with_progress(&self, download_url: &str, on_progress: &mut dyn FnMut(u64,u64)) -> Result<Vec<u8>, FanzaError>`（302 追跡、HTML 拒否）
  - `type FanzaError = ...`（`SessionExpired`、`Unauthorized`、`DrmProtected`、`Http(status)`、`Parse`）
- 再利用: `tbf::transport::Transport` / `UreqTransport` をそのまま使用。
- **商品ページパーサー**: SSR HTML から `dl.informationList` の dt/dd + `genreTagList .genreTag__txt` を読み、`配信開始日`/`作品コメント`/`題材`/`ページ数`/`ジャンルタグ`/`価格`/`作者` を抽出（`booth.rs` の `parse_library` と同流儀、テスト可能な純粋関数 `parse_product_page(html) -> FanzaProductPage`）。
- テスト: scripted `Transport` で①purchased ページング/`hasNext` 打ち切り②detail が download_link/drm/maker_id にマップ③product_page の HTML パースが各フィールドを抽出④download が 302→ZIP 追跡・HTML 拒否⑤Cookie ヘッダ付与。

### Slice 3 — 永続化 / site 登録 / 分類フィルタ / メタ列
- マイグレーション: **runtime DDL**（`crates/core/src/db/mod.rs` の `migrate()` 内、`ensure_column` ヘルパーで PRAGMA 存在確認 + ALTER TABLE、開発中のためマイグレーションファイルは作らない）。`0001_init.sql` は触らない。
  - `sites` に `('fanza','FANZA同人',...)` / `('dlsite','DLsite',...)` を `INSERT OR IGNORE`。
  - `bookshelf_items` / `books` に追加列:
    | 列 | 型 | 用途 |
    |---|---|---|
    | `media_category` | TEXT | comic / cg / voice / game / video |
    | `ai_type` | TEXT | none / partial / full |
    | `is_drm` | INTEGER DEFAULT 0 | DRM 付き除外判定 |
    | `release_date` | TEXT | 配信開始日 |
    | `description` | TEXT | 作品コメント（あらすじ） |
    | `theme` | TEXT | 題材（オリジナル/漫画・アニメ/…） |
    | `maker_id` | TEXT | makerId |
    | `page_count` | INTEGER | ページ数（宣言値） |
    | `age_rating` | TEXT | 年齢指定（全年齢/R18 等） |
    | `series_name` | TEXT | シリーズ名 |
  - ジャンルタグ複数 → タグ扱い（`tags_json` / `book_tags`）。
- `crates/core/src/fanza/sync.rs`: `pub fn save_purchases(pool, client) -> Result<usize, FanzaError>`
  - `purchased()` → 各要素 `classify` → `is_viewable_included` を満たすものだけ `bookshelf::upsert`（`site_id="fanza"`, `database_id=content_id`）。除外は upsert しない。
  - リッチメタ（`release_date`/`description`/`theme`/`page_count`/`genre_tags`/`maker_id`/`author`）は `detail` + `product_page` から取得してメタ列 + `tags_json` に反映。取得失敗時は一覧の値だけでフォールバック（リッチメタはベストエフォート）。
  - 削除ポリシー: ライブラリから消えた unlinked 行のみ削除（local は保持）。
- テスト: 空 DB マイグレーション + 混在カテゴリ save → `(site_id='fanza',...)` に comic/cg(+AI) のみ、voice/game/video 無し。`media_category`/`ai_type`/`is_drm`/`release_date`/`description`/`theme`/`maker_id`/`page_count` が保存され、ジャンルタグが `tags_json` に入る。

### Slice 4 — ダウンロード / import / リンク（サイト分岐複合化）
- `crates/app/src/views/bookshelf.rs`:
  - `BookshelfView::download_item` の BOOTH/TBF 2 分岐を**明示 3 分岐**へ（FANZA が TBF に誤フォールバックしない）。FANZA は `detail` で download_link → `download_with_progress`。
  - インポート: 既存 `import_zip_bytes` / `import_pdf` / `import_image_bytes` を拡張子でディスパッチ。
  - リンク: `books::set_site_id(...,"fanza")` + `set_tbf_product_id(..., fanza_id)`、`media_category`/`ai_type`/`is_drm`/リッチメタを `books` へ反映、ジャンルタグを `book_tags` へ。
- エッジ処理:
  - `is_drm` は import せず、カードに「DRM」表示 or 非表示。
  - CG セット ZIP は**自然順ソート**（`2.jpg` が `10.jpg` より前）。`import/mod.rs` の画像ソートを自然順へ。
  - 宣言 `page_count` と実測 `imported_documents.total_pages` は別物として保持（表示は実測優先）。
- テスト: 小さい FANZA 画像 ZIP + PDF import → `.opfspack` 生成、`source_type='image-set'`、順序付き `document_images`、`books.site_id='fanza'`、メタ列/`book_tags` 反映、正の page count、自然順ソート。

### Slice 5 — アプリ認証 / sync / UI 配線
- `crates/app/src/app_state.rs`: `fanza_session`/`fanza_logged_in`、`save_fanza_session`（`app_settings` キー `fanza.session` に JSON。Cookie 巨大でも keyring 制限を回避）、復元。
- `crates/app/src/views/fanza_login.rs`: `booth_login.rs` を元に WebView ログイン（`www.dmm.co.jp` → 年齢確認 `はい` → `accounts.dmm.co.jp` → 戻り）。ログイン後 Cookie 収集 → `FanzaSession` 保存。
- `BookshelfView::sync_fanza` + `sync_all` に `Some("fanza")`。専用スレッド + timeout。
- サイドバー / アカウントパネル / フィルタ / ログアウトに FANZA 追加。
- テスト（GPUI）: FANZA 選択で `sync_fanza` のみ dispatch、フィルタで FANZA 行、auth provider が FANZA ログインを開く。

### Slice 6 — クロスサイト identity 強化（後続・任意）
- カード連結・`download_states`・`downloaded_ids` を `(site_id, database_id)` 複合キー化。`find_by_source`/`resolve_reuse_id` の key も site 込みに。
- テスト: BOOTH と FANZA に同一 ID を置き、各カードが独立して link/download、FANZA import 本がビューアーで正しく表示。

## Critical files & anchors

1. `crates/core/src/fanza/{mod,client,sync}.rs`（新規）— 分類・クライアント・商品ページパーサー・save。`tbf/transport.rs` の `Transport`/`UreqTransport` 再利用。
2. `crates/core/src/db/bookshelf.rs` / `books.rs` — `BookshelfItem`/`Book` に `media_category`/`ai_type`/`is_drm`/`release_date`/`description`/`theme`/`maker_id`/`page_count` フィールド追加、`book_tags` へのジャンルタグ書き込み。
3. `crates/core/src/db/mod.rs`（runtime DDL、`ensure_column` + `sites` 行）+ `crates/core/src/db/{books,bookshelf}.rs`（列フィールド + COLUMNS + insert/upsert）。マイグレーションファイルは作らない。
4. `crates/app/src/views/bookshelf.rs` — `sync_all`/`sync_fanza`、`download_item` 3 分岐、import 後メタ補完。
5. `crates/app/src/views/fanza_login.rs`（新規）+ `crates/app/src/app_state.rs` — 認証/セッション。

## Verification

- `mise exec -- cargo test -p thundoku-core` / `mise run test`（分類・クライアント・商品ページパーサー・DB/メタ列）
- `mise exec -- cargo test -p thundoku-app`（sync 配線・ビューアー）
- `mise exec -- cargo build` / `mise run build`、`mise run lint`（clippy）
- 手動: アプリ起動 → FANZA ログイン → 同期 → comic/CG のみカード表示 → クリックで .opfspack import → ビューアー表示。ボイス/ゲーム/動画がカードに出ないこと。カードに価格/題材/ページ数/ジャンルタグが表示されること。

## Assumptions & contingencies

- セッション Cookie は `app_settings`（`fanza.session`）に JSON 保存。破損時は未ログイン扱いで再ログイン誘導。
- 分類は `image_src` パスを信頼し、無い場合のみ `genre` 文字列。未知値は exclude 側（安全側）。
- リッチメタ（`release_date`/`description`/`theme`/`page_count`/`genre_tags`）は `product_page` パースがベストエフォート。パース失敗・未取得時は一覧/詳細の値だけで続行（同期をブロックしない）。
- `sites` 行・列追加は冪等（`INSERT OR IGNORE` / PRAGMA 存在チェック）。既存 DB の `migrate()` で適用。
- ページング上限は `total` で打ち切る。`error_code != 0` または 401 は `SessionExpired` → ログイン誘導。
- ダウンロード proxy は 302 → CDN ZIP。`content-type` が `text/html` なら拒否（既存 BOOTH と同じ）。
- 商品ページは一般公開だが age-check Cookie 前提（WebView でログインしていれば付与済み）。`__MACOSX` 等の junk は既存フィルタで除外。
- CG セットのページ数は ZIP 内自然順ソートで決定。不安定なソートはファイル名昇順にフォールバック。
