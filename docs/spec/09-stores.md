# 09. 対応ストア同期の詳細手順

> `docs/spec/README.md`（索引）から参照される設計仕様書の一部。
> 本章は **技術書典 / BOOTH / FANZA / DLsite の同期手順（番号付き）と対象データ**。
> 認証情報の保存先は `docs/spec/06-sync-auth-drive.md`、通知と非機能は `docs/spec/08-notifications-and-nonfunctional.md`。
> 事実にはアンカー付き。断定できない事項は 08 章の「不明点 / 推測」に集約してある。

## 4. 対応ストア詳細

各ストアの同期手順・ログイン方式・認証情報・レート制限・状態管理・ID 規約を以下に列挙する。

### 7.0 ストア横断サマリ

| ストア | site_id | 同期エントリポイント | 取得対象 | ログイン方式 | セッション保存先 / 保護 | 差分判定 |
|---|---|---|---|---|---|---|
| DLsite | `"dlsite"` `crates/core/src/dlsite/sync.rs:13` | `dlsite::sync::save_purchases` `crates/core/src/dlsite/sync.rs:23`（アプリ側は `BookshelfView::sync_dlsite` `crates/app/src/views/bookshelf.rs:2557`） | 購入済み一覧。走査フロア `["maniax","home","books","ai"]` `crates/core/src/dlsite/client.rs:15`、`RJ\d+` のみ採用 | アプリ内 WebView（DLsite ログイン → `www` + `login` の Cookie 収集）`crates/app/src/views/dlsite_login.rs:114-146` | `app_settings["dlsite.session"]`（**平文 JSON**）`crates/app/src/app_state.rs:439-441` | なし（毎回 全ストア × 全ページ + 全件 UPSERT、削除なし）`crates/core/src/dlsite/sync.rs:23-100` |
| FANZA同人 | `"fanza"` `crates/core/src/fanza/sync.rs:13` | `fanza::sync::save_purchases` `crates/core/src/fanza/sync.rs:51` | mylibrary API。`PAGE_LIMIT = 20` 件/ページ `crates/core/src/fanza/client.rs:17`、安全弁 2000 件 `:212` | アプリ内 WebView（購入済み作品ページ → 年齢確認）`crates/app/src/views/fanza_login.rs:3` | `app_settings["fanza.session"]`（**平文 JSON**）`crates/app/src/app_state.rs:419-421` | なし（全件 UPSERT）`crates/core/src/fanza/sync.rs:52-100` |
| BOOTH | `"booth"` 固定 `crates/app/src/views/bookshelf.rs:2341` | **core に同期エンジンが無くアプリ層** `BookshelfView::sync_booth` `crates/app/src/views/bookshelf.rs:2253` | `BoothClient::library()`（`accounts.booth.pm/library`）と `BoothClient::orders()`（`accounts.booth.pm/orders`）`crates/core/src/booth.rs:200`, `:266` | アプリ内 WebView（incognito）+ 1 秒間隔の URL 監視 `crates/app/src/views/booth_login.rs:46`, `:83` | `app_settings["booth.session"]`（**平文 JSON**）`crates/app/src/app_state.rs:394-396` | 全件 UPSERT + 集合差 DELETE 3 種 `crates/app/src/views/bookshelf.rs:2383-2430` |
| 技術書典 | `"techbookfest"` `crates/core/src/tbf/mod.rs:22` | `tbf::sync::save_bookshelf` `crates/core/src/tbf/sync.rs:70` / `save_events` `:130` / `refresh_checklist` `:244` | GraphQL。本棚・チェックリスト・イベント。`first: 100` + `endCursor` カーソル `crates/core/src/tbf/mod.rs:289-327` | アプリ内 WebView（`techbookfest.org/user/signin`）+ 1 秒間隔の URL 監視 | **OS keyring**（`techbookfest`）に `TbfSession` JSON `crates/core/src/secrets.rs:10` | `refresh_checklist` が `SyncPollOutcome.changed` を返す `crates/core/src/tbf/sync.rs:211-244` |

### 7.1 DLsite

出典: `crates/core/src/dlsite/`（`sync.rs` / `client.rs` / `mod.rs`）, `crates/app/src/views/dlsite_login.rs`

情報源（全行精読）:
- `crates/core/src/dlsite/sync.rs`（358 行 / 14,579 B）
- `crates/core/src/dlsite/mod.rs`（309 行 / 10,136 B）
- `crates/core/src/dlsite/client.rs`（723 行 / 29,505 B）
- `crates/app/src/views/dlsite_login.rs`（289 行 / 12,680 B）
- 参照: `crates/app/src/app_state.rs`・`crates/app/src/views/bookshelf.rs`・`crates/app/src/views/settings.rs`・`crates/app/src/views/auth.rs`・`crates/app/src/views/workspace.rs`・`crates/core/src/db/bookshelf.rs`・`crates/core/src/db/books.rs`・`crates/core/src/db/mod.rs`・`crates/core/src/db/settings.rs`・`crates/core/src/db/sync_state.rs`・`crates/core/src/db/schema.sql`・`crates/core/src/tbf/transport.rs`

注: 課題票の「sync.rs 42.6KB」は実測 14,579 B（358 行）。本ファイルの数値は実測値。

---

#### 1. 同期手順

#### 1.1 エントリポイント / シグネチャ

| 項目 | 事実 | アンカー |
|---|---|---|
| 公開定数 | `pub const SITE_ID_DLSITE: &str = "dlsite";` | `crates/core/src/dlsite/sync.rs:13` |
| エントリ関数 | `pub fn save_purchases(pool: &SqlitePool, client: &mut DlsiteClient) -> Result<usize, DlsiteError>` | `crates/core/src/dlsite/sync.rs:23` |
| 戻り値 | upsert に成功した **保存件数**（`usize`）。除外カテゴリは含まない | `crates/core/src/dlsite/sync.rs:97-99` |
| 時刻ヘルパ | `fn now() -> String` = `chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")`（**UTC**、ローカル時刻ではない） | `crates/core/src/dlsite/sync.rs:15-17` |
| クライアント生成 | `DlsiteClient::with_transport(Box<dyn Transport>, DlsiteSession)` | `crates/core/src/dlsite/client.rs:117-119` |
| app 側の呼び出し元 | `BookshelfView::sync_dlsite`（`pub fn sync_dlsite(&mut self, cx: &mut Context<Self>)`）→ `std::thread::spawn` → `thundoku_core::dlsite::sync::save_purchases(&db, &mut client)` | `crates/app/src/views/bookshelf.rs:2557`, `:2581`, `:2586` |
| 実行スレッド | 専用 `std::thread` 1 本（UI 非ブロック）。結果は `std::sync::mpsc::channel::<Result<usize, String>>()` で返す | `crates/app/src/views/bookshelf.rs:2580-2589` |

#### 1.2 番号付き処理手順（`save_purchases` 本体）

1. `client.purchased()?` で購入済み一覧を **全ストア・全ページ** 取得（`?` なので失敗＝即 `Err`、部分結果は返らない）。`crates/core/src/dlsite/sync.rs:24`
2. 一覧の `content_id` を `Vec<&str>` に集める。`crates/core/src/dlsite/sync.rs:26`
3. `client.product_info(&ids).unwrap_or_default()` でリッチメタを **一括取得**。エラーは空 `HashMap` に潰して続行（ベストエフォート）。`crates/core/src/dlsite/sync.rs:27`
4. 各作品 `p` について `meta = metas.get(&p.content_id)`（`Option`）。`crates/core/src/dlsite/sync.rs:29-30`
5. `site_id` = meta の `site_id`、無ければ一覧 HTML 由来の `p.site_id`。`crates/core/src/dlsite/sync.rs:31-34`
6. `age_category = meta.and_then(|m| m.age_category)`（meta 無しは `None`）。`crates/core/src/dlsite/sync.rs:35`
7. `classify(&p.work_type, &p.genre_icons, site_id, age_category)` で 2 軸分類＋年齢。`crates/core/src/dlsite/sync.rs:36`
8. `is_viewable_included(&cfg, true)`（**drm_ok は常に `true` 固定**）が false なら `continue`（= upsert しない）。`crates/core/src/dlsite/sync.rs:37-38`
9. `ts = now()`（UTC 文字列）を 1 度だけ作る。`crates/core/src/dlsite/sync.rs:39`
10. `tags_json` = `custom_genres` が空なら `None`、そうでなければ `serde_json::to_string(&custom_genres)`（失敗時 `None`）。`crates/core/src/dlsite/sync.rs:41-47`
11. `thumbnail_url` の優先順位: (a) `meta.work_image` → (b) `p.thumbnail_url`（`data:` 始まりを除外）→ `//` 始まりは `https:` 前置に正規化。`crates/core/src/dlsite/sync.rs:48-58`
12. `BookshelfItem` を構築（§1.5 の表）。`crates/core/src/dlsite/sync.rs:59-95`
13. `bookshelf::upsert(pool, &item)?` を **1 件ずつ** 実行（明示トランザクション無し、失敗は即 `Err` で残りを中断）。`crates/core/src/dlsite/sync.rs:96`
14. `saved += 1`。`crates/core/src/dlsite/sync.rs:97`
15. ループ終了後 `Ok(saved)`。`crates/core/src/dlsite/sync.rs:99`

#### 1.3 `purchased()`（一覧取得）の詳細

- 走査対象ストアフロア: `pub const STORES: [&str; 4] = ["maniax", "home", "books", "ai"];`（`soft` / `app` は対象外とコメント）。`crates/core/src/dlsite/client.rs:15`
- リクエスト: `GET https://www.dlsite.com/{store}/mypage/userbuy/=/type/all/start/all/sort/1/order/1/page/{page}`（`type/all` / `start/all` / `sort/1` / `order/1`、ページは 1 始まり）。`crates/core/src/dlsite/client.rs:163-165`
- `page` は 1 から開始、ストアごとにリセット。`crates/core/src/dlsite/client.rs:158`
- 終了条件（この順に評価）:
  1. `page > MAX_PAGES_PER_STORE`（= 200）→ break（**取得前**チェック）。`crates/core/src/dlsite/client.rs:160-162`
  2. そのページの行が 0 件 → break（フォールバック打ち切り）。`crates/core/src/dlsite/client.rs:174-176`
  3. `parse_last_page` が返した最大ページ番号 `last` に対し `page >= lp` → break。`crates/core/src/dlsite/client.rs:177-181`
  4. それ以外は `page += 1` で継続（上限なしの無限ページングは 1. が抑止）。`crates/core/src/dlsite/client.rs:182`
- 総件数取得: **専用 API なし**。終端は「HTML 内の `/page/(\d+)` の最大値」と「行 0 件」で判定。`crates/core/src/dlsite/client.rs:445-451`
- 重複排除: 全ストア横断で `HashSet<String>` に `content_id` を入れ、**初出のみ**採用（`STORES` の並び順＝ maniax が優先）。`crates/core/src/dlsite/client.rs:156`, `:169-173`
- 取得失敗時: `get_html` の `?` で即 `Err`（1 ページでも失敗すると全体失敗、部分結果は返らない）。`crates/core/src/dlsite/client.rs:166`
- 1 ページあたりの件数: コード上の明示定数 **なし**（`MAX_PAGES_PER_STORE` のコメントは「1 ページあたりの行数境界」だが実際は最大ページ数として使用）。`crates/core/src/dlsite/client.rs:16-17`
- HTTP ヘッダ（HTML 取得 = `cookie_headers()`）: `Cookie` / `User-Agent` / `Referer: https://www.dlsite.com/` / `Accept: text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8` / `Accept-Language: ja,en;q=0.9` / `Sec-Fetch-Dest: document` / `Sec-Fetch-Mode: navigate` / `Sec-Fetch-Site: same-origin` / `Upgrade-Insecure-Requests: 1`。`crates/core/src/dlsite/client.rs:121-138`
- User-Agent 実値（1 行定数）: `Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36`。`crates/core/src/dlsite/client.rs:20`
- メソッドは GET、ボディは `None`、`redirects: 3`。`crates/core/src/dlsite/client.rs:313-321`

#### 1.4 `product_info()`（作品メタ）の詳細

- シグネチャ: `pub fn product_info(&mut self, ids: &[&str]) -> Result<HashMap<String, DlsiteWorkMeta>, DlsiteError>`。`crates/core/src/dlsite/client.rs:189-192`
- チャンク: `ids.chunks(20)` = **20 件/リクエスト**、`product_id` クエリにカンマ区切りで連結。`crates/core/src/dlsite/client.rs:194-197`
- URL: `GET https://www.dlsite.com/maniax/product/info/ajax?product_id={joined}`（**ストアは maniax 固定**。作品 ID はフロアを跨いで解決されるとのコメント）。`crates/core/src/dlsite/client.rs:196-197`
- ヘッダ: `ajax_headers()` = `cookie_headers()` の `Accept` を `application/json` に置換し `X-Requested-With: XMLHttpRequest` を追加。`crates/core/src/dlsite/client.rs:140-147`
- メソッド GET / body `None` / `redirects: 3`。`crates/core/src/dlsite/client.rs:200-205`
- ステータス判定: `401|403` → `DlsiteError::Unauthorized(status)`、`200` 以外 → `DlsiteError::Http(status)`、200 → `String::from_utf8_lossy` で JSON 化。`crates/core/src/dlsite/client.rs:208-215`
- レスポンス JSON のキー = 作品 ID、値オブジェクトのフィールド: `site_id` / `work_type` / `maker_id` / `work_name` / `regist_date` / `price`(i64) / `down_url` / `custom_genres`(文字列配列) / `options` / `age_category`(i64) / `title_name` / `work_image`。`crates/core/src/dlsite/client.rs:419-441`
- JSON パース失敗時は `Value::Null` 扱い＝ **空マップ**（エラーにしない）。`crates/core/src/dlsite/client.rs:420-421`

#### 1.5 `bookshelf::upsert` へ渡す値（DLsite 固定値つき）

| フィールド | 値 | アンカー |
|---|---|---|
| `site_id` | `"dlsite"`（定数） | `crates/core/src/dlsite/sync.rs:60` |
| `database_id` | `content_id`（例 `RJ01234567`） | `crates/core/src/dlsite/sync.rs:61` |
| `title` | 一覧 HTML の `class="work_name"` 内 `<a>` テキスト | `crates/core/src/dlsite/sync.rs:62` |
| `circle_name` | 一覧 HTML の `class="maker_name"` 内 `<a>` テキスト | `crates/core/src/dlsite/sync.rs:63` |
| `author` | `String::new()`（**常に空文字**。同期では作者を取らない） | `crates/core/src/dlsite/sync.rs:64` |
| `thumbnail_url` | §1.2-11 の正規化後 `Option<String>` | `crates/core/src/dlsite/sync.rs:65` |
| `format` | `"ZIP"` 固定 | `crates/core/src/dlsite/sync.rs:66` |
| `caused_at` | 一覧 HTML `class="buy_date"` の生テキスト（例 `2026/05/04 17:03`、**正規化しない**） | `crates/core/src/dlsite/sync.rs:67` |
| `event_name` / `event_slug` / `event_id` / `file_name` | すべて `None` | `crates/core/src/dlsite/sync.rs:68-72` |
| `download_url` | 一覧 HTML の `href="...\/download\/..."` | `crates/core/src/dlsite/sync.rs:73` |
| `is_downloadable` | `1` | `crates/core/src/dlsite/sync.rs:74` |
| `is_checked` | `0` | `crates/core/src/dlsite/sync.rs:75` |
| `is_purchased` | `1` | `crates/core/src/dlsite/sync.rs:76` |
| `is_new` | `0` | `crates/core/src/dlsite/sync.rs:77` |
| `is_active` | `1` | `crates/core/src/dlsite/sync.rs:78` |
| `is_favorite` / `is_hidden` / `hidden_at` | `0` / `0` / `None`（DB 側で**保持**される。§1.6） | `crates/core/src/dlsite/sync.rs:79-81` |
| `tags_json` | `custom_genres` の JSON 配列 or `None` | `crates/core/src/dlsite/sync.rs:82` |
| `synced_at` / `created_at` / `updated_at` | すべて同一の `now()`（UTC `%Y-%m-%d %H:%M:%S`） | `crates/core/src/dlsite/sync.rs:83-85` |
| `media_category` | `media_to_str(cfg.media)`（`comic`/`cg`/…） | `crates/core/src/dlsite/sync.rs:86` |
| `ai_type` | `ai_to_str(cfg.ai)`（`none`/`partial`/`full`） | `crates/core/src/dlsite/sync.rs:87` |
| `is_drm` | `0` 固定 | `crates/core/src/dlsite/sync.rs:88` |
| `release_date` | meta の `regist_date`（形式例 `2025-06-17 16:00:00`、DLsite は唯一 release_date を取得するサイト） | `crates/core/src/dlsite/sync.rs:89`, `docs/features.md:209` |
| `description` / `theme` / `page_count` | `None` | `crates/core/src/dlsite/sync.rs:90-92` |
| `maker_id` | meta の `maker_id`（`RG\d+`） | `crates/core/src/dlsite/sync.rs:93` |
| `age_rating` | `cfg.age` を `String` 化（`"all"` / `"r18"` / `None`） | `crates/core/src/dlsite/sync.rs:94` |
| `series_name` | meta の `title_name` | `crates/core/src/dlsite/sync.rs:95` |

#### 1.6 DB 反映の実体（UPSERT）

- 対象テーブル: `bookshelf_items`。主キーは `PRIMARY KEY (site_id, database_id)`。`crates/core/src/db/schema.sql:76-101`
- 文: `INSERT INTO bookshelf_items (…35 列…) VALUES (?1..?35) ON CONFLICT(site_id, database_id) DO UPDATE SET …`。`crates/core/src/db/bookshelf.rs:63-102`
- トランザクション単位: **1 件 1 文（autocommit）**。`bookshelf::upsert` は `block_on(async { sqlx::query(...).execute(pool).await })` のみで `BEGIN`/`COMMIT` を発行しない。`crates/core/src/db/bookshelf.rs:61-62`, `:139-143`
- DB 接続設定: `journal_mode(WAL)` / `busy_timeout = 5 秒` / `foreign_keys(true)`。`crates/core/src/db/mod.rs:51-57`
- `block_on` はプロセス共通ランタイム（`worker_threads(2)` のマルチスレッド tokio）。`crates/core/src/db/mod.rs:30-45`
- DO UPDATE で **更新される**列（DLsite 同期の値で上書き）: `title` / `circle_name` / `author` / `thumbnail_url` / `format` / `causedAt` / `event_name` / `event_slug` / `event_id` / `file_name` / `download_url` / `is_downloadable` / `is_checked` / `is_purchased` / `is_new` / `is_active` / `synced_at` / `updated_at` / `media_category` / `ai_type` / `is_drm` / `release_date` / `description` / `theme` / `maker_id` / `page_count` / `age_rating` / `series_name`。`crates/core/src/db/bookshelf.rs:67-102`
- DO UPDATE で **保持される**列: `is_hidden` / `hidden_at`（既存値のまま）、`tags_json` は `COALESCE(excluded.tags_json, bookshelf_items.tags_json)`（NULL のときだけ既存保持）。`crates/core/src/db/bookshelf.rs:84-86`
- `author` は DO UPDATE 内で **2 回**代入される（前半 `author = excluded.author`、後半 `author = CASE WHEN excluded.author = '' THEN bookshelf_items.author ELSE excluded.author END`）。`crates/core/src/db/bookshelf.rs:70`, `:87-90`
- `is_favorite` と `created_at` は DO UPDATE の SET に **含まれない** → 初回値が保持される。`crates/core/src/db/bookshelf.rs:67-102`
- 削除伝播: **なし**。同期で消えた作品・除外カテゴリに変わった作品の行は削除されない（DELETE 文は存在しない）。`crates/core/src/dlsite/sync.rs:23-100`
- 失敗時の部分成功: 途中の `upsert` がエラーになると `?` で中断するが、それ以前の行は autocommit 済みで **残る**（ロールバックされない）。`crates/core/src/dlsite/sync.rs:96`
- `product_info` 段の失敗は全体失敗にしない（メタ無し＝一覧の値のみで全件 upsert 継続）。`crates/core/src/dlsite/sync.rs:27`

#### 1.7 差分判定・ハッシュ・更新日時

- **差分判定は存在しない**。毎回「全ストア × 全ページ取得 → 画像系全件 upsert」のフル同期（比較キー・ハッシュ・`If-Modified-Since` 等の実装なし）。`crates/core/src/dlsite/client.rs:152-185`, `crates/core/src/dlsite/sync.rs:23-100`
- 結果として毎回 `synced_at` と `updated_at` が現在 UTC 時刻で書き換わる。`crates/core/src/dlsite/sync.rs:83-85`
- 購入履歴の HTML には 1 行ごとに購入日時（`class="buy_date"`、例 `2026/05/04 17:03`）があり、`causedAt` にそのまま入る（DLsite の日付形式は `YYYY/MM/DD` と docs に記載）。`crates/core/src/dlsite/client.rs:365`, `docs/features.md:206`

#### 1.8 エラー時の挙動 / キャンセル

| 事象 | 挙動 | アンカー |
|---|---|---|
| HTTP 401 / 403（一覧・メタ・DL・CDN） | `DlsiteError::Unauthorized(u16)`（表示文言 `不正アクセス（{status}）`） | `crates/core/src/dlsite/client.rs:208-209`, `:255-256`, `:297-298`, `:323-324` |
| HTTP 200 以外 | `DlsiteError::Http(u16)`（表示 `HTTP {status}`） | `crates/core/src/dlsite/client.rs:211-212`, `:300-301`, `:326-327` |
| 302 応答に `location` なし | `DlsiteError::Parse("302 応答に location がありません")` | `crates/core/src/dlsite/client.rs:250-254` |
| DL で 404 / 410 | `DlsiteError::NotDownloadable`（販売終了・未購入等） | `crates/core/src/dlsite/client.rs:257-258` |
| DL 応答が `<!doctype` / `<html` 始まり | `DlsiteError::Parse("HTML レスポンス（ファイルではない）")` | `crates/core/src/dlsite/client.rs:303-306` |
| JSON パース失敗（メタ） | エラーにしない（空マップ） | `crates/core/src/dlsite/client.rs:420-421` |
| `serde_json::Error` | `DlsiteError::Parse(msg)` へ変換 | `crates/core/src/dlsite/client.rs:40-43` |
| ネットワーク | `TbfError` → `DlsiteError::Transport` | `crates/core/src/dlsite/client.rs:207`, `:249`, `:295`, `:322` |
| DB | `sqlx::Error` → `DlsiteError::Database` | `crates/core/src/dlsite/client.rs:34-35` |
| `DlsiteError::SessionExpired` | **宣言のみで未使用**（構築箇所なし。401/403 は `Unauthorized` になる） | `crates/core/src/dlsite/client.rs:24-25` |
| app 側の失敗表示 | `Err(String)` をトースト（`ToastKind::Error`）に表示し、メッセージに `"not logged in"` か `"セッション"` を含むときだけログインモーダルを開く | `crates/app/src/views/bookshelf.rs:2615-2624` |
| app 側の未ログイン時 | `dlsite_logged_in == false` ならトースト「DLsite にログインしてから同期してください」＋ログインモーダルを開き、同期は開始しない | `crates/app/src/views/bookshelf.rs:2558-2571` |
| app 側の成功時 | トースト「DLsite サイトから {count} 件取得しました」＋ `reload` | `crates/app/src/views/bookshelf.rs:2606-2614` |

- キャンセル / 中断機構: **なし**。同期スレッドは最後まで走る（`std::thread::spawn` のハンドルを保持せず、切断用フラグも無い）。UI 側は `rx.try_recv()` を 120 ms 間隔でポーリングして完了を検知するだけ。`crates/app/src/views/bookshelf.rs:2589-2601`
- タイムアウトによる中断は transport 任せ（§4）。

---

#### 2. ログイン方式

#### 2.1 フロー関数一覧

| 関数 / 要素 | 役割 | アンカー |
|---|---|---|
| `AuthProvider::Dlsite` | 設定／本棚からログインモーダルを開くアクション | `crates/app/src/views/auth.rs:104` |
| `DlsiteLoginView::new(window, cx)` | WebView 生成 → 監視タスク開始 | `crates/app/src/views/dlsite_login.rs:29-37` |
| `try_create_webview(window, cx) -> Option<Entity<WebView>>` | `lb_wry::WebViewBuilder` で WebView を作り、初期 URL を読み込む | `crates/app/src/views/dlsite_login.rs:39-74` |
| `start_url_check(&mut self, cx)` | 1 秒間隔の URL 監視ループを `cx.spawn`（`detach`） | `crates/app/src/views/dlsite_login.rs:76-103` |
| `check_login(&mut self, cx) -> bool` | Cookie 収集・認証判定・永続化・`DlsiteLoginDone` 発行 | `crates/app/src/views/dlsite_login.rs:106-154` |
| `show` / `close` | モーダル表示／非表示（`check_generation` を増やして旧監視ループを無効化）、`close` は `DlsiteLoginCancelled` を発行 | `crates/app/src/views/dlsite_login.rs:164-169`, `:238-239` |
| `save_dlsite_session(cx, &session)` | DB 永続化＋グローバル状態更新 | `crates/app/src/app_state.rs:434-442` |

#### 2.2 番号付きログイン手順

1. モーダル生成時（`new`）に WebView を作成。ビルダは `lb_wry::WebViewBuilder::new().with_incognito(true)`（**非永続 = メモリのみ**。永続ストアだと SSO で自動再ログインされるため使わない旨コメント）。`#[cfg(debug_assertions)]` のときだけ `with_devtools(true)`。`crates/app/src/views/dlsite_login.rs:42-43`
2. `window.window_handle()` 取得失敗／`build()` 失敗時は `log::error!` して `None`（WebView なし＝監視は常に false）。`crates/app/src/views/dlsite_login.rs:47-60`
3. 初期 URL を読み込む（**ストアのログインルート**）: `https://www.dlsite.com/home/login/=/skip_register/1/_query/https://www.dlsite.com/home/mypage`。直後に `hide()`。`crates/app/src/views/dlsite_login.rs:62-73`
4. `start_url_check` が `check_generation` をインクリメントし、`cx.entity().downgrade()`（WeakEntity）でループを起動。`background_executor().timer(Duration::from_secs(1))` = **1 秒周期**。`crates/app/src/views/dlsite_login.rs:76-84`, `:87-91`
5. ループ先頭で `check_generation != generation` なら `true`（＝終了）。`handle.update` が `Err`（ビュー drop 済み）なら break。`done == true` で break。`crates/app/src/views/dlsite_login.rs:86-99`
6. `check_login` は `webview.read(cx).raw().url()` を取得（失敗は `false` 継続、`log::debug!("dlsite login check: url={url}")`）。`crates/app/src/views/dlsite_login.rs:107-113`
7. Cookie 収集: `cookies_for_url("https://www.dlsite.com")` と `cookies_for_url("https://login.dlsite.com")` の 2 origin を順に読み、`HashMap<String,String>` に `entry(name).or_insert(value)`（**同名は www 側を優先**＝先勝ち）。`crates/app/src/views/dlsite_login.rs:118-131`
8. 認証判定（両方を満たせば完了）: `__DLsite_SID` が存在し、かつ `uhashjp` **または** `uid_jp` が存在する。欠ける場合は `log::debug!` で未認証理由を出して `false`。`crates/app/src/views/dlsite_login.rs:133-145`
9. 完了時: `DlsiteSession::new(cookie_pairs)` を作り、`log::info!("dlsite login: {n} cookies captured")`、`save_dlsite_session(cx, &session)`、WebView を `hide()`、`cx.emit(DlsiteLoginDone)`、`true` 返却。`crates/app/src/views/dlsite_login.rs:146-154`
10. `auth.rs` 側は `DlsiteLoginDone` でモーダルを閉じる（`show_dlsite_login = false` など）。`DlsiteLoginCancelled` でも閉じる。`crates/app/src/views/auth.rs:203-219`
11. 自動ナビゲーションは **一切しない**（SSO 連鎖を中断するとゲスト `__DLsite_SID` のままになり、同期が `regist/user` へ 302 されて 0 件になる旨のコメント）。`crates/app/src/views/dlsite_login.rs:114-117`

#### 2.3 Cookie 名・取得方法・成否判定

| 項目 | 事実 | アンカー |
|---|---|---|
| 取得方法 | **アプリ内 WebView（gpui-wry / lb_wry）**。HTTP ログイン（フォーム POST）は行わない | `crates/app/src/views/dlsite_login.rs:1-2`, `:39-74` |
| セッション Cookie 名（必須） | `__DLsite_SID` | `crates/app/src/views/dlsite_login.rs:135` |
| 認証 ID Cookie（どちらか必須） | `uhashjp` または `uid_jp` | `crates/app/src/views/dlsite_login.rs:135-136` |
| ダウンロード用 Cookie | `jwt`（DL の 302 `Set-Cookie` から捕捉。セッションには保存せず、その 1 回のリクエストヘッダにのみ付与） | `crates/core/src/dlsite/client.rs:262-271` |
| ログイン成否判定 | WebView の Cookie 有無のみ。**API 叩き直し・トークン検証は行わない** | `crates/app/src/views/dlsite_login.rs:133-145` |
| `DlsiteSession::logged_in()` | `!self.cookies.is_empty()`（Cookie が 1 つでもあれば true） | `crates/core/src/dlsite/client.rs:57-59` |
| 起動時復元の判定 | `app_settings["dlsite.session"]` の JSON を `DlsiteSession` に復元し `logged_in()` でフィルタ | `crates/app/src/app_state.rs:222-227` |
| 年齢確認の分岐 | **DLsite ログインでの明示処理なし**（コード上に `age_check` 相当の分岐・文言なし。比較: FANZA は `age_check` を監視） | `crates/app/src/views/dlsite_login.rs:106-154`（対比: `crates/app/src/views/fanza_login.rs:109-113`） |
| 2 段階認証の分岐 | **なし**（WebView 内の viviON ID フローに委譲。コードに TOTP / 追加確認の分岐は存在しない） | `crates/app/src/views/dlsite_login.rs:62-73` |
| セッション切れの検知 | 同期・DL 時の HTTP 401/403（`Unauthorized`）と、app 側の文字列一致（`"セッション"` を含む `Err` メッセージ）でのみ | `crates/core/src/dlsite/client.rs:208-209`, `crates/app/src/views/bookshelf.rs:2618-2624` |

#### 2.4 モーダル UI の数値

| 項目 | 値 | アンカー |
|---|---|---|
| WebView サイズ | 640.0 × 480.0 px（ウィンドウ中央配置、位置は `(window_w - 640)/2`, `(window_h - 480)/2`） | `crates/app/src/views/dlsite_login.rs:179-196` |
| 背景 | `hsla(0.0, 0.0, 0.0, 0.45)`（全面 absolute） | `crates/app/src/views/dlsite_login.rs:216` |
| 閉じるボタン | 36.0 × 36.0 px、`top_3` / `right_3`、背景 `rgba(0xffffff26)` → hover `rgba(0xffffff40)`、アイコン `IconName::Close` 18.0 px | `crates/app/src/views/dlsite_login.rs:218-244` |

---

#### 3. 認証情報の保存先と保護

| 項目 | 事実 | アンカー |
|---|---|---|
| 保存先 | `app_settings` テーブル（key-value） | `crates/core/src/db/schema.sql:25-30`, `crates/core/src/db/settings.rs:11-28` |
| 保存キー | `"dlsite.session"` | `crates/app/src/app_state.rs:222`, `:438`, `:447` |
| 保存値の形式 | `serde_json::to_string(&DlsiteSession)` の JSON 文字列。`DlsiteSession` は `cookies: HashMap<String,String>` 1 フィールドなので実体は `{"cookies":{"<name>":"<value>", …}}` | `crates/app/src/app_state.rs:437-439`, `crates/core/src/dlsite/client.rs:47-50` |
| 保護 | **平文**（暗号化・難読化なし。`app_settings.value` は TEXT そのまま） | `crates/app/src/app_state.rs:437-439`, `crates/core/src/db/settings.rs:16-26` |
| keyring を使わない理由（コメント） | 「Cookie が巨大で keyring 上限を超えるため DB 保存」（BOOTH / FANZA と同流儀） | `crates/app/src/app_state.rs:433-434`, `:218-222` |
| 書き込みタイミング | ログイン完了直後（`save_dlsite_session`）のみ | `crates/app/src/views/dlsite_login.rs:148` |
| メモリ側 | `AppState.dlsite_session: Arc<Mutex<Option<DlsiteSession>>>` と `dlsite_logged_in: Arc<Mutex<bool>>` を同時更新 | `crates/app/src/app_state.rs:57-59`, `:440-441` |
| 削除タイミング 1 | 設定画面のログアウト `SettingsView::logout_dlsite`（`background_spawn` で `db::settings::delete`） | `crates/app/src/views/settings.rs:737-752` |
| 削除タイミング 2 | `clear_dlsite_session(cx)`（同期側の破棄 API。呼び出し箇所は `logout_dlsite` が直接 delete する実装） | `crates/app/src/app_state.rs:445-450` |
| Cookie ヘッダ生成 | `cookie_header()` = `{k}={v}` を `"; "` で連結。`HashMap` 反復なので **順序は不定** | `crates/core/src/dlsite/client.rs:61-67` |
| `jwt` の扱い | 302 応答の `Set-Cookie` から `jwt` のみ拾い、既存ヘッダに `jwt=` が無いときだけ追記。`self.session` には書き戻さない（＝永続化されない） | `crates/core/src/dlsite/client.rs:262-271` |

---

#### 4. レート制限・待機・リトライ・並列度・タイムアウト

| 項目 | 実値 | アンカー |
|---|---|---|
| `sleep` / `tokio::time::sleep` | **DLsite コード内に 0 箇所**（`dlsite/` 配下に sleep なし） | `crates/core/src/dlsite/client.rs`・`sync.rs`（全文） |
| リトライ | **なし**（1 リクエスト失敗で即 `Err`） | `crates/core/src/dlsite/client.rs:207`, `:166`, `:322` |
| バックオフ | **なし** | 同上 |
| レート制限ヘッダ / 429 対応 | **なし**（`429` は `Http(429)` として失敗するだけ） | `crates/core/src/dlsite/client.rs:211-212` |
| 最大ページ数/ストア | `MAX_PAGES_PER_STORE = 200`（過剰アクセス防止の上限。コメントは「1 ページあたりの行数境界」だが実装は最大ページ数） | `crates/core/src/dlsite/client.rs:16-17`, `:160` |
| メタ一括チャンク | 20 件/リクエスト（`ids.chunks(20)`） | `crates/core/src/dlsite/client.rs:194` |
| 並列度 | **1**（`purchased` はストア × ページを直列、`product_info` はチャンクを直列。`Semaphore` / `buffer_unordered` / `spawn` は不使用） | `crates/core/src/dlsite/client.rs:157-184`, `:194-218` |
| app 側スレッド | 同期 1 本のみ（`std::thread::spawn`） | `crates/app/src/views/bookshelf.rs:2581` |
| UI ポーリング間隔 | 120 ms（`background_executor().timer(Duration::from_millis(120))`） | `crates/app/src/views/bookshelf.rs:2597-2598` |
| connect タイムアウト | 5 秒（`UreqTransport` の production 実装。DLsite 同期・DL は `UreqTransport::new()` を使用） | `crates/core/src/tbf/transport.rs:94-106`, `crates/app/src/views/bookshelf.rs:2584-2585` |
| read タイムアウト | 15 秒（同上。手動リダイレクト用エージェントも 5 秒 / 15 秒） | `crates/core/src/tbf/transport.rs:94-106` |
| リダイレクト | `RequestSpec.redirects` で指定: HTML・JSON・CDN = `3`、302 手動検査 = `0`（ureq の agent は redirects 無効版を別に保持） | `crates/core/src/dlsite/client.rs:205`, `:244`, `:291`, `:320`, `crates/core/src/tbf/transport.rs:100-107` |
| DL 進捗コールバック | 1 % 単位で `on_progress(downloaded, total)`（read バッファ 64 KiB） | `crates/core/src/tbf/transport.rs:181-198` |
| 一覧ページの `sleep` 相当 | なし（連続リクエスト） | `crates/core/src/dlsite/client.rs:157-184` |

---

#### 5. 同期の状態管理・多重起動防止

| 項目 | 事実 | アンカー |
|---|---|---|
| DLsite 用 `sync_state` テーブル | **存在しない**。`drive_sync_state` は Google Drive 専用（`pack_id` / `drive_file_id` / `md5` / `modified_time` / `last_synced_at`） | `crates/core/src/db/sync_state.rs:1-10` |
| 最終同期時刻（グローバル） | **記録しない**。`API` 側の `api.last_sync_at` は技術書典専用（`tbf/sync.rs` が更新、設定画面が表示） | `crates/core/src/db/settings.rs:1-28`, `crates/core/src/tbf/sync.rs:241-262`, `crates/app/src/views/settings.rs:1675` |
| 最終同期時刻（行単位） | `bookshelf_items.synced_at` に `now()`（UTC）を毎回上書き | `crates/core/src/dlsite/sync.rs:83`, `crates/core/src/db/bookshelf.rs:91` |
| サイト行 | `sites` に `('dlsite', 'DLsite', 'https://www.dlsite.com/', display_order 3, is_visible 1)` を `INSERT OR IGNORE`（冪等） | `crates/core/src/db/mod.rs:389-392` |
| DLsite 固有列 | `bookshelf_items` / `books` へ `media_category` / `ai_type` / `is_drm` / `release_date` / `description` / `theme` / `maker_id` / `page_count` / `age_rating` / `series_name` を `ensure_column` で冪等追加 | `crates/core/src/db/mod.rs:361-378` |
| 多重同期の防止（UI） | `sync_busy: usize` カウンタ。`sync_all` は `sync_busy > 0` で早期 return | `crates/app/src/views/bookshelf.rs:782-783`, `:2229-2232` |
| `sync_dlsite` 自体のガード | **`sync_busy` を見ない**（未ログインチェックのみ）。別経路から再度呼ぶと並行実行し得る | `crates/app/src/views/bookshelf.rs:2557-2575` |
| 同期中の他操作 | `sync_busy > 0` の間はサイト切替・ダウンロード開始・favorite/hidden トグル・reload を無視（ダウンロード要求は `pending_download` に退避） | `crates/app/src/views/bookshelf.rs:1114`, `:2720`, `:3293`, `:3324`, `:3376-3378` |
| プロセス多重起動防止 | config の `instance.lock` による単一インスタンス（2 個目はウィンドウを開かず終了） | `docs/features.md:78-84` |
| 実行時ロック（DB レベル） | なし（`busy_timeout` 5 秒のみ） | `crates/app/src/views/bookshelf.rs:2581`, `crates/core/src/db/mod.rs:55` |

---

#### 6. 商品 ID / 作品 ID の規約

| 種別 | 形式・規則 | アンカー |
|---|---|---|
| 作品 ID（= `bookshelf_items.database_id`） | 正規表現 `(?i)product_id\/(RJ\d+)\.html`（大文字小文字無視、`.html` 必須）。**RJ + 数字のみ**。マッチしない行は行ごと破棄 | `crates/core/src/dlsite/client.rs:359` |
| サークル ID | 正規表現 `(?i)maker_id\/(RG\d+)\.html` → `maker_id`（`RG\d+`） | `crates/core/src/dlsite/client.rs:363` |
| ストアフロア ID（`site_id`） | 行内で最初に現れる `https?://www\.dlsite\.com/([a-z0-9]+)/` のキャプチャ（例 `maniax` / `home` / `books` / `ai`）。取得失敗は空文字 | `crates/core/src/dlsite/client.rs:371` |
| 作品ページ URL | `https://www.dlsite.com/{store}/work/=/product_id/{id}.html` | `crates/core/src/dlsite/sync.rs:135`, `:142`（テスト HTML） |
| ダウンロードページ URL（`download_url` に保存） | `https://www.dlsite.com/{store}/download/=/product_id/{id}.html` | `crates/core/src/dlsite/sync.rs:139`, `crates/core/src/dlsite/client.rs:370` |
| サークルページ URL | `https://www.dlsite.com/{store}/circle/profile/=/maker_id/{maker_id}.html` | `crates/core/src/dlsite/sync.rs:136` |
| メタ API URL | `https://www.dlsite.com/maniax/product/info/ajax?product_id={id[,id…]}`（最大 20 件/回） | `crates/core/src/dlsite/client.rs:196-197` |
| 作者ページ URL | `https://www.dlsite.com/maniax/work/=/product_id/{id}.html` | `crates/core/src/dlsite/client.rs:226` |
| CDN 実ファイル URL（302 `location`） | `https://download.dlsite.com/get/=/type/work/domain/doujin/dir/{RJxxxx}/file/{id}.zip/_/{yyyymmdd}?update_date={yyyymmdd}`（テストフィクスチャ実値） | `crates/core/src/dlsite/client.rs:672` |
| サムネイル URL（一覧 HTML） | `<source srcset>` の先頭トークン → `<img data-src>` → `<img src>`（`data:` 始まり除外）。例 `//img.dlsite.jp/resize/images2/work/doujin/RJ{first8}/{content_id}_img_main_240x240.webp` | `crates/core/src/dlsite/client.rs:366-369`, `crates/core/src/dlsite/sync.rs:132` |
| サムネイル URL（`product_info`） | `work_image`（例 `//img.dlsite.jp/...img_main.jpg`）を優先。`//` 始まりは `https:` 前置 | `crates/core/src/dlsite/sync.rs:48-58`, `crates/core/src/dlsite/client.rs:105-107` |
| ブック側の紐づけ | ダウンロード後に `books.tbf_product_id = product_id`（= RJ ID）＋ `books.site_id = "dlsite"` として保存され、`find_by_source(site_id, tbf_product_id)` で重複抑止に使う | `crates/app/src/views/bookshelf.rs:2988`, `crates/core/src/db/books.rs:314-329` |
| 抽出に使う `work_type` コード | `MNG`→Comic / `ICG`→Cg / `SOU`→Voice / `NRE`,`DNV`→Novel / `VCM`,`WBT`→Video / `ACN`,`ADV`,`QIZ`,`RPG`,`STG`,`SLN`,`TBL`,`TYP`,`PZL`,`ETC`→Game / それ以外→`None` | `crates/core/src/dlsite/mod.rs:40-50` |
| ジャンルアイコン | `class="work_genre"...>` 〜 `</dd>` 内の `class="icon_([A-Za-z0-9]+)"` を `icon_` 前置で再構成。`icon_AIG`→AI 生成（full）、`icon_AIP`→AI 一部利用（partial） | `crates/core/src/dlsite/client.rs:373-384`, `crates/core/src/dlsite/mod.rs:67-78` |
| `work_type` の導出 | ジャンルアイコンのうち `media_from_work_type` が解釈できる最初のコード。無ければ空文字 | `crates/core/src/dlsite/client.rs:385-393` |
| 分類の優先順位 | `media` = `work_type` → ジャンルアイコン → `Other`（安全側＝除外）。`ai` = `site_id == "ai"` なら `FullAi`、そうでなければアイコン判定 | `crates/core/src/dlsite/mod.rs:93-113` |
| 年齢区分 | `age_category == Some(1)`→`"all"` / `Some(n) n>1`→`"r18"` / `Some(その他)`→`"all"` / `None` かつ `site_id == "maniax"`→`"r18"` / それ以外→`None` | `crates/core/src/dlsite/mod.rs:81-89` |
| DB 保存値 | `media_category`: `comic`/`cg`/`voice`/`game`/`novel`/`video`/`other`、`ai_type`: `none`/`partial`/`full` | `crates/core/src/dlsite/mod.rs:124-135`, `:138-147` |
| ビューアー対象判定 | `drm_ok && media ∈ {Comic, Cg}`（DLsite 同期は `drm_ok = true` 固定） | `crates/core/src/dlsite/mod.rs:115-122`, `crates/core/src/dlsite/sync.rs:37` |

---

### 7.2 FANZA同人

出典: `crates/core/src/fanza/`（`sync.rs` / `client.rs` / `mod.rs`）, `crates/app/src/views/fanza_login.rs`

情報源:
- `crates/core/src/fanza/sync.rs`（全 220 行精読）
- `crates/core/src/fanza/mod.rs`（全 250 行精読）
- `crates/core/src/fanza/client.rs`（全 756 行精読）
- `crates/app/src/views/fanza_login.rs`（全 277 行精読）
- 関連参照: `crates/core/src/db/mod.rs` / `crates/core/src/db/bookshelf.rs` / `crates/core/src/db/settings.rs` / `crates/core/src/tbf/transport.rs` / `crates/app/src/app_state.rs` / `crates/app/src/views/bookshelf.rs` / `crates/app/src/views/auth.rs` / `crates/app/src/views/settings.rs`
- docs: `docs/import-patterns.md` / `docs/features.md` / `docs/database.md` / `docs/logout.md`
- FANZA のランタイムは `crates/core/src/fanza/` の 3 ファイルのみ（`mod.rs` / `client.rs` / `sync.rs`）。`crates/core/src/fanza/` に sleep / リトライ / タイマーのコードは 1 箇所も存在しない（`grep` で `sleep|Duration|retry|attempt` が 0 件）。

---

#### 1. 同期手順

#### 1.1 エントリポイントとシグネチャ

| 関数 | シグネチャ | アンカー |
|---|---|---|
| コア同期本体 | `pub fn save_purchases(pool: &SqlitePool, client: &mut FanzaClient) -> Result<usize, FanzaError>` | `crates/core/src/fanza/sync.rs:51` |
| 一覧取得 | `pub fn purchased(&mut self) -> Result<Vec<FanzaPurchase>, FanzaError>` | `crates/core/src/fanza/client.rs:187` |
| 詳細取得 | `pub fn detail(&mut self, content_id: &str) -> Result<FanzaDetail, FanzaError>` | `crates/core/src/fanza/client.rs:222` |
| 作品ページ取得 | `pub fn product_page(&mut self, cid: &str) -> Result<FanzaProductPage, FanzaError>` | `crates/core/src/fanza/client.rs:267` |
| ダウンロード | `pub fn download_with_progress(&mut self, download_url: &str, on_progress: &mut dyn FnMut(u64, u64)) -> Result<Vec<u8>, FanzaError>` | `crates/core/src/fanza/client.rs:288` |
| UI 側起動 | `pub fn sync_fanza(&mut self, cx: &mut Context<Self>)` | `crates/app/src/views/bookshelf.rs:2479` |

- 同期は **UI からの手動操作のみ**で起動する（自動同期・ポーリングなし）: `crates/app/src/views/bookshelf.rs:2237`（サイト絞り込み `"fanza"` 選択時）、`:2242`（全サイト同期時）。
- UI はセッションを clone してから `std::thread::spawn` で同期を実行し、`std::sync::mpsc` で結果を返す: `crates/app/src/views/bookshelf.rs:2500-2511`。受け取りは 120 ms 間隔の `try_recv` ポーリング: `crates/app/src/views/bookshelf.rs:2520-2523`。
- ログ: 開始 `log::info!("sync_fanza: 開始")` `crates/app/src/views/bookshelf.rs:2495`、完了 `log::info!("sync_fanza: 完了（{count} 件）")` `crates/app/src/views/bookshelf.rs:2529`、失敗 `log::error!("sync_fanza failed: {message}")` `crates/app/src/views/bookshelf.rs:2538`。

#### 1.2 処理手順（番号付き）

1. `client.purchased()` で購入済み一覧を**全ページ**取得する（`crates/core/src/fanza/sync.rs:52`）。
2. 取得した `Vec<FanzaPurchase>` を先頭から順に処理する（`crates/core/src/fanza/sync.rs:54`）。**並列度 1**。
3. 各作品を `classify(&p.image_src, &p.genre)` で 2 軸分類（media / ai）する（`crates/core/src/fanza/sync.rs:55`、`crates/core/src/fanza/mod.rs:79`）。
4. `is_viewable_included(&meta, true)` が `false` なら **upsert せず次へ**（`crates/core/src/fanza/sync.rs:56-58`）。第 2 引数 `drm_ok` は**常に `true` 固定**（同期時は DRM を見ない。`crates/core/src/fanza/mod.rs:91-93` の定義上、media が `Comic`/`Cg` のときだけ保存される）。
5. `now()` でタイムスタンプ文字列を 1 件ごとに生成: UTC `"%Y-%m-%d %H:%M:%S"`（`crates/core/src/fanza/sync.rs:15-17`、呼び出し `:59`）。
6. `BookshelfItem` を構築（全 35 カラム。→ §1.5 の表）。
7. `bookshelf::upsert(pool, &item)?` を **1 件ごとに** 実行（`crates/core/src/fanza/sync.rs:97`、実装 `crates/core/src/db/bookshelf.rs:61`）。
8. 成功件数を `saved` に加算し、全件処理後に `Ok(saved)` を返す（`crates/core/src/fanza/sync.rs:98-100`）。
9. エラー時は `?` で即座に中断し `FanzaError` を返す（**残り件数は未処理のまま破棄**。ロールバック対象は無い＝トランザクションなし）。

#### 1.3 一覧 API の HTTP 仕様

| 項目 | 実値 | アンカー |
|---|---|---|
| ベース URL 定数 | `pub const LIBRARY_BASE: &str = "https://www.dmm.co.jp/dc/doujin/api/mylibraries/";` | `crates/core/src/fanza/client.rs:15` |
| 1 ページ件数 | `pub const PAGE_LIMIT: usize = 20;`（= 20 件） | `crates/core/src/fanza/client.rs:17` |
| メソッド | `GET` | `crates/core/src/fanza/client.rs:171` |
| URL 組み立て | `{LIBRARY_BASE}?page={page}&sort=purchasedate_desc&genre=all&limit={PAGE_LIMIT}` | `crates/core/src/fanza/client.rs:191-193` |
| クエリ `sort` | `purchasedate_desc`（購入日降順） | `crates/core/src/fanza/client.rs:192` |
| クエリ `genre` | `all`（全ジャンル） | `crates/core/src/fanza/client.rs:192` |
| リクエストヘッダ | `Cookie: {cookie_header()}` / `Accept: application/json` / `User-Agent: {USER_AGENT}` | `crates/core/src/fanza/client.rs:151-157`（生成）、`:173`（適用） |
| User-Agent 実値 | `Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36` | `crates/core/src/fanza/client.rs:21` |
| リダイレクト追跡 | `redirects: 3`（一覧/詳細 API） | `crates/core/src/fanza/client.rs:175` |
| Cookie 送信形式 | 全 Cookie を `name=value` 形式で `"; "` 連結（並び順は `HashMap` の反復順＝**不定**） | `crates/core/src/fanza/client.rs:62-68` |
| CSRF | 送らない（コメント「GET に CSRF 不要」） | `crates/core/src/fanza/client.rs:3-4` |

- User-Agent を固定する理由（コメント）: 「DMM/FANZA は非ブラウザの User-Agent を 403 で弾くため」（`crates/core/src/fanza/client.rs:19-21`）。

#### 1.4 ページング（1 ページ件数 / 総件数 / 終了条件）

| 項目 | 実値 | アンカー |
|---|---|---|
| 初期ページ | `page = 1usize`（1 始まり） | `crates/core/src/fanza/client.rs:189` |
| ページ繰り上げ | `page += 1`（各ループ末尾） | `crates/core/src/fanza/client.rs:215` |
| 終了条件 1 | `!hasNext`（`data.hasNext` が `false` / 欠落 / bool 以外） | `crates/core/src/fanza/client.rs:207-210`, `:212` |
| 終了条件 2 | `out.len() >= total`（`data.total`。`i64` → `usize`。欠落時は `0`） | `crates/core/src/fanza/client.rs:211`, `:212` |
| 終了条件 3（安全弁） | `out.len() >= PAGE_LIMIT * 100` = **2000 件**（50 ページ相当） | `crates/core/src/fanza/client.rs:212` |
| sleep / 待機 | **なし**（ページ間にいかなる待機も入れない） | `crates/core/src/fanza/client.rs:190-216` |
| items の形 | `data.items` は `{ "<購入日文字列>": [作品, …] }` のオブジェクト。キー順に走査 | `crates/core/src/fanza/client.rs:198-206` |
| 購入日の出典 | `items` の**グループ key**（例 `"2026年09月03日"`）を `purchase_date` に入れる | `crates/core/src/fanza/client.rs:200`, `:203`、テスト `crates/core/src/fanza/client.rs:504` |
| `items` がオブジェクト以外 | 無視（空扱い）。エラーにしない | `crates/core/src/fanza/client.rs:199-206` |
| 日付書式 | `YYYY年MM月DD日`（`docs/features.md:206` に明記） | `docs/features.md:205-208` |

#### 1.5 DB 反映（UPSERT 対象テーブル・カラム・単位）

| 項目 | 実値 | アンカー |
|---|---|---|
| 対象テーブル | `bookshelf_items`（1 テーブルのみ） | `crates/core/src/db/bookshelf.rs:63` |
| SQL | `INSERT INTO bookshelf_items (…) VALUES (35 個のプレースホルダ) ON CONFLICT(site_id, database_id) DO UPDATE SET …` | `crates/core/src/db/bookshelf.rs:63-67` |
| 競合キー | `(site_id, database_id)` = `("fanza", contentId)` | `crates/core/src/db/bookshelf.rs:67`、`crates/core/src/fanza/sync.rs:61-62` |
| 実行単位 | 1 件 = 1 ステートメント。**トランザクションなし**（`pool.execute` 直呼び＝オートコミット） | `crates/core/src/db/bookshelf.rs:104-140` |
| DB 接続 | SQLite / WAL / `busy_timeout = 5 s` / `foreign_keys = true` | `crates/core/src/db/mod.rs:52-56` |
| 同期実行のブリッジ | 同期 API。`block_on`（プロセス共有 tokio runtime, worker 2）で sqlx を回す | `crates/core/src/db/mod.rs:31-45` |

書き込む値（`crates/core/src/fanza/sync.rs:60-96`）:

| カラム | 値 | アンカー |
|---|---|---|
| `site_id` | 定数 `SITE_ID_FANZA` = `"fanza"` | `crates/core/src/fanza/sync.rs:13`, `:61` |
| `database_id` | `p.content_id`（一覧の `contentId`） | `crates/core/src/fanza/sync.rs:62` |
| `title` | `p.title` | `crates/core/src/fanza/sync.rs:63` |
| `circle_name` | `p.maker_name`（一覧の `makerName`） | `crates/core/src/fanza/sync.rs:64` |
| `author` | `String::new()`（空文字。後で作品ページ由来の作者が `update_author` で入る） | `crates/core/src/fanza/sync.rs:65` |
| `thumbnail_url` | `Some(widen_thumb(&p.image_src))`（`pl-100x75` → `pl-200x150` に置換） | `crates/core/src/fanza/sync.rs:66`, `:21-23` |
| `format` | `"ZIP"` 固定 | `crates/core/src/fanza/sync.rs:67` |
| `causedAt` | `p.purchase_date`（一覧の日付グループ key。`Option`） | `crates/core/src/fanza/sync.rs:68` |
| `event_name` / `event_slug` / `event_id` | `None` | `crates/core/src/fanza/sync.rs:69-71` |
| `file_name` / `download_url` | `None`（FANZA は一覧で DL URL を持たない） | `crates/core/src/fanza/sync.rs:72-73` |
| `is_downloadable` | `1` | `crates/core/src/fanza/sync.rs:74` |
| `is_checked` | `0` | `crates/core/src/fanza/sync.rs:75` |
| `is_purchased` | `1` | `crates/core/src/fanza/sync.rs:76` |
| `is_new` | `0` | `crates/core/src/fanza/sync.rs:77` |
| `is_active` | `1` | `crates/core/src/fanza/sync.rs:78` |
| `is_favorite` | `0`（INSERT 時のみ。DO UPDATE に含まれないため既存値は保持） | `crates/core/src/fanza/sync.rs:79`、`crates/core/src/db/bookshelf.rs:67-98` |
| `is_hidden` / `hidden_at` | `0` / `None`（DO UPDATE では既存値を保持: `is_hidden = bookshelf_items.is_hidden`） | `crates/core/src/fanza/sync.rs:80-81`、`crates/core/src/db/bookshelf.rs:84-85` |
| `tags_json` | `None`（既存値は `COALESCE(excluded.tags_json, bookshelf_items.tags_json)` で保持） | `crates/core/src/fanza/sync.rs:82`、`crates/core/src/db/bookshelf.rs:86` |
| `synced_at` / `created_at` / `updated_at` | すべて同じ `now()` 文字列（UTC `YYYY-MM-DD HH:MM:SS`）。`created_at` は DO UPDATE に含まれないため初回値が残る | `crates/core/src/fanza/sync.rs:83-85`、`crates/core/src/db/bookshelf.rs:88-89` |
| `media_category` | `media_to_str(meta.media)` = `"comic"` / `"cg"`（保存対象はこの 2 値のみ） | `crates/core/src/fanza/sync.rs:86`, `crates/core/src/fanza/mod.rs:96-105` |
| `ai_type` | `ai_to_str(meta.ai)` = `"none"` / `"partial"` / `"full"` | `crates/core/src/fanza/sync.rs:87`, `crates/core/src/fanza/mod.rs:107-115` |
| `is_drm` | `0` 固定（DRM は同期時に判定しない。ダウンロード時に `detail().is_drm` で判定） | `crates/core/src/fanza/sync.rs:88`、`crates/app/src/views/bookshelf.rs:2804-2808` |
| `release_date` / `description` / `theme` / `maker_id` / `page_count` / `age_rating` / `series_name` | すべて `None`（FANZA では未使用。一覧 API が返さない） | `crates/core/src/fanza/sync.rs:89-95` |

- 列・テーブルは DB 起動時に冪等 DDL で追加される（`bookshelf_items` と `books` の両方に `media_category` / `ai_type` / `is_drm` / `release_date` / `description` / `theme` / `maker_id` / `page_count` / `age_rating` / `series_name`）: `crates/core/src/db/mod.rs:360-381`。
- `sites` 行は冪等 INSERT: `('fanza', 'FANZA同人', 'https://www.dmm.co.jp/dc/doujin/', 2, 1)`（display_order = 2、is_visible = 1）: `crates/core/src/db/mod.rs:382-386`。
- 読み出し順（本棚 / テストで使用）: `WHERE site_id = ?1 ORDER BY causedAt IS NULL, causedAt DESC, title ASC`: `crates/core/src/db/bookshelf.rs:177-186`。

#### 1.6 差分判定

- **差分判定は存在しない**。毎回、一覧 API の全件を取得して全件 UPSERT する（`crates/core/src/fanza/sync.rs:52-100`）。
- 比較に使うのは UPSERT の競合キー `(site_id, database_id)` のみ。**ハッシュ・更新日時比較は無い**。
- リモートから消えた作品のローカル行は**削除しない**（同期コードに `DELETE` も `delete` 呼び出しも無い。`crates/core/src/fanza/sync.rs` 全体）。
- リモート側の変更は「常に上書き」（`title` / `causedAt` / `media_category` 等は DO UPDATE で無条件上書き。`crates/core/src/db/bookshelf.rs:68-98`）。

#### 1.7 エラー時の挙動

| 事象 | 挙動 | アンカー |
|---|---|---|
| HTTP 401 / 403 | `FanzaError::Unauthorized(u16)` を返す（`"不正アクセス（{status}）"`） | `crates/core/src/fanza/client.rs:160-162`, `:26-28` |
| HTTP 200 以外 | `FanzaError::Http(u16)` を返す（`"HTTP {status}"`） | `crates/core/src/fanza/client.rs:163-166`, `:30-32` |
| 本文が JSON でない | `FanzaError::Parse(...)`（`serde_json::Error` から `From` 変換。`"parsing failed: {e}"`） | `crates/core/src/fanza/client.rs:40-45`, `:34`, `:179` |
| JSON `error_code != 0`（欠落含む） | `FanzaError::SessionExpired`（`"セッション切れ・未ログイン"`）。**セッション失効の唯一の検出手段** | `crates/core/src/fanza/client.rs:180-182`, `:24-26` |
| `data` キー欠落 | `FanzaError::Parse("no data")` | `crates/core/src/fanza/client.rs:195-197`（一覧）, `:225-227`（詳細） |
| transport 失敗（接続/タイムアウト等） | `FanzaError::Transport(TbfError)` | `crates/core/src/fanza/client.rs:177`, `:36-38` |
| DB 失敗 | `FanzaError::Database(#[from] sqlx::Error)` | `crates/core/src/fanza/client.rs:36`、`crates/core/src/fanza/sync.rs:97` |
| 1 件目の DB エラー | 即 `Err` を返し、以降の作品は処理しない | `crates/core/src/fanza/sync.rs:97-98` |
| 個別要素の JSON 崩れ | `serde_json::from_value` 失敗時は**全フィールド空の既定値にフォールバック**（エラーにしない） | `crates/core/src/fanza/client.rs:396-405` |
| UI 側の表示 | 成功: `ToastKind::Success` `"FANZA サイトから {count} 件取得しました"`。失敗: `ToastKind::Error` にエラー文字列 | `crates/app/src/views/bookshelf.rs:2530-2537`, `:2539-2541` |
| セッション起因エラーの再ログイン誘導 | メッセージに `"not logged in"` または `"セッション"` を含む場合のみ認証モーダルを開く | `crates/app/src/views/bookshelf.rs:2542-2549` |
| 未ログインで同期開始 | 同期せず `ToastKind::Info` `"FANZA にログインしてから同期してください"` → 認証モーダルを開く | `crates/app/src/views/bookshelf.rs:2480-2493` |

- `FanzaError::DrmProtected`（`"DRM 付きで取り込めない"`）は**宣言のみで生成箇所が存在しない**（デッドバリアント）: `crates/core/src/fanza/client.rs:30`。

#### 1.8 キャンセル / 中断

- **キャンセル機構は無い**。HTTP リクエストにキャンセルトークンやタイムアウト上限（件数以外）は無く、`save_purchases` を途中で止める API も無い（`crates/core/src/fanza/sync.rs` / `crates/core/src/fanza/client.rs` 全体）。
- 中断に相当するのは §1.4 の 3 終了条件（`hasNext` / `total` / 2000 件上限）とエラー時の早期 `Err` のみ。
- UI 側の二重起動防止は `sync_busy` カウンタ（0 より大きいと再実行しない）: `crates/app/src/views/bookshelf.rs:2230-2232`、加算 `:2496`、減算 `:2526`。
- 同期中は本棚の他操作（ダウンロード・タグ編集・サイト切替）が無視される: `crates/app/src/views/bookshelf.rs:1114`, `:1866`, `:2720`, `:3293`, `:3324`。

---

#### 2. ログイン方式

#### 2.1 方式の要約

- **アプリ内 WebView（`gpui-wry` / `lb_wry`）による手動ログイン**。HTTP ログイン実装・パスワード送信コードは存在しない（`crates/app/src/views/fanza_login.rs` が唯一のログイン実装）。
- WebView は **incognito（non-persistent）ストア**で生成（永続ストアだと SSO で自動再ログインされ、ログアウトが効かないように見えるため）: `crates/app/src/views/fanza_login.rs:38-40`。
- デバッグビルドでは devtools 有効: `crates/app/src/views/fanza_login.rs:41-42`。
- 起点 URL: `https://www.dmm.co.jp/dc/-/mylibrary/`: `crates/app/src/views/fanza_login.rs:61`。
- 流れ: 年齢確認（はい）→ `accounts.dmm.co.jp` のパスワードログイン → `www.dmm.co.jp` へ復帰 → Cookie 自動取得（コメント `crates/app/src/views/fanza_login.rs:3-5`）。

#### 2.2 ログイン完了判定の手順（番号付き）

1. `FanzaLoginView::new` が WebView を生成（失敗時は `None` で以降の監視は常に `false`）: `crates/app/src/views/fanza_login.rs:26-36`, `:37-64`。
2. `start_url_check` が 1 秒間隔の監視タスクを開始し、`check_generation`（`u64`）で世代管理する: `crates/app/src/views/fanza_login.rs:67-92`。
3. タスクは `cx.entity().downgrade()` の弱参照で回し、ビューが drop されたら終了（リーク防止）: `crates/app/src/views/fanza_login.rs:70-72`, `:81-85`。回帰テスト `crates/app/src/views/fanza_login.rs:254-268`。
4. 毎 tick、`webview.read(cx).raw().url()` で現在 URL を取得（失敗なら `false` で継続）: `crates/app/src/views/fanza_login.rs:98-104`。
5. 完了条件（**3 つすべて成立**）:
   - `url::Url` パース後の host が **`www.dmm.co.jp` と完全一致**（`is_some_and`）: `crates/app/src/views/fanza_login.rs:105-108`
   - URL に `"age_check"` を含まない: `crates/app/src/views/fanza_login.rs:110`, `:112`
   - URL に `"accounts.dmm.co.jp"` も `"/service/login"` も含まない: `crates/app/src/views/fanza_login.rs:111-112`
6. Cookie 収集: `cookies_for_url` を **`https://www.dmm.co.jp` → `https://accounts.dmm.co.jp` の順**に呼び、全 Cookie の `name`/`value` を `HashMap` へ入れる。同名 Cookie は**先勝ち**（`or_insert`）: `crates/app/src/views/fanza_login.rs:115-128`。
7. 収集結果が空なら `log::warn!("fanza login: セッション Cookie を取得できませんでした")` を出して `false`（監視継続）: `crates/app/src/views/fanza_login.rs:130-133`。
8. `FanzaSession::new(cookie_pairs)` を生成し、`log::info!("fanza login: {} cookies captured", session.cookies_count())`: `crates/app/src/views/fanza_login.rs:134-135`。
9. `crate::app_state::save_fanza_session(cx, &session)` で DB 永続化: `crates/app/src/views/fanza_login.rs:136`（→ §3）。
10. WebView を `hide()` し、`FanzaLoginDone` を emit して `true` を返す（監視ループ終了）: `crates/app/src/views/fanza_login.rs:137-141`。
11. `auth.rs` が `FanzaLoginDone` を受けて `show_fanza_login = false` / `fanza_login = None` / `fanza_subscription = None` にし、`CloseAuth` アクションを dispatch: `crates/app/src/views/auth.rs:171-184`。
12. キャンセル（右上の閉じるボタン）は `close()` → `FanzaLoginCancelled` を emit。`auth.rs` はモーダルを閉じるだけ（セッションは触らない）: `crates/app/src/views/fanza_login.rs:152-157`, `:222-229`, `crates/app/src/views/auth.rs:185-193`。

#### 2.3 セッション Cookie

| 項目 | 実値 | アンカー |
|---|---|---|
| Cookie 名 | **特定名に依存しない**。`www.dmm.co.jp` と `accounts.dmm.co.jp` の全 Cookie を無差別に保存 | `crates/app/src/views/fanza_login.rs:117-128` |
| 収集元オリジン | `https://www.dmm.co.jp`、`https://accounts.dmm.co.jp`（この 2 つのみ） | `crates/app/src/views/fanza_login.rs:117` |
| 型 | `HashMap<String, String>`（Cookie 名 → 値） | `crates/core/src/fanza/client.rs:49-51` |
| テストで使う Cookie 名 | `login_id`（実値の例示ではなくテスト用モック） | `crates/core/src/fanza/client.rs:479-484`, `crates/core/src/fanza/sync.rs:194` |
| `logged_in()` 判定 | `!self.cookies.is_empty()`（**空でないかだけ**。サーバー検証はしない） | `crates/core/src/fanza/client.rs:58-60` |
| Cookie ヘッダ生成 | `k=v` を `"; "` 連結（順序不定） | `crates/core/src/fanza/client.rs:62-68` |
| Cookie 数 | `cookies_count()` = `self.cookies.len()` | `crates/core/src/fanza/client.rs:70-72` |
| サーバー側検証 | 一覧/詳細 API の `error_code != 0` を `SessionExpired` として検出（Cookie 検証 API は無い） | `crates/core/src/fanza/client.rs:180-182` |
| 追加認証（メールログイン / OTP / 2FA）の分岐 | **コード上に存在しない**。すべて WebView 内のユーザー操作に委ねる | `crates/app/src/views/fanza_login.rs` 全体 |
| ログアウト時のサーバー側セッション破棄 | **無い**（`docs/logout.md` は技術書典・BOOTH のみを対象。FANZA はローカルクリアのみ） | `docs/logout.md:1`, `:16`, `crates/app/src/views/settings.rs:720-733` |

#### 2.4 モーダル UI の実値

| 項目 | 実値 | アンカー |
|---|---|---|
| モーダル領域 | 中央 **640.0 × 480.0 px**（論理 px。`window.bounds()` から中央寄せ、`PhysicalPosition/PhysicalSize`＝整数 px にキャスト） | `crates/app/src/views/fanza_login.rs:164-199` |
| backdrop | 全面絶対配置・背景 `hsla(0.0, 0.0, 0.0, 0.45)` | `crates/app/src/views/fanza_login.rs:200-210`（背景色は `:204`） |
| 閉じるボタン | 36.0 × 36.0 px、背景 `rgba(0xffffff26)`、hover `rgba(0xffffff40)`、角丸 `rounded_md` | `crates/app/src/views/fanza_login.rs:211-221`（`:216`, `:217`, `:219`） |
| 閉じるアイコン | `IconName::Close`、サイズ 18.0 px | `crates/app/src/views/fanza_login.rs:232` |
| `show()` | WebView を再表示し `check_generation += 1` して監視を再開 | `crates/app/src/views/fanza_login.rs:144-150` |
| `close()` | `check_generation += 1` してから `webview.take()`（`Option` を `None` に＝Entity drop） | `crates/app/src/views/fanza_login.rs:152-157` |

---

#### 3. 認証情報の保存先と保護

| 項目 | 実値 | アンカー |
|---|---|---|
| 保存先 | **DB（`app_settings` テーブル）**。keyring は**使わない** | `crates/app/src/app_state.rs:414-419` |
| 保存キー | `"fanza.session"`（`app_settings.key` の 1 行） | `crates/app/src/app_state.rs:418` |
| 形式 | `serde_json::to_string(&FanzaSession)` → `{"cookies":{"<name>":"<value>",…}}`（`FanzaSession` は `#[derive(Serialize, Deserialize)]`、フィールド名そのまま） | `crates/core/src/fanza/client.rs:48-51`, `crates/app/src/app_state.rs:417` |
| 暗号化 | **なし（平文）**。`app_settings.value` は TEXT で、暗号化・難読化のコードは存在しない | `crates/core/src/db/settings.rs:17-28`, `crates/core/src/db/schema.sql:25-29` |
| keyring を使わない理由（コメント） | 「セッション Cookie は Windows Credential Manager の上限（2560 UTF-16 文字）を超えることがあるため」 | `crates/app/src/app_state.rs:191-192`, `:412-413` |
| keyring の FANZA 定数 | **存在しない**（`crates/core/src/secrets.rs` には `techbookfest` / `google` / `booth` / `thundoku-shelf.db-key` のみ） | `crates/core/src/secrets.rs:9-14` |
| 書き込み API | `db::settings::set(pool, "fanza.session", &json)` = `INSERT … ON CONFLICT(key) DO UPDATE SET value = excluded.value, updated_at = CURRENT_TIMESTAMP` | `crates/core/src/db/settings.rs:17-28` |
| 保存時のメモリ更新 | `*state.fanza_session.lock() = Some(session.clone())`、`*state.fanza_logged_in.lock() = logged_in` | `crates/app/src/app_state.rs:420-421` |
| 保存呼び出し元 | `crates/app/src/views/fanza_login.rs:136` のみ | `crates/app/src/views/fanza_login.rs:136` |
| 起動時復元 | `db::settings::get(db, "fanza.session")` → `serde_json::from_str` → `.filter(|s: &FanzaSession| s.logged_in())`（空 Cookie なら破棄）。`fanza_logged_in = fanza_session.is_some()` | `crates/app/src/app_state.rs:211-218`, `:243-244` |
| テスト初期化 | `fanza_session = None` / `fanza_logged_in = false`（DB はメモリ） | `crates/app/src/app_state.rs:302-303`, `:265-268` |
| 削除タイミング | 設定画面のログアウト操作時のみ: `SettingsView::logout_fanza` が `*fanza_session.lock() = None` / `*fanza_logged_in.lock() = false` → `cx.background_spawn` で `db::settings::delete(&db, "fanza.session")` | `crates/app/src/views/settings.rs:720-733` |
| ログアウト後のトースト | `"FANZA からログアウトしました"`（サーバー側セッションは残る旨の注記なし＝FANZA はサーバー破棄しない） | `crates/app/src/views/settings.rs:732` |
| 削除の別経路 | `clear_fanza_session(cx)`（`delete` + メモリクリア）が定義されているが**呼び出し元なし** | `crates/app/src/app_state.rs:424-430` |
| メモリ上の保持 | `Arc<Mutex<Option<FanzaSession>>>`（`parking_lot::Mutex`、`.lock()` が `Result` を返さない）。`fanza_logged_in: Arc<Mutex<bool>>` | `crates/app/src/app_state.rs:55-56` |
| WebView 側の Cookie | incognito のため非永続。アプリ再起動・WebView 破棄で消える | `crates/app/src/views/fanza_login.rs:38-40` |
| セッション失効時の扱い | API エラーをトーストで通知し認証モーダルを開くのみ。**DB のセッション行は自動削除しない** | `crates/app/src/views/bookshelf.rs:2542-2549` |
| バックアップ / 初期リセットでの扱い | JSON バックアップが除外するのは環境依存設定（`drive.*` / `api.last_sync_at` 等）のみで、`fanza.session` は**含まれる**。owner_sub 移行時の全削除でも `app_settings` では `drive.*` の 3 キーだけが消され、`fanza.session` は残る | `crates/core/src/db/backup.rs:5-7`, `crates/core/src/db/mod.rs:436-453` |

---

#### 4. レート制限 / 待機 / タイムアウト

| 項目 | 実値 | アンカー |
|---|---|---|
| API 間の sleep | **なし（0 ms）**。`crates/core/src/fanza/` に `sleep` も `Duration` も存在しない | `crates/core/src/fanza/client.rs:187-218` |
| ページ間の sleep | **なし** | `crates/core/src/fanza/client.rs:190-216` |
| リトライ | **なし（0 回）**。バックオフもなし | `crates/core/src/fanza/client.rs:169-186`, `crates/core/src/fanza/sync.rs:51-101` |
| 並列度 | **1**（`purchased` は逐次ループ、`save_purchases` は逐次 for） | `crates/core/src/fanza/client.rs:189-216`, `crates/core/src/fanza/sync.rs:54-99` |
| リクエストタイムアウト | FanzaClient 自身は設定しない。`UreqTransport` の agent 既定 = **connect 5 s / read 15 s** | `crates/core/src/tbf/transport.rs:94-106` |
| リダイレクト上限 | JSON API / 作品ページ = `redirects: 3`（`:175`, `:274`）。CDN ダウンロード = `redirects: 3`（`:356`）。proxy は `redirects: 0`（`:304`、手動 302 追跡） | `crates/core/src/fanza/client.rs:175`, `:274`, `:304`, `:356` |
| User-Agent | 固定 1 種（§1.3） | `crates/core/src/fanza/client.rs:21` |
| 同期 UI ポーリング | 120 ms 間隔 `try_recv` | `crates/app/src/views/bookshelf.rs:2521-2523` |
| ログイン URL 監視 | 1 秒間隔 `timer` | `crates/app/src/views/fanza_login.rs:75-77` |
| ダウンロード進捗 | `on_progress: &mut dyn FnMut(u64, u64)`（downloaded, total）。UI は `downloaded/total` を fraction（`total > 0` のときのみ。0 なら 0.0）にして `DownloadState::Downloading` を送る | `crates/core/src/fanza/client.rs:288-291`, `crates/app/src/views/bookshelf.rs:2817-2828` |
| ダウンロードのヘッダ（proxy） | `Cookie` / `User-Agent` / `Referer: https://www.dmm.co.jp/` | `crates/core/src/fanza/client.rs:294-298` |
| ダウンロードのヘッダ（CDN） | 上記 + 署名 Cookie（`CloudFront-*`）+ `Accept: text/html,application/xhtml+xml,application/xml;q=0.9,image/avif,image/webp,*/*;q=0.8` / `Accept-Language: ja,en;q=0.9` / `Sec-Fetch-Dest: document` / `Sec-Fetch-Mode: navigate` / `Sec-Fetch-Site: cross-site` / `Upgrade-Insecure-Requests: 1` | `crates/core/src/fanza/client.rs:337-349` |
| 署名 Cookie の取得 | proxy の 302 応答 `Set-Cookie` のうち `CloudFront-` 接頭辞のみを、未含有ならセッション Cookie 文字列へ追記 | `crates/core/src/fanza/client.rs:328-335`（コメント `:324-327`） |
| ZIP 判定 | 本文が `<!doctype` または `<html` で始まる場合は `FanzaError::Parse("HTML response (not a file)")` | `crates/core/src/fanza/client.rs:363-364` |
| proxy の status 分岐 | 302 → `Location` 必須（無ければ `Parse("プロキシ応答に location がありません")`） / 401・403 → `Unauthorized` / それ以外 → `Http` | `crates/core/src/fanza/client.rs:310-323` |
| サムネイル取得（表紙） | `cover_url_candidates("fanza", url)` = `[原寸 URL, 保存 URL]`（原寸が失敗すれば保存 URL に戻す） | `crates/app/src/views/bookshelf.rs:6682-6691` |

---

#### 5. 同期の状態管理

| 項目 | 実値 | アンカー |
|---|---|---|
| 専用の同期状態テーブル | **FANZA（および BOOTH / DLsite）用の同期状態テーブルは存在しない**。`drive_sync_state` は Google Drive 専用 | `crates/core/src/db/sync_state.rs:1-4`, `crates/core/src/db/schema.sql:3-9` |
| 最終同期時刻（グローバル） | **保存しない**。`app_settings` の `api.last_sync_at` は技術書典の `refresh_checklist` 専用。`drive.last_sync_at` は Drive 専用 | `crates/core/src/tbf/sync.rs:241-262`, `crates/app/src/views/settings.rs:1645`, `:1675` |
| 行単位の同期時刻 | `bookshelf_items.synced_at` = 同期時刻（UTC `%Y-%m-%d %H:%M:%S`）。`created_at` / `updated_at` も同値で INSERT され、競合時は `synced_at` / `updated_at` のみ上書き | `crates/core/src/fanza/sync.rs:59`, `:83-85`, `crates/core/src/db/bookshelf.rs:88-89` |
| 実行中フラグ | `sync_busy: usize`（ビューのフィールド）。`sync_fanza` で `+= 1`、完了ハンドラで `saturating_sub(1)` | `crates/app/src/views/bookshelf.rs:783`, `:1065`, `:2496`, `:2526` |
| 二重起動防止 | `if self.sync_busy > 0 { return; }`（TBF/BOOTH/FANZA/DLsite 共通のカウンタ） | `crates/app/src/views/bookshelf.rs:2230-2232` |
| ロック（コア層） | **なし**。`save_purchases` はトランザクションもアプリケーションロックも取らない | `crates/core/src/fanza/sync.rs:51-101` |
| ロック（DB 層） | SQLite WAL + `busy_timeout = 5 s` + FK 有効。書き込みは件数分のオートコミット | `crates/core/src/db/mod.rs:52-56`, `crates/core/src/db/bookshelf.rs:104-140` |
| 同期中の UI 制約 | `sync_busy > 0` の間、サイト切替（`:1114`）・保留ダウンロード実行（`:1866`）・お気に入りトグル（`:3293`）・非表示トグル（`:3324`）・ダウンロード開始（`:2720`）・手動 DL 要求（`:3376`）を無視 | `crates/app/src/views/bookshelf.rs:1114`, `:1866`, `:2720`, `:3293`, `:3324`, `:3376` |
| 同期後の UI 反映 | 成功時 `this.reload(cx)` で本棚を再読込 | `crates/app/src/views/bookshelf.rs:2535` |
| タグの更新経路 | ダウンロード時に `bookshelf::update_tags(&db, "fanza", &product_id, &genre_tags)` が `tags_json` を**上書き**（`updated_at = CURRENT_TIMESTAMP`）。同期の upsert は `COALESCE` で既存タグを保持 | `crates/app/src/views/bookshelf.rs:3080-3089`, `crates/core/src/db/bookshelf.rs:192-210`, `crates/core/src/db/bookshelf.rs:86` |
| 作者の更新経路 | 取り込み時に `bookshelf::update_author(db, site_id, product_id, author)`（`author` が取れた場合のみ） | `crates/app/src/views/bookshelf.rs:6905-6930`, `crates/core/src/db/bookshelf.rs:214-228` |
| 手動ジャンル再取得 | `refetch_genre_tags` が 1 リクエストで `product_page` を呼び、未保有タグを編集中タグへ追加（トースト `"ジャンルを再取得しました（{added} 件追加）"`） | `crates/app/src/views/bookshelf.rs:3741-3785` |

---

#### 6. 商品 ID / URL の規約

#### 6.1 ID

| 項目 | 実値 | アンカー |
|---|---|---|
| 商品 ID（`contentId`）の形式 | `d_` + 数字（実例: `d_815503`（`crates/core/src/fanza/client.rs:722`）、`d_100588`（`crates/core/src/fanza/sync.rs:29`）、`d_818290`（`crates/core/src/fanza/client.rs:119`）、`d_305009`（`docs/import-patterns.md:49`）） | `crates/core/src/fanza/sync.rs:27-29`, `crates/core/src/fanza/client.rs:119`, `:722`, `docs/import-patterns.md:49` |
| ID の検証 | **正規表現・長さ・文字種の検証は無い**。`contentId` 文字列をそのまま `database_id` に使う | `crates/core/src/fanza/sync.rs:62`, `crates/core/src/fanza/client.rs:396-414` |
| ID が空のとき | 空文字のまま `database_id` に入る（防御なし）。1 件の JSON 崩れは全フィールド空になる | `crates/core/src/fanza/client.rs:396-405` |
| ローカル ID（`books.id`） | ダウンロード時に別途採番（`book_id` 再利用の仕組みは import 側）。`database_id` = FANZA の `contentId` | `crates/app/src/views/bookshelf.rs:3367-3369` |

#### 6.2 URL 一覧

| 用途 | URL / 規則 | アンカー |
|---|---|---|
| 一覧 API | `https://www.dmm.co.jp/dc/doujin/api/mylibraries/?page={n}&sort=purchasedate_desc&genre=all&limit=20` | `crates/core/src/fanza/client.rs:15`, `:191-193` |
| 詳細 API | `https://www.dmm.co.jp/dc/doujin/api/mylibraries/details/{contentId}/` | `crates/core/src/fanza/client.rs:223` |
| 作品ページ（SSR HTML） | `https://www.dmm.co.jp/dc/doujin/-/detail/=/cid={cid}/` | `crates/core/src/fanza/client.rs:268` |
| 画像セット構造 API | `GET https://www.dmm.co.jp/dc/doujin/api/mylibraries/folder-structures/{productId}/` — **コード未実装**（docs の調査記録のみ。docs は「MVP では使わない」と決定） | `docs/import-patterns.md:14`, `:25` |
| ダウンロード proxy | 詳細の `data.downloadLinks["1"]`。先頭が `/` なら `https://www.dmm.co.jp` を前置、それ以外はそのまま | `crates/core/src/fanza/client.rs:229-241` |
| CDN（実 ZIP） | proxy の `302 Location` をそのまま使用（例: `https://doujin.contents.doujin.dmm.co.jp/...`）。手動追跡 | `crates/core/src/fanza/client.rs:310-323`, `crates/core/src/fanza/client.rs:599-673`（テスト） |
| ログイン起点 | `https://www.dmm.co.jp/dc/-/mylibrary/` | `crates/app/src/views/fanza_login.rs:61` |
| Cookie 収集オリジン | `https://www.dmm.co.jp`, `https://accounts.dmm.co.jp` | `crates/app/src/views/fanza_login.rs:117` |
| `sites` テーブルの URL | `https://www.dmm.co.jp/dc/doujin/`（`display_order = 2`, `is_visible = 1`） | `crates/core/src/db/mod.rs:383-386` |

#### 6.3 サムネイル URL の変換規則（数値付き）

| 変換 | 規則 | アンカー |
|---|---|---|
| 一覧 → 保存 | `replace("pl-100x75", "pl-200x150")`（一覧の `imageSrc` は 100×75 px のため 200×150 px 版に差し替え） | `crates/core/src/fanza/sync.rs:19-23` |
| 保存 → 原寸（表示時） | 拡張子を `rsplit_once('.')` で分離 → ステムを `rsplit_once('-')` で分離 → 後半を `split_once('x')` で幅・高さに分離 → **両方が全部 ASCII 数字なら**サイズ指定を落として `{stem}.{extension}` を返す。いずれかで失敗したら入力そのまま | `crates/core/src/fanza/sync.rs:25-46` |
| 実測値（コメント記載、2026-09-11） | `d_815503pl-200x150.jpg` = 200×150 px (13 KB) → `d_815503pl.jpg` = 560×420 px (71 KB)、`d_100588pl-200x150.jpg` = 107×150 px (5 KB) → `d_100588pl.jpg` = 290×408 px (23 KB) | `crates/core/src/fanza/sync.rs:26-30` |
| DLsite は対象外 | アンダースコア区切り（`_240x240`）は `rsplit_once('-')` に当たらないため変換されない | `crates/core/src/fanza/sync.rs:131-138`（テスト） |
| 表示時の候補順 | `[原寸, 保存 URL]`（原寸で失敗したら保存 URL に戻す） | `crates/app/src/views/bookshelf.rs:6682-6691` |

#### 6.4 分類規約（media / ai）

| 軸 | 判定規則（優先順） | 正規化文字列 | アンカー |
|---|---|---|---|
| media（第 1） | `image_src` を `/` で分割し、`comic` / `cg` / `voice` / `game` / `video` のいずれかのセグメントを**最初に見つかったもの**で判定 | `comic` / `cg` / `voice` / `game` / `video` | `crates/core/src/fanza/mod.rs:37-50`, `:96-105` |
| media（第 2・フォールバック） | `image_src` で判読不能なら `genre` の **`・` より前**の部分を完全一致で照合: `コミック` / `CG` / `ボイス` / `ゲーム` / `動画` | 同上 | `crates/core/src/fanza/mod.rs:52-64` |
| media（既定） | 両方で判読不能なら **`Video`**（安全側＝除外） | `video` | `crates/core/src/fanza/mod.rs:79-86` |
| ai | `genre` の**末尾一致**。`・一部AI` を先に判定（`コミック・一部AI` は `・AI` にも末尾一致するため） | `none` / `partial` / `full` | `crates/core/src/fanza/mod.rs:66-77`, `:107-115` |
| 保存対象 | `drm_ok && (media == Comic \|\| Cg)`。同期では `drm_ok = true` 固定なので **comic / cg のみ保存**（AI バリアント含む） | — | `crates/core/src/fanza/mod.rs:91-93`, `crates/core/src/fanza/sync.rs:55-57` |
| 詳細 API の分類 | `FanzaDetail::meta()` は `classify("", &genre)` を呼ぶ（image_src 空なので genre フォールバック） | — | `crates/core/src/fanza/client.rs:102-107` |

#### 6.5 HTML 抽出の正規表現（作品ページ）

| 用途 | パターン | アンカー |
|---|---|---|
| ジャンルタグ | `class="genreTag__txt"[^>]*>([^<]+)</a>`（`captures_iter`。空文字は除外、前後空白 trim） | `crates/core/src/fanza/client.rs:125-131` |
| 作者 | `(?s)<dt class="informationList__ttl">\s*作者\s*</dt>\s*<dd class="informationList__txt">\s*<a[^>]*>([^<]*)</a>`（`AUTHOR_RE`。trim 後空なら `None`） | `crates/core/src/fanza/client.rs:120`, `:132-136` |
| 作者抽出の検証 | 実機 HTML（2026-09-11 / `d_818290`）で確認済み | `crates/core/src/fanza/client.rs:118-120`, `:691-706`（テスト） |
| 抽出失敗時 | `product_page` 自体は成功していればタグ 0 件 / 作者 `None`。取り込み側は「取得失敗してもダウンロードは続行」 | `crates/app/src/views/bookshelf.rs:2812-2816` |

---

### 7.3 BOOTH

出典: `crates/core/src/booth.rs`, `crates/app/src/views/booth_login.rs`, `crates/app/src/views/bookshelf.rs`（`sync_booth`）

情報源:
`crates/core/src/booth.rs`（全 751 行）、`crates/app/src/views/booth_login.rs`（全 296 行）、
`crates/app/src/views/bookshelf.rs`（sync_booth / ダウンロード経路の該当範囲）、
`crates/app/src/app_state.rs`、`crates/app/src/views/auth.rs`、`crates/app/src/views/settings.rs`、
`crates/core/src/db/{mod.rs,bookshelf.rs,books.rs,settings.rs,sync_state.rs,schema.sql}`、
`crates/core/src/secrets.rs`、`crates/core/src/owner.rs`、`crates/core/migrations/0001_init.sql`、
`docs/{features.md,import-patterns.md,database.md,logout.md,account-switch.md}`、
`Cargo.toml` / `Cargo.lock`（ureq 2.8.0 の既定値確認）。

構造上の前提（重要）:
- BOOTH には他ストアのような core 側の同期エンジンが**無い**。`crates/core/src/` に `booth/` ディレクトリは存在せず（`crates/core/src/dlsite/sync.rs` / `fanza/sync.rs` / `tbf/sync.rs` / `drive/sync.rs` に対応するものが無い）、同期オーケストレーションは**アプリ層**の `BookshelfView::sync_booth` にある `crates/app/src/views/bookshelf.rs:2253`。
- `crates/core/src/booth.rs` は HTTP クライアント（`BoothClient`）と HTML パーサのみを提供する。
- `sync_state` テーブル（`drive_sync_state`）は Google Drive 専用で BOOTH は使わない `crates/core/src/db/sync_state.rs:1-6`。

#### 1. 同期手順

#### 1.1 エントリポイントとシグネチャ

| 関数 | シグネチャ | 位置 |
|---|---|---|
| 同期ボタンの入口（ディスパッチ） | `pub fn sync_all(&mut self, cx: &mut Context<Self>)` | `crates/app/src/views/bookshelf.rs:2229` |
| BOOTH 同期本体 | `pub fn sync_booth(&mut self, cx: &mut Context<Self>)` | `crates/app/src/views/bookshelf.rs:2253` |
| HTTP クライアント生成 | `pub fn new(session: &BoothSession) -> Self` | `crates/core/src/booth.rs:88` |
| ライブラリ全ページ取得 | `pub fn library(&self) -> Result<Vec<BoothLibraryItem>, BoothError>` | `crates/core/src/booth.rs:200` |
| 購入履歴全ページ取得 | `pub fn orders(&self) -> Result<Vec<BoothOrder>, BoothError>` | `crates/core/src/booth.rs:266` |
| 商品詳細（表紙） | `pub fn item_detail(&self, item_id: u64) -> Result<BoothItemDetail, BoothError>` | `crates/core/src/booth.rs:360` |
| 作者名取得 | `pub fn item_author(&self, item_id: u64) -> Result<Option<String>, BoothError>` | `crates/core/src/booth.rs:399` |
| ファイル DL | `pub fn download(&self, download_url: &str) -> Result<Vec<u8>, BoothError>` / `pub fn download_with_progress(&self, download_url: &str, on_progress: &mut dyn FnMut(u64, u64)) -> Result<Vec<u8>, BoothError>` | `crates/core/src/booth.rs:303` / `crates/core/src/booth.rs:309` |
| ログアウト（サーバー側破棄） | `pub fn logout(&self) -> Result<(), BoothError>` | `crates/core/src/booth.rs:107` |

ディスパッチ規則: サイドバーのサイト選択が `Some("booth")` のときだけ BOOTH を同期 `crates/app/src/views/bookshelf.rs:2236`。選択なし（`_`）は techbookfest → booth → fanza → dlsite の順に**全て**起動 `crates/app/src/views/bookshelf.rs:2239-2244`。

#### 1.2 処理手順（番号付き）

`sync_booth` の実行順（アンカーは `crates/app/src/views/bookshelf.rs`）:

1. ログイン判定: `*AppState::global(cx).booth_logged_in.lock()` が false なら Info トースト「BOOTH にログインしてから同期してください」を出し、`OpenAuthProvider{provider: Booth}` を defer で発行して **return**（同期は走らない） — `:2254-2268`。
2. `self.sync_busy += 1` — `:2270`。
3. Info トースト「BOOTH サイトのデータを取得中です」 — `:2271`。
4. `state.booth_session.lock().clone()`（セッションのスナップショット）と `state.db_pool.clone()` を取得 — `:2274-2275`。
5. `std::sync::mpsc::channel::<Result<usize, String>>()` を作成 — `:2276`。
6. `std::thread::spawn` で**専用スレッド**を起動（GPUI ワーカーをブロックしないため） — `:2277-2278`。スレッド内処理は 7〜16。
7. セッションが `None` なら `Err("BOOTH セッションがありません")` — `:2280`。
8. `BoothClient::new(&session)` — `:2281`。
9. `client.library()` でライブラリ全ページ取得（失敗は `?` で即中断、`BoothError` を `to_string()` して `Err` 化） — `:2283`。
10. `client.orders()` で購入履歴全ページ取得（同上） — `:2284`。
11. 購入日マップ構築: `HashMap<title, ordered_at>`、同一タイトルは**先勝ち**（`.or_insert_with`） — `:2286-2291`。
12. 表紙 URL 収集: `AtomicUsize next` でインデックスを配り、`std::thread::scope` で**4 スレッド**が `client.item_detail(item.item_id)` を実行 — `:2297-2329`。
    - 成功時: `detail.images.first()`（先頭 1 枚のみ）を `Mutex<HashMap<u64, String>> covers` に挿入 — `:2313-2318`。
    - `BoothError::NotFound` のときだけ `Mutex<Vec<u64>> vanished` に item_id を追加（商品ページ消滅＝削除対象） — `:2321-2322`。
    - その他のエラーは**無視**（`Err(_) => {}`） — `:2324`。
13. `bookshelf_items` へ 1 件ずつ upsert（`site_id = "booth"`）。`item.download_url.is_none()` の商品はスキップ（物理本のみ等を本棚に出さない） — `:2335-2338`、`:2353-2381`。
14. ライブラリに DL URL が無くなった行を一括 DELETE — `:2382-2389`。
15. ライブラリに存在しない行（購入キャンセル・返品等）を一括 DELETE。**ダウンロード済み（`books` に紐づく）は残す** — `:2391-2417`。
16. `vanished`（商品ページ 404）の行を一括 DELETE。ダウンロード済みは残す — `:2419-2430`。
17. 結果送信: `Ok::<_, String>(library.len())`（**ライブラリ取得件数**。upsert 件数ではない） — `:2431-2433`。
18. UI 側: `rx.try_recv()` を 120 ms 間隔でポーリングし、待つ間 `cx.notify()` を叩き続ける — `:2437-2446`。
19. `sync_busy` を `saturating_sub(1)` — `:2449`。
20. 成功: `log::info!("sync_booth: 完了（{count} 件）")` + Success トースト「BOOTH サイトから {count} 件取得しました」+ `this.reload(cx)` — `:2451-2458`。失敗: `log::error!` + Error トースト（メッセージ文字列をそのまま表示） — `:2461-2462`。`message.contains("not logged in")` のときは `OpenAuthProvider{Booth}` を defer 発行 — `:2463-2469`。

#### 1.3 HTTP リクエスト一覧

共通ヘッダ（`BoothClient::get`）: `User-Agent`（固定、下記定数）、`Accept`（呼び出し側指定）、`Accept-Language: ja,en-US;q=0.9,en;q=0.8`、`Cookies` は `cookie_header` が空でなければ `Cookie: name=value; ...` を付与 — `crates/core/src/booth.rs:171-179`。

| # | 用途 | メソッド + URL | Accept / 追加ヘッダ | 実装 |
|---|---|---|---|---|
| 1 | ライブラリ 1 ページ | `GET https://accounts.booth.pm/library?page={page}`（page は 1 始まり） | `text/html` | `crates/core/src/booth.rs:207-210` |
| 2 | 購入履歴 1 ページ | `GET https://accounts.booth.pm/orders?page={page}`（同上） | `text/html` | `crates/core/src/booth.rs:270-273` |
| 3 | 商品詳細（表紙 JSON） | `GET https://booth.pm/ja/items/{item_id}` | `application/json` | `crates/core/src/booth.rs:361-364` |
| 4 | 商品ページ（作者名） | `GET https://booth.pm/ja/items/{item_id}` | `text/html` | `crates/core/src/booth.rs:400` |
| 5 | ファイル本体 | `GET {download_url}`（`https://booth.pm/downloadables/{id}`） | `application/octet-stream, */*` | `crates/core/src/booth.rs:316-318` |
| 6 | CSRF トークン取得（ログアウト時） | `GET https://booth.pm/ja` | `text/html; charset=utf-8` | `crates/core/src/booth.rs:108` |
| 7 | pixiv セッション破棄 | `POST https://accounts.booth.pm/users/sign_out`、body `_method=delete`（form） | `Accept: */*`, `X-CSRF-Token`, `Referer`/`Origin: https://accounts.booth.pm`、`Cookie` | `crates/core/src/booth.rs:117-127` |
| 8 | booth.pm セッション破棄 | `POST https://booth.pm/users/sign_out`、body `_method=delete`（form） | `Accept: */*`, `X-CSRF-Token`, `Referer: https://booth.pm/ja`, `Origin: https://booth.pm`、`Cookie` | `crates/core/src/booth.rs:143-154` |

- 固定 User-Agent（BOOTH 用）: `Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/126.0.0.0 Safari/537.36` — `crates/core/src/booth.rs:85`。
- リダイレクトは明示設定なし（ureq 2.8.0 の既定 = 最大 5 回、`ureq-2.8.0/src/agent.rs:262`）。`GET https://booth.pm/downloadables/{id}` は 302 → 署名付き一時 S3 URL（**180 秒有効**）へ自動追従してファイル本体が返る — `crates/core/src/booth.rs:300-302`。
- 同期（1〜4）はすべて Cookie 付き GET のみ。POST はログアウト時のみ。
- `crates/core/src/booth.rs` に**テスト専用**モックは無い（`#[cfg(test)]` はパーサ単体テストのみ、`:627-751`）。

#### 1.4 ページング

`library()` — `crates/core/src/booth.rs:200-263`:

| 項目 | 値 / 条件 | アンカー |
|---|---|---|
| 開始ページ | `page = 1` | `crates/core/src/booth.rs:202` |
| 最大ページ検出 | 受信 HTML 中の `/library?page=(\d+)` を全件走査し `max_page = max(現在値, 見つかった最大)` | `crates/core/src/booth.rs:204-218` |
| 1 ページ件数 | **10 件/ページ**（コメント記載。コード上の定数ではない） | `crates/core/src/booth.rs:199` |
| 終了条件 1 | `parse_library` の結果が 0 件 → break | `crates/core/src/booth.rs:250-252` |
| 終了条件 2 | `page >= max_page` → break（最終ページ到達） | `crates/core/src/booth.rs:254-256` |
| 安全弁 | `page > 100` で break（上限 100 ページ） | `crates/core/src/booth.rs:258-260` |
| sleep | **なし**（ページ間に待機を入れない） | 該当コードなし（`sleep`/`retry`/`backoff` の grep 結果 0 件） |
| ページ間の継続判定 | `max_page` を毎ページ再評価して更新 | `crates/core/src/booth.rs:212-218` |

`orders()` — `crates/core/src/booth.rs:266-295`:

| 項目 | 値 / 条件 | アンカー |
|---|---|---|
| 開始ページ | `page = 1` | `crates/core/src/booth.rs:268` |
| 終了条件 1 | パース結果 0 件 → break | `crates/core/src/booth.rs:281-284` |
| 終了条件 2 | パース件数が **12 件未満** → break（1 ページ 12 件と推定した閾値。定数名なし） | `crates/core/src/booth.rs:287-289` |
| 安全弁 | `page > 100` で break | `crates/core/src/booth.rs:291-293` |
| sleep | なし | 該当コードなし |

#### 1.5 差分判定と DB 反映

- 差分判定は「**全件 upsert → 集合差で DELETE**」方式。前回同期のスナップショット比較やハッシュ比較はしない。
- 反映テーブルは `bookshelf_items` のみ（同期では `books` を作らない）。
- upsert は 1 商品 1 文（`INSERT ... ON CONFLICT(site_id, database_id) DO UPDATE`）。**同期全体を囲むトランザクションは無い**（`bookshelf::upsert` は `pool.execute` を 1 回、`crates/core/src/db/bookshelf.rs:61-145`）。DELETE 3 種も明示トランザクションなしの単文実行 — `crates/app/src/views/bookshelf.rs:2383-2389` / `:2393-2417` / `:2420-2430`。
- 書き込む列（BOOTH 同期が渡す値） — `crates/app/src/views/bookshelf.rs:2339-2381`:

| 列 | 値 |
|---|---|
| `site_id` | 固定 `"booth"` `:2341` |
| `database_id` | `item.item_id.to_string()`（10 進文字列） `:2342` |
| `title` | `item.title`（HTML のタイトル div をタグ除去した文字列） `:2343` |
| `circle_name` | `item.shop_name` `:2344` |
| `author` | `item.shop_name`（コメント: BOOTH の shop は作成者を兼ねる） `:2346` |
| `thumbnail_url` | `covers.get(&item.item_id)`（item_detail の `images[0].original`） `:2347` |
| `format` | 固定 `"PDF"` `:2349` |
| `causedAt` | `bought_at.get(&item.title)`（購入履歴のタイトル完全一致。無ければ `NULL`） `:2350` |
| `event_name` / `event_slug` / `event_id` | `None` `:2351-2353` |
| `file_name` | `item.file_name`（ライブラリ HTML のファイル名行） `:2354` |
| `download_url` | `item.download_url` `:2355` |
| `is_downloadable` / `is_purchased` | `1` / `1` `:2356`,`:2358` |
| `is_checked` / `is_new` / `is_favorite` / `is_hidden` | `0` / `0` / `0` / `0` `:2357`,`:2359`,`:2360`,`:2361` |
| `hidden_at` | `None` `:2362` |
| `tags_json` | `None` `:2364` |
| `synced_at` / `created_at` / `updated_at` | **固定文字列 `"2026-08-25 00:00:00"`**（現在時刻ではない） `:2365-2367` |
| `media_category` / `ai_type` / `is_drm` / `release_date` / `description` / `theme` / `maker_id` / `page_count` / `age_rating` / `series_name` | `None` / `None` / `0` / `None` ×7 `:2368-2381` |

- upsert の競合更新（`crates/core/src/db/bookshelf.rs:67-91`）で**上書きされない列**（ユーザーローカル状態・作品ページ由来値の保護）:
  - `is_favorite` は DO UPDATE 句に含まれない（＝既存値保持） `crates/core/src/db/bookshelf.rs:67-91`（`is_favorite` の代入が存在しない）。
  - `is_hidden` / `hidden_at` は既存行の値で固定 `crates/core/src/db/bookshelf.rs:84-85`。
  - `tags_json` は `COALESCE(excluded.tags_json, 既存値)` — 同期は `None` を渡すので既存タグを保持 `crates/core/src/db/bookshelf.rs:86`。
  - `author` は `CASE WHEN excluded.author = '' THEN 既存 author ELSE excluded.author END` `crates/core/src/db/bookshelf.rs:87-90`。※同一文の 70 行目にも `author = excluded.author` があり、87-90 の CASE が後に来る（SQLite は同一 UPDATE 内の重複代入で後勝ち） `crates/core/src/db/bookshelf.rs:70`,`:87-90`。
- 主キー: `(site_id, database_id)`（`bookshelf_items` の複合 PK） — `docs/database.md:83`。
- 削除 SQL（すべて `site_id = 'booth'` 限定、`books` 側の保護条件付き）:
  1. `DELETE FROM bookshelf_items WHERE site_id='booth' AND (download_url IS NULL OR download_url='')` — `crates/app/src/views/bookshelf.rs:2385`。
  2. ライブラリ集合に無い行の削除。保護条件は `database_id NOT IN (SELECT tbf_product_id FROM books WHERE site_id='booth')`。`ids` が空のときはこの保護条件のみで全削除する分岐がある — `crates/app/src/views/bookshelf.rs:2396-2416`。
  3. `vanished`（404 商品）の削除。同じ保護条件付き — `crates/app/src/views/bookshelf.rs:2422-2429`。
  - 2 と 3 の戻り値は `let _ =` で**握り潰し**（失敗しても同期成功扱い） — `crates/app/src/views/bookshelf.rs:2393`,`:2429`。

#### 1.6 エラー時の挙動

`BoothError` は 4 変種 `crates/core/src/booth.rs:35-44`: `Network(String)` / `NotLoggedIn` / `NotFound` / `InvalidResponse(String)`。

| 状況 | 判定 | 挙動 |
|---|---|---|
| HTTP 401 / 403（セッション切れ・Cloudflare `cf_clearance` 失効） | `ureq::Error::Status(401|403)` | `BoothError::NotLoggedIn` `crates/core/src/booth.rs:183` |
| HTTP 404（商品ページ消滅） | `ureq::Error::Status(404)` | `BoothError::NotFound` `crates/core/src/booth.rs:185` |
| その他の通信失敗 | 上記以外 | `BoothError::Network(文字列)` `crates/core/src/booth.rs:186` |
| 302 かつ URL に `sign_in` を含む | 応答ステータス検査 | `BoothError::NotLoggedIn` `crates/core/src/booth.rs:188-190` |
| ライブラリ応答がログインページ | `<title>ログイン - BOOTH</title>` を含む / `ログイン</h1>` を含む / **HTML 長 < 20,000 バイト** | `log::warn!` して `BoothError::NotLoggedIn`（0 件成功として扱わない） `crates/core/src/booth.rs:227-237` |
| JSON パース失敗（item_detail） | `serde_json::from_str` 失敗 | `BoothError::InvalidResponse(文字列)` `crates/core/src/booth.rs:365-366` |
| ダウンロードで HTML が返る | `content-type` に `text/html` を含む | `BoothError::Network("HTML が返りました（リンクが無効の可能性）: {url} (content-type=...)")` として失敗させる `crates/core/src/booth.rs:325-335` |
| ログアウト（booth.pm 側）401/403 | `ureq::Error::Status(401|403)` | `BoothError::NotLoggedIn` `crates/core/src/booth.rs:156-158` |
| CSRF トークンが取れない | `extract_csrf_token` が `None` | `BoothError::Network("csrf token not found in page")` `crates/core/src/booth.rs:109-110` |
| ログアウト（plaza 側）失敗 | 任意のエラー | **無視して続行**（`log::info!("booth logout(plaza): failed ({e})")`） `crates/core/src/booth.rs:135-137` |
| HTML ダンプ書き込み失敗 | `std::fs::write` のエラー | `log::warn!` のみで続行 `crates/core/src/booth.rs:240-242`,`:276-278` |
| 同期全体の失敗 | 上記いずれかが `?` で伝播 | スレッドから `Err(String)` を返し、UI で `log::error!` + Error トースト。`"not logged in"` を含む場合のみ再ログインダイアログを自動表示 `crates/app/src/views/bookshelf.rs:2461-2469` |

- **リトライは一切無い**（429/5xx 含む。`retry` / `backoff` / `attempt` の grep 結果 0 件）。
- 購入履歴のパース 0 件はエラーではなく「履歴なし」として正常終了（購入日が `NULL` になるだけ） `crates/core/src/booth.rs:281-284`。
- 表紙取得の非 404 エラーは黙殺（`thumbnail_url` が `None` のまま upsert される） `crates/app/src/views/bookshelf.rs:2324`。

#### 1.7 キャンセル / 中断

- **ユーザー操作によるキャンセル機構は無い**。`sync_booth` は開始後に中断できない（`std::thread::spawn` の完了を待つのみ。`JoinHandle` も保持しない） `crates/app/src/views/bookshelf.rs:2278-2433`。
- 二重起動防止は `sync_all` 冒頭の `if self.sync_busy > 0 { return; }` のみ `crates/app/src/views/bookshelf.rs:2230-2232`（`sync_booth` 自身には入口ガードが無いので直接呼べば多重起動できる）。
- 同期中は他操作をブロック: サイト切替 `:1114`、ダウンロード開始 `:2720-2722`、お気に入りトグル `:3293-3295`、非表示トグル `:3324-3326`、保留ダウンロードの実行は `sync_busy == 0` まで遅延 `:1866-1869`。
- ログイン WebView 側はキャンセル可能: 閉じるボタン → `close()` が `check_generation` を進めて監視タスクを停止し、`webview.take()` で WebView を破棄、`BoothLoginCancelled` を emit `crates/app/src/views/booth_login.rs:167-172`,`:245-246`。

#### 2. ログイン方式

#### 2.1 フロー（番号付き）

実体は `crates/app/src/views/booth_login.rs` の `BoothLoginView`。埋め込み WebView（wry）方式で、HTTP によるログインは行わない。

1. `AuthDialog` が `AuthProvider::Booth` を受けると `show_booth_login = true` `crates/app/src/views/auth.rs:102`。
2. render 後の `cx.defer_in` で `BoothLoginView::new(window, cx)` を生成（ウィンドウ借用中の RefCell 再入で固まるのを避けるため） `crates/app/src/views/auth.rs:142-145`。
3. `BoothLoginView::new` は `try_create_webview` → 失敗しても `None` で続行（backdrop のみ描画） → `start_url_check(cx)` 開始 `crates/app/src/views/booth_login.rs:28-39`。
4. WebView 生成: `lb_wry::WebViewBuilder::new().with_incognito(true)`（Cookie はメモリのみ、永続ストアにしない）、`#[cfg(debug_assertions)]` では `with_devtools(true)`、`window.window_handle()` を要求 `crates/app/src/views/booth_login.rs:46-62`。
5. 初期 URL: `https://booth.pm/users/sign_in` を `load_url`、その後 `hide()`（モーダルを開くまで非表示） `crates/app/src/views/booth_login.rs:64-68`。ユーザーは WebView 内で pixiv OAuth ログインを完了する。
6. `show()` で WebView を `show()` し、`check_generation` を進めて URL 監視を再開 `crates/app/src/views/booth_login.rs:158-164`。
7. URL 監視タスク（`start_url_check`）: `cx.entity().downgrade()` の弱参照で 1 秒ごとに `check_login` を呼ぶ。ビューが drop されると `handle.update` が `Err` を返しループ終了（リーク防止） `crates/app/src/views/booth_login.rs:74-99`。
8. `check_login` が true を返したらループ終了 → Cookie は保存済み・`BoothLoginDone` emit 済み `crates/app/src/views/booth_login.rs:91-95`,`:153`。
9. `BoothLoginDone` 購読でモーダルを閉じ `CloseAuth` を発行、`booth_login = None` にして次回は新しい WebView + 監視を作る `crates/app/src/views/auth.rs:146-155`。
10. `BoothLoginCancelled` 購読では `show_booth_login = false` / `booth_login = None`（WebView 破棄） `crates/app/src/views/auth.rs:157-162`。

#### 2.2 定数 / タイマー

| 項目 | 値 | アンカー |
|---|---|---|
| ログイン開始 URL | `https://booth.pm/users/sign_in` | `crates/app/src/views/booth_login.rs:65` |
| WebView ストア | incognito（non-persistent、Cookie はメモリのみ） | `crates/app/src/views/booth_login.rs:46` |
| devtools | debug ビルドのみ有効 | `crates/app/src/views/booth_login.rs:47-48` |
| URL 監視間隔 | 1 秒（`Duration::from_secs(1)`） | `crates/app/src/views/booth_login.rs:82-84` |
| 監視の世代管理 | `check_generation: u64`、不一致なら監視終了 | `crates/app/src/views/booth_login.rs:24`,`:75-76`,`:86-88` |
| ログイン完了判定ホスト | ホスト名が `booth.pm` で終わる（`endswith`） | `crates/app/src/views/booth_login.rs:113-116` |
| ログインページ除外 | URL に `/users/sign_in` を含む間は待機 | `crates/app/src/views/booth_login.rs:119-122` |
| Cookie 取得元 | `https://booth.pm` と `https://accounts.booth.pm` の 2 URL（順序固定） | `crates/app/src/views/booth_login.rs:127` |
| Cookie マージ規則 | 同名は**先勝ち**（`.or_insert(value)`）＝ booth.pm 側が優先 | `crates/app/src/views/booth_login.rs:137` |
| モーダル矩形 | 480 px × 640 px、ウィンドウ中央 | `crates/app/src/views/booth_login.rs:183-186` |
| backdrop | 全画面 + `hsla(0, 0, 0, 0.45)` | `crates/app/src/views/booth_login.rs:216-222` |
| 閉じるボタン | 36 px × 36 px、右上 `top_3`/`right_3`、背景 `rgba(0xffffff26)`、hover `rgba(0xffffff40)`、アイコン 18 px | `crates/app/src/views/booth_login.rs:226-251` |

#### 2.3 成否判定と Cookie 検証

- 成否判定は **URL ベースのみ**: 「ホストが `booth.pm` で終わる」かつ「URL に `/users/sign_in` を含まない」 → セッション確立とみなす `crates/app/src/views/booth_login.rs:113-122`。
- 追加条件: 収集した Cookie が 0 件なら `log::warn!` して false（未完了扱い、WebView は開いたまま） `crates/app/src/views/booth_login.rs:140-143`。
- **サーバーへの検証リクエストは行わない**（`accounts.booth.pm/library` を叩いて 200 を確認する等はしない）。実際の検証は次の同期時の `NotLoggedIn` 判定で行われる `crates/core/src/booth.rs:183`,`:227-237`。
- Cookie は `cookies_for_url(url)` の戻り（`raw()` 経由）から `name()` / `value()` を取り出して `HashMap<String,String>` に詰める `crates/app/src/views/booth_login.rs:128-138`。
- 取得する Cookie 名は**ハードコードされていない**（ドメインに紐づく全 Cookie を無条件で保存）。
- コード/ドキュメント上で名前が出てくる Cookie:
  - `_plaza_session_*`（`accounts.booth.pm` の pixiv セッション。ログアウトに必要） — `crates/app/src/views/booth_login.rs:124`、`docs/logout.md:39`、`crates/core/src/booth.rs:102-103`。
  - `_booth_session`（booth.pm の Rails セッション。テスト内の例） — `crates/core/src/booth.rs:649`、`crates/core/src/booth.rs:634-636`。
  - `locale`（テスト内の例） — `crates/core/src/booth.rs:635`。
- incognito を使う理由（コメント）: 永続ストアだと pixiv/booth のセッションがアプリ再起動をまたいで残り、ログアウト後の再ログイン WebView で pixiv SSO により自動再ログインされ「ログアウトが効かない」ように見えるため `crates/app/src/views/booth_login.rs:42-45`、`docs/logout.md:35-39`。

#### 3. 認証情報の保存先と保護

| 項目 | 事実 | アンカー |
|---|---|---|
| 保存先 | SQLite の `app_settings` テーブル、キー `"booth.session"` | `crates/app/src/app_state.rs:392`、`crates/core/src/db/schema.sql:25-30` |
| 形式 | `BoothSession` の `serde_json` 文字列（`{"cookies":{"name":"value",...}}`）をそのまま `value` 列へ | `crates/app/src/views/booth_login.rs:144-148`、`crates/app/src/app_state.rs:390-392` |
| 暗号化 | **なし（平文 JSON）**。`db::settings::set` は素の `INSERT ... ON CONFLICT(key) DO UPDATE` | `crates/core/src/db/settings.rs:17-28` |
| keyring を使わない理由（コメント） | セッション Cookie は Windows Credential Manager の上限（**2560 UTF-16 文字**）を超えることがあるため DB 保存 | `crates/app/src/app_state.rs:191-192`、`:386-387` |
| 実行時キャッシュ | `AppState.booth_session: Arc<parking_lot::Mutex<Option<BoothSession>>>` と `booth_logged_in: Arc<Mutex<bool>>` | `crates/app/src/app_state.rs:52-53` |
| 保存関数 | `pub fn save_booth_session(cx: &App, session: &BoothSession)`（JSON 化失敗時は DB 保存をスキップし、メモリ状態は更新する） | `crates/app/src/app_state.rs:388-402` |
| 起動時復元 | `db::settings::get(&db_pool, "booth.session")` → JSON parse → `logged_in()` フィルタ → `booth_logged_in` を決定。パース失敗は `None`（未ログイン扱い） | `crates/app/src/app_state.rs:193-198` |
| 削除（ログアウト） | `SettingsView::logout_booth`: サーバー側 `logout()` 実行 → メモリクリア → `db::settings::delete(db, "booth.session")` を `cx.background_spawn` で非同期実行（結果は `let _ =` で無視） | `crates/app/src/views/settings.rs:678-704` |
| 削除（汎用ヘルパー） | `pub fn clear_booth_session(cx: &App)` も存在するが、**呼び出し箇所が無い**（`crates` 全体の grep で定義 1 件のみ） | `crates/app/src/app_state.rs:405-410` |
| 保存タイミング | ログイン成立時（`check_login` 内）のみ。同期では保存しない | `crates/app/src/views/booth_login.rs:148` |
| `secrets.rs` の `USER_BOOTH` | `pub const USER_BOOTH: &str = "booth";`（keyring 用キーとして定義）— **リポジトリ内で参照箇所ゼロ**（未使用） | `crates/core/src/secrets.rs:12` |
| keyring の共通定数 | `SERVICE = "com.megablacklabel.thundoku-shelf"`、`USER_DB_KEY = "thundoku-shelf.db-key"` | `crates/core/src/secrets.rs:5`,`:14` |
| `owner_sub`（別物） | BOOTH の ID からは作られない。ダウンロード時に Google ログイン中の `sub` を AES-256-GCM で暗号化して `books.owner_sub` に入れる（未ログイン時は `NULL`） | `crates/app/src/views/bookshelf.rs:2742-2745`,`:3007-3013`,`:3126-3132`、`crates/core/src/db/books.rs:250-265`、`crates/core/src/owner.rs:14-27` |
| `owner_sub` 列 | `books.owner_sub TEXT`（NULL 可）。実行時 DDL で冪等に追加（`pragma_table_info` 確認後 `ALTER TABLE books ADD COLUMN owner_sub TEXT`） | `crates/core/src/db/schema.sql:50`、`crates/core/src/db/mod.rs:236-244` |

#### 4. レート制限・待機・タイムアウト

| 項目 | 実値 | アンカー |
|---|---|---|
| ページ間 sleep | **なし**（ライブラリも購入履歴も） | `crates/core/src/booth.rs:206-260`,`:269-293` |
| リトライ回数 | **0**（リトライ実装なし） | `crates/core/src/booth.rs` 全体（`retry`/`backoff`/`attempt` の grep 0 件） |
| バックオフ | なし | 同上 |
| 接続タイムアウト | 5 秒（`timeout_connect`） | `crates/core/src/booth.rs:92` |
| 読み取りタイムアウト | 15 秒（`timeout_read`） | `crates/core/src/booth.rs:93` |
| リダイレクト上限 | 明示設定なし → ureq 2.8.0 既定 5 回 | `crates/core/src/booth.rs:91-94`、`ureq-2.8.0/src/agent.rs:262` |
| 表紙取得の並列度 | **4 スレッド固定**（`std::thread::scope` + `for _ in 0..4`、コメント: BOOTH API への負荷を抑えるため固定） | `crates/app/src/views/bookshelf.rs:2296-2302` |
| 表紙ダウンロード（本棚の表紙取得）の並列度 | 4 スレッド（別エージェント、接続 5 秒 / 読み取り 15 秒） | `crates/app/src/views/bookshelf.rs:1697-1700`,`:1721` |
| 設定画面の表紙再取得の並列度 | 4 スレッド（接続 5 秒 / 読み取り 15 秒） | `crates/app/src/views/settings.rs:311-319` |
| 同期結果ポーリング間隔 | 120 ms | `crates/app/src/views/bookshelf.rs:2443` |
| ログイン URL 監視間隔 | 1 秒 | `crates/app/src/views/booth_login.rs:83` |
| ダウンロード読み取りバッファ | 65,536 バイト（64 KiB）。読み取りループは BOOTH と 3 ストア（TBF/FANZA/DLsite）で共有し、**進捗コールバックが `false` を返すと中断**して途中のバイト列は破棄する（`read_body_with_progress`） | `crates/core/src/tbf/transport.rs:134-171` |
| ダウンロード総量の上限 | **なし**（`Vec<u8>` に全量保持）。ただしユーザーの中止（本棚の「ダウンロード中止」）で中断・破棄される | `crates/core/src/tbf/transport.rs:141-171`; `crates/app/src/views/bookshelf.rs:2986-3002,4197-4220` |
| BOOTH 側の一時 URL 有効期限 | 署名付き S3 URL は 180 秒（コメント記載） | `crates/core/src/booth.rs:301-302` |
| ページ数の安全弁 | 最大 100 ページ（library / orders 共通） | `crates/core/src/booth.rs:258-260`,`:291-293` |

#### 5. 同期の状態管理

| 項目 | 事実 | アンカー |
|---|---|---|
| BOOTH 用の同期状態テーブル | **存在しない**。`sync_state` モジュールは `drive_sync_state`（Google Drive 専用） | `crates/core/src/db/sync_state.rs:1-6`、`crates/core/src/db/mod.rs:20` |
| 最終同期時刻 | **記録しない**。`bookshelf_items.synced_at` は固定リテラル `"2026-08-25 00:00:00"` を書き込むだけ | `crates/app/src/views/bookshelf.rs:2365` |
| `app_settings` の BOOTH キー | `"booth.session"` のみ（最終同期時刻・前回件数などのキーは無い） | `crates/app/src/app_state.rs:193`,`:392`,`:407` |
| 実行中フラグ | `BookshelfView.sync_busy: usize`（全サイト共通カウンタ。初期値 0） | `crates/app/src/views/bookshelf.rs:783`,`:1065` |
| 二重起動防止 | `sync_all` の `if self.sync_busy > 0 { return; }` のみ | `crates/app/src/views/bookshelf.rs:2229-2232` |
| 同期開始/終了 | `+= 1`（`:2270`） / `saturating_sub(1)`（`:2449`、結果適用時） | `crates/app/src/views/bookshelf.rs:2270`,`:2449` |
| busy の副作用 | サイト切替 `:1114`、明示 reload `:3293` 系、ダウンロード開始 `:2720`、保留ダウンロード実行 `:1866` を抑止。UI の busy 表示は `self.sync_busy > 0` | `crates/app/src/views/bookshelf.rs:5809` |
| 同期完了後の反映 | `this.reload(cx)`（本棚再読込）を成功時のみ実行 | `crates/app/src/views/bookshelf.rs:2457` |
| セッションのロック | `Arc<parking_lot::Mutex<Option<BoothSession>>>`（`booth_logged_in` も別 Mutex） | `crates/app/src/app_state.rs:52-53` |
| DB の同時実行設定 | `foreign_keys(true)`、`journal_mode = WAL`、`busy_timeout = 5 秒`、`create_if_missing(true)` | `crates/core/src/db/mod.rs:51-56` |
| 失敗時の状態 | エラーでも `sync_busy` は減算され（`:2449`）、Error トーストのみ。DB は部分反映のまま（後続 DELETE は実行されない） | `crates/app/src/views/bookshelf.rs:2449-2471` |
| 同期フラグの永続化 | なし（メモリのみ。アプリ再起動で busy 状態は消える） | `crates/app/src/views/bookshelf.rs:783` |

#### 6. 商品 ID / URL の規約

#### 6.1 ID 形式

| 名称 | 型 / 形式 | アンカー |
|---|---|---|
| 商品 ID（ライブラリ） | `u64`（HTML の `items/(\d+)` を `parse::<u64>().unwrap_or(0)`、0 は無効） | `crates/core/src/booth.rs:50`,`:416-419` |
| `bookshelf_items.database_id` | 商品 ID の 10 進文字列 | `crates/app/src/views/bookshelf.rs:2342` |
| `books.tbf_product_id` | 同じ 10 進文字列（ダウンロード時に対応付け） | `crates/app/src/views/bookshelf.rs:2988`,`:3068`、`crates/core/src/db/books.rs:192-203` |
| `books.site_id` | `"booth"`（`sites` テーブルの id） | `crates/core/src/db/schema.sql:22-23`、`crates/app/src/views/bookshelf.rs:2984` |
| ダウンロード ID | `downloadables/{\d+}`（商品 ID とは別の数値。例: items/7825209 → downloadables/8191306） | `crates/core/src/booth.rs:567-591`、`crates/core/src/booth.rs:658-668` |
| `item_id` の逆引き | `database_id.parse::<u64>()`（失敗時は表紙の代替取得をスキップ） | `crates/app/src/views/bookshelf.rs:6996`、`:2775-2778` |

#### 6.2 正規表現一覧（すべて `regex` crate、`booth.rs` 内）

| 対象 | パターン | アンカー |
|---|---|---|
| 商品リンク + タイトル | `<a[^>]*href="https?://(?:[a-z0-9-]+\.)?booth\.pm/(?:ja/)?items/(\d+)"[^>]*><div class="text-text-default font-bold[^"]*"[^>]*>(.*?)</div></a>` | `crates/core/src/booth.rs:411-412` |
| ページネーション検出 | `/library\?page=(\d+)` | `crates/core/src/booth.rs:204-205` |
| ショップ名 | `<img alt="([^"]+)" class="rounded-\[50%\]"` | `crates/core/src/booth.rs:481` |
| 作者名（優先 1） | `class="user-avatar"[^>]*title="([^"]+)"` | `crates/core/src/booth.rs:499` |
| 作者名（優先 2） | `<div class="shop-name[^"]*">\s*<a[^>]*>([^<]+)</a>` | `crates/core/src/booth.rs:504` |
| JSON-LD 抽出 | `(?s)<script[^>]*type="application/ld\+json"[^>]*>(.*?)</script>` → `brand.name`（trim、空は不採用、壊れた JSON は次へ） | `crates/core/src/booth.rs:527-543` |
| ファイル名 | `class="min-w-0 break-words whitespace-pre-line"[^>]*>(.*?)</div>`（タグ除去 + trim、空は `None`） | `crates/core/src/booth.rs:550-556` |
| DL URL（優先 1: 本体） | `data-href[" ]?="(https://booth\.pm/downloadables/\d+[^"]*)"[^>]*data-test="downloadable"` | `crates/core/src/booth.rs:567-568` |
| DL URL（優先 2: browsable からクエリ除去） | `data-href="(https://booth\.pm/downloadables/\d+)\?browse=1"` | `crates/core/src/booth.rs:579` |
| DL URL（後方互換フォールバック） | `data-href="(https://booth\.pm/downloadables/\d+[^"]*)""` → `?` 以降を切り捨て | `crates/core/src/booth.rs:587-591` |
| サムネイル | `<img class="l-library-item-thumbnail" src="([^"]+)"` | `crates/core/src/booth.rs:598` |
| 購入履歴（タグ→`|` 置換後、連続 `|` を 1 個に圧縮） | `発送完了\s*\|\s*(.+?)\s*\|\s*注文日時:\s*(\d{4}/\d{2}/\d{2} \d{2}:\d{2}:\d{2})` | `crates/core/src/booth.rs:608-613` |
| タグ除去 | `<[^>]+>` → `""` + trim | `crates/core/src/booth.rs:471-476` |
| CSRF トークン | 文字列分割（正規表現ではない）: `csrf-token" content="` の次を `"` まで | `crates/core/src/booth.rs:463-468` |

#### 6.3 解析ブロックの切り出し（`parse_library` の補完処理）

- 商品ごとに `html.find("items/{item_id}")` の位置から **4,000 バイト**を切り出し、UTF-8 文字境界へクランプ（`is_char_boundary` で後退）。その範囲からショップ名・ファイル名・DL URL・サムネイルを抽出 — `crates/core/src/booth.rs:431-442`。
- 既知の制約: 1 商品に複数ダウンロードがある場合、`data-href` は**最初の 1 つ**しか拾えない（ブロック先頭 4,000 バイト制限 + 先頭一致） — `docs/import-patterns.md:705-717`。
- DL URL が `None` かつ（ファイル名 or サムネイルが `Some`）のとき `log::warn!` で item_id / title / file_name / block 先頭 400 文字を出力 — `crates/core/src/booth.rs:447-457`。

#### 6.4 URL・値のその他の規約

| 項目 | 規約 | アンカー |
|---|---|---|
| 商品ページ URL | `https://booth.pm/ja/items/{id}` | `crates/core/src/booth.rs:362`,`:400` |
| ショップ URL（サブドメイン） | `https://{subdomain}.booth.pm/`（作者名は `user-avatar` の `title`） | `crates/core/src/booth.rs:491-494` |
| ダウンロード URL | `https://booth.pm/downloadables/{id}`（`?browse=1` は除去して保存） | `crates/core/src/booth.rs:57`,`:578-591` |
| サムネイル URL | `booth.pximg.net` の 300x300 サムネイル（公開画像、Cookie 不要） | `crates/core/src/booth.rs:55-56`、`crates/app/src/views/bookshelf.rs:6994-7005` |
| 表紙（原寸）URL | `item_detail` JSON の `images[].original`（先頭のみ採用） | `crates/core/src/booth.rs:373-381`、`crates/app/src/views/bookshelf.rs:2313-2318` |
| 購入日時文字列 | `"YYYY/MM/DD HH:MM:SS"`（例 `2026/01/01 19:36:23`）。並び替えは `date_sort_key` で正規化 | `crates/core/src/booth.rs:65`,`:613`、`crates/app/src/views/bookshelf.rs:363-367` |
| `format` 値 | ライブラリ同期では常に `"PDF"`（実際のファイル種別は取り込み時に判定） | `crates/app/src/views/bookshelf.rs:2349`、`:2908-2921` |
| 取り込みファイル名 | 本棚の `file_name` を優先、無ければ `"{title}.pdf"` | `crates/app/src/views/bookshelf.rs:7298-7302` |
| 作者名の反映先 | `books.author`（`set_metadata`）と `bookshelf_items.author`（`update_author`）。`site_id` が `booth`/`fanza`/`dlsite` のときだけ実行 | `crates/app/src/views/bookshelf.rs:6899-6933`、`crates/core/src/db/bookshelf.rs:214-231` |
| `owner_sub` | 上記 3 章参照。BOOTH の商品 ID とは無関係 | `crates/core/src/db/books.rs:250-265` |
| 重複ダウンロード抑止 | `books::resolve_reuse_id(&db, key, site_id, product_id, sub)` で同一 `(site_id, tbf_product_id)` の既存 `book_id` を再利用 | `crates/app/src/views/bookshelf.rs:2950-2964`、`crates/core/src/db/books.rs:336-360` |

#### 6.5 ドキュメント側の記述

- `docs/features.md:234-235`: 技術書典・BOOTH の同期ボタンは「専用スレッド + タイムアウト付き」。
- `docs/features.md:255-256`: 履歴カードのイベント名欄は BOOTH では購入日を表示。
- `docs/database.md:13-24`: `sites` テーブル初期データに `booth`（BOOTH、`https://booth.pm`、display_order = 1、is_visible = 1）。
- `docs/database.md:79-87`: `bookshelf_items` は「同期データのスナップショット」、PK は `site_id + database_id`、`author` は「技術書典は空、BOOTH 等は作成者名」。
- `docs/database.md:314`: 同期 → `bookshelf_items` 等というデータフロー記載。
- `docs/import-patterns.md:705-717`: BOOTH の 1 商品 2 ファイル問題（未解決制約、PDF 側のみ取得される）。
- `docs/logout.md:16-39`: ログアウト 2 段階（plaza → booth.pm）、CSRF は `GET https://booth.pm/ja` の meta、成功時 204、incognito の理由、Cookie 収集元 2 ドメイン。
- `docs/account-switch.md`: BOOTH に関する記述は**無い**（grep で 0 件）。

### 7.4 技術書典

出典: `crates/core/src/tbf/`（`mod.rs` / `sync.rs` / `queries.rs` / `transport.rs`）, `crates/app/src/views/tbf_login.rs`, `crates/app/src/views/auth.rs`

情報源（全行精読）: `crates/core/src/tbf/mod.rs`(1223 行) / `crates/core/src/tbf/queries.rs`(150) / `crates/core/src/tbf/sync.rs`(462) / `crates/core/src/tbf/transport.rs`(219) / `crates/app/src/views/tbf_login.rs`(292) / `crates/app/src/views/auth.rs`(790) / `crates/core/src/secrets.rs`(86) / 参照: `crates/app/src/app_state.rs`, `crates/app/src/workspace.rs`, `crates/app/src/views/{checklist.rs,bookshelf.rs,settings.rs}`, `crates/core/src/db/{mod.rs,books.rs,bookshelf.rs,checklist.rs,samples.rs,settings.rs}`, `crates/core/tests/{tbf.rs,sync_fk.rs}`, `docs/{features.md,database.md,logout.md,account-switch.md,import-patterns.md}`
（読み取りのみ実施。ビルド・テスト・整形は未実行）

#### 0. ファイル構成（指定との差分）

- 指定された `crates/core/src/tbf/client.rs`（28.8KB）は**存在しない**。TBF のクライアント本体は `crates/core/src/tbf/mod.rs` に集約されている（`glob(**/client.rs)` の結果は `crates/core/src/fanza/client.rs` と `crates/core/src/dlsite/client.rs` のみ）。
- `crates/core/src/tbf/` の構成: `mod.rs`（クライアント + マッピング + canonical マスタ）+ `queries.rs`（GraphQL 文字列）+ `sync.rs`（DB 反映）+ `transport.rs`（HTTP 抽象）。
- モジュール宣言は `crates/core/src/tbf/mod.rs:10-12`、transport の再公開は `crates/core/src/tbf/mod.rs:16`。

#### 1. `transport.rs` / `queries.rs` の役割（1 行ずつ）

| ファイル | 役割（1 行） | アンカー |
|---|---|---|
| `transport.rs` | HTTP 抽象: `RequestSpec`（method/url/headers/body/redirects）と `ResponseSpec`（status/headers/body）の定義、`Transport` trait、本番実装 `UreqTransport`（ureq + rustls）、ダウンロード進捗コールバックを提供 | `crates/core/src/tbf/transport.rs:5-19`, `:53-69`, `:77-108`, `:127-219` |
| `queries.rs` | Web 実装（`packages/thundoku-api`）から**verbatim コピーした GraphQL クエリ文字列定数**を保持（整形禁止コメント付き） | `crates/core/src/tbf/queries.rs:1-3`, `:6`, `:9-121`, `:124-141`, `:145-146`, `:150` |
| `sync.rs` | `TbfClient` の取得結果を SQLite（`bookshelf_items` / `tbf_events` / `checked_items`）へ UPSERT し、差分フラグを返す | `crates/core/src/tbf/sync.rs:1-3`, `:70`, `:130`, `:159`, `:244` |

#### 1.1 transport の詳細定数

| 項目 | 値 | アンカー |
|---|---|---|
| 接続タイムアウト（通常 agent） | 5 s | `crates/core/src/tbf/transport.rs:95` |
| 読み取りタイムアウト（通常 agent） | 15 s | `crates/core/src/tbf/transport.rs:96` |
| 接続タイムアウト（手動リダイレクト agent） | 5 s | `crates/core/src/tbf/transport.rs:104` |
| 読み取りタイムアウト（手動リダイレクト agent） | 15 s | `crates/core/src/tbf/transport.rs:105` |
| 手動リダイレクト agent の `redirects` | 0（3xx をそのまま返す） | `crates/core/src/tbf/transport.rs:103`, `:129-135` |
| リトライ / バックオフ | **実装なし**（tbf モジュール全体に sleep/retry/backoff が 0 件） | `crates/core/src/tbf/transport.rs:127-219`（該当コードなし） |
| ダウンロード読み取りバッファ | 64 KiB（`vec![0u8; 64 * 1024]`） | `crates/core/src/tbf/transport.rs:181` |
| 進捗通知の間引き | 「1 % 変化ごとに 1 回」のみコールバック | `crates/core/src/tbf/transport.rs:183-190` |
| `content-length` 不明時 | `total = 0` → 進捗コールバックなし | `crates/core/src/tbf/transport.rs:172-175`, `:186-189` |
| 読み込みエラー時 | `Interrupted` は continue、その他は break（部分ボディを返す） | `crates/core/src/tbf/transport.rs:196-197` |
| デフォルト `send_download` | `send()` で全体を取得し `on_progress(total,total)` を 1 回呼ぶ | `crates/core/src/tbf/transport.rs:59-68` |
| `Set-Cookie` パース | 最初の `;` まで、`name=value` に分解（値は trim） | `crates/core/src/tbf/transport.rs:33-50` |
| ヘッダ参照 | 大小文字無視、**最後の出現**を返す | `crates/core/src/tbf/transport.rs:24-30` |

#### 1.2 queries.rs の定数

| 定数 | 元ネタ（コメント） | アンカー |
|---|---|---|
| `BOOKSHELF_QUERY`（`BookShelfQuery($first, $after)`） | books.ts:462 verbatim | `crates/core/src/tbf/queries.rs:6` |
| `CHECKLIST_QUERY`（`EventOfflineCircleChecklistQuery`） | checklist-graphql.ts:78-192 verbatim | `crates/core/src/tbf/queries.rs:9-121` |
| `PRODUCT_IMAGES_QUERY`（`ProductImagesQuery`、`images(first: 8)`） | samples.ts:97-115 verbatim | `crates/core/src/tbf/queries.rs:124-141`, `:129` |
| `EVENT_QUERY`（`TbfEventQuery($eventID)`） | events.ts:96 verbatim | `crates/core/src/tbf/queries.rs:145-146` |
| `LOGIN_MUTATION`（`UserLoginMutation`） | 2026-08-21 にブラウザで実採取 | `crates/core/src/tbf/queries.rs:148-150` |
| すべて `pub(crate)` | モジュール外非公開 | `crates/core/src/tbf/queries.rs:6`, `:9`, `:124`, `:145`, `:150` |

#### 2. 定数（実値一覧）

| 名称 | 値 | アンカー |
|---|---|---|
| `TBF_HOME` | `https://techbookfest.org/` | `crates/core/src/tbf/mod.rs:18` |
| `TBF_GRAPHQL` | `https://techbookfest.org/api/graphql` | `crates/core/src/tbf/mod.rs:19` |
| `TBF_GRAPHQL_V2` | `https://techbookfest.org/api/2/graphql` | `crates/core/src/tbf/mod.rs:20` |
| `TBF_DOWNLOAD_BASE` | `https://techbookfest.org/api/product-dlc` | `crates/core/src/tbf/mod.rs:21` |
| `SITE_ID_TECHBOOKFEST` | `"techbookfest"` | `crates/core/src/tbf/mod.rs:22` |
| `USER_AGENT` | `Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36` | `crates/core/src/tbf/mod.rs:25` |
| 本棚クエリ URL サフィックス | `?operationName=BookShelfQuery&appVersion=20260417a-web` | `crates/core/src/tbf/mod.rs:296` |
| チェックリスト URL サフィックス | `?operationName=EventOfflineCircleChecklistQuery&appVersion=20260424a-web` | `crates/core/src/tbf/mod.rs:417` |
| ログイン URL | `{TBF_GRAPHQL}?operationName=UserLoginMutation` | `crates/core/src/tbf/mod.rs:215` |
| ログアウト URL | `{TBF_GRAPHQL}?operationName=LogoutUserMutation` | `crates/core/src/tbf/mod.rs:257` |
| 試し読み URL | `{TBF_GRAPHQL}?operationName=ProductImagesQuery` | `crates/core/src/tbf/mod.rs:478` |
| イベント探索 URL | `{TBF_GRAPHQL_V2}?operationName=TbfEventQuery` | `crates/core/src/tbf/mod.rs:377` |
| 本棚ページサイズ | `first = 100` 件 | `crates/core/src/tbf/mod.rs:292` |
| チェックリストページサイズ | `checkedProductInfosFirst = 100` 件 | `crates/core/src/tbf/mod.rs:410` |
| `followingOrganizationsFirst` | `0`（フォロー組織は取得しない） | `crates/core/src/tbf/mod.rs:412` |
| サークル criteria | `circles(first: 1, criteria: {eventID: $eventID})` | `crates/core/src/tbf/queries.rs:46`, `:73` |
| バリアント取得数 | `productVariants(first: 10)` | `crates/core/src/tbf/queries.rs:29` |
| 試し読み取得数 | `images(first: 8)` | `crates/core/src/tbf/queries.rs:129` |
| 将来イベント探索範囲 | `tbf21`〜`tbf30`（`.rev()` で降順） | `crates/core/src/tbf/mod.rs:333` |
| canonical イベント数 | 21 件（`tbf20`…`tbf1` + `tbf-ouen-matsuri`） | `crates/core/src/tbf/mod.rs:968-1137` |
| ログイン拡張 `clientLibrary` | `{"name":"@apollo/client","version":"4.1.9"}` | `crates/core/src/tbf/mod.rs:212`（ログイン）, `:254`（ログアウト） |
| Accept（ダウンロード解決） | `text/html,application/xhtml+xml,application/xml;q=0.9,image/webp,*/*;q=0.8` | `crates/core/src/tbf/mod.rs:534` |
| Accept-Language | `ja,en-US;q=0.9,en;q=0.8` | `crates/core/src/tbf/mod.rs:190`, `:536` |

#### 3. 同期手順（本棚 / BookShelfQuery）

エントリポイント（コア）: `TbfClient::bookshelf(&mut self) -> Result<Vec<TbfShelfItem>, TbfError>` — `crates/core/src/tbf/mod.rs:287`
エントリポイント（UI）: `BookshelfView::sync_tbf(&mut self, cx)` — `crates/app/src/views/bookshelf.rs:2635`
保存エントリポイント: `save_bookshelf(pool, items) -> Result<usize, sqlx::Error>` — `crates/core/src/tbf/sync.rs:70`

1. UI は `tbf_logged_in` を確認。false ならトースト「ログインしてから同期してください」+ `OpenAuth` アクションを dispatch して終了 — `crates/app/src/views/bookshelf.rs:2636-2645`
2. `sync_busy` をインクリメント（`sync_busy > 0` の間は再同期・ダウンロード・タグ／非表示操作を拒否） — `crates/app/src/views/bookshelf.rs:2648`, `:2229-2232`, `:2719-2722`, `:3292-3295`
3. **専用 OS スレッド**（`std::thread::spawn`）でネットワーク処理を実行（GPUI ワーカーブロック回避）。結果は `std::sync::mpsc::channel::<Result<usize,String>>` で返す — `crates/app/src/views/bookshelf.rs:2654-2663`
4. `AppState::tbf` の `Mutex<TbfClient>` を lock（手動同期とポーラーを直列化） — `crates/app/src/views/bookshelf.rs:2658`
5. GraphQL: **POST** `https://techbookfest.org/api/graphql?operationName=BookShelfQuery&appVersion=20260417a-web`、ヘッダ `Content-Type: application/json` + `Cookie:` + `X-XSRF-TOKEN:`、`redirects=5` — `crates/core/src/tbf/mod.rs:296`, `:597-612`, `:636-661`
6. ページング: `variables = { first: 100, after: <endCursor|null> }`。ループ終了条件は `pageInfo.hasNextPage == false`、または `hasNextPage == true` かつ `endCursor` が null/非文字列 — `crates/core/src/tbf/mod.rs:289-327`
7. **ページ間 sleep は無し**（ループは即座に次ページ要求）。ページ数上限・総件数上限の定数も無し — `crates/core/src/tbf/mod.rs:290-327`
8. 各 `edge.node` を `map_shelf_node` で `TbfShelfItem` に変換 — `crates/core/src/tbf/mod.rs:722-755`
9. `data.viewer.bookShelfItems` が無い場合は `TbfError::InvalidResponse("missing bookShelfItems")` で中断（部分結果は破棄） — `crates/core/src/tbf/mod.rs:301-303`
10. 保存: `save_bookshelf` が 1 件ずつ `bookshelf::upsert`（UPSERT 単位＝1 行、トランザクション無し）— `crates/core/src/tbf/sync.rs:70-127`, `crates/core/src/db/bookshelf.rs:61-67`
11. 保存件数（`items.len()`）を返す — `crates/core/src/tbf/sync.rs:127`
12. UI は 120 ms 間隔でチャネルをポーリングしてスピナーを回し、完了時に `sync_busy` を減算、成功トースト「技術書典サイトから N 件取得しました」/ 失敗トースト（Error）— `crates/app/src/views/bookshelf.rs:2666-2700`
13. 失敗メッセージに `"session expired"` が含まれる場合 `OpenAuth` を dispatch（`TbfError::SessionExpired` の Display 文字列一致） — `crates/app/src/views/bookshelf.rs:2696-2702`, `crates/core/src/tbf/mod.rs:70-71`

#### 3.1 本棚ノードのマッピング（`map_shelf_node`）

| 出力フィールド | 入力 JSON パス / 変換 | アンカー |
|---|---|---|
| `id` | `product.databaseID`（文字列以外・欠落時は空文字） | `crates/core/src/tbf/mod.rs:736` |
| `title` | `product.name` | `crates/core/src/tbf/mod.rs:737` |
| `circle_name` | `product.organization.name` | `crates/core/src/tbf/mod.rs:738-740` |
| `thumbnail_url` | `product.coverImage.url` を `absolutize` | `crates/core/src/tbf/mod.rs:730`, `:741` |
| `format` | `downloadContent.fileName` の拡張子を大文字化。拡張子なし/空は `"BOOK"` | `crates/core/src/tbf/mod.rs:725-729` |
| `caused_at` | `node.causedAt` | `crates/core/src/tbf/mod.rs:742` |
| `event_name` | `marketHandshake.event.name` | `crates/core/src/tbf/mod.rs:743` |
| `event_slug` | `marketHandshake.event.id` を `normalize_event_slug` | `crates/core/src/tbf/mod.rs:744` |
| `file_name` | `downloadContent.fileName` | `crates/core/src/tbf/mod.rs:745` |
| `download_url` | `downloadContent.downloadURL` を `absolutize` | `crates/core/src/tbf/mod.rs:726-727`, `:746` |
| `is_downloadable` | `downloadContent` が null でない | `crates/core/src/tbf/mod.rs:747` |
| `tags` | `product.tags` の文字列要素のみ `Vec<String>`（配列以外は `None`） | `crates/core/src/tbf/mod.rs:748-754` |
| `absolutize` | 先頭 `/` のとき `https://techbookfest.org` を前置、絶対 URL はそのまま | `crates/core/src/tbf/mod.rs:691-697` |

#### 4. 同期手順（チェックリスト / EventOfflineCircleChecklistQuery）

エントリポイント（コア）: `TbfClient::checklist(&mut self, event_slug: &str)` — `crates/core/src/tbf/mod.rs:402`
エントリポイント（UI 手動）: `ChecklistView::sync(&mut self, cx)` — `crates/app/src/views/checklist.rs:397`
お気に入り取り込み（UI）: `ChecklistView::import_favorites(&mut self, cx)` — `crates/app/src/views/checklist.rs:446`
定期ポーリング: `Workspace::start_checklist_poller` — `crates/app/src/workspace.rs:236`
保存+差分: `refresh_checklist(db, client, event_slug) -> Result<SyncPollOutcome, String>` — `crates/core/src/tbf/sync.rs:244`

#### 4.1 `refresh_checklist` の処理手順（番号付き）

1. 保存前シグネチャを取得: `checklist::list_events(db)` / `checklist::list_items(db, slug)` — `crates/core/src/tbf/sync.rs:250-251`
2. `client.events()` を呼ぶ（イベントマスタ。後述 §5） — `crates/core/src/tbf/sync.rs:252`
3. `client.checklist(event_slug)` を呼ぶ（本節） — `crates/core/src/tbf/sync.rs:253`
4. `save_events(db, &events)` → `tbf_events` を UPSERT — `crates/core/src/tbf/sync.rs:255`
5. `save_checklist(db, event_slug, &entries)` → `checked_items` を UPSERT + 差分削除 — `crates/core/src/tbf/sync.rs:256`
6. `app_settings` に `api.last_sync_at` = `chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")` を set（**UTC 文字列**）— `crates/core/src/tbf/sync.rs:258-262`
7. 保存後シグネチャを再取得 — `crates/core/src/tbf/sync.rs:264-265`
8. `changed` = イベントシグネチャ差分 or 項目シグネチャ差分 — `crates/core/src/tbf/sync.rs:267-268`
9. 返り値 `SyncPollOutcome { count: entries.len(), changed }` — `crates/core/src/tbf/sync.rs:270-273`, 構造体定義 `:210-214`

差分シグネチャの定義:

| 対象 | 比較キー | アンカー |
|---|---|---|
| イベント | `(slug, event_name)` のみ | `crates/core/src/tbf/sync.rs:217-223` |
| チェック項目 | `(id, circle_name, space_number, product_title)`。`is_checked` 等ユーザー状態は**比較対象外** | `crates/core/src/tbf/sync.rs:226-236` |

#### 4.2 `TbfClient::checklist` の処理手順（番号付き）

1. `event_id = "Event:{event_slug}"` を組み立てる — `crates/core/src/tbf/mod.rs:403`
2. `window = event_window(event_slug)`（canonical マスタの開催期間、ループ前に 1 回だけ計算） — `crates/core/src/tbf/mod.rs:405`, `crates/core/src/tbf/mod.rs:929-938`
3. ループ: `variables = { checkedProductInfosFirst: 100, checkedProductInfosAfter: cursor, followingOrganizationsFirst: 0, eventID }` — `crates/core/src/tbf/mod.rs:409-413`
4. GraphQL: **POST** `{TBF_GRAPHQL}?operationName=EventOfflineCircleChecklistQuery&appVersion=20260424a-web`、`Content-Type: application/json` + Cookie + XSRF、`redirects=5` — `crates/core/src/tbf/mod.rs:414-421`, `:597-612`
5. `data.viewer.checkedProductInfos` 欠落 → `InvalidResponse("missing checkedProductInfos")` — `crates/core/src/tbf/mod.rs:422-424`
6. 各 edge を `map_checked_product` で変換。`None`（オンライン限定サークル等）は捨てる — `crates/core/src/tbf/mod.rs:431`, `crates/core/src/tbf/mod.rs:757-794`
7. **イベント絞り込み 2 段**: `entry.id.starts_with("{event_slug}:")` かつ `within_event_window(&window, created_at)` の両方を満たすものだけ採用 — `crates/core/src/tbf/mod.rs:437-441`
8. ページング終了条件: `pageInfo.hasNextPage == false` または `endCursor` が null — `crates/core/src/tbf/mod.rs:444-457`
9. ページ間 sleep なし・並列なし・リトライなし — `crates/core/src/tbf/mod.rs:406-458`
10. `favorites(event_slug)` は `checklist(event_slug)` の別名（同一クエリ・同一マッピング） — `crates/core/src/tbf/mod.rs:464-466`

#### 4.3 チェック項目マッピング（`map_checked_product`）

| 出力 | 入力 / 変換 | アンカー |
|---|---|---|
| `id` | `"{event_slug}:{product_identity}"`。`event_slug` は `exhibit.event.databaseID` から `resolve_event_slug` | `crates/core/src/tbf/mod.rs:788-791` |
| `product_identity` | `productInfo.databaseID`、無ければ `productInfo.id`、両方無ければ空文字 | `crates/core/src/tbf/mod.rs:784-787` |
| 除外条件 | `hasOfflineCourse == false` のサークルは `None`（オンライン限定を除外） | `crates/core/src/tbf/mod.rs:762-764` |
| サークル選択 | `choose_best_circle`: `tbf` 番号が**最小**のサークルが勝つ | `crates/core/src/tbf/mod.rs:797-812` |
| `circle_name` | `productInfo.organization.name` | `crates/core/src/tbf/mod.rs:794` |
| `space_number` | `exhibit.spaces[0]`、無ければ `"TBD"` | `crates/core/src/tbf/mod.rs:795-800` |
| `tbf_circle_id` | `exhibit.databaseID`、無ければ `exhibit.id` | `crates/core/src/tbf/mod.rs:801-804` |
| `product_title` | `productInfo.name` | `crates/core/src/tbf/mod.rs:805` |
| `thumbnail_url` | `productInfo.coverImage.url` を `absolutize` | `crates/core/src/tbf/mod.rs:806` |
| `price` | `extract_price`: `productVariants.edges` の先頭から `status != "DRAFT"` の最初の `price`。全滅時は先頭 variant の `price` | `crates/core/src/tbf/mod.rs:807`, `:836-847` |
| `is_purchased` | `productInfo.loginUserBookShelfItem` が null でない | `crates/core/src/tbf/mod.rs:808` |
| `created_at` | `node.createdAt`（購入日時） | `crates/core/src/tbf/mod.rs:809` |

#### 4.4 購入日時フィルタ（`within_event_window`）

- 営業日判定は `created_at` の先頭 10 文字（`YYYY-MM-DD`）を開始日・終了日と**辞書順比較**（両端含む） — `crates/core/src/tbf/mod.rs:945-956`
- `window` が `None`（canonical に無い slug / 期間未登録）または `created_at` が `None` の場合は**常に true**（絞り込みしないフォールバック） — `crates/core/src/tbf/mod.rs:951-955`
- 目的（コメント）: `checkedProductInfos` は criteria で対象イベントのサークルに絞られるため、対象イベントに出展しているだけの商品（他イベントで購入）まで `{対象イベント}:` 形式の id になるのを防ぐ — `crates/core/src/tbf/mod.rs:940-944`
- テスト実測: `tbf20` = 2026-04-11〜2026-04-26。期間内購入は保持、`tbf19` 期間内（2025-11-13）は除外、境界日は保持 — `crates/core/src/tbf/mod.rs:1195-1207`
- テスト実測: `event_window("tbf06")` は `None`（canonical の slug は `tbf6` であって `tbf06` ではないため一致しない）→ フォールバックで保持 — `crates/core/src/tbf/mod.rs:1210-1221` / slug 定義 `:1090`(`tbf6`)

#### 4.5 お気に入り取り込み（`import_favorites`）

- `client.favorites(slug)` → `save_checklist(db, slug, &entries)`。ファイル/ネットワーク処理は `background_executor().spawn` + `Mutex` lock — `crates/app/src/views/checklist.rs:464-472`
- 完了トースト「お気に入りを N 件取り込みました」、エラー時 `"session expired"` を含めば `OpenAuth` — `crates/app/src/views/checklist.rs:478-489`

#### 5. 同期手順（イベントマスタ / TbfEventQuery + canonical）

エントリポイント: `TbfClient::events(&mut self)` — `crates/core/src/tbf/mod.rs:331`

1. `for n in (21..=30).rev()`: slug `tbf{n}` を組み立て、`discover_event_name(slug)` を呼ぶ。**Err は `continue`（握り潰し）** — `crates/core/src/tbf/mod.rs:333-337`
2. `discover_event_name`: **POST** `{TBF_GRAPHQL_V2}?operationName=TbfEventQuery`、body `{operationName, query: EVENT_QUERY, variables:{eventID:"Event:{slug}"}}`、`Content-Type: application/json`、`redirects=5` — `crates/core/src/tbf/mod.rs:369-381`
3. status が 2xx 以外 → `TbfError::Upstream("event query status {status}")`。JSON でない → `InvalidResponse`。`errors` あり → `Upstream("event query errors")` — `crates/core/src/tbf/mod.rs:382-392`
4. 成功時は `data.event.name`（無ければ `None` → `event_name = slug`）で `TbfEventInfo` を作る。`event_format="offline"`, `is_cancelled=false`, `display_order=0`, `is_featured=false`, 期間は `None` — `crates/core/src/tbf/mod.rs:338-349`
5. 探索結果を先に push し、続いて `canonical_events()` を **slug 重複チェック付き**で push（探索が先勝ち） — `crates/core/src/tbf/mod.rs:351-361`
6. 最終ループで `display_order = index`、`is_featured = (index == 0)` を全件に再設定 — `crates/core/src/tbf/mod.rs:362-365`
7. ネットワーク不通でも canonical だけで成功扱い（テスト: 全探索 404 でも 21 件以上返る） — `crates/core/tests/tbf.rs:487-499`

`canonical_events()` の実値（`crates/core/src/tbf/mod.rs:968-1135`）:

| slug | event_name | start | end | format | is_cancelled |
|---|---|---|---|---|---|
| tbf20 | 技術書典20 | 2026-04-11 | 2026-04-26 | hybrid | false |
| tbf19 | 技術書典19 | 2025-11-15 | 2025-11-30 | hybrid | false |
| tbf18 | 技術書典18 | 2025-05-31 | 2025-06-15 | hybrid | false |
| tbf17 | 技術書典17 | 2024-11-02 | 2024-11-17 | hybrid | false |
| tbf16 | 技術書典16 | 2024-05-25 | 2024-06-09 | hybrid | false |
| tbf15 | 技術書典15 | 2023-11-11 | 2023-11-26 | hybrid | false |
| tbf14 | 技術書典14 | 2023-05-20 | 2023-06-04 | hybrid | false |
| tbf13 | 技術書典13 | 2022-09-10 | 2022-09-25 | hybrid | false |
| tbf12 | 技術書典12 | 2022-01-22 | 2022-01-30 | online | false |
| tbf11 | 技術書典11 | 2021-07-10 | 2021-07-25 | hybrid | false |
| tbf10 | 技術書典10 | 2020-12-26 | 2021-01-06 | online | false |
| tbf9 | 技術書典9 | 2020-09-12 | 2020-09-22 | online | false |
| tbf8 | 技術書典8 | 2020-02-29 | 2020-03-01 | offline | **true** |
| tbf-ouen-matsuri | 技術書典 応援祭 | 2020-03-07 | 2020-04-05 | online | false |
| tbf7 | 技術書典7 | 2019-09-22 | 2019-09-22 | offline | false |
| tbf6 | 技術書典6 | 2019-04-14 | 2019-04-14 | offline | false |
| tbf5 | 技術書典5 | 2018-10-08 | 2018-10-08 | offline | false |
| tbf4 | 技術書典4 | 2018-04-22 | 2018-04-22 | offline | false |
| tbf3 | 技術書典3 | 2017-10-22 | 2017-10-22 | offline | false |
| tbf2 | 技術書典2 | 2017-04-09 | 2017-04-09 | offline | false |
| tbf1 | 技術書典1 | 2016-06-25 | 2016-06-25 | offline | false |

- `tbf_event_id` は全件 `format!("Event:{slug}")`、`event_date` = start と同値 — `crates/core/src/tbf/mod.rs:1138-1153`
- `is_cancelled = true` は `tbf8` のみ — `crates/core/src/tbf/mod.rs:1065-1072`, テスト `crates/core/tests/tbf.rs:496-498`

#### 6. ダウンロード URL 解決とファイル取得

| 関数 | 手順 / 値 | アンカー |
|---|---|---|
| `resolve_download_url(&mut self, url)` | **GET**（受け取った URL をそのまま）、ヘッダ `User-Agent` + `Accept` + `Accept-Language` + Cookie + XSRF、`redirects = 0`（3xx を手動処理） | `crates/core/src/tbf/mod.rs:526-540`, `:539` |
| 401/403 | `TbfError::SessionExpired` | `crates/core/src/tbf/mod.rs:541-543` |
| 3xx + `Location` | `absolutize` → `validate_download_url` が真なら**その絶対 URL を返す**（本文は取得しない） | `crates/core/src/tbf/mod.rs:545-552` |
| それ以外 | `TbfError::NotFound` | `crates/core/src/tbf/mod.rs:553` |
| `validate_download_url` | 相対は `api/product-dlc/` 前置のみ許可。絶対は `https://techbookfest.org/api/product-dlc/` または `https://storage.googleapis.com/tbf-tokyo-product-dlc/` のみ許可 | `crates/core/src/tbf/mod.rs:702-719` |
| `download_with_progress(url, on_progress)` | **GET** ヘッダ `User-Agent` + `Cookie`（非空時）+ `X-XSRF-TOKEN`（存在時）、`redirects = 5`、`Transport::send_download` で進捗通知 | `crates/core/src/tbf/mod.rs:562-585` |
| ダウンロードの status | 2xx 以外は `TbfError::Upstream("download status {status}")` | `crates/core/src/tbf/mod.rs:586-591` |
| `download(url)` | `download_with_progress` + 空クロージャ | `crates/core/src/tbf/mod.rs:556-558` |
| UI 側フォールバック URL | `items.download_url` が無い場合 `{TBF_DOWNLOAD_BASE}/{product_id}/download`（`product_id` = `bookshelf_items.database_id`） | `crates/app/src/views/bookshelf.rs:2880-2887`, `:2740` |
| UI 側の後処理 | ダウンロード直後に `books::set_site_id` / `books::set_tbf_product_id` / `apply_site_metadata` / `seed_progress_if_absent` / `set_owner_sub` を実行し、TBF クライアントの lock は PDF レンダリング前に drop | `crates/app/src/views/bookshelf.rs:2983-3013`, `:2903-2905` |

#### 7. DB 反映（テーブル / カラム / 単位）

#### 7.1 `bookshelf_items`（`save_bookshelf` → `bookshelf::upsert`）

| カラム | 書き込む値 | アンカー |
|---|---|---|
| `site_id` | `"techbookfest"` 固定 | `crates/core/src/tbf/sync.rs:83` |
| `database_id` | `TbfShelfItem.id`（= `product.databaseID`） | `crates/core/src/tbf/sync.rs:84` |
| `title` / `circle_name` | そのまま | `crates/core/src/tbf/sync.rs:85-86` |
| `author` | **常に空文字**（技術書典に作者情報が無い。UPSERT 時に空なら既存値を保持する SQL） | `crates/core/src/tbf/sync.rs:87-88`, `crates/core/src/db/bookshelf.rs:87-89` |
| `thumbnail_url` / `format` / `causedAt` / `event_name` / `event_slug` / `file_name` / `download_url` | そのまま | `crates/core/src/tbf/sync.rs:89-96` |
| `event_id` | `event_slug`（空文字は `None`） | `crates/core/src/tbf/sync.rs:97-101` |
| `is_downloadable` / `is_checked` / `is_purchased` / `is_new` / `is_active` / `is_favorite` / `is_hidden` | `is_downloadable as i64`, `1,1,0,1,0,0` 固定 | `crates/core/src/tbf/sync.rs:102-108` |
| `hidden_at` | `None`（UPSERT 時は既存値を保持） | `crates/core/src/tbf/sync.rs:109`, `crates/core/src/db/bookshelf.rs:84-85` |
| `tags_json` | `item.tags` を `serde_json::to_string`（失敗時 `None`。UPSERT は `COALESCE` で既存保持） | `crates/core/src/tbf/sync.rs:75-79`, `crates/core/src/db/bookshelf.rs:86` |
| `synced_at` / `created_at` / `updated_at` | すべて `now()` = `Utc::now().format("%Y-%m-%d %H:%M:%S")` | `crates/core/src/tbf/sync.rs:8-10`, `:110-112` |
| 共有ソースメタ列（`media_category`,`ai_type`,`is_drm`,`release_date`,`description`,`theme`,`maker_id`,`page_count`,`age_rating`,`series_name`） | `None` / `0` 固定（TBF では未使用） | `crates/core/src/tbf/sync.rs:113-124` |
| 競合キー | `ON CONFLICT(site_id, database_id)` | `crates/core/src/db/bookshelf.rs:67` |
| トランザクション | **無し**（1 行 1 ステートメントを `block_on` で逐次実行） | `crates/core/src/db/bookshelf.rs:61-77`, `crates/core/src/db/mod.rs:44-45` |
| 返り値 | `items.len()`（実際の UPSERT 成功件数ではなく入力件数） | `crates/core/src/tbf/sync.rs:127` |

#### 7.2 `tbf_events`（`save_events` / `ensure_event` → `checklist::upsert_event`）

| カラム | 値 | アンカー |
|---|---|---|
| `id` | slug（安定キー） | `crates/core/src/tbf/sync.rs:135`, `:49` |
| `site_id` | `"techbookfest"` | `crates/core/src/tbf/sync.rs:136`, `:51` |
| `slug` | slug | `crates/core/src/tbf/sync.rs:137`, `:52` |
| `tbf_event_id` | `Event:{slug}`（`ensure_event`）/ `TbfEventInfo.tbf_event_id`（`save_events`） | `crates/core/src/tbf/sync.rs:53`, `:138` |
| `event_name` | `TbfEventInfo.event_name` / `ensure_event` では `event_name.unwrap_or(slug)` | `crates/core/src/tbf/sync.rs:139`, `:54` |
| `event_date` / `event_start_date` / `event_end_date` / `event_format` / `is_cancelled` / `display_order` / `is_featured` | `TbfEventInfo` の値（`ensure_event` は canonical から引き、無ければ `(None,None,"offline",false)`） | `crates/core/src/tbf/sync.rs:140-146`, `:30-46` |
| `poll_sync_enabled` | 挿入時 0、**UPSERT 時は既存値を保持**（`poll_sync_enabled = tbf_events.poll_sync_enabled`）＝UI トグルが同期で消えない | `crates/core/src/tbf/sync.rs:148`, `crates/core/src/db/checklist.rs:66` |
| `created_at` / `updated_at` | `now()` | `crates/core/src/tbf/sync.rs:149-150` |
| 競合キー | `ON CONFLICT(id)` | `crates/core/src/db/checklist.rs:54` |

#### 7.3 `checked_items`（`save_checklist` → `checklist::upsert_item`）

| カラム | 値 | アンカー |
|---|---|---|
| `id` | `{eventSlug}:{productIdentity}` | `crates/core/src/tbf/sync.rs:183`, `crates/core/src/tbf/mod.rs:790` |
| `event_id` | 引数の `event_slug`（FK → `tbf_events.id` は `ensure_event` で自動生成） | `crates/core/src/tbf/sync.rs:184`, `:163` |
| `circle_name` / `space_number` / `tbf_circle_id` / `product_id` / `product_title` / `thumbnail_url` / `price` / `is_purchased` / `created_at` | `TbfChecklistEntry` の値。`created_at` は `None` のとき `now()` | `crates/core/src/tbf/sync.rs:185-205` |
| `memo` | `"product:{product_id} {product_title}"`、`product_id` が `None` なら空文字 | `crates/core/src/tbf/sync.rs:176-179` |
| `is_checked` | 送信値は 0、UPSERT 時は **既存値を保持**（`is_checked = checked_items.is_checked`）＝ユーザーのチェック状態を壊さない | `crates/core/src/tbf/sync.rs:188`, `crates/core/src/db/checklist.rs:139` |
| `sort_order` | 取得順の index（0 始まり） | `crates/core/src/tbf/sync.rs:189` |
| `thumbnail_data` | `None`（UPSERT では `excluded` を書き込む） | `crates/core/src/tbf/sync.rs:191` |
| `sample_fetch_attempted_at` | `None`（毎回上書き） | `crates/core/src/tbf/sync.rs:194` |
| 競合キー | `ON CONFLICT(id)`（preserved: `is_checked`） | `crates/core/src/db/checklist.rs:134`, `:139` |
| トランザクション | 無し（削除ループ + 1 行ずつ UPSERT） | `crates/core/src/tbf/sync.rs:163-207` |

削除（完全同期）:

1. `checklist::list_items(pool, event_slug)` で既存行を列挙 — `crates/core/src/tbf/sync.rs:169`
2. 今回の取得結果に無い id の行は `samples::delete_for_item` → `checklist::delete_item` の順に削除（FK 依存のため試し読みを先に消す） — `crates/core/src/tbf/sync.rs:170-173`, `crates/core/src/db/samples.rs:44-49`
3. 他イベントの行には触れない（`event_id` で絞っているため）— テスト `crates/core/src/tbf/sync.rs:414-423`
4. 同じ slug を 2 件→1 件で再同期すると 1 件になる（テスト）— `crates/core/src/tbf/sync.rs:395-411`

#### 7.4 FK 自動生成（`ensure_event`）

- `bookshelf_items.event_id` / `checked_items.event_id` の FK 違反を防ぐため、対象 slug の `tbf_events` 行が無ければ挿入する（既存なら何もしない）— `crates/core/src/tbf/sync.rs:15-31`
- 新規行の開催日は canonical マスタから引く（Web のイベント同期相当）。無ければ `(None, None, "offline", false)` — `crates/core/src/tbf/sync.rs:30-46`
- 検証テスト: canonical に無い `tbf21` / `tbf22` を保存しても FK 違反にならず、`tbf22` 行が作られる — `crates/core/tests/sync_fk.rs:31-33`, `:61-65`

#### 7.5 `app_settings` / その他

| キー・テーブル | 用途 | アンカー |
|---|---|---|
| `api.last_sync_at` | `refresh_checklist` 成功時に UTC `%Y-%m-%d %H:%M:%S` を set。設定画面の「最終同期」表示に使用 | `crates/core/src/tbf/sync.rs:258-262`, `crates/app/src/views/settings.rs:1675`, `crates/app/src/views/checklist.rs:106` |
| `checklist.poll.interval_min` | ポーリング間隔（分）。既定 5、`max(1)` で下限 1、UI 入力も `parse().unwrap_or(5).max(1)` | `crates/app/src/workspace.rs:245-250`, `crates/app/src/views/settings.rs:160-184` |
| `tbf_events.poll_sync_enabled` | イベントごとのポーリング ON/OFF（UI トグル） | `crates/core/src/db/checklist.rs:104-110`, `crates/app/src/views/checklist.rs:1166-1185` |
| `product_sample_pages` | 試し読み画像（base64）。挿入は `samples::insert_sample_page`、旧ページは `delete_for_item` で全削除してから保存 | `crates/core/src/db/samples.rs:20-42`, `crates/app/src/views/checklist.rs:533-570` |
| `books.site_id` / `books.tbf_product_id` | ダウンロード後に `"techbookfest"` / `bookshelf_items.database_id` を記録 | `crates/app/src/views/bookshelf.rs:2983-2988`, `crates/core/src/db/books.rs:192-204`, `:420-427` |

- backup 対象外の設定: `docs/database.md:38-39` に `api.last_sync_at` が「環境依存の設定」として列挙（`crates/core/src/db/backup.rs:5-7`）。

#### 8. 同期の状態管理 / ロック / スケジューリング

| 項目 | 実装 | アンカー |
|---|---|---|
| ポーリングループ | `Workspace::start_checklist_poller` が常駐タスクを 1 本 spawn（`Workspace::new` から起動） | `crates/app/src/workspace.rs:156`, `:236-301` |
| 周期 | `checklist.poll.interval_min`（既定 5 分 / 下限 1 分）を**毎周期読み直す** | `crates/app/src/workspace.rs:245-250`, `:297-299` |
| 未ログイン時 | 何もせず interval 分 sleep して continue | `crates/app/src/workspace.rs:251-256` |
| 対象イベント 0 件時 | 何もせず interval 分 sleep して continue | `crates/app/src/workspace.rs:257-263` |
| 対象イベント列挙 | `db::checklist::list_enabled_slugs`（`poll_sync_enabled = 1` かつ `slug IS NOT NULL` を `display_order` 順） | `crates/core/src/db/checklist.rs:116-124`, `crates/app/src/workspace.rs:257` |
| 逐次実行 | 有効 slug を**順番に** `refresh_checklist`（並列度 1）。1 件失敗してもログを出して次へ進む | `crates/app/src/workspace.rs:267-289` |
| ロック | `AppState::tbf: Arc<Mutex<TbfClient>>` をポーラーと手動同期が共有。手動側は専用スレッド/バックグラウンドタスクで lock するため直列化される | `crates/app/src/app_state.rs:42`, `crates/app/src/workspace.rs:265-268`, `crates/app/src/views/checklist.rs:417-419`, `crates/app/src/views/bookshelf.rs:2658` |
| セッション切れ | メッセージに `"session expired"` を含むとき `OpenAuth` を dispatch。ポーラーは `auth_dispatched` フラグで**1 回だけ**出す | `crates/app/src/workspace.rs:243`, `:278-290` |
| Drive 連携 | `changed == true` のときだけ `settings.sync_drive_now(cx)` を呼ぶ | `crates/app/src/workspace.rs:294-296`, `docs/features.md:568-570` |
| 最終同期時刻 | `api.last_sync_at`（`refresh_checklist` が毎回更新、本棚同期 `save_bookshelf` では**更新しない**） | `crates/core/src/tbf/sync.rs:258-262` |
| 本棚側のロック/状態 | `BookshelfView::sync_busy`（実行中の同期タスク数カウンタ。`> 0` で再同期・ダウンロード・お気に入り・非表示を拒否） | `crates/app/src/views/bookshelf.rs:782-783`, `:2230-2232`, `:2648`, `:2681` |
| 専用 `sync_state` テーブル | **TBF 用は存在しない**。`drive_sync_state` は Google Drive 用（pack 単位） | `crates/core/src/db/sync_state.rs:1-2`, `docs/database.md:280-283` |
| アプリ起動時の復元 | keyring から TBF セッションを読み、`tbf.is_authenticated()` で `tbf_logged_in` を決める（読み込み失敗時は false） | `crates/app/src/app_state.rs:151-161`, `:240` |
| 手動同期の多重起動防止 | ボタンから `sync_tbf` を呼ぶ前に `sync_busy > 0` で return。失敗時は `Error` トースト + `"session expired"` を含めば `OpenAuth` | `crates/app/src/views/bookshelf.rs:2229-2232`, `:2692-2703` |

#### 9. ログイン方式

#### 9.1 アプリで使用される方式（WebView Cookie 取得）

| 段階 | 内容 | アンカー |
|---|---|---|
| ビュー生成 | `TbfLoginView::new(window, cx)` が WebView を作り、`https://techbookfest.org/user/signin` をロード、非表示にする | `crates/app/src/views/tbf_login.rs:29-42`, `:33` |
| WebView 実装 | `lb_wry::WebViewBuilder`（`#[cfg(debug_assertions)]` では `with_devtools(true)`）。`window.window_handle()` 失敗時は `webview = None` を返す | `crates/app/src/views/tbf_login.rs:46-67`, `:48-49`, `:50-58` |
| 表示/非表示 | `show()` で `WebView::show()` + `check_generation += 1` + 監視再開。`close()` で `check_generation += 1`（監視停止）+ 非表示 + Entity を `take()` | `crates/app/src/views/tbf_login.rs:70-84` |
| モーダル起動 | `AuthDialog::ensure_states` が `cx.defer_in` で `TbfLoginView` を生成（render 後の defer で RefCell 再入を回避）。`AuthDialog::login_tbf` / `open_with_provider(TechBookFest)` が `show_tbf_login = true` にする | `crates/app/src/views/auth.rs:283-311`, `:320-323`, `:92-105` |
| WebView 未生成時 | テスト環境等では WebView が無く、`check_login` は常に false | `crates/app/src/views/tbf_login.rs:118-122`, テスト `:272-289` |
| URL 監視 | 1 秒間隔のループ（`Duration::from_secs(1)`）。`WeakEntity` を使い、ビュー drop で `update` が Err → ループ終了（リーク防止） | `crates/app/src/views/tbf_login.rs:88-116`, `:96-98`, `:93`, `:106-110` |
| 完了判定 1 | 現在 URL が `techbookfest.org` をホスト末尾に持ち、かつ URL に `/user/signin` を含まない | `crates/app/src/views/tbf_login.rs:124-137`, `:134` |
| 完了判定 2 | `cookies_for_url("https://techbookfest.org")` から Cookie を取得し、`TbfSession::from_cookies` の `is_logged_in()` が true であること（= `XSRF-TOKEN` 以外の Cookie が 1 つ以上） | `crates/app/src/views/tbf_login.rs:138-151`, `crates/core/src/tbf/mod.rs:40-61` |
| 保存 | `crate::app_state::save_tbf_session(cx, &session)` → keyring 保存 + `AppState::tbf` へ `restore_session` + `tbf_logged_in = true` | `crates/app/src/views/tbf_login.rs:153-154`, `crates/app/src/app_state.rs:373-383` |
| 完了通知 | WebView を `hide()` して `cx.emit(TbfLoginDone)`、`check_login` は true を返しループ終了 | `crates/app/src/views/tbf_login.rs:155-160` |
| 完了時の UI 処理 | `AuthDialog` が `tbf_logged_in = true`、`show_tbf_login = false`、ビュー購読を破棄し `CloseAuth` を defer dispatch | `crates/app/src/views/auth.rs:283-299` |
| キャンセル時 | `TbfLoginCancelled` で `show_tbf_login = false` + ビュー/購読を破棄（次回は新規フロー）。閉じるボタンは WebView の右外側 36 px 角（収まらなければ左外側） | `crates/app/src/views/auth.rs:300-308`, `crates/app/src/views/tbf_login.rs:203-234` |
| モーダル寸法 | WebView = 中央 480 x 640 px、閉じるボタン 36 x 36 px、間隔 10 px、背景 `hsla(0,0,0,0.45)` | `crates/app/src/views/tbf_login.rs:169-176`, `:204-209`, `:218-220` |
| UI 文言（フォーム） | 「技術書典のログインはアプリ内ブラウザ（WebView）で行います。…」+「技術書典ログイン画面を開く」ボタンのみ（メール/パスワード入力欄は実装されていない。ソースコメントは "email/password フォーム" だが実体はボタン） | `crates/app/src/views/auth.rs:617-640` |

#### 9.2 HTTP 直叩きのログイン（コア実装・アプリ未使用）

| 関数 | 手順 | アンカー |
|---|---|---|
| `bootstrap()` | **GET** `https://techbookfest.org/`、ヘッダ `User-Agent` + `Accept-Language: ja,en-US;q=0.9,en;q=0.8`、`redirects = 5` → `Set-Cookie` を吸収。`XSRF-TOKEN` が無ければ `InvalidResponse("no XSRF-TOKEN cookie in response")` | `crates/core/src/tbf/mod.rs:184-202`, `:193` |
| `login(email, password)` | **POST** `{TBF_GRAPHQL}?operationName=UserLoginMutation`、body = `{operationName:"UserLoginMutation", variables:{loginInput:{email,password}}, extensions:{clientLibrary:{name:"@apollo/client",version:"4.1.9"}}, query:LOGIN_MUTATION}`、`Content-Type: application/json`、`redirects = 5` | `crates/core/src/tbf/mod.rs:208-222` |
| 成否判定 | 401/403 → `InvalidCredentials`。`errors` キーあり → `InvalidCredentials`。`data.loginUser.user.id` が空でない文字列 **かつ** `is_authenticated()` が真 → `session()` を返す。それ以外 → `InvalidCredentials` | `crates/core/src/tbf/mod.rs:224-242` |
| Cookie 吸収 | `absorb_cookies`: `Set-Cookie` を名前で置換（同名は削除して末尾に追加）。`XSRF-TOKEN` は `xsrf_raw`（生値）と `xsrf_token`（`percent_decode` 済み）の両方を保持 | `crates/core/src/tbf/mod.rs:672-684`, `:898-915` |
| 使用状況 | アプリ本体からの呼び出しは無く、`crates/core/tests/tbf.rs:51,111,137` のみ（テスト専用）。実運用のログインは §9.1 の WebView | `crates/core/tests/tbf.rs:51`, `:111-112`, `:137-140` |

#### 9.3 Cookie / トークン仕様

| 項目 | 値 | アンカー |
|---|---|---|
| Cookie 名（セッション） | 特定名に依存しない。`XSRF-TOKEN` **以外**の Cookie が 1 つでもあればログイン済みと判定（テストでは `session`） | `crates/core/src/tbf/mod.rs:59-61`, `crates/core/tests/tbf.rs:103-115` |
| CSRF ヘッダ | `X-XSRF-TOKEN: <percent_decode 済み XSRF-TOKEN 値>` を **全リクエスト**（`request()` 経由）に付与 | `crates/core/src/tbf/mod.rs:655-658`, `:674-677` |
| `Cookie:` ヘッダ | `name=value` を `"; "` 連結。空なら付与しない | `crates/core/src/tbf/mod.rs:664-670` |
| percent-decode | `%XX` のみ自前デコード（`hex_val`、大文字小文字対応）。不正シーケンスはそのまま | `crates/core/src/tbf/mod.rs:898-926` |
| セッション有効期限 | **アプリ側に有効期限の実装なし**。期限切れはサーバー応答（401/403 または auth 系 GraphQL エラー）で検出して `SessionExpired` を返す方式 | `crates/core/src/tbf/mod.rs:224-230`, `:614-627`, `:870-896`（該当実装なし） |
| auth 系エラー判定 | `is_auth_related`: `errors[].message` または `errors[].extensions.code` を小文字化し、`unauthorized` / `unauthenticated` / `forbidden` / `login` / `session` のいずれかを含めば真 | `crates/core/src/tbf/mod.rs:870-896` |
| ログアウト | **POST** `{TBF_GRAPHQL}?operationName=LogoutUserMutation`、body `{operationName:"LogoutUserMutation", variables:{input:{}}, extensions:…, query:"mutation LogoutUserMutation($input: LogoutUserInput!) { logoutUser(input: $input) { clientMutationId user { id email __typename } } }"}`、`redirects = 5`。401/403 → `InvalidCredentials`、`errors` → `InvalidResponse(errors[0].message)`, 成功時は cookies/xsrf をクリア | `crates/core/src/tbf/mod.rs:250-284`, `:263`, `:265-274` |
| ログアウトで使ってはいけない API | `POST https://techbookfest.org/user/signout` は 200 を返すがログアウトしない（疑似成功） | `docs/logout.md:14` |
| ログアウト UI | `SettingsView::logout_tbf`: サーバーログアウト → ローカルセッションを空で `restore_session` → `tbf_logged_in = false` → `secrets.delete(USER_TECHBOOKFEST)` をバックグラウンド実行。トーストは成功/失敗で出し分け | `crates/app/src/views/settings.rs:607-649`, `docs/logout.md:43-48` |

#### 10. 認証情報の保存先と保護

| 項目 | 内容 | アンカー |
|---|---|---|
| keyring サービス名 | `com.megablacklabel.thundoku-shelf` | `crates/core/src/secrets.rs:7` |
| ユーザキー | `USER_TECHBOOKFEST = "techbookfest"` | `crates/core/src/secrets.rs:9` |
| ライブラリ | `keyring` crate v3（features: `apple-native`, `windows-native`）/ `crates/core/Cargo.toml:21` | `crates/core/Cargo.toml:21`, `crates/core/src/secrets.rs:40-46` |
| 保存形式 | `TbfSession` の **JSON 文字列**（`{"cookies":[["name","value"],…],"xsrf_raw":"…","xsrf_token":"…"}`）。`TbfSession` は `Serialize/Deserialize` derive | `crates/app/src/app_state.rs:373-379`, `crates/core/src/tbf/mod.rs:27-34` |
| 暗号化 | **アプリ独自の暗号化なし**（OS の資格情報ストアに平文相当で格納。`docs/features.md:432-433` は「セッションは keyring に保存」と記載） | `crates/core/src/secrets.rs:40-46`, `docs/features.md:430-433` |
| 読み込み | 起動時に `secrets.load(USER_TECHBOOKFEST)` → `serde_json::from_str` → `tbf.restore_session()` → `tbf.is_authenticated()` を `tbf_logged_in` に反映。パース失敗時は false（エラーはログ無し） | `crates/app/src/app_state.rs:151-162`, `:240` |
| 保存タイミング | WebView ログイン完了時（`save_tbf_session`）のみ | `crates/app/src/views/tbf_login.rs:154`, `crates/app/src/app_state.rs:373-383` |
| 削除タイミング | ログアウト時（`SettingsView::logout_tbf`、バックグラウンド `SecretStore::delete`）。サーバーログアウト失敗でもローカル削除は実行 | `crates/app/src/views/settings.rs:630-635` |
| サイズ制約 | TBF セッションは keyring 保存のため、Windows Credential Manager の上限（2560 UTF-16 文字）に関する考慮・ガードは**実装に無い**（BOOTH/FANZA/DLsite は上限を理由に `app_settings` の DB 保存へ切り替えている） | `crates/app/src/app_state.rs:190-192`, `:220-227`, `crates/app/src/app_state.rs:392`, `:418`, `:438` |
| 他キー | `USER_GOOGLE="google"` / `USER_BOOTH="booth"` / `USER_DB_KEY="thundoku-shelf.db-key"`（`owner_sub` 暗号鍵 32 byte を BASE64 で保存） | `crates/core/src/secrets.rs:10-14`, `:71-84` |

#### 11. レート制限 / 待機 / リトライ / 並列度 / タイムアウト

| 項目 | 実値 | アンカー |
|---|---|---|
| HTTP 接続タイムアウト | 5 s（両 agent） | `crates/core/src/tbf/transport.rs:95`, `:104` |
| HTTP 読み取りタイムアウト | 15 s（両 agent） | `crates/core/src/tbf/transport.rs:96`, `:105` |
| リトライ回数 | 0（リトライ実装なし） | `crates/core/src/tbf/transport.rs:127-219` |
| バックオフ | なし | 同上 |
| リクエスト間 sleep（ページング） | なし（本棚・チェックリストとも即次ページ） | `crates/core/src/tbf/mod.rs:290-327`, `:406-458` |
| イベント探索間 sleep | なし（`tbf30`→`tbf21` を連続 10 リクエスト） | `crates/core/src/tbf/mod.rs:333-350` |
| 429 / Retry-After 対応 | なし | `crates/core/src/tbf/mod.rs:597-634` |
| 並列度（コア） | 1（すべて同期 API。並列実行なし） | `crates/core/src/tbf/mod.rs:287`, `:402` |
| 並列度（アプリ） | TBF クライアントは `Mutex` で直列化。手動同期は専用 OS スレッド 1 本ずつ（`sync_busy` ガード） | `crates/app/src/app_state.rs:42`, `crates/app/src/views/bookshelf.rs:2635-2663`, `:2230-2232` |
| ポーリング間隔 | `checklist.poll.interval_min` 分（既定 5、下限 1） | `crates/app/src/workspace.rs:245-250` |
| ポーリング 1 周期あたりの HTTP 数 | `Σ_slug (10 回のイベント探索 + ⌈チェックリスト件数/100⌉ 回)`（探索は失敗 slug でもリクエスト自体は送る。例: 1 slug / 100 件以下なら 11 リクエスト） | `crates/core/src/tbf/mod.rs:333`, `:410`, `crates/app/src/workspace.rs:271` |
| UI の結果ポーリング | 120 ms 間隔 | `crates/app/src/views/bookshelf.rs:2674-2676`, `crates/app/src/views/checklist.rs`（同方式） |
| ログイン URL 監視 | 1 s 間隔 | `crates/app/src/views/tbf_login.rs:96-98` |
| ダウンロード進捗通知 | 1 % 刻み | `crates/core/src/tbf/transport.rs:183-190` |
| リダイレクト上限 | 既定 agent 5（`RequestSpec.redirects = 0` のときだけ手動） | `crates/core/src/tbf/transport.rs:79-83`, `:131-135` |
| キャンセル | コア API にキャンセル機構なし（`Task::drop` でも中断しない設計: 専用スレッド + channel） | `crates/app/src/views/bookshelf.rs:2654-2663`, `:2754-2759` |

#### 12. エラー型とエラー時挙動

`TbfError`（`crates/core/src/tbf/mod.rs:65-78`）:

| variant | 表示文字列 | 発生条件（主） |
|---|---|---|
| `Network(String)` | `network error: {0}` | `UreqTransport` の transport エラー（`ureq::Error::Status` 以外）— `crates/core/src/tbf/mod.rs:67`, `crates/core/src/tbf/transport.rs:147`, `:217` |
| `InvalidCredentials` | `invalid credentials` | ログイン 401/403・GraphQL errors・user id 空・セッション Cookie 無し／ログアウト 401/403 — `crates/core/src/tbf/mod.rs:69`, `:224-241`, `:265-266` |
| `SessionExpired` | `session expired` | 任意 API の 401/403、auth 系 GraphQL エラー — `crates/core/src/tbf/mod.rs:71`, `:486-488`, `:614-616`, `:626-627`, `:541-543` |
| `NotFound` | `not found` | `resolve_download_url` が 3xx + 有効 Location を得られない — `crates/core/src/tbf/mod.rs:73`, `:553` |
| `Upstream(String)` | `upstream error: {0}` | 2xx 以外の GraphQL／ダウンロード、イベント探索の非 2xx・errors — `crates/core/src/tbf/mod.rs:75`, `:617-621`, `:586-591`, `:382-392` |
| `InvalidResponse(String)` | `invalid response: {0}` | 非 JSON ボディ、`bookShelfItems` / `checkedProductInfos` 欠落、`XSRF-TOKEN` 無し、ログアウト errors — `crates/core/src/tbf/mod.rs:77`, `:196-198`, `:302`, `:424`, `:623-624` |

| エラー時の挙動 | 内容 | アンカー |
|---|---|---|
| UI がセッション切れを検知する方法 | `format!("{err}")` に `"session expired"` が含まれるか（文字列一致）→ `OpenAuth` を dispatch | `crates/app/src/workspace.rs:279-283`, `crates/app/src/views/checklist.rs:433-436`, `:484-487`, `:585-588`, `crates/app/src/views/bookshelf.rs:2696-2702` |
| 本棚同期の部分失敗 | 1 ページ目でエラーが出ると全体が Err（`?` で中断、保存もされない） | `crates/core/src/tbf/mod.rs:294-303`, `crates/core/src/tbf/sync.rs:252-256` |
| チェックリストの部分失敗 | 2 ページ目以降の失敗は全体 Err（既に集めた `all` は捨てられる） | `crates/core/src/tbf/mod.rs:414-421` |
| イベント探索の失敗 | その slug だけスキップ（`continue`）。`events()` 自体は Err を返さない | `crates/core/src/tbf/mod.rs:335-337` |
| 試し読みの非致命的失敗 | 401/403 のみ `SessionExpired`。それ以外の非 2xx／非 JSON／errors は **空配列（成功扱い）** | `crates/core/src/tbf/mod.rs:485-495` |
| DB エラー | `sync.rs` は `sqlx::Error` を `String` に写像して返す（`refresh_checklist` の戻りが `Result<_, String>`） | `crates/core/src/tbf/sync.rs:244-249` |
| 保存失敗時のロールバック | 無し（トランザクション未使用のため、途中まで書かれた行は残る） | `crates/core/src/tbf/sync.rs:70-127`, `:159-207` |

#### 13. ID 規約

| ID | 形式 / 由来 | アンカー |
|---|---|---|
| 作品 ID（本棚） | 技術書典 GraphQL `product.databaseID`（文字列。DLC id とは異なる場合がある）。`bookshelf_items.database_id` に保存 | `crates/core/src/tbf/mod.rs:736`, `:525-526`, `crates/core/src/tbf/sync.rs:84` |
| 作品 ID（チェックリスト） | `productInfo.databaseID`、無ければ `productInfo.id`（`ProductInfo:xxxx` 形式の可能性） | `crates/core/src/tbf/mod.rs:784-787` |
| チェックリスト項目 ID | `{eventSlug}:{productIdentity}`（例 `tbf20:p1`）。コメントに "stable per event+product" | `crates/core/src/tbf/mod.rs:98`, `:790`, テスト `crates/core/tests/tbf.rs:470` |
| イベント slug | `resolve_event_slug`: database id を小文字化して最初の `tbf` 以降の数字までを切り出し（`tbf20` 等）。`normalize_event_slug` は `Event:` 前置を除去 → 上記 → 失敗時は数字のみ抽出して `tbf{digits}` | `crates/core/src/tbf/mod.rs:821-833`, `:850-867` |
| イベント ID（API 用） | `Event:{slug}` | `crates/core/src/tbf/mod.rs:403`, `:340`, `crates/core/src/tbf/sync.rs:53` |
| イベント DB id | `tbf_events.id` = slug（`UNIQUE(site_id, slug)` も存在） | `crates/core/src/tbf/sync.rs:135`, `docs/database.md:75` |
| サークル ID | `exhibit.databaseID`、無ければ `exhibit.id`（`checked_items.tbf_circle_id`。DB 上 `tbf_circle_id`） | `crates/core/src/tbf/mod.rs:801-804` |
| ローカル本との対応 | ダウンロード時に `books.site_id = "techbookfest"` / `books.tbf_product_id = {作品 ID}` を記録し、`bookshelf_items.database_id` と突合（UI の「ダウンロード済み」判定） | `crates/app/src/views/bookshelf.rs:2983-2988`, `crates/core/src/db/books.rs:192-204` |
| 後方互換の補完 | 既存本で `tbf_product_id` が NULL の場合は `bookshelf_items.file_name == books.file_name` または `title` 一致で紐付けて `set_tbf_product_id` | `crates/app/src/views/bookshelf.rs:1431-1446` |
| `owner_sub` との関係 | `owner_sub` は技術書典ではなく **Google アカウントの sub** に由来。TBF 同期は `owner_sub` を書き換えない。`books.owner_sub = encrypt(db_key, google_sub)` はダウンロード／インポート完了時にのみ設定（Google 未ログイン時は NULL＝未所属） | `crates/app/src/views/bookshelf.rs:2742-2745`, `:3008-3013`, `:3127-3132`, `crates/core/src/db/books.rs:250-260`, `crates/core/src/owner.rs:1-4` |
| `(source, owner)` 重複抑止 | `books::find_by_source` / `resolve_reuse_id` は `site_id + tbf_product_id` で既存行を探し、`owner_sub` を復号比較して同一 owner の行のみ再利用 | `crates/core/src/db/books.rs:314-352`, `docs/account-switch.md:55-66`, `:155-158` |

- canonical の slug は**ゼロ埋めなし**（`tbf6`）だが、`resolve_event_slug` はサーバーの database id 由来（`tbf06` 等）もそのまま返しうる。`event_window` は完全一致検索のため、`tbf06` は canonical にヒットせず期間フィルタが無効化される — `crates/core/src/tbf/mod.rs:929-938`, `:1090`, `:1210-1216`

---
