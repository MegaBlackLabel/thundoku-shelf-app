-- Desktop-only tables (appended to the verbatim Web schema).

CREATE TABLE IF NOT EXISTS drive_sync_state (
  pack_id TEXT PRIMARY KEY,
  drive_file_id TEXT NOT NULL,
  md5 TEXT NOT NULL,
  modified_time TEXT,
  last_synced_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);
