//! バックアップ用の JSON エクスポート。
//!
//! DB ファイル全体（画像 base64 を含み 200MB 級になる）をそのまま
//! Drive に上げるのは重いため、主要テーブルのテキストデータだけを
//! JSON にまとめる。画像データ（`thumbnail_data` / `image_data`）と
//! 環境依存の設定（`drive.*` / `api.last_sync_at` 等）は含めない:
//! サイズを抑え、同期のたびに内容が変わって毎回アップロードされるのを防ぐ。

use serde_json::{Map, Number, Value};
use sqlx::{Row, TypeInfo, ValueRef};

use opfspack::{BackupEnvelope, PackRootKey};

use crate::db::SqlitePool;

/// エクスポート対象のテーブル（テキストデータのみ）。
const TABLES: &[&str] = &[
    "books",
    // tbf_events は bookshelf_items.event_id / checked_items.event_id から
    // 参照されるため、必ず参照元より先に INSERT する（FK 順序）。
    "tbf_events",
    "bookshelf_items",
    "checked_items",
    "book_contents",
    "content_formats",
    "reading_progress",
    "page_views",
    "book_tags",
    "favorite_tags",
    "favorite_entities",
    "imported_documents",
    "document_images",
    "book_first_events",
    // 付箋メモ（ユーザーが書いたデータ）。ページ本文や解析結果と違い
    // 復元できないため、バックアップ対象に含める。
    "page_notes",
    "zenn_tag_metadata",
    "view_history",
];
/// 画像・バイナリ・本文テキストとして除外するカラム名。
///
/// `extracted_text`（`document_images`）は暗号化 pack から取り出したページ本文で、
/// 平文のまま Drive のバックアップ JSON に載ると、pack を暗号化した意味が失われる。
/// アプリはこの列を読まない（本文は `document_text` 側にあり、そちらは
/// バックアップ対象テーブルに含まれない）ため、除外しても復元結果は変わらない。
const EXCLUDED_COLUMNS: &[&str] = &["thumbnail_data", "image_data", "extracted_text"];

/// 比較のときに無視する揮発列（アプリ自身が同期のたびに書き換える時刻）。
/// 内容が同じでも値が変わるため、そのまま比較すると毎回「差分あり」になる。
const VOLATILE_COLUMNS: &[(&str, &[&str])] = &[
    ("books", &["updated_at"]),
    ("bookshelf_items", &["synced_at", "updated_at"]),
    ("tbf_events", &["updated_at"]),
];

/// バックアップ JSON を比較用の正規形へ変換する。
///
/// - `keep_tables` が `Some` のとき、その名前のテーブルだけを残す（Drive 側に
///   まだ無いテーブルを比較対象から外す）
/// - 揮発列（[`VOLATILE_COLUMNS`]）を落とす
/// - 行を PK 順に並べる（`SELECT` の物理順＝挿入順に依存しない）
pub fn canonicalize_json(
    value: &serde_json::Value,
    keep_tables: Option<&[String]>,
) -> serde_json::Value {
    let Some(object) = value.as_object() else {
        return value.clone();
    };
    let mut out = Map::new();
    for (table, rows) in object {
        // 表は配列。`format_version` のような付帯情報（表ではない）は比較に含めない
        // （内容が同じなら版が増えても md5 を変えない＝無駄なアップロードを起こさない）。
        if !rows.is_array() {
            continue;
        }
        if let Some(keep) = keep_tables
            && !keep.iter().any(|name| name == table)
        {
            continue;
        }
        let volatile: &[&str] = VOLATILE_COLUMNS
            .iter()
            .find(|(name, _)| name == table)
            .map(|(_, columns)| *columns)
            .unwrap_or_default();
        let mut normalized: Vec<Value> = rows
            .as_array()
            .map(|rows| {
                rows.iter()
                    .map(|row| {
                        let Some(row) = row.as_object() else {
                            return row.clone();
                        };
                        let mut filtered = Map::new();
                        for (column, cell) in row {
                            if !volatile.contains(&column.as_str()) {
                                filtered.insert(column.clone(), cell.clone());
                            }
                        }
                        Value::Object(filtered)
                    })
                    .collect()
            })
            .unwrap_or_default();
        normalized.sort_by_key(|row| row_sort_key(table, row));
        out.insert(table.clone(), Value::Array(normalized));
    }
    Value::Object(out)
}

/// 行の並べ替えキー（PK 列の値。PK が分からないテーブルは行そのもの）。
fn row_sort_key(table: &str, row: &serde_json::Value) -> String {
    match pk_columns(table) {
        Some(pk) => {
            let cells: Vec<Value> = pk
                .iter()
                .map(|column| row.get(*column).cloned().unwrap_or(Value::Null))
                .collect();
            Value::Array(cells).to_string()
        }
        None => row.to_string(),
    }
}

/// 正規形の md5（比較と、最後にアップロードした内容の基準値に使う）。
pub fn canonical_md5(value: &serde_json::Value, keep_tables: Option<&[String]>) -> String {
    format!(
        "{:x}",
        md5::compute(canonicalize_json(value, keep_tables).to_string().as_bytes())
    )
}

/// JSON 文字列の正規形 md5。
pub fn canonical_md5_str(
    json: &str,
    keep_tables: Option<&[String]>,
) -> Result<String, sqlx::Error> {
    let value: Value =
        serde_json::from_str(json).map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
    Ok(canonical_md5(&value, keep_tables))
}

/// 所有者フィルタ。`owner_sub` 列を持つテーブルを「現在の sub の行」に絞る。
///
/// `books` は `book_ids`（呼び出し側が復号して求めた id 集合）で絞るが、本棚・
/// チェックリスト・お気に入りは `book_id` を持たない（本に紐づかない）ため、
/// それぞれの `owner_sub` を復号して判定する。`owner_sub` は毎回 IV が変わる
/// 暗号文なので SQL では比較できない。
pub struct OwnerFilter<'a> {
    /// `owner_sub` の復号鍵（keyring の DB 鍵）
    pub key: &'a [u8; 32],
    /// 現在の sub。`None`（未ログイン）は未所属（`owner_sub IS NULL`）の行。
    pub sub: Option<&'a str>,
}

/// `owner_sub` で絞る（= Google アカウントに紐づくデータ）テーブル。
/// 公開メタ（`tbf_events` / `zenn_tag_metadata`）と、`books` から id で辿れる
/// テーブルは対象外。
const OWNER_SCOPED_TABLES: &[&str] = &[
    "bookshelf_items",
    "checked_items",
    "book_first_events",
    "favorite_tags",
    "favorite_entities",
];

/// 行の `owner_sub`（暗号文 or NULL）が現在の sub に帰属するか。
fn owner_matches(filter: &OwnerFilter<'_>, blob: Option<&str>) -> bool {
    match filter.sub {
        Some(sub) => {
            blob.and_then(|blob| crate::owner::decrypt(filter.key, blob))
                .as_deref()
                == Some(sub)
        }
        None => blob.is_none(),
    }
}

/// バックアップ JSON の形式版。
///
/// 版が無いバックアップは **1**（`is_drm` に「未確認」の意味で `0` を書いていた時代）と
/// みなす。3 状態（0 = なし / 1 = あり / 2 = 不明）になってからの `0` は「DRM なしと
/// 確認できた」なので、復元時に意味を取り違えないよう版を持たせる。
pub const FORMAT_VERSION: i64 = 2;

// ---- Drive のバックアップファイル（`thundoku-backup.json`） ---------------------

/// Drive の `thundoku-backup.json` の中身（`docs/spec/10-pack-keys.md` §11）。
///
/// v3 は PRK から導出した鍵で暗号化した封筒（[`DriveBackup::Encrypted`]）、
/// v2 以前は平文のバックアップ JSON（[`DriveBackup::Plain`]）。**どちらも読める**
/// （既存のバックアップを読めなくしない）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriveBackup {
    /// v3: 暗号化された封筒（ファイルの `format_version` = 3）。
    Encrypted(BackupEnvelope),
    /// v2 以前: 平文のバックアップ JSON。
    Plain(String),
}

/// Drive のバックアップを読めなかった理由。
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    /// JSON として読めない・構造が不正。
    #[error("バックアップが壊れています: {0}")]
    Corrupt(String),
    /// 自分より新しい形式（暗号化されているのに版が違う）。
    #[error("対応していないバックアップ形式です（v{0}）。新しいアプリで作成されています")]
    UnsupportedVersion(i64),
    /// 暗号化バックアップだが、この端末に鍵（PRK）が無い。
    #[error("暗号化されたバックアップを復号する鍵がありません")]
    KeyRequired,
    /// 封筒を復号できない（鍵違い・改変・別アカウントの封筒）。
    #[error("暗号化されたバックアップを復号できません（鍵が違うか、内容が改変されています）")]
    Rejected,
}

impl DriveBackup {
    /// ファイルのバイト列を解釈する。**復号はしない**（鍵を持たない経路でも呼べる）。
    ///
    /// - `encryption` があるファイルは**必ず封筒として**扱う。読めなければエラーで、
    ///   平文バックアップには落とさない（暗号文を内容として取り込まないため）
    /// - `encryption` が無ければ従来の平文バックアップ（v2 / 版なし）
    pub fn parse(bytes: &[u8]) -> Result<Self, BackupError> {
        let value: serde_json::Value = serde_json::from_slice(bytes)
            .map_err(|e| BackupError::Corrupt(format!("JSON として読めない: {e}")))?;
        // `encryption` があれば（型が違っても）封筒として扱う。平文バックアップには
        // このキーが無いので、ここで迷ったら**安全側（復号できないなら拒否）**に倒す。
        let encrypted = value
            .get("encryption")
            .is_some_and(|encryption| !encryption.is_null());
        if encrypted {
            let version = value
                .get("format_version")
                .and_then(serde_json::Value::as_i64)
                .ok_or_else(|| BackupError::Corrupt("封筒に format_version が無い".to_string()))?;
            if version != i64::from(opfspack::BACKUP_FORMAT_VERSION) {
                return Err(BackupError::UnsupportedVersion(version));
            }
            let envelope = BackupEnvelope::from_json(bytes)
                .map_err(|error| BackupError::Corrupt(error.to_string()))?;
            return Ok(Self::Encrypted(envelope));
        }
        let json = String::from_utf8(bytes.to_vec())
            .map_err(|e| BackupError::Corrupt(format!("UTF-8 として読めない: {e}")))?;
        Ok(Self::Plain(json))
    }

    /// 暗号化された封筒か（＝鍵が要るか）。
    pub fn is_encrypted(&self) -> bool {
        matches!(self, Self::Encrypted(_))
    }

    /// 封筒の所有者（`owner_id`）。平文バックアップは `None`。
    ///
    /// keyring の鍵スロット（`thundoku-shelf.pack-root-key:<owner_id>`）を選ぶのに使う。
    pub fn owner_id(&self) -> Option<&str> {
        match self {
            Self::Encrypted(envelope) => Some(envelope.owner_id()),
            Self::Plain(_) => None,
        }
    }

    /// 変更検知の基準値。v3 = 封筒の `content_hmac`（hex）、v2 = 正規形 md5。
    ///
    /// **暗号文は nonce が乱数で毎回変わるので、暗号文の md5 を基準値にしてはいけない**
    /// （毎回「変わった」と判定して無駄なアップロードになる）。
    pub fn change_token(&self) -> Result<String, BackupError> {
        match self {
            Self::Encrypted(envelope) => Ok(envelope.content_hmac_hex()),
            Self::Plain(json) => {
                canonical_md5_str(json, None).map_err(|e| BackupError::Corrupt(e.to_string()))
            }
        }
    }

    /// 平文のバックアップ JSON を返す。暗号化されている場合は `root`（PRK）が要る。
    pub fn plaintext(self, root: Option<&PackRootKey>) -> Result<String, BackupError> {
        match self {
            Self::Encrypted(envelope) => {
                let root = root.ok_or(BackupError::KeyRequired)?;
                let plaintext = envelope
                    .open(root, envelope.owner_id())
                    .map_err(|_| BackupError::Rejected)?;
                String::from_utf8(plaintext)
                    .map_err(|e| BackupError::Corrupt(format!("平文が UTF-8 でない: {e}")))
            }
            Self::Plain(json) => Ok(json),
        }
    }
}

/// 主要テーブルを JSON 文字列にエクスポートする。
/// `book_ids` が `Some(ids)` のとき、所有者（本の id 集合）に連動して
/// `books` とその下位テーブルだけをエクスポートする（P3）。`None` は全件。
/// `owner` が `Some` のとき、[`OWNER_SCOPED_TABLES`] を現在の sub に絞る。
#[allow(clippy::explicit_auto_deref)]
pub fn export_json(
    pool: &SqlitePool,
    book_ids: Option<&std::collections::HashSet<String>>,
    owner: Option<&OwnerFilter<'_>>,
) -> Result<String, sqlx::Error> {
    crate::db::block_on(async {
        // 複数テーブルを跨いで読み出すため、トランザクションで一貫した
        // スナップショットから取り出す（並行書き込みでテーブル間が
        // 食い違ったバックアップを作らない）。
        let mut tx = pool.begin().await?;
        let mut payload = Map::new();
        // 形式版。表ではないので本文（行の比較）には含めない（`canonicalize_json` は
        // 配列以外の値を無視する）。
        payload.insert(
            "format_version".to_string(),
            Value::Number(Number::from(FORMAT_VERSION)),
        );
        for table in TABLES {
            let rows = table_rows(&mut *tx, table, book_ids, owner).await?;
            payload.insert((*table).to_string(), Value::Array(rows));
        }
        tx.commit().await?;
        // 直列化に失敗したら空文字を返さない（空バックアップで Drive 上の
        // 既存バックアップを置き換えてしまう事故を防ぐ）。
        serde_json::to_string(&Value::Object(payload))
            .map_err(|e| sqlx::Error::Protocol(e.to_string()))
    })
}

/// `export_json` で書き出したバックアップを DB に反映する。
///
/// 各テーブルを PK 競合時に `DO UPDATE` する UPSERT でマージする（Drive 優先）。
/// SQLite の `INSERT ... ON CONFLICT DO UPDATE` は DELETE を伴わないため、
/// FK の `ON DELETE CASCADE`（例: `books` → `view_history`）を発火させない。
/// テーブルは `TABLES` の順（FK 参照元が先）で処理する。
pub fn import_json(pool: &SqlitePool, json: &str) -> Result<(), sqlx::Error> {
    let payload: serde_json::Value =
        serde_json::from_str(json).map_err(|e| sqlx::Error::Protocol(e.to_string()))?;
    // 形式版が無いバックアップは 1（`is_drm` の `0` が「未確認」の意味だった時代）。
    // 1 のバックアップは `0` を「不明」に寄せて復元する（下の `legacy_drm`）。
    let format_version = payload
        .get("format_version")
        .and_then(serde_json::Value::as_i64)
        .unwrap_or(1);
    let legacy_drm = format_version < FORMAT_VERSION;
    crate::db::block_on(async {
        // 1 行でも失敗したら全て巻き戻す（部分復元を残さない）。
        let mut tx = pool.begin().await?;
        for table in TABLES {
            let Some(rows) = payload.get(*table).and_then(serde_json::Value::as_array) else {
                continue; // バックアップに無いテーブルはスキップ
            };
            let Some(pk) = pk_columns(table) else {
                log::warn!("drive restore: no PK mapping for {table}, skipping");
                continue;
            };
            // 改変バックアップ対策: `books.id` / `books.pack_id` は
            // `packs/{id}.opfspack` のファイル名になる。保存領域の外を指す id を
            // 取り込むと読み出し・改名・削除が領域外へ及ぶ（CWE-22）。
            // 行だけ捨てると子テーブル（reading_progress 等）の FK 違反で
            // 復元全体がロールバックするため、**復元を拒否**して利用者に見せる。
            if *table == "books" {
                for row in rows {
                    for column in ["id", "pack_id"] {
                        let Some(id) = row.get(column).and_then(serde_json::Value::as_str) else {
                            continue;
                        };
                        if !crate::pack_path::is_safe_id(id) {
                            return Err(sqlx::Error::Protocol(format!(
                                "バックアップの books.{column} が不正な値のため復元を中止しました: {id:?}"
                            )));
                        }
                    }
                }
            }
            // 競合判定は自然キーがあればそちらを使う（PK と別の UNIQUE 制約を
            // 持つ表で、もう片方の制約違反により復元が失敗するのを防ぐ）。
            let conflict = conflict_columns(table).unwrap_or(pk);
            upsert_rows(&mut tx, table, pk, conflict, rows, legacy_drm).await?;
        }
        tx.commit().await?;
        Ok(())
    })
}

/// UPSERT の競合判定に使う列（既定は PRIMARY KEY）。
///
/// `PRIMARY KEY` とは別に自然キーの UNIQUE 制約を持つテーブルは、そちらを
/// 同一性として扱う。片方だけで競合判定すると、もう片方の UNIQUE 違反で
/// INSERT が失敗し（トランザクションのため復元全体がロールバックする）、
/// 復元できなくなる。
fn conflict_columns(table: &str) -> Option<&'static [&'static str]> {
    match table {
        // 付箋の同一性は (book, content, page)。id は端末ごとに生成され得るため
        // （アプリ自身の upsert も `ON CONFLICT(book_id, content_id, page)` を使う）、
        // id を競合判定に使うと別端末の同じ付箋を二重登録しようとして失敗する。
        "page_notes" => Some(&["book_id", "content_id", "page"]),
        other => pk_columns(other),
    }
}

/// テーブルごとの PRIMARY KEY カラム（`TABLES` の定義と一致させる）。
fn pk_columns(table: &str) -> Option<&'static [&'static str]> {
    Some(match table {
        "books" => &["id"],
        "bookshelf_items" => &["site_id", "database_id"],
        "checked_items" => &["id"],
        "tbf_events" => &["id"],
        "book_contents" => &["content_id"],
        "content_formats" => &["format_id"],
        "reading_progress" => &["book_id", "content_id"],
        "page_views" => &["book_id", "content_id", "page_number"],
        "book_tags" => &["id"],
        "favorite_tags" => &["tag_name"],
        "favorite_entities" => &["entity_kind", "entity_name"],
        "imported_documents" => &["id"],
        "document_images" => &["id"],
        "book_first_events" => &["site_id", "database_id"],
        "page_notes" => &["id"],
        "zenn_tag_metadata" => &["tag_name"],
        "view_history" => &["id"],
        _ => return None,
    })
}

#[allow(clippy::explicit_auto_deref)]
async fn table_rows(
    conn: &mut sqlx::SqliteConnection,
    table: &str,
    book_ids: Option<&std::collections::HashSet<String>>,
    owner: Option<&OwnerFilter<'_>>,
) -> Result<Vec<Value>, sqlx::Error> {
    // 画像（blob）カラムは SELECT から除外して読み込みコストを削る
    let cols: Vec<String> = {
        let info = sqlx::query(&format!("PRAGMA table_info({table})"))
            .fetch_all(&mut *conn)
            .await?;
        let mut names = Vec::new();
        for row in info {
            let name: String = row.get(1);
            if !EXCLUDED_COLUMNS.contains(&name.as_str()) {
                names.push(name);
            }
        }
        names
    };
    let col_list = cols.join(", ");
    // 所有者（本の id 集合）に連動して下位テーブルを絞る（P3）。json_each で IN を組む。
    let mut where_sql = String::new();
    let mut bind_json: Option<String> = None;
    if let Some(ids) = book_ids {
        let json = serde_json::to_string(ids).unwrap_or_else(|_| "[]".into());
        match table {
            "document_images" => {
                where_sql = " WHERE document_id IN (SELECT id FROM imported_documents \
                             WHERE book_id IN (SELECT value FROM json_each(?)))"
                    .into();
                bind_json = Some(json);
            }
            "books" => {
                where_sql = " WHERE id IN (SELECT value FROM json_each(?))".into();
                bind_json = Some(json);
            }
            "reading_progress" | "page_views" | "book_tags" | "view_history"
            | "imported_documents" | "book_contents" | "page_notes" => {
                where_sql = " WHERE book_id IN (SELECT value FROM json_each(?))".into();
                bind_json = Some(json);
            }
            "content_formats" => {
                where_sql = " WHERE content_id IN (SELECT content_id FROM book_contents \
                             WHERE book_id IN (SELECT value FROM json_each(?)))"
                    .into();
                bind_json = Some(json);
            }
            _ => {}
        }
    }
    // 行の並びを PK 順に固定する。物理順（挿入順・索引の選択）に依存すると、
    // 内容が同じでも JSON の md5 が変わり、無変更なのに再アップロードになる。
    let order_sql = match pk_columns(table) {
        Some(pk) => format!(" ORDER BY {}", pk.join(", ")),
        None => String::new(),
    };
    let sql = format!("SELECT {col_list} FROM {table}{where_sql}{order_sql}");
    let mut query = sqlx::query(&sql);
    if let Some(json) = &bind_json {
        query = query.bind(json);
    }
    let rows = query.fetch_all(&mut *conn).await?;
    let mut out = Vec::new();
    for row in rows {
        let mut obj = Map::new();
        for (index, column) in cols.iter().enumerate() {
            let raw = row.try_get_raw(index)?;
            // NULL はストレージクラスが NULL（declared type は TEXT 等に
            // フォールバックされる）なので、先に is_null で判定する。
            // それ以外は値のストレージクラス（TEXT / INTEGER / REAL / BLOB）
            // で rusqlite の ValueRef 相当に振り分ける。BLOB は null 扱い。
            let value = if raw.is_null() {
                Value::Null
            } else {
                match raw.type_info().name() {
                    "TEXT" => Value::String(row.try_get::<String, _>(index)?),
                    "INTEGER" => Value::Number(Number::from(row.try_get::<i64, _>(index)?)),
                    "REAL" => Number::from_f64(row.try_get::<f64, _>(index)?)
                        .map(Value::Number)
                        .unwrap_or(Value::Null),
                    _ => Value::Null,
                }
            };
            obj.insert(column.clone(), value);
        }
        out.push(Value::Object(obj));
    }
    // アカウントに紐づくテーブルは現在の sub の行だけを出す（他アカウント・
    // 未所属のメタを Drive バックアップへ混ぜない）。`owner_sub` は暗号文なので
    // SQL では絞れず、ここで復号して判定する。
    if let Some(filter) = owner
        && OWNER_SCOPED_TABLES.contains(&table)
    {
        out.retain(|row| {
            owner_matches(
                filter,
                row.get("owner_sub").and_then(serde_json::Value::as_str),
            )
        });
    }
    Ok(out)
}

async fn upsert_rows(
    conn: &mut sqlx::SqliteConnection,
    table: &str,
    pk: &[&str],
    conflict_cols: &[&str],
    rows: &[serde_json::Value],
    // 旧仕様（`format_version` 無し = 1）のバックアップか。旧仕様の `is_drm = 0` は
    // 「DRM なしと確認できた」ではなく「判定していない」の意味だった（同期が `0` 固定で
    // 書いていた）。そのまま入れると本棚が嘘のメタを出すので「不明」に寄せる。
    legacy_drm: bool,
) -> Result<(), sqlx::Error> {
    // 画像（blob）カラムは対象外（エクスポート時と同じ除外リスト）
    let table_cols: Vec<String> = {
        let info = sqlx::query(&format!("PRAGMA table_info({table})"))
            .fetch_all(&mut *conn)
            .await?;
        let mut names = Vec::new();
        for row in info {
            let name: String = row.get(1);
            if !EXCLUDED_COLUMNS.contains(&name.as_str()) {
                names.push(name);
            }
        }
        names
    };
    if table_cols.is_empty() {
        return Ok(());
    }
    // 競合判定に使う列は UPDATE 対象から外す（同一性そのものなので書き換えない）
    let conflict_set: std::collections::HashSet<&str> = conflict_cols.iter().copied().collect();
    let conflict = conflict_cols.join(", ");
    for row in rows {
        let Some(obj) = row.as_object() else {
            continue;
        };
        // 旧バックアップ（`is_drm` 列を足す前に取ったもの）は、そのまま復元すると
        // スキーマの DEFAULT（0 = 「DRM なしと確認済み」）になり**嘘のメタ**になる。
        // 欠落しているときは「不明」として補う。旧仕様（形式版 1）で `0` が入っている行も
        // 同じ意味（未確認）なので「不明」に寄せる。
        let with_drm_default;
        let obj = if matches!(table, "books" | "bookshelf_items")
            && (legacy_drm && obj.get("is_drm").and_then(serde_json::Value::as_i64) == Some(0)
                || !obj.contains_key("is_drm"))
        {
            let mut owned = obj.clone();
            owned.insert(
                "is_drm".to_string(),
                Value::Number(Number::from(crate::drm::DrmStatus::Unknown.as_db())),
            );
            with_drm_default = owned;
            &with_drm_default
        } else {
            obj
        };
        // 旧バックアップ（`poll_sync_enabled` 列を足す前に取ったもの）は、そのまま復元すると
        // スキーマの DEFAULT（0 = ポーリング対象外）になり、注目イベントでも 5 分ポーラーが
        // 動かない。欠落しているときだけ注目イベントへ既定（1）を補う
        // （列を持つ新しいバックアップの利用者設定はそのまま復元する）。
        let with_poll_default;
        let obj = if table == "tbf_events" && !obj.contains_key("poll_sync_enabled") {
            let mut owned = obj.clone();
            let featured = obj.get("is_featured").and_then(Value::as_i64).unwrap_or(0) != 0;
            owned.insert(
                "poll_sync_enabled".to_string(),
                Value::Number(Number::from(if featured { 1 } else { 0 })),
            );
            with_poll_default = owned;
            &with_poll_default
        } else {
            obj
        };
        // PK / 競合判定列が欠けている行は INSERT しない（NULL を作らない）
        if let Some(missing) = pk
            .iter()
            .chain(conflict_cols.iter())
            .find(|k| !obj.contains_key(**k))
        {
            log::warn!("drive restore: {table} の行に必須列 {missing} が無いためスキップ");
            continue;
        }
        // バックアップに含まれる列だけを書き込み対象にする。バックアップに
        // 無い列を NULL で書くと、既存値の消失（NULL 上書き）や NOT NULL
        // 制約違反になるため、欠落列はスキーマの DEFAULT に任せる。
        let cols: Vec<&str> = table_cols
            .iter()
            .map(String::as_str)
            .filter(|c| obj.contains_key(*c))
            .collect();
        if cols.is_empty() {
            continue;
        }
        let update_cols: Vec<&str> = cols
            .iter()
            .copied()
            .filter(|c| !conflict_set.contains(c))
            .collect();
        let col_list = cols.join(", ");
        let placeholders = cols
            .iter()
            .enumerate()
            .map(|(i, _)| format!("?{}", i + 1))
            .collect::<Vec<_>>()
            .join(", ");
        let sql = if update_cols.is_empty() {
            format!(
                "INSERT INTO {table} ({col_list}) VALUES ({placeholders}) \
                 ON CONFLICT ({conflict}) DO NOTHING"
            )
        } else {
            let update_set = update_cols
                .iter()
                .map(|c| format!("{c} = excluded.{c}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!(
                "INSERT INTO {table} ({col_list}) VALUES ({placeholders}) \
                 ON CONFLICT ({conflict}) DO UPDATE SET {update_set}"
            )
        };
        let mut query = sqlx::query(&sql);
        for col in &cols {
            let v = obj.get(*col).cloned().unwrap_or(Value::Null);
            match v {
                Value::Null => {
                    query = query.bind(None::<String>);
                }
                Value::String(s) => {
                    query = query.bind(s);
                }
                Value::Number(n) => {
                    if let Some(i) = n.as_i64() {
                        query = query.bind(i);
                    } else {
                        query = query.bind(n.as_f64().unwrap_or(0.0));
                    }
                }
                Value::Bool(b) => {
                    query = query.bind(b as i64);
                }
                _ => {
                    query = query.bind(None::<String>);
                }
            }
        }
        query.execute(&mut *conn).await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_json_contains_tables_without_images() {
        let pool = crate::db::test_pool();
        // 本とチェックリスト項目を 1 件ずつ入れる
        crate::db::books::insert(
            &pool,
            &crate::db::books::Book {
                id: "book-1".into(),
                title: "テスト本".into(),
                author: "著者".into(),
                circle_name: "サークル".into(),
                purchase_date: None,
                file_name: "book-1.pdf".into(),
                file_size: 10,
                opfs_path: "book-1.opfspack".into(),
                cover_thumbnail: None,
                tbf_product_id: None,
                site_id: None,
                tags_fetched: 0,
                pack_id: Some("book-1".into()),
                is_favorite: 0,
                is_hidden: 0,
                created_at: "2026-08-23 00:00:00".into(),
                updated_at: "2026-08-23 00:00:00".into(),
                media_category: None,
                ai_type: None,
                is_drm: 0,
                release_date: None,
                description: None,
                theme: None,
                maker_id: None,
                page_count: None,
                age_rating: None,
                series_name: None,
            },
        )
        .unwrap();
        // checked_items の FK 用にイベントを seed する
        crate::db::checklist::upsert_event(
            &pool,
            &crate::db::checklist::TbfEvent {
                id: "tbf20".into(),
                site_id: "techbookfest".into(),
                slug: Some("tbf20".into()),
                tbf_event_id: Some("Event:tbf20".into()),
                event_name: "技術書典20".into(),
                event_date: Some("2026-04-11".into()),
                event_start_date: Some("2026-04-11".into()),
                event_end_date: Some("2026-04-26".into()),
                event_format: "hybrid".into(),
                is_cancelled: 0,
                display_order: 0,
                is_featured: 1,
                poll_sync_enabled: 0,
                created_at: "2026-08-23 00:00:00".into(),
                updated_at: "2026-08-23 00:00:00".into(),
            },
        )
        .unwrap();
        crate::db::checklist::upsert_item(
            &pool,
            &crate::db::checklist::CheckedItem {
                id: "tbf20:p1".into(),
                event_id: "tbf20".into(),
                circle_name: "サークルA".into(),
                space_number: "あ-01".into(),
                memo: String::new(),
                is_checked: 0,
                sort_order: 0,
                tbf_circle_id: None,
                product_id: Some("p1".into()),
                product_title: "本".into(),
                thumbnail_url: Some("https://example.com/img.png".into()),
                thumbnail_data: Some("aGVsbG8=".into()),
                price: Some(1000),
                is_purchased: 0,
                sample_fetch_attempted_at: None,
                created_at: "2026-08-23 00:00:00".into(),
            },
        )
        .unwrap();

        let json = export_json(&pool, None, None).unwrap();
        let payload: Value = serde_json::from_str(&json).unwrap();
        // books テーブルに 1 件
        let books = payload["books"].as_array().unwrap();
        assert_eq!(books.len(), 1);
        assert_eq!(books[0]["title"], "テスト本");
        // checked_items に thumbnail_data が含まれない（画像は除外）
        let items = payload["checked_items"].as_array().unwrap();
        assert_eq!(items.len(), 1);
        assert!(
            items[0].get("thumbnail_data").is_none(),
            "image columns must be excluded"
        );
    }
    #[test]
    fn import_json_round_trips_main_tables_and_updates() {
        use crate::db::progress::ReadingProgress;
        // ソース DB に本・進捗・閲覧履歴を入れてエクスポートする
        let src = crate::db::test_pool();
        crate::db::books::insert(
            &src,
            &crate::db::books::Book {
                id: "book-1".into(),
                title: "テスト本".into(),
                author: "著者".into(),
                circle_name: "サークル".into(),
                purchase_date: None,
                file_name: "book-1.pdf".into(),
                file_size: 10,
                opfs_path: "book-1.opfspack".into(),
                cover_thumbnail: None,
                tbf_product_id: None,
                site_id: None,
                tags_fetched: 0,
                pack_id: Some("book-1".into()),
                is_favorite: 0,
                is_hidden: 0,
                created_at: "2026-08-23 00:00:00".into(),
                updated_at: "2026-08-23 00:00:00".into(),
                media_category: None,
                ai_type: None,
                is_drm: 0,
                release_date: None,
                description: None,
                theme: None,
                maker_id: None,
                page_count: None,
                age_rating: None,
                series_name: None,
            },
        )
        .unwrap();
        // フェーズ2: コンテンツ構造もバックアップ／復元の対象
        crate::db::contents::insert_batch(
            &src,
            &[crate::db::contents::BookContent {
                content_id: "c1".into(),
                book_id: "book-1".into(),
                display_name: "本文".into(),
                media_kind: "image".into(),
                is_primary: 1,
                sort_order: 0,
                created_at: "2026-08-23 00:00:00".into(),
            }],
            &[crate::db::contents::ContentFormat {
                format_id: "f1".into(),
                content_id: "c1".into(),
                label: "画像".into(),
                format_kind: "image".into(),
                page_count: 2,
                pack_entry_prefix: Some("pages".into()),
                sort_order: 0,
                created_at: "2026-08-23 00:00:00".into(),
            }],
        )
        .unwrap();
        crate::db::progress::upsert(
            &src,
            &ReadingProgress {
                book_id: "book-1".into(),
                content_id: "c1".into(),
                current_page: 12,
                total_pages: Some(120),
                finished_at: None,
                last_read_at: "2026-08-23 09:00:00".into(),
            },
        )
        .unwrap();
        crate::db::view_history::start(&src, "book-1").unwrap();
        assert_eq!(
            crate::db::view_history::view_count(&src, "book-1").unwrap(),
            1
        );
        // ページ毎閲覧記録を入れる（バックアップ対象に含まれるべき）
        crate::db::page_views::record_view(&src, "book-1", "c1", 1).unwrap();
        crate::db::page_views::record_view(&src, "book-1", "c1", 2).unwrap();
        crate::db::page_views::add_dwell(&src, "book-1", "c1", 1, 3.5).unwrap();
        crate::db::page_views::add_dwell(&src, "book-1", "c1", 2, 1.25).unwrap();

        let json = export_json(&src, None, None).unwrap();
        // view_history がバックアップに含まれる
        let payload: Value = serde_json::from_str(&json).unwrap();
        let vh = payload["view_history"].as_array().unwrap();
        assert_eq!(vh.len(), 1, "view_history must be in the backup");
        // page_views もバックアップに含まれる
        let pv = payload["page_views"].as_array().unwrap();
        assert_eq!(pv.len(), 2, "page_views must be in the backup");
        let p1 = pv.iter().find(|r| r["page_number"] == 1).unwrap();
        assert_eq!(p1["view_count"], 1);
        assert!((p1["total_seconds"].as_f64().unwrap() - 3.5).abs() < 1e-9);
        // コンテンツ構造もバックアップに含まれる
        assert_eq!(payload["book_contents"].as_array().unwrap().len(), 1);
        assert_eq!(payload["content_formats"].as_array().unwrap().len(), 1);

        // 空の DB にインポートすると本・進捗・閲覧履歴が復元される
        let dst = crate::db::test_pool();
        import_json(&dst, &json).unwrap();
        let restored = crate::db::books::get(&dst, "book-1").unwrap().unwrap();
        assert_eq!(restored.title, "テスト本");
        // コンテンツ構造も復元される（FK 順が正しくないと失敗する）
        let restored_contents = crate::db::contents::list_for_book(&dst, "book-1").unwrap();
        assert_eq!(restored_contents.len(), 1);
        assert_eq!(restored_contents[0].display_name, "本文");
        let restored_formats = crate::db::contents::formats_for_content(&dst, "c1").unwrap();
        assert_eq!(restored_formats.len(), 1);
        assert_eq!(restored_formats[0].page_count, 2);
        let progress = crate::db::progress::get(&dst, "book-1").unwrap().unwrap();
        assert_eq!(progress.current_page, 12);
        assert_eq!(
            crate::db::view_history::view_count(&dst, "book-1").unwrap(),
            1
        );
        // page_views も復元される
        let rows = crate::db::page_views::for_book(&dst, "book-1").unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].page_number, 1);
        assert_eq!(rows[0].view_count, 1);
        assert!((rows[0].total_seconds - 3.5).abs() < 1e-9);
        assert!((rows[1].total_seconds - 1.25).abs() < 1e-9);
    }

    #[test]
    fn export_json_filters_by_owner() {
        let pool = crate::db::test_pool();
        crate::db::migrate(&pool).unwrap();
        let key = [17u8; 32];
        let mk = |id: &str| crate::db::books::Book {
            id: id.into(),
            title: "本".into(),
            author: String::new(),
            circle_name: String::new(),
            purchase_date: None,
            file_name: "f.pdf".into(),
            file_size: 1,
            opfs_path: format!("{id}.opfspack"),
            cover_thumbnail: None,
            tbf_product_id: None,
            site_id: None,
            tags_fetched: 1,
            pack_id: Some(id.into()),
            is_favorite: 0,
            is_hidden: 0,
            created_at: "2026-01-01 00:00:00".into(),
            updated_at: "2026-01-01 00:00:00".into(),
            media_category: None,
            ai_type: None,
            is_drm: 0,
            release_date: None,
            description: None,
            theme: None,
            maker_id: None,
            page_count: None,
            age_rating: None,
            series_name: None,
        };
        crate::db::books::insert(&pool, &mk("book-A")).unwrap();
        crate::db::books::insert(&pool, &mk("book-B")).unwrap();
        crate::db::books::set_owner_sub(&pool, "book-A", Some(crate::owner::encrypt(&key, "A")))
            .unwrap();
        crate::db::books::set_owner_sub(&pool, "book-B", Some(crate::owner::encrypt(&key, "B")))
            .unwrap();
        // 進捗も入れる
        use crate::db::progress::ReadingProgress;
        for (id, page) in [("book-A", 5), ("book-B", 9)] {
            crate::db::progress::upsert(
                &pool,
                &ReadingProgress {
                    book_id: id.into(),
                    content_id: String::new(),
                    current_page: page,
                    total_pages: Some(10),
                    finished_at: None,
                    last_read_at: "2026-01-01 00:00:00".into(),
                },
            )
            .unwrap();
        }

        // A の所有のみ → book-A とその進捗だけ
        let owned_a: std::collections::HashSet<String> = ["book-A".into()].into();
        let json = export_json(&pool, Some(&owned_a), None).unwrap();
        let v: Value = serde_json::from_str(&json).unwrap();
        let books = v["books"].as_array().unwrap();
        assert_eq!(books.len(), 1);
        assert_eq!(books[0]["id"], "book-A");
        let prog = v["reading_progress"].as_array().unwrap();
        assert_eq!(prog.len(), 1);
        assert_eq!(prog[0]["book_id"], "book-A");

        // 全件（None）→ 両方
        let json_full = export_json(&pool, None, None).unwrap();
        let vf: Value = serde_json::from_str(&json_full).unwrap();
        assert_eq!(vf["books"].as_array().unwrap().len(), 2);
        assert_eq!(vf["reading_progress"].as_array().unwrap().len(), 2);
    }

    /// アカウントに紐づくテーブル（本棚・チェックリスト・お気に入り）も、
    /// 現在の sub の行だけが出ること。
    ///
    /// これらは `book_id` を持たない（または本に紐づかない）ため `books` の
    /// 所有者フィルタでは絞れない。他アカウントの購入情報がバックアップに
    /// 混ざると、アカウント切替後の Drive に前のアカウントのデータが残る。
    #[test]
    fn export_json_filters_account_scoped_tables_by_owner() {
        let pool = crate::db::test_pool();
        crate::db::migrate(&pool).unwrap();
        let key = [29u8; 32];
        let owner_a = crate::owner::encrypt(&key, "A");
        let owner_b = crate::owner::encrypt(&key, "B");

        crate::db::block_on(async {
            sqlx::query(
                "INSERT INTO tbf_events (id, site_id, event_name) VALUES ('tbf20', 'techbookfest', '技術書典20')",
            )
            .execute(&pool)
            .await
            .unwrap();
            for (site, database_id, owner) in [
                ("booth", "shelf-a", Some(&owner_a)),
                ("booth", "shelf-b", Some(&owner_b)),
                ("booth", "shelf-none", None),
            ] {
                sqlx::query(
                    "INSERT INTO bookshelf_items (site_id, database_id, title, owner_sub) \
                     VALUES (?1, ?2, '本', ?3)",
                )
                .bind(site)
                .bind(database_id)
                .bind(owner)
                .execute(&pool)
                .await
                .unwrap();
            }
            // book_first_events は book_first_events.bookshelf_items への FK を持つ子テーブル。
            sqlx::query(
                "INSERT INTO book_first_events (site_id, database_id, first_event_name, owner_sub) \
                 VALUES ('booth', 'shelf-a', '技術書典20', ?1)",
            )
            .bind(&owner_a)
            .execute(&pool)
            .await
            .unwrap();
            for (id, owner) in [
                ("check-a", Some(&owner_a)),
                ("check-b", Some(&owner_b)),
                ("check-none", None),
            ] {
                sqlx::query(
                    "INSERT INTO checked_items (id, event_id, circle_name, owner_sub) \
                     VALUES (?1, 'tbf20', 'サークル', ?2)",
                )
                .bind(id)
                .bind(owner)
                .execute(&pool)
                .await
                .unwrap();
            }
            for (tag, owner) in [
                ("tag-a", Some(&owner_a)),
                ("tag-b", Some(&owner_b)),
                ("tag-none", None),
            ] {
                sqlx::query("INSERT INTO favorite_tags (tag_name, owner_sub) VALUES (?1, ?2)")
                    .bind(tag)
                    .bind(owner)
                    .execute(&pool)
                    .await
                    .unwrap();
            }
            for (name, owner) in [
                ("circle-a", Some(&owner_a)),
                ("circle-b", Some(&owner_b)),
                ("circle-none", None),
            ] {
                sqlx::query(
                    "INSERT INTO favorite_entities (entity_kind, entity_name, owner_sub) \
                     VALUES ('circle', ?1, ?2)",
                )
                .bind(name)
                .bind(owner)
                .execute(&pool)
                .await
                .unwrap();
            }
        });

        let filter_a = OwnerFilter {
            key: &key,
            sub: Some("A"),
        };
        let json = export_json(&pool, None, Some(&filter_a)).unwrap();
        let value: Value = serde_json::from_str(&json).unwrap();
        let ids = |table: &str, column: &str| -> Vec<String> {
            value[table]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row[column].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(ids("bookshelf_items", "database_id"), vec!["shelf-a"]);
        assert_eq!(ids("book_first_events", "database_id"), vec!["shelf-a"]);
        assert_eq!(ids("checked_items", "id"), vec!["check-a"]);
        assert_eq!(ids("favorite_tags", "tag_name"), vec!["tag-a"]);
        assert_eq!(ids("favorite_entities", "entity_name"), vec!["circle-a"]);

        // 未ログイン（sub = None）は未所属の行だけ。他アカウントの行は出ない。
        let filter_anon = OwnerFilter {
            key: &key,
            sub: None,
        };
        let json = export_json(&pool, None, Some(&filter_anon)).unwrap();
        let value: Value = serde_json::from_str(&json).unwrap();
        let ids = |table: &str, column: &str| -> Vec<String> {
            value[table]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row[column].as_str().unwrap().to_string())
                .collect()
        };
        assert_eq!(ids("bookshelf_items", "database_id"), vec!["shelf-none"]);
        assert_eq!(ids("checked_items", "id"), vec!["check-none"]);
        assert_eq!(ids("favorite_tags", "tag_name"), vec!["tag-none"]);
        assert_eq!(ids("favorite_entities", "entity_name"), vec!["circle-none"]);
        assert!(ids("book_first_events", "database_id").is_empty());
    }

    /// `attribute_owner` は未所属（NULL）の行だけを埋める。
    ///
    /// 既に所有者が付いた行を上書きすると、A の購入一覧が B の同期で B のものに
    /// 化けて B のバックアップに混ざる。
    #[test]
    fn attribute_owner_never_rewrites_an_existing_owner() {
        let pool = crate::db::test_pool();
        crate::db::migrate(&pool).unwrap();
        let key = [31u8; 32];
        let owner_a = crate::owner::encrypt(&key, "A");
        let owner_b = crate::owner::encrypt(&key, "B");

        crate::db::block_on(async {
            sqlx::query(
                "INSERT INTO bookshelf_items (site_id, database_id, title, owner_sub) \
                 VALUES ('booth', 'already-a', '本', ?1), ('booth', 'unowned', '本', NULL)",
            )
            .bind(&owner_a)
            .execute(&pool)
            .await
            .unwrap();
        });

        // B として同期しても、A の行は A のまま。未所属の行だけが B になる。
        let claimed =
            crate::db::bookshelf::attribute_owner(&pool, "booth", Some(&owner_b)).unwrap();
        assert_eq!(claimed, 1, "書き換えてよいのは未所属の 1 行だけ");
        let rows: Vec<(String, Option<String>)> = crate::db::block_on(async {
            sqlx::query_as(
                "SELECT database_id, owner_sub FROM bookshelf_items ORDER BY database_id",
            )
            .fetch_all(&pool)
            .await
            .unwrap()
        });
        assert_eq!(rows[0].1.as_deref(), Some(owner_a.as_str()));
        assert_eq!(rows[1].1.as_deref(), Some(owner_b.as_str()));
    }

    // ---- Drive のバックアップファイル（v3 の封筒 / v2 の平文） -----------------

    const OWNER_ID: &str = "6366bfc3b6ab37feaf2adb385aeaa515c4aa52cf09e70cac890d888e4409f3b0";

    fn plain_backup() -> String {
        r#"{"format_version":2,"books":[{"id":"book-1","title":"テスト本"}]}"#.to_string()
    }

    #[test]
    fn plaintext_backups_are_still_readable() {
        // v2（平文）は今までどおり読める。基準値は従来の正規形 md5。
        let json = plain_backup();
        let backup = DriveBackup::parse(json.as_bytes()).unwrap();
        assert!(!backup.is_encrypted());
        assert_eq!(backup.owner_id(), None);
        let token = backup.change_token().unwrap();
        assert_eq!(token, canonical_md5_str(&json, None).unwrap());
        assert_eq!(backup.plaintext(None).unwrap(), json);
    }

    #[test]
    fn encrypted_backups_need_the_right_root_key() {
        let root = PackRootKey::from_bytes([3u8; 32]);
        let json = plain_backup();
        let envelope =
            BackupEnvelope::seal(json.as_bytes(), &root, OWNER_ID).to_json().unwrap();

        let backup = DriveBackup::parse(&envelope).unwrap();
        assert!(backup.is_encrypted());
        assert_eq!(backup.owner_id(), Some(OWNER_ID));
        // 基準値は封筒の `content_hmac`（暗号文の md5 ではない）
        let token = backup.change_token().unwrap();
        assert_eq!(token.len(), 64);
        assert_ne!(token, format!("{:x}", md5::compute(&envelope)));

        // 鍵が無ければ復号しない（平文を返さない）
        assert!(matches!(
            backup.clone().plaintext(None),
            Err(BackupError::KeyRequired)
        ));
        // 別の鍵でも復号しない
        let other = PackRootKey::from_bytes([4u8; 32]);
        assert!(matches!(
            backup.clone().plaintext(Some(&other)),
            Err(BackupError::Rejected)
        ));
        // 正しい鍵なら元の平文に戻る
        assert_eq!(backup.plaintext(Some(&root)).unwrap(), json);
    }

    #[test]
    fn encrypted_backups_are_never_treated_as_plaintext() {
        // 壊れた封筒・未知の版はエラーにする（暗号文を内容として取り込まない）
        let root = PackRootKey::from_bytes([5u8; 32]);
        let envelope = BackupEnvelope::seal(b"{}", &root, OWNER_ID)
            .to_json()
            .unwrap();
        let json = String::from_utf8(envelope).unwrap();

        let bumped = json.replace("\"format_version\":3", "\"format_version\":9");
        assert!(matches!(
            DriveBackup::parse(bumped.as_bytes()),
            Err(BackupError::UnsupportedVersion(9))
        ));
        // encryption があるのに版が無い
        let no_version = json.replace("\"format_version\":3,", "");
        assert!(matches!(
            DriveBackup::parse(no_version.as_bytes()),
            Err(BackupError::Corrupt(_))
        ));
        // 中身が base64 でない
        let broken = json.replace("aes-256-gcm", "rot13");
        assert!(matches!(
            DriveBackup::parse(broken.as_bytes()),
            Err(BackupError::Corrupt(_))
        ));
        // JSON ですらない
        assert!(matches!(
            DriveBackup::parse(b"not json"),
            Err(BackupError::Corrupt(_))
        ));
        // `encryption` の型が違っても平文には落とさない（安全側に倒す）
        let wrong_type = br#"{"format_version":3,"encryption":"aes-256-gcm","content_hmac":"00"}"#;
        assert!(matches!(
            DriveBackup::parse(wrong_type),
            Err(BackupError::Corrupt(_))
        ));
        // 版が無ければ平文として読む（v2 以前のバックアップ）
        let legacy = br#"{"books":[{"id":"book-1"}]}"#;
        assert!(matches!(
            DriveBackup::parse(legacy),
            Ok(DriveBackup::Plain(_))
        ));
    }

    #[test]
    fn an_envelope_plaintext_is_importable() {
        // 封筒の中身は通常のバックアップ JSON なので、復元経路（import_json）に載る
        let pool = crate::db::test_pool();
        crate::db::migrate(&pool).unwrap();
        let source = crate::db::books::list(&pool).unwrap();
        assert!(source.is_empty());
        crate::db::books::insert(&pool, &test_book("book-1")).unwrap();
        let json = export_json(&pool, None, None).unwrap();

        let root = PackRootKey::from_bytes([6u8; 32]);
        let envelope = BackupEnvelope::seal(json.as_bytes(), &root, OWNER_ID)
            .to_json()
            .unwrap();
        let restored = DriveBackup::parse(&envelope)
            .unwrap()
            .plaintext(Some(&root))
            .unwrap();
        assert_eq!(restored, json);
    }

    fn test_book(id: &str) -> crate::db::books::Book {
        crate::db::books::Book {
            id: id.into(),
            title: "復元される本".into(),
            author: String::new(),
            circle_name: String::new(),
            purchase_date: None,
            file_name: format!("{id}.opfspack"),
            file_size: 1,
            opfs_path: format!("{id}.opfspack"),
            cover_thumbnail: None,
            tbf_product_id: None,
            site_id: None,
            tags_fetched: 1,
            pack_id: Some(id.into()),
            is_favorite: 0,
            is_hidden: 0,
            created_at: "2026-09-01 00:00:00".into(),
            updated_at: "2026-09-01 00:00:00".into(),
            media_category: None,
            ai_type: None,
            is_drm: 0,
            release_date: None,
            description: None,
            theme: None,
            maker_id: None,
            page_count: None,
            age_rating: None,
            series_name: None,
        }
    }

    /// `poll_sync_enabled` 列を持たない古いバックアップを復元したとき、
    /// 注目イベントは既定（1 = ポーリング対象）で復元される
    /// （列が無いとスキーマの DEFAULT 0 になり、5 分ポーラーが動かない）。
    #[test]
    fn import_json_fills_poll_default_for_legacy_tbf_events() {
        let pool = crate::db::test_pool();
        let payload = serde_json::json!({
            "format_version": 2,
            "tbf_events": [
                {
                    "id": "tbf20", "site_id": "techbookfest", "slug": "tbf20",
                    "tbf_event_id": "Event:tbf20", "event_name": "技術書典20",
                    "event_date": "2026-04-11", "event_start_date": "2026-04-11",
                    "event_end_date": "2026-04-26", "event_format": "hybrid",
                    "is_cancelled": 0, "display_order": 0, "is_featured": 1,
                    "created_at": "2026-08-23 00:00:00", "updated_at": "2026-08-23 00:00:00"
                },
                {
                    "id": "tbf19", "site_id": "techbookfest", "slug": "tbf19",
                    "tbf_event_id": "Event:tbf19", "event_name": "技術書典19",
                    "event_date": "2025-11-15", "event_start_date": "2025-11-15",
                    "event_end_date": "2025-11-30", "event_format": "hybrid",
                    "is_cancelled": 0, "display_order": 1, "is_featured": 0,
                    "created_at": "2026-08-23 00:00:00", "updated_at": "2026-08-23 00:00:00"
                }
            ]
        });
        import_json(&pool, &payload.to_string()).unwrap();
        assert_eq!(
            crate::db::checklist::list_enabled_slugs(&pool).unwrap(),
            vec!["tbf20".to_string()]
        );
    }

    /// 列を持つ（新しい）バックアップのユーザー設定は、復元で勝手に変えない。
    #[test]
    fn import_json_keeps_poll_setting_from_backup() {
        let pool = crate::db::test_pool();
        let payload = serde_json::json!({
            "format_version": 3,
            "tbf_events": [
                {
                    "id": "tbf20", "site_id": "techbookfest", "slug": "tbf20",
                    "tbf_event_id": "Event:tbf20", "event_name": "技術書典20",
                    "event_date": "2026-04-11", "event_start_date": "2026-04-11",
                    "event_end_date": "2026-04-26", "event_format": "hybrid",
                    "is_cancelled": 0, "display_order": 0, "is_featured": 1,
                    "poll_sync_enabled": 0,
                    "created_at": "2026-08-23 00:00:00", "updated_at": "2026-08-23 00:00:00"
                }
            ]
        });
        import_json(&pool, &payload.to_string()).unwrap();
        assert!(crate::db::checklist::list_enabled_slugs(&pool).unwrap().is_empty());
    }
}
