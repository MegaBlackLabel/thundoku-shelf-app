//! `favorite_entities` repository: お気に入りのサークル / 作者。
//!
//! タグのお気に入り（`favorite_tags`）と同じく、本棚チップのハートで登録する。
//! 名前は種別（サークル / 作者）ごとに独立して扱う。

use sqlx::Row;

use crate::db::SqlitePool;

/// お気に入り対象の種別。`favorite_entities.entity_kind` の値に対応する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntityKind {
    Circle,
    Author,
}

impl EntityKind {
    /// DB に保存する種別名。
    pub fn as_str(self) -> &'static str {
        match self {
            EntityKind::Circle => "circle",
            EntityKind::Author => "author",
        }
    }
}

/// サークル / 作者のお気に入りを登録・解除する（登録は冪等）。
pub fn set_favorite(
    pool: &SqlitePool,
    kind: EntityKind,
    name: &str,
    favorite: bool,
) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        if favorite {
            sqlx::query(
                "INSERT INTO favorite_entities (entity_kind, entity_name) VALUES (?1, ?2) \
                 ON CONFLICT(entity_kind, entity_name) DO NOTHING",
            )
            .bind(kind.as_str())
            .bind(name)
            .execute(pool)
            .await?;
        } else {
            sqlx::query(
                "DELETE FROM favorite_entities WHERE entity_kind = ?1 AND entity_name = ?2",
            )
            .bind(kind.as_str())
            .bind(name)
            .execute(pool)
            .await?;
        }
        Ok(())
    })
}

/// 指定した種別のお気に入り名（名前順）。
pub fn list_favorites(pool: &SqlitePool, kind: EntityKind) -> Result<Vec<String>, sqlx::Error> {
    crate::db::block_on(async {
        let rows = sqlx::query(
            "SELECT entity_name FROM favorite_entities WHERE entity_kind = ?1 ORDER BY entity_name",
        )
        .bind(kind.as_str())
        .fetch_all(pool)
        .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(row.get::<String, _>(0));
        }
        Ok(out)
    })
}
