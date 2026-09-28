//! 表紙バンドル（`thundoku-thumbs.json`）に載せる画像の派生キャッシュ。
//!
//! Drive へ上げるためにエンコード済みの画像（本棚の表紙 = 256px WebP、
//! チェックリスト = 既存の 256px JPEG）を保持する。**DB バックアップの対象外**
//! （`db::backup::TABLES` の許可リストに入れない）: 復元先では空から作り直せる
//! 派生データで、バックアップ JSON を太らせる意味がない。
//!
//! キーは本棚 = `{site_id}:{database_id}`、チェックリスト = `checked_items.id`。
//! `owner_sub` は親テーブル（`bookshelf_items` / `checked_items`）にしか無いため、
//! 所有者フィルタは親を復号して求めたキー集合（[`OwnedKeys`]）で行う
//! （`db::backup` の `OWNER_SCOPED_TABLES` と同じ流儀）。

use std::collections::HashSet;

use sqlx::Row;

use crate::db::{SqlitePool, block_on};

/// 本棚の表紙（`bookshelf_items` の `site_id` + `database_id`）。
pub const KIND_SHELF: &str = "shelf";
/// チェックリストのサムネイル（`checked_items.id`）。
pub const KIND_CHECKLIST: &str = "checklist";

/// バンドルに載せる 1 枚（アップロード用にエンコード済み）。
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThumbEntry {
    /// [`KIND_SHELF`] / [`KIND_CHECKLIST`]。
    pub kind: String,
    /// 本棚 = `{site_id}:{database_id}` / チェックリスト = 項目 id。
    pub key: String,
    /// `image/webp`（本棚）/ `image/jpeg`（チェックリスト）。
    pub mime: String,
    pub width: u32,
    pub height: u32,
    /// エンコード後バイト列の SHA-256（小文字 hex）。Web 側の重複排除に使える。
    pub sha256: String,
    /// 画像そのもの。
    pub bytes: Vec<u8>,
    /// 生成元（448px PNG キャッシュなど）の mtime（秒）。変化検出に使う。
    pub source_mtime: Option<i64>,
    /// 生成元のサイズ（バイト）。同上。
    pub source_size: Option<i64>,
}

/// 現在の sub に帰属するキー集合（[`owned_keys`] の結果）。
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OwnedKeys {
    /// 本棚（`{site_id}:{database_id}`）。
    pub shelf: HashSet<String>,
    /// チェックリスト（`checked_items.id`）。
    pub checklist: HashSet<String>,
}

impl OwnedKeys {
    /// このキーを Drive へ上げてよいか。
    pub fn contains(&self, kind: &str, key: &str) -> bool {
        match kind {
            KIND_SHELF => self.shelf.contains(key),
            KIND_CHECKLIST => self.checklist.contains(key),
            _ => false,
        }
    }

    /// 所有する全キー `(kind, key)`。
    pub fn iter(&self) -> impl Iterator<Item = (&'static str, &str)> {
        self.shelf
            .iter()
            .map(|key| (KIND_SHELF, key.as_str()))
            .chain(
                self.checklist
                    .iter()
                    .map(|key| (KIND_CHECKLIST, key.as_str())),
            )
    }
}

/// 本棚の表紙のキー（`{site_id}:{database_id}`）。
///
/// サイト id は既知（`dlsite` / `fanza` / `booth` / `techbookfest`）で、
/// `database_id` に `:` は入らない（`crates/core/src/db` の ID 規約）。
pub fn shelf_key(site_id: &str, database_id: &str) -> String {
    format!("{site_id}:{database_id}")
}

/// 現在の sub が所有するキーを求める。
///
/// `owner_sub` は毎回 IV が変わる暗号文なので SQL では比較できず、親テーブルを
/// 復号して判定する（`db::backup::OwnerFilter` と同じ流儀）。`sub = None`
/// （未ログイン）は未所属（`owner_sub IS NULL`）の行だけを指す。
pub fn owned_keys(
    pool: &SqlitePool,
    key: &[u8; 32],
    sub: Option<&str>,
) -> Result<OwnedKeys, sqlx::Error> {
    let shelf_rows = block_on(async {
        sqlx::query("SELECT site_id, database_id, owner_sub FROM bookshelf_items")
            .fetch_all(pool)
            .await
    })?;
    let mut shelf = HashSet::new();
    for row in shelf_rows {
        let site_id: String = row.try_get("site_id")?;
        let database_id: String = row.try_get("database_id")?;
        let blob: Option<String> = row.try_get("owner_sub")?;
        if crate::owner::matches(key, blob.as_deref(), sub) {
            shelf.insert(shelf_key(&site_id, &database_id));
        }
    }

    let checklist_rows = block_on(async {
        sqlx::query("SELECT id, owner_sub FROM checked_items")
            .fetch_all(pool)
            .await
    })?;
    let mut checklist = HashSet::new();
    for row in checklist_rows {
        let id: String = row.try_get("id")?;
        let blob: Option<String> = row.try_get("owner_sub")?;
        if crate::owner::matches(key, blob.as_deref(), sub) {
            checklist.insert(id);
        }
    }

    Ok(OwnedKeys { shelf, checklist })
}

/// 所有するキーの [`ThumbEntry`]（バイト列込み）。
///
/// 親（`bookshelf_items` / `checked_items`）が消えたキーの行は返さない
/// （キャッシュが残っていても Drive へは上げない）。
pub fn entries_for_owner(
    pool: &SqlitePool,
    owned: &OwnedKeys,
) -> Result<Vec<ThumbEntry>, sqlx::Error> {
    let rows = block_on(async {
        sqlx::query(
            "SELECT kind, item_key, mime, width, height, sha256, bytes, source_mtime, \
             source_size FROM thumbnail_share ORDER BY kind, item_key",
        )
        .fetch_all(pool)
        .await
    })?;
    let mut out = Vec::new();
    for row in rows {
        let kind: String = row.try_get("kind")?;
        let key: String = row.try_get("item_key")?;
        if !owned.contains(&kind, &key) {
            continue;
        }
        out.push(ThumbEntry {
            kind,
            key,
            mime: row.try_get("mime")?,
            width: row.try_get::<i64, _>("width")?.max(0) as u32,
            height: row.try_get::<i64, _>("height")?.max(0) as u32,
            sha256: row.try_get("sha256")?,
            bytes: row.try_get("bytes")?,
            source_mtime: row.try_get("source_mtime")?,
            source_size: row.try_get("source_size")?,
        });
    }
    Ok(out)
}

/// 1 枚を保存する（同じ `(kind, key)` は置き換える）。
pub fn upsert(pool: &SqlitePool, entry: &ThumbEntry) -> Result<(), sqlx::Error> {
    let now = chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string();
    block_on(async {
        sqlx::query(
            "INSERT INTO thumbnail_share (kind, item_key, mime, width, height, sha256, bytes, \
             source_mtime, source_size, updated_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10) \
             ON CONFLICT(kind, item_key) DO UPDATE SET \
               mime = excluded.mime, width = excluded.width, height = excluded.height, \
               sha256 = excluded.sha256, bytes = excluded.bytes, \
               source_mtime = excluded.source_mtime, source_size = excluded.source_size, \
               updated_at = excluded.updated_at",
        )
        .bind(&entry.kind)
        .bind(&entry.key)
        .bind(&entry.mime)
        .bind(i64::from(entry.width))
        .bind(i64::from(entry.height))
        .bind(&entry.sha256)
        .bind(&entry.bytes)
        .bind(entry.source_mtime)
        .bind(entry.source_size)
        .bind(&now)
        .execute(pool)
        .await
        .map(|_| ())
    })
}

/// 1 枚を読む（存在しなければ `None`）。
pub fn get(pool: &SqlitePool, kind: &str, key: &str) -> Result<Option<ThumbEntry>, sqlx::Error> {
    let row = block_on(async {
        sqlx::query(
            "SELECT kind, item_key, mime, width, height, sha256, bytes, source_mtime, \
             source_size FROM thumbnail_share WHERE kind = ?1 AND item_key = ?2",
        )
        .bind(kind)
        .bind(key)
        .fetch_optional(pool)
        .await
    })?;
    let Some(row) = row else {
        return Ok(None);
    };
    Ok(Some(ThumbEntry {
        kind: row.try_get("kind")?,
        key: row.try_get("item_key")?,
        mime: row.try_get("mime")?,
        width: row.try_get::<i64, _>("width")?.max(0) as u32,
        height: row.try_get::<i64, _>("height")?.max(0) as u32,
        sha256: row.try_get("sha256")?,
        bytes: row.try_get("bytes")?,
        source_mtime: row.try_get("source_mtime")?,
        source_size: row.try_get("source_size")?,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{SqlitePool, block_on, test_pool};

    const KEY: [u8; 32] = [29u8; 32];

    fn shelf_entry(database_id: &str, bytes: &[u8]) -> ThumbEntry {
        ThumbEntry {
            kind: KIND_SHELF.to_string(),
            key: shelf_key("dlsite", database_id),
            mime: "image/webp".to_string(),
            width: 256,
            height: 360,
            sha256: format!("sha-{database_id}"),
            bytes: bytes.to_vec(),
            source_mtime: Some(1_700_000_000),
            source_size: Some(274_000),
        }
    }

    fn checklist_entry(id: &str, bytes: &[u8]) -> ThumbEntry {
        ThumbEntry {
            kind: KIND_CHECKLIST.to_string(),
            key: id.to_string(),
            mime: "image/jpeg".to_string(),
            width: 256,
            height: 256,
            sha256: format!("sha-{id}"),
            bytes: bytes.to_vec(),
            source_mtime: None,
            source_size: None,
        }
    }

    fn seed_shelf_item(pool: &SqlitePool, database_id: &str, owner_sub: Option<&str>) {
        block_on(async {
            sqlx::query(
                "INSERT INTO bookshelf_items (site_id, database_id, title, owner_sub) \
                 VALUES ('dlsite', ?1, '本', ?2)",
            )
            .bind(database_id)
            .bind(owner_sub)
            .execute(pool)
            .await
            .unwrap();
        });
    }

    fn seed_checklist_item(pool: &SqlitePool, id: &str, owner_sub: Option<&str>) {
        block_on(async {
            sqlx::query(
                "INSERT INTO checked_items (id, event_id, circle_name, owner_sub) \
                 VALUES (?1, 'tbf20', 'サークル', ?2)",
            )
            .bind(id)
            .bind(owner_sub)
            .execute(pool)
            .await
            .unwrap();
        });
    }

    fn seed_event(pool: &SqlitePool) {
        block_on(async {
            sqlx::query(
                "INSERT INTO tbf_events (id, site_id, event_name) \
                 VALUES ('tbf20', 'techbookfest', '技術書典20')",
            )
            .execute(pool)
            .await
            .unwrap();
        });
    }

    #[test]
    fn upsert_round_trips_every_field() {
        let pool = test_pool();
        let entry = shelf_entry("RJ1", b"cover-bytes");
        upsert(&pool, &entry).unwrap();

        assert_eq!(
            get(&pool, KIND_SHELF, "dlsite:RJ1").unwrap(),
            Some(entry),
            "保存した列が 1 つでも欠けるとバンドルの判定と画像が壊れる"
        );
        assert_eq!(get(&pool, KIND_SHELF, "dlsite:RJ2").unwrap(), None);
        assert_eq!(
            get(&pool, KIND_CHECKLIST, "dlsite:RJ1").unwrap(),
            None,
            "kind が違えば別の行"
        );
    }

    #[test]
    fn upsert_replaces_the_same_key() {
        let pool = test_pool();
        upsert(&pool, &shelf_entry("RJ1", b"old")).unwrap();
        upsert(&pool, &shelf_entry("RJ1", b"new")).unwrap();

        let entry = get(&pool, KIND_SHELF, "dlsite:RJ1").unwrap().unwrap();
        assert_eq!(
            entry.bytes, b"new",
            "表紙を取り直したら古いバイト列を残さない"
        );
        assert_eq!(entry.sha256, "sha-RJ1");
    }

    #[test]
    fn entries_for_owner_keeps_only_the_current_sub() {
        let pool = test_pool();
        seed_event(&pool);
        let owner_a = crate::owner::encrypt(&KEY, "A");
        let owner_b = crate::owner::encrypt(&KEY, "B");
        seed_shelf_item(&pool, "RJ-a", Some(&owner_a));
        seed_shelf_item(&pool, "RJ-b", Some(&owner_b));
        seed_shelf_item(&pool, "RJ-none", None);
        seed_checklist_item(&pool, "check-a", Some(&owner_a));
        seed_checklist_item(&pool, "check-b", Some(&owner_b));
        for id in ["RJ-a", "RJ-b", "RJ-none"] {
            upsert(&pool, &shelf_entry(id, b"cover")).unwrap();
        }
        upsert(&pool, &checklist_entry("check-a", b"check")).unwrap();
        upsert(&pool, &checklist_entry("check-b", b"check")).unwrap();

        let owned = owned_keys(&pool, &KEY, Some("A")).unwrap();
        assert_eq!(
            owned.shelf,
            ["dlsite:RJ-a".to_string()].into_iter().collect(),
            "他アカウントの本棚が混ざっている"
        );
        assert_eq!(
            owned.checklist,
            ["check-a".to_string()].into_iter().collect(),
            "他アカウントのチェックリストが混ざっている"
        );

        let entries = entries_for_owner(&pool, &owned).unwrap();
        assert_eq!(
            entries.len(),
            2,
            "所有分だけが返る（実際: {:?}）",
            entries.iter().map(|e| &e.key).collect::<Vec<_>>()
        );
        assert!(entries.iter().any(|e| e.key == "dlsite:RJ-a"));
        assert!(entries.iter().any(|e| e.key == "check-a"));
    }

    #[test]
    fn entries_for_owner_without_login_keeps_unowned_rows() {
        let pool = test_pool();
        let owner_a = crate::owner::encrypt(&KEY, "A");
        seed_shelf_item(&pool, "RJ-a", Some(&owner_a));
        seed_shelf_item(&pool, "RJ-none", None);
        upsert(&pool, &shelf_entry("RJ-a", b"cover")).unwrap();
        upsert(&pool, &shelf_entry("RJ-none", b"cover")).unwrap();

        let owned = owned_keys(&pool, &KEY, None).unwrap();
        assert_eq!(
            owned.shelf,
            ["dlsite:RJ-none".to_string()].into_iter().collect(),
            "未ログインでは未所属の行だけ（復号できない行は含めない）"
        );
        assert_eq!(entries_for_owner(&pool, &owned).unwrap().len(), 1);
    }

    #[test]
    fn entries_for_owner_drops_shares_without_a_parent() {
        let pool = test_pool();
        // 本棚から消えた本のキャッシュ（親が無い行）は上げない
        upsert(&pool, &shelf_entry("RJ-gone", b"cover")).unwrap();
        upsert(&pool, &checklist_entry("check-gone", b"check")).unwrap();

        let owned = owned_keys(&pool, &KEY, Some("A")).unwrap();
        assert!(owned.shelf.is_empty() && owned.checklist.is_empty());
        assert!(entries_for_owner(&pool, &owned).unwrap().is_empty());
    }
}
