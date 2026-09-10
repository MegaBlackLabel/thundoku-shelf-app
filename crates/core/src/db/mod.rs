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
pub mod contents;
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

/// 開発中のためマイグレーションファイルは作らず、既存の runtime DDL（hidden_at /
/// owner_sub 等）と同様に PRAGMA で列の有無を確認してから `ALTER TABLE ... ADD COLUMN` で
/// 冪等に列を追加する。
async fn ensure_column(
    conn: &mut sqlx::SqliteConnection,
    table: &str,
    column: &str,
    definition: &str,
) -> Result<(), sqlx::Error> {
    let exists: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM pragma_table_info(?1) WHERE name = ?2")
            .bind(table)
            .bind(column)
            .fetch_one(&mut *conn)
            .await?;
    if exists == 0 {
        sqlx::query(&format!("ALTER TABLE {table} ADD COLUMN {definition}"))
            .execute(&mut *conn)
            .await?;
    }
    Ok(())
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
        // 作者名カラム（プログラム的マイグレーション。hidden_at と同様に PRAGMA で
        // 存在確認してから ALTER TABLE する。技術書典は author を持たないため空のまま、
        // BOOTH 等は作成者（shop）に相当する作者名を格納する）
        {
            let has_author: bool = sqlx::query_scalar(
                "SELECT COUNT(*) FROM pragma_table_info('bookshelf_items') \
                 WHERE name = 'author'",
            )
            .fetch_one(&mut *conn)
            .await?;
            if !has_author {
                sqlx::query(
                    "ALTER TABLE bookshelf_items ADD COLUMN author TEXT NOT NULL DEFAULT ''",
                )
                .execute(&mut *conn)
                .await?;
            }
        }
        // 所有者 sub（暗号化済み / NULL = 未所属）。アカウント切替・複数アカウント対応
        // （改訂版）。既存の migration ファイルは checksum 管理されるため変更せず、
        // PRAGMA で存在確認してから ALTER TABLE する。
        {
            let has_owner_sub: bool = sqlx::query_scalar(
                "SELECT COUNT(*) FROM pragma_table_info('books') WHERE name = 'owner_sub'",
            )
            .fetch_one(&mut *conn)
            .await?;
            if !has_owner_sub {
                sqlx::query("ALTER TABLE books ADD COLUMN owner_sub TEXT")
                    .execute(&mut *conn)
                    .await?;
            }
        }
        // 複数アカウント対応: 同一 source を owner ごとに複数行持てるようにするため、
        // `(site_id, tbf_product_id)` の UNIQUE 制約を外す（乙案）。
        // 既存 DB / 新規 DB とも、ここで冪等に DROP する。
        sqlx::query("DROP INDEX IF EXISTS books_site_tbf_product_id_unique")
            .execute(&mut *conn)
            .await?;
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
        // コンテンツ（読む単位）とレンディション（切替可能な表示形態）。1 冊に複数の
        // 本文・別冊・PDF版/画像版を持たせるための構造（docs/import-patterns.md §3.3）。
        // 開発中のためマイグレーションファイルは作らず、他の後発テーブルと同様に
        // IF NOT EXISTS で冪等に適用する。
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS book_contents (               content_id TEXT PRIMARY KEY,               book_id TEXT NOT NULL REFERENCES books(id) ON DELETE CASCADE,               display_name TEXT NOT NULL,               media_kind TEXT NOT NULL,               is_primary INTEGER NOT NULL DEFAULT 0,               sort_order INTEGER NOT NULL DEFAULT 0,               created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP             );             CREATE INDEX IF NOT EXISTS idx_book_contents_book ON book_contents(book_id)",
        )
        .execute(&mut *conn)
        .await?;
        sqlx::query(
            "CREATE TABLE IF NOT EXISTS content_formats (               format_id TEXT PRIMARY KEY,               content_id TEXT NOT NULL REFERENCES book_contents(content_id) ON DELETE CASCADE,               label TEXT NOT NULL,               format_kind TEXT NOT NULL,               page_count INTEGER NOT NULL DEFAULT 0,               pack_entry_prefix TEXT,               sort_order INTEGER NOT NULL DEFAULT 0,               created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP             );             CREATE INDEX IF NOT EXISTS idx_content_formats_content ON content_formats(content_id)",
        )
        .execute(&mut *conn)
        .await?;
        // `document_images` にコンテンツ / レンディションへの参照を追加する
        // （NULL = フェーズ2以前に取り込んだ旧データ。単一コンテンツ扱い）。
        ensure_column(
            &mut conn,
            "document_images",
            "content_id",
            "content_id TEXT REFERENCES book_contents(content_id)",
        )
        .await?;
        ensure_column(
            &mut conn,
            "document_images",
            "format_id",
            "format_id TEXT REFERENCES content_formats(format_id)",
        )
        .await?;
        // フェーズ2以前の旧ラベル（画像 / PDF / EPUB）を実データに合わせて書き換える
        // （データ移行。対象が無ければ何もしない）
        let migrated_labels = contents::migrate_legacy_labels(&mut conn).await?;
        if migrated_labels > 0 {
            log::info!("migrate: content_formats.label を {migrated_labels} 件更新");
        }
        // 共有ソースメタ列（FANZA同人 / DLsite）。開発中のためマイグレーションファイルは
        // 作らず、既存 runtime DDL（hidden_at / owner_sub 等）と同様に冪等に適用する。
        {
            let cols = [
                "media_category TEXT",
                "ai_type TEXT",
                "is_drm INTEGER NOT NULL DEFAULT 0",
                "release_date TEXT",
                "description TEXT",
                "theme TEXT",
                "maker_id TEXT",
                "page_count INTEGER",
                "age_rating TEXT",
                "series_name TEXT",
            ];
            for table in ["bookshelf_items", "books"] {
                for def in cols {
                    let col = def.split_whitespace().next().unwrap();
                    ensure_column(&mut *conn, table, col, def).await?;
                }
            }
        }
        // FANZA同人 / DLsite の sites 行（冪等）。
        sqlx::query(
            "INSERT OR IGNORE INTO sites (id, name, url, display_order, is_visible) \
             VALUES ('fanza', 'FANZA同人', 'https://www.dmm.co.jp/dc/doujin/', 2, 1)",
        )
        .execute(&mut *conn)
        .await?;
        sqlx::query(
            "INSERT OR IGNORE INTO sites (id, name, url, display_order, is_visible) \
             VALUES ('dlsite', 'DLsite', 'https://www.dlsite.com/', 3, 1)",
        )
        .execute(&mut *conn)
        .await?;
        Ok(())
    })
}

/// P4: 初回起動（`owner_sub_model.initialized` フラグが無い）で既存データをクリアして
/// 新モデル（ownersub）で開始する。2 回目以降は何もしない。
/// 既存データは「開発版・旧フォーマット」のため削除して作り直す。
/// 戻り値: 初回でクリアしたら `true`。
pub fn clear_owner_model_if_first_run(
    pool: &SqlitePool,
    packs_dir: &Path,
    thumbnails_dir: &Path,
) -> Result<bool, sqlx::Error> {
    let initialized: i64 = block_on(async {
        sqlx::query_scalar(
            "SELECT COUNT(*) FROM app_settings WHERE key = 'owner_sub_model.initialized'",
        )
        .fetch_one(pool)
        .await
    })?;
    if initialized > 0 {
        return Ok(false);
    }
    // FK 安全の順（参照子 → 親）でクリアする。sites / tbf_events 等の参照テーブルは残す。
    for table in [
        "product_sample_pages",
        "token_analysis",
        "document_text",
        "document_images",
        "content_formats",
        "book_contents",
        "imported_documents",
        "book_tags",
        "reading_progress",
        "view_history",
        "page_views",
        "book_first_events",
        "checked_items",
        "bookshelf_items",
        "books",
        "drive_sync_state",
    ] {
        let _ = block_on(async {
            sqlx::query(&format!("DELETE FROM {table}"))
                .execute(pool)
                .await
        });
    }
    let _ = std::fs::remove_dir_all(packs_dir);
    let _ = std::fs::create_dir_all(packs_dir);
    let _ = std::fs::remove_dir_all(thumbnails_dir);
    let _ = std::fs::create_dir_all(thumbnails_dir);
    // 同期状態も「未同期」に戻す（新モデル開始時に最初のログインで
    // 「Google Drive と同期しますか？」を促すため）。
    for key in [
        "drive.last_sync_at",
        "drive.sync.enabled",
        "drive.sync.folder_id",
    ] {
        let _ = settings::delete(pool, key);
    }
    settings::set(pool, "owner_sub_model.initialized", "1")?;
    Ok(true)
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
