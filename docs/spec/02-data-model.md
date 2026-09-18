# 02. データモデル（SQLite スキーマ / ID 規約）

> `docs/spec/README.md`（索引）から参照される設計仕様書の一部。
> 本章は **DB スキーマの全量（CREATE TABLE 全文）と ID 規約**。読み手は AI（別言語での再実装・Web 版への移植を想定）。
> 事実にはアンカー付き。断定できない事項は章末の「不明点 / 推測」に分離してある。
> 情報源: crates/core/src/db/schema.sql, crates/core/migrations/0001_init.sql, crates/core/src/db/*.rs

情報源（すべて読み取り専用で参照。行番号はアンカーとして付記）:

- `crates/core/src/db/mod.rs` / `schema.sql` / `desktop.sql` / `backup.rs` / `books.rs` / `bookshelf.rs` / `checklist.rs` / `contents.rs` / `documents.rs` / `favorites.rs` / `notes.rs` / `page_views.rs` / `progress.rs` / `samples.rs` / `settings.rs` / `sync_state.rs` / `tags.rs` / `view_history.rs`
- `crates/core/migrations/0001_init.sql`
- `crates/core/src/import/mod.rs` / `classify.rs` / `pdf.rs` / `zip_names.rs` / `export_text.rs`
- `crates/opfspack/src/lib.rs` / `format.rs` / `builder.rs` / `reader.rs` / `crypto.rs` / `Cargo.toml` / `tests/interop.rs` / `examples/roundtrip_emit.rs`
- 参照（意図・決定の根拠としてのみ）: `docs/database.md`, `docs/import-patterns.md`（§5.1 §11.1 §11.2 R1〜R6）
- 補助的な裏取り: `crates/core/tests/import.rs`（パイプライン挙動の統合テスト 1,566 行）、`crates/app/src/app_state.rs`（データディレクトリ配置）

アンカー表記は `<相対パス>:<行番号>`。複数行は `:A-B`。行番号は読み取り時点のもの。

---

## 0. モジュール構成（担当領域の全体像）

| モジュール | 役割 | アンカー |
|---|---|---|
| `db::mod` | SQLite 接続（WAL / FK ON / busy_timeout）、sqlx マイグレーション実行、プログラム的 DDL、同期→非同期ブリッジ（`block_on`）、初回クリア | `crates/core/src/db/mod.rs:1-6`, `:50-60`, `:192-400`, `:403-458` |
| `db::schema.sql` | 「正本」スキーマ（20 テーブル）。**コードからは読み込まれない**（`grep` で参照はコメントと `docs/database.md:4` のみ） | `crates/core/src/db/schema.sql` |
| `db::desktop.sql` | Desktop 専用テーブル 1 件（`drive_sync_state`）。`0001_init.sql` 末尾にも同一内容が含まれる | `crates/core/src/db/desktop.sql:1-9` |
| `db::*` 各ファイル | テーブル単位のリポジトリ（同期 API、`Result<_, sqlx::Error>`） | `crates/core/src/db/mod.rs:9-26`（`pub mod` 一覧） |
| `import::mod` | 取り込みパイプライン本体（判定→計画→pack 生成→DB 登録） | `crates/core/src/import/mod.rs:1-21` |
| `import::classify` | ZIP エントリ種別判定（名前のみ） | `crates/core/src/import/classify.rs:1-6` |
| `import::pdf` | PDF → WebP ページ + テキスト抽出（全プラットフォーム共通。PDFium を実行時ロード） | `crates/core/src/import/pdf.rs:1-21`, `:39`, `:84` |
| `import::zip_names` | ZIP エントリ名/本文の CP932 デコード | `crates/core/src/import/zip_names.rs:1-14` |
| `import::export_text` | `_export.txt`（`<<NPage>>` マーカー）パース | `crates/core/src/import/export_text.rs:1-5` |
| `opfspack` | `.opfspack` バイナリ形式の reader/writer + 暗号（TS 実装とバイト互換） | `crates/opfspack/src/lib.rs:1-16` |

---

## 1. スキーマ全量

### 1.1 スキーマがどう適用されるか（重要）

| 事実 | 内容 | アンカー |
|---|---|---|
| 実行時 DDL の順序 | 1) `sqlx::migrate!("./migrations")` が `migrations/0001_init.sql` を適用（`_sqlx_migrations` でチェックサム管理）→ 2) その後 `migrate()` 内の「プログラム的マイグレーション」（`CREATE TABLE IF NOT EXISTS` / `PRAGMA` で存在確認しての `ALTER TABLE`) を毎回冪等に適用 | `crates/core/src/db/mod.rs:192-200` |
| 追加方式 | 既存の migration ファイルは**変更しない**（チェックサム管理のため変更すると既存 DB が `VersionMismatch` で開けなくなる）。後発のテーブル・列はすべて `migrate()` の runtime DDL で追加する | `crates/core/src/db/mod.rs:62-64`, `:233-236`, `:270-273` / `docs/database.md:297-301` |
| 列の追加 | `SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2` が 0 のときだけ `ALTER TABLE {table} ADD COLUMN {definition}`（`ensure_column`）| `crates/core/src/db/mod.rs:65-84` |
| 列の有無判定 | `has_column`（同じ `pragma_table_info` クエリ）| `crates/core/src/db/mod.rs:86-103` |
| PK 変更 | `ALTER TABLE` では不可能なため、旧テーブルを `RENAME TO *_old` → 新テーブル `CREATE` → `INSERT ... SELECT` → `DROP` | `crates/core/src/db/mod.rs:105-181` |
| `schema.sql` の位置づけ | スキーマの「正本」とされる参照用ファイル。**アプリのコードパスからは実行されない**（`include_str!` 等の参照なし。`grep -rn "schema.sql"` のヒットは `db/mod.rs:200` のコメントと `docs/database.md:4` のみ） | `crates/core/src/db/schema.sql` / `docs/database.md:3-7` |
| シード行 | `sites` の `techbookfest` / `booth` は `0001_init.sql` と `schema.sql` の `INSERT OR IGNORE`。`fanza` / `dlsite` は `migrate()` が `INSERT OR IGNORE` で追加 | `crates/core/migrations/0001_init.sql:19-23`, `crates/core/src/db/schema.sql:19-23`, `crates/core/src/db/mod.rs:382-392` |

### 1.2 `crates/core/src/db/schema.sql`（全文・282 行、verbatim）

テーブル定義位置: sites(:3) / app_settings(:25) / books(:32) / tbf_events(:55) / bookshelf_items(:76) / reading_progress(:108) / checked_items(:119) / book_contents(:138) / content_formats(:150) / imported_documents(:163) / document_images(:175) / page_views(:192) / document_text(:204) / token_analysis(:213) / book_tags(:226) / zenn_tag_metadata(:235) / favorite_tags(:243) / favorite_entities(:249) / book_first_events(:256) / product_sample_pages(:266)。

```sql
-- TBF Cabinet SQLite Schema

CREATE TABLE IF NOT EXISTS sites (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  url TEXT NOT NULL,
  display_order INTEGER NOT NULL DEFAULT 0,
  is_visible INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(id)) > 0),
  CHECK (length(trim(name)) > 0),
  CHECK (length(trim(url)) > 0)
);

CREATE INDEX IF NOT EXISTS sites_display_order_idx ON sites(display_order);
CREATE INDEX IF NOT EXISTS sites_is_visible_idx ON sites(is_visible);

INSERT OR IGNORE INTO sites (id, name, url, display_order, is_visible)
VALUES ('techbookfest', '技術書典', 'https://techbookfest.org', 0, 1);

INSERT OR IGNORE INTO sites (id, name, url, display_order, is_visible)
VALUES ('booth', 'BOOTH', 'https://booth.pm', 1, 1);

CREATE TABLE IF NOT EXISTS app_settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS books (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  author TEXT NOT NULL DEFAULT '',
  circle_name TEXT NOT NULL DEFAULT '',
  purchase_date TEXT,
  file_name TEXT NOT NULL,
  file_size INTEGER NOT NULL,
  opfs_path TEXT NOT NULL UNIQUE,
  cover_thumbnail TEXT,
  tbf_product_id TEXT,
  site_id TEXT REFERENCES sites(id),
  tags_fetched INTEGER NOT NULL DEFAULT 1,
  pack_id TEXT,
  is_favorite INTEGER NOT NULL DEFAULT 0,
  is_hidden INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  owner_sub TEXT,
  page_turn TEXT
);

CREATE INDEX IF NOT EXISTS books_site_id_idx ON books(site_id);

CREATE TABLE IF NOT EXISTS tbf_events (
  id TEXT PRIMARY KEY,
  site_id TEXT NOT NULL REFERENCES sites(id),
  slug TEXT,
  tbf_event_id TEXT,
  event_name TEXT NOT NULL,
  event_date TEXT,
  event_start_date TEXT,
  event_end_date TEXT,
  event_format TEXT NOT NULL DEFAULT 'offline',
  is_cancelled INTEGER NOT NULL DEFAULT 0,
  display_order INTEGER NOT NULL DEFAULT 0,
  is_featured INTEGER NOT NULL DEFAULT 0,
  poll_sync_enabled INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(site_id, slug)
);

CREATE INDEX IF NOT EXISTS tbf_events_site_id_idx ON tbf_events(site_id);

CREATE TABLE IF NOT EXISTS bookshelf_items (
  site_id TEXT NOT NULL REFERENCES sites(id),
  database_id TEXT NOT NULL,
  title TEXT NOT NULL,
  circle_name TEXT NOT NULL DEFAULT '',
  author TEXT NOT NULL DEFAULT '',
  thumbnail_url TEXT,
  format TEXT NOT NULL DEFAULT '',
  causedAt TEXT,
  event_name TEXT,
  event_slug TEXT,
  event_id TEXT REFERENCES tbf_events(id),
  file_name TEXT,
  download_url TEXT,
  is_downloadable INTEGER NOT NULL DEFAULT 0,
  is_checked INTEGER NOT NULL DEFAULT 0,
  is_purchased INTEGER NOT NULL DEFAULT 0,
  is_new INTEGER NOT NULL DEFAULT 0,
  is_active INTEGER NOT NULL DEFAULT 1,
  is_favorite INTEGER NOT NULL DEFAULT 0,
  is_hidden INTEGER NOT NULL DEFAULT 0,
  hidden_at TEXT,
  tags_json TEXT,
  synced_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (site_id, database_id)
);

CREATE INDEX IF NOT EXISTS bookshelf_items_site_id_idx ON bookshelf_items(site_id);
CREATE INDEX IF NOT EXISTS bookshelf_items_event_id_idx ON bookshelf_items(event_id);

CREATE TABLE IF NOT EXISTS reading_progress (
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  content_id TEXT NOT NULL DEFAULT '',
  current_page INTEGER NOT NULL DEFAULT 0,
  total_pages INTEGER,
  finished_at TEXT,
  last_read_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  scroll_position REAL NOT NULL DEFAULT 0,
  PRIMARY KEY (book_id, content_id)
);

CREATE TABLE IF NOT EXISTS checked_items (
  id TEXT PRIMARY KEY,
  event_id TEXT NOT NULL REFERENCES tbf_events(id),
  circle_name TEXT NOT NULL,
  space_number TEXT NOT NULL DEFAULT '',
  memo TEXT NOT NULL DEFAULT '',
  is_checked INTEGER NOT NULL DEFAULT 0,
  sort_order INTEGER NOT NULL DEFAULT 0,
  tbf_circle_id TEXT,
  product_id TEXT,
  product_title TEXT NOT NULL DEFAULT '',
  thumbnail_url TEXT,
  thumbnail_data TEXT,
  price INTEGER,
  is_purchased INTEGER NOT NULL DEFAULT 0,
  sample_fetch_attempted_at TEXT,
  createdAt TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS book_contents (
  content_id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  display_name TEXT NOT NULL,
  media_kind TEXT NOT NULL,
  is_primary INTEGER NOT NULL DEFAULT 0,
  sort_order INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_book_contents_book ON book_contents(book_id);

CREATE TABLE IF NOT EXISTS content_formats (
  format_id TEXT PRIMARY KEY,
  content_id TEXT NOT NULL REFERENCES book_contents(content_id) ON DELETE CASCADE,
  label TEXT NOT NULL,
  format_kind TEXT NOT NULL,
  page_count INTEGER NOT NULL DEFAULT 0,
  pack_entry_prefix TEXT,
  sort_order INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_content_formats_content ON content_formats(content_id);

CREATE TABLE IF NOT EXISTS imported_documents (
  id TEXT PRIMARY KEY,
  book_id TEXT REFERENCES books(id),
  source_type TEXT NOT NULL,
  file_hash TEXT NOT NULL,
  total_pages INTEGER NOT NULL,
  metadata TEXT,
  status TEXT NOT NULL DEFAULT 'pending',
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS document_images (
  id TEXT PRIMARY KEY,
  document_id TEXT NOT NULL REFERENCES imported_documents(id),
  content_id TEXT REFERENCES book_contents(content_id),
  format_id TEXT REFERENCES content_formats(format_id),
  page_number INTEGER NOT NULL,
  image_type TEXT NOT NULL,
  opfs_path TEXT NOT NULL,
  width INTEGER NOT NULL,
  height INTEGER NOT NULL,
  mime_type TEXT NOT NULL,
  file_size INTEGER NOT NULL,
  extracted_text TEXT,
  pack_entry_path TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS page_views (
  book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,
  content_id TEXT NOT NULL DEFAULT '',
  page_number INTEGER NOT NULL,
  view_count INTEGER NOT NULL DEFAULT 0,
  total_seconds REAL NOT NULL DEFAULT 0,
  last_viewed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (book_id, content_id, page_number)
);

CREATE INDEX IF NOT EXISTS idx_page_views_book ON page_views(book_id);

CREATE TABLE IF NOT EXISTS document_text (
  id TEXT PRIMARY KEY,
  document_id TEXT NOT NULL REFERENCES imported_documents(id),
  content_id TEXT NOT NULL DEFAULT '',
  page_number INTEGER NOT NULL,
  text_content TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS token_analysis (
  id TEXT PRIMARY KEY,
  document_id TEXT NOT NULL REFERENCES imported_documents(id),
  content_id TEXT NOT NULL DEFAULT '',
  page_number INTEGER NOT NULL,
  token TEXT NOT NULL,
  pos TEXT NOT NULL,
  base_form TEXT,
  reading TEXT,
  frequency INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS book_tags (
  id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL REFERENCES books(id),
  tag_name TEXT NOT NULL,
  source TEXT NOT NULL,
  confidence REAL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS zenn_tag_metadata (
  tag_name TEXT PRIMARY KEY,
  description TEXT,
  article_count INTEGER,
  follower_count INTEGER,
  synced_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS favorite_tags (
  tag_name TEXT PRIMARY KEY,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

-- サークル / 作者のお気に入り（チップのハート）。種別ごとに独立。
CREATE TABLE IF NOT EXISTS favorite_entities (
  entity_kind TEXT NOT NULL,
  entity_name TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (entity_kind, entity_name)
);

CREATE TABLE IF NOT EXISTS book_first_events (
  site_id TEXT NOT NULL,
  database_id TEXT NOT NULL,
  first_event_name TEXT,
  first_event_slug TEXT,
  fetched_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (site_id, database_id),
  FOREIGN KEY (site_id, database_id) REFERENCES bookshelf_items(site_id, database_id)
);

CREATE TABLE IF NOT EXISTS product_sample_pages (
  id TEXT PRIMARY KEY,
  checklist_item_id TEXT NOT NULL REFERENCES checked_items(id),
  product_id TEXT,
  page_number INTEGER NOT NULL,
  image_url TEXT,
  image_data TEXT,
  mime_type TEXT NOT NULL DEFAULT 'image/jpeg',
  width INTEGER,
  height INTEGER,
  file_size INTEGER,
  pack_id TEXT,
  pack_entry_path TEXT,
  fetched_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);```

### 1.3 `crates/core/migrations/0001_init.sql`（全文・238 行、verbatim）

**実行時に最初に適用される DDL。** `schema.sql` の部分集合 + `drive_sync_state` で、`book_contents` / `content_formats` / `page_views` / `favorite_entities` を含まない（それらは §1.4 の runtime DDL で追加される）。

```sql
-- TBF Cabinet SQLite Schema

CREATE TABLE IF NOT EXISTS sites (
  id TEXT PRIMARY KEY,
  name TEXT NOT NULL,
  url TEXT NOT NULL,
  display_order INTEGER NOT NULL DEFAULT 0,
  is_visible INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  CHECK (length(trim(id)) > 0),
  CHECK (length(trim(name)) > 0),
  CHECK (length(trim(url)) > 0)
);

CREATE INDEX IF NOT EXISTS sites_display_order_idx ON sites(display_order);
CREATE INDEX IF NOT EXISTS sites_is_visible_idx ON sites(is_visible);

INSERT OR IGNORE INTO sites (id, name, url, display_order, is_visible)
VALUES ('techbookfest', '技術書典', 'https://techbookfest.org', 0, 1);

INSERT OR IGNORE INTO sites (id, name, url, display_order, is_visible)
VALUES ('booth', 'BOOTH', 'https://booth.pm', 1, 1);

CREATE TABLE IF NOT EXISTS app_settings (
  key TEXT PRIMARY KEY,
  value TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS books (
  id TEXT PRIMARY KEY,
  title TEXT NOT NULL,
  author TEXT NOT NULL DEFAULT '',
  circle_name TEXT NOT NULL DEFAULT '',
  purchase_date TEXT,
  file_name TEXT NOT NULL,
  file_size INTEGER NOT NULL,
  opfs_path TEXT NOT NULL UNIQUE,
  cover_thumbnail TEXT,
  tbf_product_id TEXT,
  site_id TEXT REFERENCES sites(id),
  tags_fetched INTEGER NOT NULL DEFAULT 1,
  pack_id TEXT,
  is_favorite INTEGER NOT NULL DEFAULT 0,
  is_hidden INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS books_site_id_idx ON books(site_id);
CREATE UNIQUE INDEX IF NOT EXISTS books_site_tbf_product_id_unique
  ON books(site_id, tbf_product_id)
  WHERE site_id IS NOT NULL AND tbf_product_id IS NOT NULL;

CREATE TABLE IF NOT EXISTS tbf_events (
  id TEXT PRIMARY KEY,
  site_id TEXT NOT NULL REFERENCES sites(id),
  slug TEXT,
  tbf_event_id TEXT,
  event_name TEXT NOT NULL,
  event_date TEXT,
  event_start_date TEXT,
  event_end_date TEXT,
  event_format TEXT NOT NULL DEFAULT 'offline',
  is_cancelled INTEGER NOT NULL DEFAULT 0,
  display_order INTEGER NOT NULL DEFAULT 0,
  is_featured INTEGER NOT NULL DEFAULT 0,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  UNIQUE(site_id, slug)
);

CREATE INDEX IF NOT EXISTS tbf_events_site_id_idx ON tbf_events(site_id);

CREATE TABLE IF NOT EXISTS bookshelf_items (
  site_id TEXT NOT NULL REFERENCES sites(id),
  database_id TEXT NOT NULL,
  title TEXT NOT NULL,
  circle_name TEXT NOT NULL DEFAULT '',
  thumbnail_url TEXT,
  format TEXT NOT NULL DEFAULT '',
  causedAt TEXT,
  event_name TEXT,
  event_slug TEXT,
  event_id TEXT REFERENCES tbf_events(id),
  file_name TEXT,
  download_url TEXT,
  is_downloadable INTEGER NOT NULL DEFAULT 0,
  is_checked INTEGER NOT NULL DEFAULT 0,
  is_purchased INTEGER NOT NULL DEFAULT 0,
  is_new INTEGER NOT NULL DEFAULT 0,
  is_active INTEGER NOT NULL DEFAULT 1,
  is_favorite INTEGER NOT NULL DEFAULT 0,
  is_hidden INTEGER NOT NULL DEFAULT 0,
  tags_json TEXT,
  synced_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (site_id, database_id)
);

CREATE INDEX IF NOT EXISTS bookshelf_items_site_id_idx ON bookshelf_items(site_id);
CREATE INDEX IF NOT EXISTS bookshelf_items_event_id_idx ON bookshelf_items(event_id);

CREATE TABLE IF NOT EXISTS reading_progress (
  book_id TEXT PRIMARY KEY REFERENCES books(id),
  current_page INTEGER NOT NULL DEFAULT 0,
  total_pages INTEGER,
  finished_at TEXT,
  last_read_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  scroll_position REAL NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS checked_items (
  id TEXT PRIMARY KEY,
  event_id TEXT NOT NULL REFERENCES tbf_events(id),
  circle_name TEXT NOT NULL,
  space_number TEXT NOT NULL DEFAULT '',
  memo TEXT NOT NULL DEFAULT '',
  is_checked INTEGER NOT NULL DEFAULT 0,
  sort_order INTEGER NOT NULL DEFAULT 0,
  tbf_circle_id TEXT,
  product_id TEXT,
  product_title TEXT NOT NULL DEFAULT '',
  thumbnail_url TEXT,
  thumbnail_data TEXT,
  price INTEGER,
  is_purchased INTEGER NOT NULL DEFAULT 0,
  sample_fetch_attempted_at TEXT,
  createdAt TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS imported_documents (
  id TEXT PRIMARY KEY,
  book_id TEXT REFERENCES books(id),
  source_type TEXT NOT NULL,
  file_hash TEXT NOT NULL,
  total_pages INTEGER NOT NULL,
  metadata TEXT,
  status TEXT NOT NULL DEFAULT 'pending',
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS document_images (
  id TEXT PRIMARY KEY,
  document_id TEXT NOT NULL REFERENCES imported_documents(id),
  page_number INTEGER NOT NULL,
  image_type TEXT NOT NULL,
  opfs_path TEXT NOT NULL,
  width INTEGER NOT NULL,
  height INTEGER NOT NULL,
  mime_type TEXT NOT NULL,
  file_size INTEGER NOT NULL,
  extracted_text TEXT,
  pack_entry_path TEXT,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS document_text (
  id TEXT PRIMARY KEY,
  document_id TEXT NOT NULL REFERENCES imported_documents(id),
  page_number INTEGER NOT NULL,
  text_content TEXT NOT NULL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS token_analysis (
  id TEXT PRIMARY KEY,
  document_id TEXT NOT NULL REFERENCES imported_documents(id),
  page_number INTEGER NOT NULL,
  token TEXT NOT NULL,
  pos TEXT NOT NULL,
  base_form TEXT,
  reading TEXT,
  frequency INTEGER NOT NULL DEFAULT 1,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS book_tags (
  id TEXT PRIMARY KEY,
  book_id TEXT NOT NULL REFERENCES books(id),
  tag_name TEXT NOT NULL,
  source TEXT NOT NULL,
  confidence REAL,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS zenn_tag_metadata (
  tag_name TEXT PRIMARY KEY,
  description TEXT,
  article_count INTEGER,
  follower_count INTEGER,
  synced_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS favorite_tags (
  tag_name TEXT PRIMARY KEY,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS book_first_events (
  site_id TEXT NOT NULL,
  database_id TEXT NOT NULL,
  first_event_name TEXT,
  first_event_slug TEXT,
  fetched_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  PRIMARY KEY (site_id, database_id),
  FOREIGN KEY (site_id, database_id) REFERENCES bookshelf_items(site_id, database_id)
);

CREATE TABLE IF NOT EXISTS product_sample_pages (
  id TEXT PRIMARY KEY,
  checklist_item_id TEXT NOT NULL REFERENCES checked_items(id),
  product_id TEXT,
  page_number INTEGER NOT NULL,
  image_url TEXT,
  image_data TEXT,
  mime_type TEXT NOT NULL DEFAULT 'image/jpeg',
  width INTEGER,
  height INTEGER,
  file_size INTEGER,
  pack_id TEXT,
  pack_entry_path TEXT,
  fetched_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
  created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
-- Desktop-only tables (appended to the verbatim Web schema).

CREATE TABLE IF NOT EXISTS drive_sync_state (
  pack_id TEXT PRIMARY KEY,
  drive_file_id TEXT NOT NULL,
  md5 TEXT NOT NULL,
  modified_time TEXT,
  last_synced_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);```

### 1.4 `migrate()` が実行する追加 DDL（毎回・冪等）

`CREATE TABLE` / `ALTER TABLE` / index の**正規化引用**（ソースは Rust の文字列リテラルで、連続空白は元コードでは整形用。内容は同一）。

| アンカー | 文 |
|---|---|
| `crates/core/src/db/mod.rs:115` | `CREATE TABLE reading_progress ( book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE, content_id TEXT NOT NULL DEFAULT '', current_page INTEGER NOT NULL DEFAULT 0, total_pages INTEGER, finished_at TEXT, last_read_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, scroll_position REAL NOT NULL DEFAULT 0, PRIMARY KEY (book_id, content_id) )` || `crates/core/src/db/mod.rs:142` | `CREATE TABLE page_views ( book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE, content_id TEXT NOT NULL DEFAULT '', page_number INTEGER NOT NULL, view_count INTEGER NOT NULL DEFAULT 0, total_seconds REAL NOT NULL DEFAULT 0, last_viewed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY (book_id, content_id, page_number) )` || `crates/core/src/db/mod.rs:209` | `ALTER TABLE bookshelf_items ADD COLUMN hidden_at TEXT` || `crates/core/src/db/mod.rs:226` | `ALTER TABLE bookshelf_items ADD COLUMN author TEXT NOT NULL DEFAULT ''` || `crates/core/src/db/mod.rs:242` | `ALTER TABLE books ADD COLUMN owner_sub TEXT` || `crates/core/src/db/mod.rs:250` | `DROP INDEX IF EXISTS books_site_tbf_product_id_unique` || `crates/core/src/db/mod.rs:265` | `ALTER TABLE tbf_events ADD COLUMN poll_sync_enabled INTEGER NOT NULL DEFAULT 0` || `crates/core/src/db/mod.rs:274` | `CREATE TABLE IF NOT EXISTS view_history ( id TEXT PRIMARY KEY, book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE, started_at TEXT NOT NULL, ended_at TEXT ); CREATE INDEX IF NOT EXISTS idx_view_history_book ON view_history(book_id)` || `crates/core/src/db/mod.rs:282` | `CREATE TABLE IF NOT EXISTS page_views ( book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE, content_id TEXT NOT NULL DEFAULT '', page_number INTEGER NOT NULL, view_count INTEGER NOT NULL DEFAULT 0, total_seconds REAL NOT NULL DEFAULT 0, last_viewed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY (book_id, content_id, page_number) ); CREATE INDEX IF NOT EXISTS idx_page_views_book ON page_views(book_id)` || `crates/core/src/db/mod.rs:291` | `CREATE TABLE IF NOT EXISTS page_notes ( id TEXT PRIMARY KEY, book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE, content_id TEXT NOT NULL DEFAULT '', page INTEGER NOT NULL, memo TEXT NOT NULL DEFAULT '', spread_side TEXT, is_active INTEGER NOT NULL DEFAULT 1, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, UNIQUE (book_id, content_id, page) ); CREATE INDEX IF NOT EXISTS idx_page_notes_book ON page_notes(book_id)` || `crates/core/src/db/mod.rs:305` | `ALTER TABLE page_notes ADD COLUMN is_active INTEGER NOT NULL DEFAULT 1` || `crates/core/src/db/mod.rs:315` | `CREATE TABLE IF NOT EXISTS favorite_entities ( entity_kind TEXT NOT NULL, entity_name TEXT NOT NULL, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP, PRIMARY KEY (entity_kind, entity_name) )` || `crates/core/src/db/mod.rs:324` | `CREATE TABLE IF NOT EXISTS book_contents ( content_id TEXT PRIMARY KEY, book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE, display_name TEXT NOT NULL, media_kind TEXT NOT NULL, is_primary INTEGER NOT NULL DEFAULT 0, sort_order INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP ); CREATE INDEX IF NOT EXISTS idx_book_contents_book ON book_contents(book_id)` || `crates/core/src/db/mod.rs:329` | `CREATE TABLE IF NOT EXISTS content_formats ( format_id TEXT PRIMARY KEY, content_id TEXT NOT NULL REFERENCES book_contents(content_id) ON DELETE CASCADE, label TEXT NOT NULL, format_kind TEXT NOT NULL, page_count INTEGER NOT NULL DEFAULT 0, pack_entry_prefix TEXT, sort_order INTEGER NOT NULL DEFAULT 0, created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP ); CREATE INDEX IF NOT EXISTS idx_content_formats_content ON content_formats(content_id)` |
補足（コード上の文とその意味）:

- `crates/core/src/db/mod.rs:105-181` `migrate_progress_content_id`: `reading_progress` / `page_views` を `content_id` 込みの複合 PK へ作り替える。既存行の `content_id` は「その本の優先コンテンツ」（`book_contents` を `ORDER BY is_primary DESC, sort_order, content_id LIMIT 1`）で、無ければ `''`。`document_text` / `token_analysis` へは `content_id TEXT NOT NULL DEFAULT ''` を `ensure_column` で追加（列追加のみ）。
- `crates/core/src/db/mod.rs:233-250`: `books` に `owner_sub TEXT` を追加し、`books_site_tbf_product_id_unique` UNIQUE インデックスを**毎回** `DROP INDEX IF EXISTS`（複数アカウントで同一 source を owner ごとに持てるようにするため）。
- `crates/core/src/db/mod.rs:382-392`: `sites` に `fanza`（`FANZA同人`, `https://www.dmm.co.jp/dc/doujin/`, display_order=2）と `dlsite`（`DLsite`, `https://www.dlsite.com/`, display_order=3）を `INSERT OR IGNORE`。
- `crates/core/src/db/mod.rs:355-380`: 共有ソースメタ列を `bookshelf_items` と `books` の両方へ `ensure_column` で追加。列定義の一覧は次の 10 個（`ensure_column` に渡す `definition` 文字列そのまま）: `media_category TEXT` / `ai_type TEXT` / `is_drm INTEGER NOT NULL DEFAULT 0` / `release_date TEXT` / `description TEXT` / `theme TEXT` / `maker_id TEXT` / `page_count INTEGER` / `age_rating TEXT` / `series_name TEXT`。
- `crates/core/src/db/mod.rs:271-274`: `books` に `page_turn TEXT` を `ensure_column` で追加（本ごとの綴じ方向。リリース前のためマイグレーションファイルは増やしていない）。既存 DB で列が足されることはテスト `legacy_books_get_the_page_turn_column` が固定する（`crates/core/tests/db.rs:112-183`）。

### 1.5 `schema.sql` と `0001_init.sql` の差分（再実装時に効く）

| 対象 | `0001_init.sql` | `schema.sql`（= 最終形） | 差分を作る runtime DDL |
|---|---|---|---|
| `books` | `owner_sub` なし | `owner_sub TEXT` あり | `db/mod.rs:242` |
| `books` | `page_turn` なし | `page_turn TEXT` あり（`NULL` = サイト別設定 `viewer.page_turn.{site}` に従う） | `db/mod.rs:274`（`ensure_column`） |
| `books` の UNIQUE | `books_site_tbf_product_id_unique`（部分 UNIQUE インデックス）あり | なし | `db/mod.rs:250` の `DROP INDEX IF EXISTS` |
| `bookshelf_items` | `author` / `hidden_at` / `poll_sync_enabled`(誤記注: `poll_sync_enabled` は `tbf_events`) なし、共有ソースメタ 10 列なし | あり | `db/mod.rs:209`（`hidden_at`）, `:226`（`author`）, `:355-380`（10 列） |
| `tbf_events` | `poll_sync_enabled` なし | `poll_sync_enabled INTEGER NOT NULL DEFAULT 0` あり | `db/mod.rs:265` |
| `reading_progress` | `PRIMARY KEY (book_id)` | `PRIMARY KEY (book_id, content_id)` | `db/mod.rs:105-133`（テーブル作り替え） |
| `page_views` | テーブル自体が無い | `PRIMARY KEY (book_id, content_id, page_number)` | `db/mod.rs:282`（`IF NOT EXISTS`）+ `:105`（PK 作り替え） |
| `book_contents` / `content_formats` | 無い | あり | `db/mod.rs:324`, `:329` |
| `document_images` | `content_id` / `format_id` 無し | 両方あり（FK 付き） | `db/mod.rs:335-347`（`ensure_column` で FK 付き列として追加） |
| `favorite_entities` | 無い | あり | `db/mod.rs:315` |
| `document_text` / `token_analysis` | `content_id` 無し | `content_id TEXT NOT NULL DEFAULT ''` | `db/mod.rs:167-177` |
| `drive_sync_state` | 末尾にあり（`0001_init.sql:230-238`） | `desktop.sql` に分離（同一内容） | — |
| `page_notes` / `view_history` | 無い | **`schema.sql` にも無い**（runtime DDL のみが正） | `db/mod.rs:274`（`view_history`）, `:291`（`page_notes`） |

### 1.6 テーブル一覧（用途・PK・FK・ON DELETE）

| テーブル | 用途（コメント/ドキュメント由来） | PK | 外部キー / ON DELETE | 定義 |
|---|---|---|---|---|
| `sites` | 技術書典 / BOOTH / FANZA同人 / DLsite のサイト情報 | `id` | — | `schema.sql:3-17` |
| `app_settings` | key-value 設定（ビューアモード・サイトフィルタ・Drive 同期等） | `key` | — | `schema.sql:25-30` |
| `books` | ローカルに取り込んだ本（pack と紐づく） | `id` | `site_id → sites(id)`（ON DELETE なし） | `schema.sql:32-53` |
| `tbf_events` | 技術書典イベント（チェックリスト・イベント名補完） | `id` | `site_id → sites(id)` | `schema.sql:55-74` |
| `bookshelf_items` | 本棚（同期スナップショット） | `(site_id, database_id)` | `site_id → sites(id)`, `event_id → tbf_events(id)` | `schema.sql:76-106` |
| `reading_progress` | 読書位置（コンテンツ単位） | `(book_id, content_id)` | `book_id → books(id) ON DELETE CASCADE` | `schema.sql:108-117` |
| `checked_items` | チェックリスト項目 | `id` | `event_id → tbf_events(id)` | `schema.sql:119-136` |
| `book_contents` | 読む単位（本文・別冊等） | `content_id` | `book_id → books(id) ON DELETE CASCADE` | `schema.sql:138-148` |
| `content_formats` | レンディション（PDF版/画像版等） | `format_id` | `content_id → book_contents(content_id) ON DELETE CASCADE` | `schema.sql:150-161` |
| `imported_documents` | 取り込みドキュメント | `id` | `book_id → books(id)`（ON DELETE なし） | `schema.sql:163-173` |
| `document_images` | ページ / サムネイル画像行 | `id` | `document_id → imported_documents(id)`, `content_id → book_contents(content_id)`, `format_id → content_formats(format_id)` | `schema.sql:175-190` |
| `page_views` | ページ毎の表示回数・滞在秒数 | `(book_id, content_id, page_number)` | `book_id → books(id) ON DELETE CASCADE` | `schema.sql:192-202` |
| `document_text` | ページテキスト | `id` | `document_id → imported_documents(id)` | `schema.sql:204-211` |
| `token_analysis` | 形態素（名詞）頻度 | `id` | `document_id → imported_documents(id)` | `schema.sql:213-224` |
| `book_tags` | 本のタグ（`source` = generated/manual 等） | `id` | `book_id → books(id)` | `schema.sql:226-233` |
| `zenn_tag_metadata` | Zenn タグのメタ | `tag_name` | — | `schema.sql:235-241` |
| `favorite_tags` | お気に入りタグ | `tag_name` | — | `schema.sql:243-247` |
| `favorite_entities` | サークル / 作者のお気に入り（種別ごとに独立） | `(entity_kind, entity_name)` | — | `schema.sql:249-254` |
| `book_first_events` | 本の初出イベント | `(site_id, database_id)` | 複合 FK `(site_id, database_id) → bookshelf_items(site_id, database_id)` | `schema.sql:256-264` |
| `product_sample_pages` | 試し読み画像 | `id` | `checklist_item_id → checked_items(id)` | `schema.sql:266-281` |
| `drive_sync_state` | Drive 同期の記帳 | `pack_id` | — | `crates/core/src/db/desktop.sql:3-9` / `0001_init.sql:230-238` |
| `view_history` | 閲覧セッション（開始/終了時刻） | `id` | `book_id → books(id) ON DELETE CASCADE` | `crates/core/src/db/mod.rs:274` |
| `page_notes` | 付箋（1 ページ 1 件） | `id` + `UNIQUE(book_id, content_id, page)` | `book_id → books(id) ON DELETE CASCADE` | `crates/core/src/db/mod.rs:291` |

ON DELETE の注記（事実）: `books` の子のうち `imported_documents` / `document_images` / `document_text` / `token_analysis` / `book_tags` / `book_contents` / `content_formats` には `ON DELETE CASCADE` が**無い**（`crates/core/src/db/books.rs:356-363` のコメント）。そのため `books::delete` は 孫→子→親 の順に明示 DELETE する（`books.rs:365-389`）。

### 1.7 インデックス一覧

| インデックス | 定義場所 |
|---|---|
| `sites_display_order_idx`(display_order), `sites_is_visible_idx`(is_visible) | `schema.sql:16-17` |
| `books_site_id_idx`(site_id) | `schema.sql:53` |
| `books_site_tbf_product_id_unique`（部分 UNIQUE。**常に DROP される**） | `0001_init.sql:53-55` → `db/mod.rs:250` |
| `tbf_events_site_id_idx`(site_id) | `schema.sql:74` |
| `bookshelf_items_site_id_idx`(site_id), `bookshelf_items_event_id_idx`(event_id) | `schema.sql:105-106` |
| `idx_book_contents_book`(book_id) | `schema.sql:148` / `db/mod.rs:324` |
| `idx_content_formats_content`(content_id) | `schema.sql:161` / `db/mod.rs:329` |
| `idx_page_views_book`(book_id) | `schema.sql:202` / `db/mod.rs:282` |
| `idx_view_history_book`(book_id) | `db/mod.rs:274` |
| `idx_page_notes_book`(book_id) | `db/mod.rs:291` |

### 1.8 CHECK 制約

`sites` のみ（`schema.sql:12-14`）: `CHECK (length(trim(id)) > 0)` / `CHECK (length(trim(name)) > 0)` / `CHECK (length(trim(url)) > 0)`。他テーブルに CHECK は無い。

### 1.9 接続・実行方式

| 項目 | 値 / 内容 | アンカー |
|---|---|---|
| ブリッジ | プロセス共有の tokio マルチスレッドランタイム（`worker_threads(2)`）に `block_on`。リポジトリ API は同期 | `crates/core/src/db/mod.rs:31-46` |
| 接続オプション | `foreign_keys(true)`, `journal_mode(Wal)`, `busy_timeout(5s)`, `create_if_missing(true)` | `crates/core/src/db/mod.rs:50-60` |
| テスト用プール | `:memory:` + `foreign_keys(true)` + `max_connections(1)`（同一メモリ共有のため）+ マイグレーション適用 | `crates/core/src/db/mod.rs:461-477` |
| DB ファイル | `<data_dir>/thundoku-shelf.db`（`data_dir` 既定 = `dirs::data_dir()/thundoku-shelf`） | `crates/app/src/app_state.rs:129-141` |
| packs ディレクトリ | `<data_dir>/packs`（`AppState::packs_dir`） | `crates/app/src/app_state.rs:139`, `:142` |
| サムネイルキャッシュ | `<data_dir>/thumbnails`（`{site_id}_{database_id}_448.png` 等。サイト表紙用で、pack 内 `thumbnail.webp` とは別物） | `crates/app/src/app_state.rs:149`, `crates/app/src/views/bookshelf.rs:7123-7129` |

### 1.10 データ移行・初回クリア

| 処理 | 内容 | アンカー |
|---|---|---|
| 進捗のコンテンツ単位化 | §1.4 参照。何度実行しても安全（`content_id` 列の有無で判定） | `crates/core/src/db/mod.rs:105-181` |
| 同期ラッパー | `run_progress_content_migration(pool) -> Result<u64>`（移行件数） | `crates/core/src/db/mod.rs:183-189` |
| 旧ラベルの書き換え | `content_formats.label` が旧値（`pdf`/`epub` で `PDF`/`EPUB` 以外、`image` で `画像`）の行だけ更新。画像は本のファイル名の拡張子 → `JPEG`/`PNG`/… へ、無ければ `JPEG` | `crates/core/src/db/contents.rs:214-282` |
| 初回クリア | `app_settings['owner_sub_model.initialized']` が無い初回起動時のみ、FK 安全順（子→親）で 16 テーブルを `DELETE` し、`packs` / `thumbnails` ディレクトリを作り直し、`drive.last_sync_at` / `drive.sync.enabled` / `drive.sync.folder_id` を削除してからフラグを立てる。2 回目以降は `false` を返して何もしない | `crates/core/src/db/mod.rs:403-458` |
| クリア対象テーブル（順序そのまま） | `product_sample_pages` → `token_analysis` → `document_text` → `document_images` → `content_formats` → `book_contents` → `imported_documents` → `book_tags` → `reading_progress` → `view_history` → `page_views` → `book_first_events` → `checked_items` → `bookshelf_items` → `books` → `drive_sync_state`（`sites` / `tbf_events` は残す） | `crates/core/src/db/mod.rs:419-441` |
| バックアップ対象テーブル（順序 = FK 参照元が先） | `books`, `bookshelf_items`, `checked_items`, `tbf_events`, `book_contents`, `content_formats`, `reading_progress`, `page_views`, `book_tags`, `favorite_tags`, `favorite_entities`, `imported_documents`, `document_images`, `book_first_events`, `zenn_tag_metadata`, `view_history`（16 件） | `crates/core/src/db/backup.rs:15-32` |
| バックアップから除外する列 | `thumbnail_data`, `image_data`（画像 base64 を含めない） | `crates/core/src/db/backup.rs:34` |
| バックアップ PK 対応表 | `pk_columns()` が上記 16 テーブルの PK 列を返す（`book_contents→content_id`, `content_formats→format_id`, `reading_progress→[book_id, content_id]`, `page_views→[book_id, content_id, page_number]`, `view_history→id` 等） | `crates/core/src/db/backup.rs:81-100` |

---

## 2. ID 規約

### 2.1 テーブル別の ID / 主キー生成規則

| テーブル | キー | 生成規則（事実） | アンカー |
|---|---|---|---|
| `sites` | `id` | 固定文字列 `techbookfest` / `booth`（SQL シード）、`fanza` / `dlsite`（runtime DDL） | `schema.sql:19-23`, `db/mod.rs:382-392` |
| `app_settings` | `key` | 呼び出し側が決める文字列（例 `owner_sub_model.initialized`, `drive.last_sync_at`, `drive.sync.enabled`, `drive.sync.folder_id`） | `db/settings.rs:7-40`, `db/mod.rs:449-455` |
| `books` | `id` | `book_id_for()`: ① `reuse_book_id` があればそれ ② 無ければ `Identity.pack_id` ③ 無ければ `Uuid::new_v4()` | `import/mod.rs:166-174` |
| `books` | `opfs_path` | `format!("{book_id}.opfspack")`（`UNIQUE`） | `import/mod.rs:937` |
| `books` | `pack_id` | 取り込み時は `Some(book_id)` | `import/mod.rs:944` |
| `books` | `owner_sub` | ローカル鍵で暗号化済みの sub（`None` = 未所属）。暗号化は `crate::owner` 側 | `db/books.rs:247-266` |
| `tbf_events` | `id` | 技術書典の slug（例 `tbf18`）。`tbf_event_id` は `format!("Event:{slug}")` | `core/src/tbf/mod.rs:339-340`, `:1143-1144` |
| `bookshelf_items` | `(site_id, database_id)` | `database_id` はサイト API の作品 ID（自前生成しない） | `db/bookshelf.rs:61-63` |
| `checked_items` | `id` | 技術書典 API の `entry.id` をそのまま使う | `core/src/tbf/sync.rs:182-184` |
| `book_contents` | `content_id` | `Uuid::new_v4()`（ZIP 取り込みは `commit_zip` 内で 1 コンテンツ 1 個、PDF/画像/EPUB 単体は `single_content`、復元は `infer_contents`） | `import/mod.rs:1508`, `:819`, `:1981` |
| `content_formats` | `format_id` | `Uuid::new_v4()`（1 レンディション 1 個） | `import/mod.rs:1511`, `:825`, `:1987` |
| `imported_documents` | `id` | `Uuid::new_v4()`（document 行は最後に作るが id は先に採番） | `import/mod.rs:980`, `:1771` |
| `imported_documents` | `file_hash` | 生成した pack バイト列の SHA-256（小文字 hex） | `import/mod.rs:983`, `:1857`, `:60-63` |
| `document_images` | `id` | `Uuid::new_v4()` | `import/mod.rs:1040`, `:1079`, `:2045` |
| `document_images` | `pack_entry_path` | pack 内エントリパス（`pages/page_0001.webp` 等） | `import/mod.rs:1045`, `documents.rs:21-39` |
| `document_text` | `id` | `Uuid::new_v4()`。**`content_id` は INSERT 文に含めない**ため常に既定値 `''` | `import/mod.rs:1105`, `db/documents.rs:240-258` |
| `token_analysis` | `id` | `Uuid::new_v4()`。`pos` は常に `"名詞"`、`base_form = token`、`reading = None`、`frequency = 出現回数`。`content_id` は INSERT に含めない（既定 `''`） | `import/mod.rs:1119-1128`, `db/documents.rs:338-370` |
| `book_tags` | `id` | `Uuid::new_v4()`。`source` は取り込み時 `"generated"` | `db/tags.rs:18-44`, `import/mod.rs:1129-1137` |
| `reading_progress` | `(book_id, content_id)` | `content_id` = 対象コンテンツ ID。コンテンツが無い本は `''` | `db/progress.rs:8`, `:57-64` |
| `page_views` | `(book_id, content_id, page_number)` | `content_id` = 対象コンテンツ（`''` = 未指定/旧データ） | `db/page_views.rs:10-13` |
| `view_history` | `id` | SQL 側で `hex(randomblob(16))`（32 桁 hex）。`INSERT ... RETURNING` で受け取る | `db/view_history.rs:23-33` |
| `page_notes` | `id` | `format!("note-{book_id}-{content_id}-{page}")`（**1-indexed のページ番号**）。同じページは常に同一 id で UPSERT される | `crates/app/src/views/reader.rs:598-605`, `crates/app/src/views/notes.rs:305`, `db/notes.rs:87-110` |
| `product_sample_pages` | `id` | `Uuid::new_v4()` | `crates/app/src/views/checklist.rs:550-556` |
| `drive_sync_state` | `pack_id` | pack ファイル名の stem（= `books.id`） | `db/sync_state.rs:26-45`, `core/src/drive/sync.rs:57-62` |
| `favorite_tags` | `tag_name` | タグ名そのもの | `db/tags.rs:83-99` |
| `favorite_entities` | `(entity_kind, entity_name)` | `entity_kind` は `"circle"` / `"author"` | `db/favorites.rs:17-25`, `:28-55` |
| `zenn_tag_metadata` | `tag_name` | Zenn のタグ名 | `schema.sql:235-241` |
| `book_first_events` | `(site_id, database_id)` | `bookshelf_items` と同じキー | `schema.sql:256-264` |

### 2.2 `content_id` の意味と空文字の扱い

| 事実 | アンカー |
|---|---|
| `book_contents.content_id` = **読む単位**（本文・別冊・おまけ等）の識別子。表紙・裏表紙はコンテンツにしない（決定 D4） | `db/contents.rs:1-8` / `docs/import-patterns.md:657-666` |
| `content_formats` = 同じ内容の**別形式・別バリアント**（`PDF版`/`画像版`、`文字あり`/`文字なし`、`MP3/WAV × SEあり/なし`）で、コンテンツ 1 : レンディション N | `db/contents.rs:1-8` / `docs/import-patterns.md:659`（D3） |
| `document_images.content_id` が NULL = フェーズ2以前の旧データ（単一コンテンツ扱い。`images_for_selection` は常に対象に含める） | `db/documents.rs:24-27`, `:153-158` |
| `reading_progress.content_id` / `page_views.content_id` / `page_notes.content_id` / `document_text.content_id` / `token_analysis.content_id` の `''` = 未指定（旧データ / 単一コンテンツ） | `db/progress.rs:8`, `db/page_views.rs:12`, `db/notes.rs:53`, `schema.sql:206`, `schema.sql:216` |
| 本の進捗を「既定表示コンテンツ」で読むときの解決順は `book_contents` の `ORDER BY is_primary DESC, sort_order, content_id LIMIT 1`。無ければ `''` の行を見る | `db/progress.rs:57-64`, `db/contents.rs:99-113` |
| パスからコンテンツを決める規則（pack 復元時）: `content_formats.pack_entry_prefix` がエントリパスの接頭辞であること | `import/mod.rs:1806-1818` |
| 旧ラベル移行は `format_kind`（`pdf`/`epub`/`image`/…）を見て `label` を上書きする（`content_id` は変えない） | `db/contents.rs:235-284` |

### 2.3 `book_id` / `pack_id` / ファイル名の関係

| 経路 | `books.id` | pack ファイル | アンカー |
|---|---|---|---|
| ローカル取り込み（ZIP/PDF/EPUB/画像） | `identity.pack_id`（ログイン時）または新規 UUID | `<data_dir>/packs/{book_id}.opfspack` | `import/mod.rs:166-174`, `:922-924` |
| 再取り込み（同一 source） | `reuse_book_id`（`resolve_reuse_id` が `(site_id, tbf_product_id)` + owner で既存行を探す） | 上書き | `db/books.rs:336-362`, `import/mod.rs:891-910` |
| Drive 復元 | ファイル名 stem = `pack_id` | `{pack_id}.opfspack` | `core/src/drive/sync.rs:57-62`, `:110-156` |
| 拡張子 | 定数 `PACK_EXTENSION = "opfspack"` | — | `core/src/drive/sync.rs:22` |
| 競合退避 | `{pack_id}.conflict-local.opfspack` | — | `core/src/drive/sync.rs:269` |

### 2.4 日時フォーマット

| 事実 | 値 | アンカー |
|---|---|---|
| 取り込み時の `created_at` / `updated_at` | `chrono::Utc::now().format("%Y-%m-%d %H:%M:%S")`（UTC、タイムゾーン表記なし） | `import/mod.rs:56-58` |
| DB 既定値 | `CURRENT_TIMESTAMP`（SQLite の UTC） | `schema.sql` 各テーブル |
| pack の `created_at` | `chrono::Utc::now().timestamp_millis() as u64`（UNIX ミリ秒。TS 実装のフィクスチャは `1_728_000_000_000`） | `import/mod.rs:916`, `crates/opfspack/tests/interop.rs:41-50` |

---
