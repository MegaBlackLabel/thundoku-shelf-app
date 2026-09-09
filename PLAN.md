# DLsite取り込み（PLAN）

## Context

Thundoku Shelf Desktop に、DLsite（`www.dlsite.com`）で購入した作品の**画像系のみ**を持ち込み、既存ビューアー（`.opfspack` 画像ビューアー）で表示する。FANZA同人（`crates/core/src/fanza/`）実装をミラーし、DB の共有ソースメタ列（`media_category`/`ai_type`/`is_drm`/`release_date`/`description`/`theme`/`maker_id`/`page_count`/`age_rating`/`series_name`）・ `sites` 行（`'dlsite'`）は migrate() に実装済み。

- 対象ストア: DLsite（`www.dlsite.com`）。ログインは **viviON ID**（`login.dlsite.com/login?user=self`）。購入作品は**ストア別 HTML ページ**（公開 JSON API は無い）で列挙。
- 認証: アプリ内 WebView でログイン → `www.dlsite.com` のセッション Cookie（`__DLsite_SID` / `uhashjp` / `uid_jp` / `jwt` / `dlloginjp` / `loginchecked` / `session_state`）を捕捉・永続化（`dlsite.session`）。**`jwt` がダウンロードの署名鍵**（作品ごとに `aud=RJ{id}` / `path=/content/work/{domain}/{base}/{id}.zip` / `exp` を持つ署名 Cookie）。
- 取得 API / ページ（実測済み 2026-09-09）:
  - 購入一覧 `GET https://www.dlsite.com/{store}/mypage/userbuy/=/type/all/start/all/sort/1/order/1/page/{n}` → HTML。`{store}` は `maniax` / `home` / `books` / `ai`（`soft`/`app` はゲーム・対象外）。行は `#buy_history_this table.work_list_main tr`（`td.buy_date` でヘッダ行除外）。ページング終端は `table.global_pagination td.page_no a`（`最後` リンク href `/page/(\d+)` or 数値リンク最大値）、フォールバックは `td.buy_date` 行 0 件で打ち切り。
  - 作品メタ `GET https://www.dlsite.com/{store}/product/info/ajax?product_id={id}`（Cookie + XHR ヘッダ、複数 ID はカンマ区切り可）→ `{ id: { site_id, work_type, maker_id, work_name, regist_date, price, official_price, discount_rate, down_url, custom_genres[], options, age_category, title_id/title_name, dl_count, work_image } }`。**FANZA の `details` + 商品ページを 1 本化した中身**。
  - ダウンロード: `down_url` をブラウザ UA + Referer + セッション Cookie で GET → **302** `https://download.dlsite.com/get/=/type/work/domain/{domain}/dir/{base}/file/{id}.zip/_/{update_date}?update_date=...` ＋ `jwt` を Set-Cookie で再発行 → Location を `jwt`（+ `__DLsite_SID`/`uhashjp`/`uid_jp`）付きで取得 → **200 `application/zip`**。`__cf_bm`（Cloudflare）は必須でない（最小 Cookie で取得成功を実測）。
- **分類（2 軸）**:
  - 軸1 メディア種別: `work_type`（ajax）or `.work_genre` の `icon_*`。`MNG`=漫画→comic、`ICG`=CG/イラスト→cg、`SOU`=ボイス/ASMR→voice、`ACN/ADV/QIZ/RPG/STG/SLN/TBL/TYP/PZL/ETC`=ゲーム→game、`NRE/DNV`=ノベル→novel（除外）、`VCM`=ボイスコミック→video（除外）、`WBT`=Webtoon→video（除外）、不明→other（除外）。
  - 軸2 AI 生成: `.work_genre` の `icon_AIG`（"AI生成作品"）=full、`icon_AIP`（"AI一部利用"）=partial、無し=none。`site_id=="ai"`（AI フロア）も full と一致。
  - **取り込み対象**: comic / cg（`icon_MNG`/`work_type=MNG`、`icon_ICG`/`ICG`）。
  - **除外**: voice / game / novel / video / 不明（安全側）。WBT（Webtoon）/ VCM（ボイスコミック）も除外。
- 年齢指定: `age_category`（実測 1=全年齢、2 以上は R18 と想定）+ ストアフロア（`home`/`ai`=全年齢、`maniax`=R18）→ `age_rating`。

## Approach

### Slice 1 — メディア分類（純関数・TDD）
- 新規 `crates/core/src/dlsite/mod.rs`:
  - `enum DlsiteMediaCategory { Comic, Cg, Voice, Game, Novel, Video, Other }`
  - `enum DlsiteAiType { None, PartialAi, FullAi }`
  - `struct DlsiteMeta { media: DlsiteMediaCategory, ai: DlsiteAiType, age: Option<&'static str> }`
  - `fn media_from_work_type(work_type: &str) -> Option<DlsiteMediaCategory>`（`MNG`/`ICG`/`SOU`/game 群/`NRE`/`DNV`/`VCM`/`WBT`）
  - `fn media_from_genre_icons(icons: &[String]) -> Option<DlsiteMediaCategory>`（`icon_MNG`/`icon_ICG`/`icon_SOU`/`icon_NRE` 等）
  - `fn ai_from_genre_icons(icons: &[String]) -> DlsiteAiType`（`icon_AIG`→Full、`icon_AIP`→Partial）。`site_id=="ai"` があれば Full 優先。
  - `fn age_from(site_id: &str, age_category: Option<i64>) -> Option<&'static str>`
  - `fn classify(work_type, genre_icons, site_id, age_category) -> DlsiteMeta`
  - `fn is_viewable_included(meta, drm_ok) -> bool`（drm_ok && (Comic | Cg)）
  - `fn media_to_str` / `fn ai_to_str`（DB 値: comic/cg/voice/game/novel/video/other、none/partial/full）
- 再利用: FANZA の `classify`/`is_viewable_included` と同流儀（実装は dlsite 内に自己完結。`fanza` は触らない）。
- テスト: 表形式。`(MNG, comic)`・`(ICG, cg)`・`(SOU, voice)`・`(ACN, game)`・`(NRE, novel)`・`(VCM, video)`・`(WBT, video)`・`(,,,,unknown, other)` → include は comic/cg のみ。`icon_AIG`→full、`icon_AIP`→partial。age (`home`,1)→全年齢 / (`maniax`,2)→R18。
- agent: `coder`

### Slice 2 — DlsiteClient（Transport 抽象・モック可能）
- 新規 `crates/core/src/dlsite/client.rs`:
  - `const STORES: [&str] = ["maniax", "home", "books", "ai"]`（`soft`/`app` はゲーム・対象外）。
  - `struct DlsiteSession { cookies: HashMap<String,String> }`（`logged_in()` / `cookie_header()`）
  - `struct DlsitePurchase { content_id, title, work_type, genre_icons: Vec<String>, maker_name, maker_id, price, purchase_date, thumbnail_url, down_url }`
  - `struct DlsiteWorkMeta { site_id, work_type, maker_id, work_name, regist_date, price, official_price, discount_rate, down_url, custom_genres: Vec<String>, options: String, age_category: Option<i64>, title_name, dl_count }`
  - `struct DlsiteClient { transport: Box<dyn Transport>, session: DlsiteSession }`
  - `pub fn with_transport(transport, session) -> Self`
  - `pub fn purchased(&self) -> Result<Vec<DlsitePurchase>, DlsiteError>`（各 STORE を userbuy ページング。`page_no` 最大/`最後` で終端、`td.buy_date` 行 0 件でフォールバック打ち切り）
  - `pub fn product_info(&mut self, ids: &[&str]) -> Result<HashMap<String,DlsiteWorkMeta>, DlsiteError>`（カンマ区切り一括、store は query 結果の `site_id` を使う）
  - `pub fn download_with_progress(&mut self, down_url: &str, on_progress: &mut dyn FnMut(u64,u64)) -> Result<Vec<u8>, DlsiteError>`（302 で `jwt` Set-Cookie 捕捉 → CDN へ直行。HTML 拒否）
  - `type DlsiteError`（`SessionExpired`/`Unauthorized`/`NotDownloadable`/`Http`/`Parse`/`Database`/`Transport`）
- 純関数パーサ: `parse_userbuy_row(tr_html) -> DlsitePurchase`・`parse_product_info(json) -> HashMap<...>`・`parse_last_page(html) -> Option<usize>`（booth.rs の `parse_library` と同流儀、テスト可能）。
- 再利用: `tbf::transport::Transport` / `UreqTransport`。
- ヘッダ: ブラウザ UA（`Mozilla/5.0 ... Chrome/126...`）+ `Referer: https://www.dlsite.com/`。ajax は `X-Requested-With: XMLHttpRequest` + `Accept: application/json`。
- テスト: scripted `Transport` で ①purchased が複数ストア/ページング・`page_no` 終端 ②product_info が work_type/regist_date/maker_id/down_url にマップ ③parse_userbuy_row が icon（MNG/ICG/SOU/AIG/AIP）抽出 ④download が 302→`download.dlsite.com` 追跡・`jwt` 付与・HTML 拒否 ⑤Cookie ヘッダ付与。
- agent: `coder`

### Slice 3 — 永続化 / 分類フィルタ / メタ列
- 新規 `crates/core/src/dlsite/sync.rs`:
  - `pub const SITE_ID_DLSITE: &str = "dlsite"`
  - `pub fn save_purchases(pool, client) -> Result<usize, DlsiteError>`:
    - `purchased()` → `classify(work_type, genre_icons, site_id, age_category)` → `is_viewable_included` を満たすものだけ `bookshelf::upsert`（`site_id="dlsite"`、`database_id=content_id`）。除外は upsert しない。
    - リッチメタは `product_info()`（一括）から取得し、`release_date=regist_date`・`maker_id`・`description`（商品ページ `<meta name="description">`）・`age_rating`・`series_name=title_name`・`custom_genres`→`tags_json` + `book_tags` へ反映。`theme`/`page_count` は source から取れない（`page_count` は import 時に実測）。
    - 取得失敗はベストエフォート（一覧の値のみで続行）。
  - 削除ポリシー: ライブラリから消えた unlinked 行のみ削除（local は保持）。
- 再利用: 既存 `bookshelf::upsert` / `books::set_metadata` / `tags` 書き込み。
- テスト: 空 DB マイグレーション + 混在カテゴリ save → `(site_id='dlsite')` に comic/cg のみ、voice/game/novel/video 無し。`media_category`/`ai_type`/`release_date`/`maker_id`/`age_rating`/`series_name` が保存され、custom_genres が `tags_json` に入る。
- agent: `coder`

### Slice 4 — ダウンロード / import / リンク
- `crates/app/src/views/bookshelf.rs`:
  - `download_item` の BOOTH/TBF/FANZA 分岐に **`else if site_id == "dlsite"`** を追加。DLsite は `product_info` で `down_url` → `download_with_progress`。
  - import 後 `books::set_site_id(..., "dlsite")` + `set_tbf_product_id(..., content_id)`、`media_category`/`ai_type`/`is_drm`/リッチメタ（`description`/`age_rating`/`series_name`）を `books` へ、custom_genres を `book_tags`/`tags_json` へ。
- エッジ処理:
  - DLsite に DRM フラグは無い（購入=ZIP 配信）→ `is_drm` は 0。カード DRM バッジ表示は対象外。
  - CG セット ZIP は**自然順ソート**（`import/mod.rs` の画像ソートを自然順へ。FANZA で実施済みなら流用）。
  - `page_count` は実測（import 後 `imported_documents.total_pages`）を優先。
  - DLsite ダウンロードは `jwt`（302 Set-Cookie）が要るため、`download_with_progress` が CDN へ `jwt` を含む Cookie を送る（FANZA の CloudFront 連鎖と同種だが JWT 単体で足りる）。
- 再利用: 既存 `import_zip_bytes` / `import_pdf` / `import_image_bytes`、`sniff_extension`。
- テスト: 小さい DLsite 画像 ZIP + PDF import → `.opfspack` 生成、`source_type='image-set'`、順序付き `document_images`、`books.site_id='dlsite'`、メタ列/`book_tags` 反映、正の page count、自然順ソート。
- agent: `coder`

### Slice 5 — アプリ認証 / sync / UI 配線
- `crates/app/src/app_state.rs`: `dlsite_session`/`dlsite_logged_in`、`save_dlsite_session`/`clear_dlsite_session`（`app_settings` キー `dlsite.session` に JSON）。復元は `logged_in()` フィルタ。
- `crates/app/src/views/dlsite_login.rs`: `booth_login.rs`/`fanza_login.rs` を元に WebView ログイン（`login.dlsite.com/login?user=self` → ログイン → **`www.dlsite.com` に遷移してストア Cookie（`__DLsite_SID`/`jwt`/...）を捕捉**）。viviON ID の初回ガイドは `www.dlsite.com` へ遷移すれば通過。
- `crates/app/src/views/auth.rs`: `AuthProvider::Dlsite` 追加。
- `BookshelfView::sync_dlsite` + `sync_all` に `Some("dlsite")`。専用スレッド + timeout。
- `crates/app/src/workspace.rs`: サイドバー / アカウントパネル / フィルタに DLsite 追加。
- `crates/app/src/views/settings.rs`: `logout_dlsite`、FANZA と同様に DLsite ビューアー既定値（`viewer.mode.dlsite`=spread、`viewer.page_turn.dlsite`=right-to-left）。
- `crates/app/src/components/image_viewer/mod.rs`: `site_id=="dlsite"` 既定で見開き + 右綴じ（FANZA と同様、同人漫画は右綴じ）。
- テスト（GPUI）: DLsite 選択で `sync_dlsite` のみ dispatch、フィルタで DLsite 行、auth provider が DLsite ログインを開く。
- agent: `coder`（UI 部分は `designer` が補助）

### Slice 6 — 最終レビュー / 検証
- クロスサイト identity: 同一 `RJ{id}` が BOOTH/TBF/FANZA と衝突しないか確認（site_id 別に独立）。`find_by_source`/`resolve_reuse_id` が site 込みで解決することを確認。
- `reviewer` によるセキュリティ・コードレビュー（Cookie の平文保存、ログ流出、HTTPS、エラー処理）。
- agent: `reviewer`

## Critical files & anchors

1. `crates/core/src/dlsite/{mod,client,sync}.rs`（新規）— 分類・クライアント（userbuy スクレイプ・product/info/ajax・jwt ダウンロード）・save。`tbf/transport.rs` の `Transport`/`UreqTransport` 再利用。
2. `crates/core/src/db/bookshelf.rs` / `books.rs` — `BookshelfItem`/`Book` の共有メタ列フィールド追加（FANZA で実装済み、DLsite も利用）。`book_tags` へのカスタムジャンル書き込み。
3. `crates/core/src/db/mod.rs`（migrate の runtime DDL: 共有メタ列 + `('dlsite','DLsite',...)` sites 行、実装済み）。
4. `crates/app/src/views/bookshelf.rs` — `sync_all`/`sync_dlsite`、`download_item` 4 分岐、import 後メタ補完。
5. `crates/app/src/views/dlsite_login.rs`（新規）+ `crates/app/src/app_state.rs` — viviON ID 認証 / `dlsite.session` 永続化 / ストア Cookie 捕捉。

## Verification

- `mise exec -- cargo test -p thundoku-core`（分類・クライアント・パーサ・DB/メタ列・save_purchases）
- `mise exec -- cargo test -p thundoku-app`（sync 配線・ビューアー）
- `mise exec -- cargo build` / `mise run build`、`mise run lint`（clippy）
- 手動: アプリ起動 → DLsite ログイン（viviON ID）→ 同期 → comic/CG のみカード表示 → クリックで `.opfspack` import → ビューアー表示。voice/game/ノベル/動画がカードに出ないこと。カードに発売日/作者/年齢/シリーズ/カスタムジャンルタグが表示されること。
- 実機プローブ（保存時）: `DLSITE_TEST_COOKIE` で `product/info/ajax` → `down_url` → CDN ZIP 取得が 200 で `application/zip` になること（`#[ignore]` テスト）。

## Assumptions & contingencies

- ログインは viviON ID（`login.dlsite.com/login?user=self`）。初回は viviON 紹介ページへ出るが、`www.dlsite.com` へ遷移すればログイン済み。
- セッション Cookie は `app_settings`（`dlsite.session`）に JSON 保存（FANZA 同様 keyring 上限回避）。破損時は未ログイン扱いで再ログイン誘導。
- `work_type` / `.work_genre` icon は非公開仕様。変更リスクはあるが、`td.buy_date` 行 + `work_type`（ajax）で分類し、両方取れない場合は除外（安全側）。
- ノベル / Webtoon / ボイスコミックの扱い: `NRE`/`DNV`/`VCM`/`WBT` はいずれも除外（画像系は `MNG`（漫画）/ `ICG`（CG・イラスト）のみ）。未知 work_type は `Other`→除外。
- `age_category` の値（1=全年齢想定）と `description`・`theme`・`page_count` は source に無い/取れないものがある。`description` は商品ページ `<meta name="description">`（ベストエフォート）、`theme` は None、`page_count` は import 時に実測。
- ページングは `page_no` 最大値 / `最後` リンクで終端、`td.buy_date` 行 0 件でフォールバック。未ログインのログイン HTML も 0 行で安全に終了（+ HTTP ステータス確認）。
- ダウンロードは `jwt` 署名 Cookie（302 で再発行）が要る。`__cf_bm` は不要だが、将来 Cloudflare が厳格化した場合 UA/Referer 付与で対策。`content-type` が `text/html` なら拒否。
- `AI一部利用`（`icon_AIP`）は通常フロアの作品にも存在し得る。`icon_AIG`（AI フロア `site_id=ai`）と `icon_AIP` を区別して取り込む（どちらも画像系なら include、AI ラベルを `ai_type` に保存）。
- `sync_all` で複数サイト同時同期時は既存の background_spawn パターンを踏襲。DLsite はストア数が多くリクエスト数が増えるため、`page_no` 終端 + 0 行打ち切りで過剰アクセスを避ける。
