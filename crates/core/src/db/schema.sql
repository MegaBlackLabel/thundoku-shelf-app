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
  owner_sub TEXT
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
