//! SQLite storage: connection handling, sqlx migrations and repository
//! layers (books / bookshelf / checklist / progress / tags / ...).
//!
//! The app talks to the DB synchronously (blocking) from GPUI background
//! tasks; sqlx is async-only, so every repository function wraps its work in
//! [`block_on`] against a process-wide tokio runtime.

pub mod backup;
pub mod books;
pub mod bookshelf;
pub mod checklist;
pub mod documents;
pub mod page_views;
pub mod progress;
pub mod samples;
pub mod settings;
pub mod sync_state;
pub mod tags;
pub mod view_history;

use std::path::Path;

use sqlx::SqliteConnection;
pub use sqlx::sqlite::SqlitePool;

/// Process-wide tokio runtime for bridging sqlx (async) and the synchronous
/// repository API.
static RUNTIME: std::sync::OnceLock<tokio::runtime::Runtime> = std::sync::OnceLock::new();

fn runtime() -> &'static tokio::runtime::Runtime {
    RUNTIME.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("failed to build tokio runtime")
    })
}

/// Run a sqlx future to completion on the process-wide tokio runtime.
pub fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    runtime().block_on(fut)
}

/// Open a pooled connection with foreign keys enabled and apply pending
/// migrations (`migrations/` directory, managed via `_sqlx_migrations`).
pub fn connect(path: &Path) -> Result<SqlitePool, sqlx::Error> {
    let options = sqlx::sqlite::SqliteConnectOptions::new()
        .filename(path)
        .foreign_keys(true)
        .journal_mode(sqlx::sqlite::SqliteJournalMode::Wal)
        .busy_timeout(std::time::Duration::from_secs(5))
        .create_if_missing(true);
    let pool = block_on(SqlitePool::connect_with(options))?;
    migrate(&pool)?;
    Ok(pool)
}

/// Apply pending migrations. Idempotent; tracked in the `_sqlx_migrations`
/// table (independent of the legacy `user_version` marker).
pub fn migrate(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    block_on(async {
        let mut conn = pool.acquire().await?;
        sqlx::migrate!("./migrations")
            .run(&mut conn)
            .await
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
        // 非表示日時カラム（プログラム的マイグレーション: SQLite の PRAGMA で
        // 存在確認してから ALTER TABLE する。新規 DB は schema.sql が含む）
        {
            let has_hidden_at: bool = sqlx::query_scalar(
                "SELECT COUNT(*) FROM pragma_table_info('bookshelf_items') \
                 WHERE name = 'hidden_at'",
            )
            .fetch_one(&mut *conn)
            .await?;
            if !has_hidden_at {
                sqlx::query("ALTER TABLE bookshelf_items ADD COLUMN hidden_at TEXT")
                    .execute(&mut *conn)
                    .await?;
            }
        }
        // チェックリストのポーリング有効フラグ（プログラム的マイグレーション。
        // hidden_at と同様に PRAGMA で存在確認してから ALTER TABLE する。
        // 既存の migration ファイルは checksum 管理されるため変更しない）
        {
            let has_poll_enabled: bool = sqlx::query_scalar(
                "SELECT COUNT(*) FROM pragma_table_info('tbf_events') \
                 WHERE name = 'poll_sync_enabled'",
            )
            .fetch_one(&mut *conn)
            .await?;
            if !has_poll_enabled {
                sqlx::query(
                    "ALTER TABLE tbf_events ADD COLUMN poll_sync_enabled INTEGER NOT NULL DEFAULT 0",
                )
                .execute(&mut *conn)
                .await?;
            }
        }
        // 閲覧履歴（プログラム的マイグレーション。既存の migration ファイルは
        // checksum 管理されるため変更せず、IF NOT EXISTS で冪等に適用する）
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS view_history (               id TEXT PRIMARY KEY,               book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,               started_at TEXT NOT NULL,               ended_at TEXT             );             CREATE INDEX IF NOT EXISTS idx_view_history_book ON view_history(book_id)",
        )
        .execute(&mut *conn)
        .await?;
        // ページ毎の閲覧記録（プログラム的マイグレーション。view_history と同様に
        // マイグレーションファイルは checksum 管理されるため変更せず、IF NOT EXISTS で
        // 冪等に適用する。1 冊 × 1 ページの累計表示回数・累計滞在秒数を持つ集計表）
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS page_views (               book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,               page_number INTEGER NOT NULL,               view_count INTEGER NOT NULL DEFAULT 0,               total_seconds REAL NOT NULL DEFAULT 0,               last_viewed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,               PRIMARY KEY (book_id, page_number)             );             CREATE INDEX IF NOT EXISTS idx_page_views_book ON page_views(book_id)",
        )
        .execute(&mut *conn)
        .await?;
        Ok(())
    })
}

/// テスト用のインメモリプール（1 接続固定で同一メモリを共有）＋マイグレーション適用。
pub fn test_pool() -> SqlitePool {
    let pool = block_on(async {
        let options = sqlx::sqlite::SqliteConnectOptions::new()
            .filename(":memory:")
            .foreign_keys(true)
            .create_if_missing(true);
        sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
    })
    .expect("test pool");
    migrate(&pool).expect("test migrate");
    pool
}

/// Acquire a connection for a synchronous callback (used by repository
/// functions that need raw connection access).
pub fn with_conn<T>(
    pool: &SqlitePool,
    f: impl FnOnce(&mut SqliteConnection) -> Result<T, sqlx::Error>,
) -> Result<T, sqlx::Error> {
    block_on(async {
        let mut conn = pool.acquire().await?;
        f(&mut conn)
    })
}
