# DB 構成情報

アプリのデータは SQLite（`thundoku-shelf.db`）に保存されます。スキーマの正本は
`crates/core/src/db/schema.sql`、マイグレーションは `crates/core/migrations/0001_init.sql`
（`sqlx::migrate!` 管理）です。後から追加されたテーブル（`view_history` / `page_views` /
`favorite_entities` など）はマイグレーションファイルではなく、`migrate()` 内の冪等な DDL
（`CREATE TABLE IF NOT EXISTS`）で適用されます。

## テーブル一覧

### sites

技術書典・BOOTH などのサイト情報。

| カラム | 型 | 説明 |
|---|---|---|
| id | TEXT PK | サイト ID（`techbookfest` / `booth`） |
| name | TEXT | 表示名 |
| url | TEXT | サイト URL |
| display_order | INTEGER | 表示順 |
| is_visible | INTEGER | 表示フラグ |
| created_at / updated_at | TEXT | 作成・更新日時 |

初期データ: `techbookfest`（技術書典）、`booth`（BOOTH）。

### app_settings

key-value のアプリ設定（ビューアーモード、サイトフィルタ、タグ取得トグル、
Drive 同期設定など）。

| カラム | 型 |
|---|---|
| key | TEXT PK |
| value | TEXT |
| created_at / updated_at | TEXT |

使用されるキー例: `viewer.mode` / `viewer.mode.{site}`、`viewer.page_turn`、
`bookshelf.site_filter`、`tag.fetch.enabled`、`drive.sync.folder_id`、
`drive.sync.enabled`、`api.last_sync_at`。

### books

ローカルに取り込んだ書籍（ダウンロード済みの本）。`.opfspack`（`pack_id`）と紐づく。

| カラム | 型 | 説明 |
|---|---|---|
| id | TEXT PK | 書籍 ID（packId / UUID） |
| title / author / circle_name | TEXT | 書名・著者・サークル名 |
| purchase_date | TEXT | 購入日 |
| file_name | TEXT | オリジナルファイル名 |
| file_size | INTEGER | ファイルサイズ |
| opfs_path | TEXT UNIQUE | pack のローカルパス |
| cover_thumbnail | TEXT | 表紙パス（任意） |
| tbf_product_id | TEXT | 技術書典の商品 ID（bookshelf_items と突合） |
| site_id | TEXT FK→sites | サイト |
| tags_fetched | INTEGER | タグ取得済みフラグ |
| pack_id | TEXT | .opfspack の ID（`packs/{pack_id}.opfspack`） |
| is_favorite | INTEGER | お気に入り |
| is_hidden | INTEGER | 非表示 |
| created_at / updated_at | TEXT | |

### tbf_events

技術書典イベント情報（チェックリスト・イベント名補完に使用）。

| カラム | 型 | 説明 |
|---|---|---|
| id | TEXT PK | イベント ID |
| site_id | TEXT FK→sites | |
| slug | TEXT | スラグ（例: `tbf18`） |
| tbf_event_id | TEXT | 技術書典側 ID |
| event_name | TEXT | イベント名（例: 技術書典18） |
| event_start_date / event_end_date | TEXT | 開催期間（購入日からのイベント名補完に使用） |
| event_format / is_cancelled / display_order / is_featured | TEXT/INT | |
| UNIQUE(site_id, slug) | | |

### bookshelf_items

技術書典・BOOTH の本棚（同期データのスナップショット）。

| カラム | 型 | 説明 |
|---|---|---|
| site_id + database_id | TEXT PK | サイトごとの商品 ID |
| title / circle_name / author | TEXT | 書名・サークル名・作者名（技術書典は author 空、BOOTH 等は作成者名） |
| thumbnail_url | TEXT | 表紙 URL（リモート） |
| format | TEXT | PDF / EPUB 等 |
| causedAt | TEXT | 購入日時 |
| event_name / event_slug / event_id | TEXT/INT | イベント情報（event_id は FK→tbf_events） |
| file_name / download_url | TEXT | ダウンロード情報 |
| is_downloadable / is_checked / is_purchased / is_new / is_active | INTEGER | フラグ群 |
| is_favorite / is_hidden | INTEGER | お気に入り・非表示 |
| tags_json | TEXT | タグの JSON スナップショット |
| synced_at / created_at / updated_at | TEXT | |

### reading_progress

読書進捗（**1 書籍 × 1 コンテンツ = 1 行**）。複数コンテンツ（本文 / 別冊 等）や
レンディション違いは**コンテンツ単位**で持ち、レンディション間では共有する
（`docs/import-patterns.md` §8）。読み出しは `progress::get`（= 優先コンテンツの行）/
`progress::get_for(book_id, content_id)`（リーダーの再開位置）。

| カラム | 型 | 説明 |
|---|---|---|
| book_id | TEXT PK FK→books (ON DELETE CASCADE) | |
| content_id | TEXT PK | `book_contents.content_id`。`''` = 未指定（旧データ / 単一コンテンツ） |
| current_page | INTEGER | **1-indexed**（0 は未読扱い） |
| total_pages | INTEGER | 全ページ数 |
| finished_at | TEXT | 読了日時。一度セットすると戻っても維持 |
| last_read_at | TEXT | 最終閲覧日時 |
| scroll_position | REAL | スクロールモードの位置 |

### view_history

閲覧履歴（1 回の閲覧 = 1 セッション）。**マイグレーションファイルではなく
`migrate()` 内の冪等 DDL で作成**。

| カラム | 型 | 説明 |
|---|---|---|
| id | TEXT PK | セッション ID（hex(randomblob(16))） |
| book_id | TEXT FK→books (ON DELETE CASCADE) | 閲覧した本 |
| started_at | TEXT | 開始時刻 |
| ended_at | TEXT | 終了時刻（閲覧中は最後に操作した時刻に更新される heartbeat） |

- `view_history::start(book_id)` で開始（ended_at は開始時刻で初期化）
- `view_history::touch(session_id)` をページ操作のたびに実行（強制終了対策）
- `view_history::end(session_id)` で終了時刻記録
- `view_count(book_id)` / `total_duration_secs(book_id)` で集計

### page_views

ページ毎の閲覧記録（1 書籍 × 1 ページ 1 行）。`view_history` と同様に
**`migrate()` 内の冪等 DDL で作成**。Google Drive バックアップにも含まれる。

| カラム | 型 | 説明 |
|---|---|---|
| book_id | TEXT PK FK→books (ON DELETE CASCADE) | 本 |
| content_id | TEXT PK | コンテンツ（`''` = 未指定 / 旧データ） |
| page_number | INTEGER PK | **1-indexed**（表示ページ番号） |
| view_count | INTEGER | そのページが表示された累計回数 |
| total_seconds | REAL | そのページでの累計滞在秒数 |
| last_viewed_at | TEXT | 最終表示時刻 |

- `page_views::record_view(book_id, content_id, page)` で表示回数を +1（単一表示は 1 ページ、見開きは左右両ページ）
- `page_views::add_dwell(book_id, page, secs)` で滞在秒数を加算
- `page_views::for_book(book_id)` でページ毎の記録を取得

### checked_items

チェックリスト項目（サークルごと。`tbf_events` に関連）。

| カラム | 型 | 説明 |
|---|---|---|
| event_id / circle_id 等 | TK | イベント・サークル |
| is_checked | INTEGER | チェック状態 |
| memo | TEXT | メモ |
| price | INTEGER | 価格 |
| is_purchased | INTEGER | 購入済みフラグ |

### book_contents / content_formats

1 冊に複数の「コンテンツ（読む単位）」と「レンディション（切替可能な表示形態）」を
持たせるための表（`docs/import-patterns.md` §3.3）。**マイグレーションファイルではなく
`migrate()` 内の冪等 DDL で作成**。

`book_contents`:

| カラム | 型 | 説明 |
|---|---|---|
| content_id | TEXT PK | コンテンツ ID（UUID） |
| book_id | TEXT FK→books (ON DELETE CASCADE) | |
| display_name | TEXT | 表示名（`本文` / `本編` / フォルダ名 / 別冊 等） |
| media_kind | TEXT | `image` / `pdf` / `epub` / `audio` / `video` |
| is_primary | INTEGER | 既定表示にするコンテンツ（0/1） |
| sort_order | INTEGER | 表示順（ZIP 内の並び） |
| created_at | TEXT | |

`content_formats`（同じ内容の別形式・別バリアント。`PDF版` / `画像版` 等）:

| カラム | 型 | 説明 |
|---|---|---|
| format_id | TEXT PK | レンディション ID（UUID） |
| content_id | TEXT FK→book_contents (ON DELETE CASCADE) | |
| label | TEXT | 切替 UI に出す名前（`画像` / `PDF`） |
| format_kind | TEXT | `image` / `pdf` / `epub` / `audio` / `video` |
| page_count | INTEGER | ページ数 |
| pack_entry_prefix | TEXT | pack 内の接頭辞（`pages` / `contents/1/r0`） |
| sort_order | INTEGER | 表示順（先頭が主レンディション） |
| created_at | TEXT | |

- 表紙・裏表紙はコンテンツにしない（カバー画像として `thumbnail.webp` / `cover.webp` に置く）
- `import_zip_bytes` は**全コンテンツ × 全レンディション**を実体化して保存する
  （既定表示コンテンツの第 1 レンディションだけ pack 内パスが従来どおり `pages/...`）
- pack の `metadata.json` にも `contents` を書き出して同期復元用にする
- `display_name` は**ユーザーが変更できる**（ビューアーのトップメニュー「名前」）。
  変更時は DB と同時に pack の `metadata.json` の `displayName` も書き換える
  （`import::rename_content_in_pack`。Drive 復元で名前が戻らないようにする）

### imported_documents

インポートされたドキュメント（PDF 等）のメタ情報。

| カラム | 型 | 説明 |
|---|---|---|
| id | TEXT PK | ドキュメント ID |
| book_id | TEXT FK→books | |
| source_type | TEXT | pdf / zip 等 |
| file_hash | TEXT | SHA-256 |
| total_pages | INTEGER | ページ数（進捗表示のフォールバック） |
| metadata | TEXT | JSON メタデータ |
| status | TEXT | completed 等 |

### document_images

ページ画像（pack 内エントリ）のメタ情報。

| カラム | 型 | 説明 |
|---|---|---|
| document_id | TEXT FK→imported_documents | |
| content_id | TEXT FK→book_contents | 所属コンテンツ（NULL = 旧データ） |
| format_id | TEXT FK→content_formats | 所属レンディション（NULL = 旧データ） |
| image_type | TEXT | page / thumbnail |
| pack_entry_path | TEXT | pack 内のパス（例: `pages/page_0001.webp`） |
| width / height | INTEGER | 表示サイズ（ズーム範囲計算に使用） |
| mime_type / file_size | TEXT/INT | |
| created_at | TEXT | |

`images_for_book` は**既定表示コンテンツの第 1 レンディションのページ**（と
`content_id IS NULL` の旧データ）だけを返す。コンテンツを切り替えるページ一覧 UI は
フェーズ4で対応する。

### document_text / token_analysis

- `document_text`: 抽出したページテキスト（タグ生成の入力）
- `token_analysis`: 形態素解析結果（名詞の頻度など）

### book_tags

書籍のタグ（`source = 'generated'`（自動） / `'manual'`（手動））。

### favorite_tags

お気に入りタグ（タグチップのハート）。

### favorite_entities

お気に入りのサークル / 作者（サークル名 / 作者名チップのハート）。

| カラム | 型 | 説明 |
|---|---|---|
| entity_kind | TEXT PK | `circle`（サークル）/ `author`（作者） |
| entity_name | TEXT PK | サークル名 / 作者名 |
| created_at | TEXT | 登録日時 |

同じ名前でも `entity_kind` が違えば別エントリ。

### zenn_tag_metadata

Zenn のタグメタデータ（`https://zenn.dev/api/tags` 相当から取得、
起動時に 1 回だけプロセス内キャッシュされる）。

### book_first_events

本に最初に関連したイベント（イベントフィルタ用）。

### product_sample_pages

試し読みページ（base64 の `image_data`）。

## マイグレーション

- `crates/core/migrations/0001_init.sql`: 初期スキーマ（`sqlx::migrate!` で
  チェックサム管理。**変更すると既存 DB が VersionMismatch で開けなくなる**）
- `view_history` / `page_views` / `favorite_entities` / `book_contents` /
  `content_formats`、`document_images.content_id` / `format_id`: `migrate()` 内の
  `CREATE TABLE IF NOT EXISTS` / `ALTER TABLE ADD COLUMN` で適用
  （新しいマイグレーションファイルは作らない方針）
- `content_formats.label` の旧値（`画像` / `PDF` / `EPUB`）は `migrate()` 内の
  データ移行（`contents::migrate_legacy_labels`）で実データに合わせて書き換える
  （画像 = 拡張子名、PDF/EPUB = 種別名）
- `reading_progress` / `page_views` は `migrate()` 内のデータ移行
  （`db::migrate_progress_content_id`）で**コンテンツ単位の PK**に作り替える
  （PK 変更は ALTER 不可のため新テーブルへ INSERT SELECT。既存行は優先コンテンツの行として引き継ぐ）
- テスト用には `test_pool()`（インメモリ + 全マイグレーション適用）を使用

## データの流れ

1. 技術書典 / BOOTH の同期 → `bookshelf_items` / `tbf_events` / `checked_items` / `product_sample_pages`
2. ダウンロード → `books` + `imported_documents` + `document_images`（.opfspack 作成）
3. 読み取り → `reading_progress` / `view_history`
4. タグ → `book_tags` / `favorite_tags` / `zenn_tag_metadata`
