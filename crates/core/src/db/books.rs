//! `books` repository.

use crate::db::SqlitePool;

#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Book {
    pub id: String,
    pub title: String,
    pub author: String,
    pub circle_name: String,
    pub purchase_date: Option<String>,
    pub file_name: String,
    pub file_size: i64,
    pub opfs_path: String,
    pub cover_thumbnail: Option<String>,
    pub tbf_product_id: Option<String>,
    pub site_id: Option<String>,
    pub tags_fetched: i64,
    pub pack_id: Option<String>,
    pub is_favorite: i64,
    pub is_hidden: i64,
    pub created_at: String,
    pub updated_at: String,
    // 共有ソースメタ列（FANZA同人 / DLsite）
    pub media_category: Option<String>,
    pub ai_type: Option<String>,
    pub is_drm: i64,
    pub release_date: Option<String>,
    pub description: Option<String>,
    pub theme: Option<String>,
    pub maker_id: Option<String>,
    pub page_count: Option<i64>,
    pub age_rating: Option<String>,
    pub series_name: Option<String>,
}

const COLUMNS: &str = "id, title, author, circle_name, purchase_date, file_name, file_size, \
     opfs_path, cover_thumbnail, tbf_product_id, site_id, tags_fetched, pack_id, \
     is_favorite, is_hidden, created_at, updated_at, media_category, ai_type, is_drm, \
     release_date, description, theme, maker_id, page_count, age_rating, series_name";

/// 本の綴じ方向（右綴じ = 右→左、左綴じ = 左→右）。
///
/// 既定はサイト単位の設定（`viewer.page_turn.{site}`）で決まり、`books.page_turn` に
/// 入るのは**ビューアでユーザーが変えた本だけ**（`NULL` = サイトの設定に従う）。
/// 保存文字列はサイト別設定と共有する。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageTurn {
    /// 右綴じ（右から左へ読む。日本の本・同人漫画）
    RightToLeft,
    /// 左綴じ（左から右へ読む。洋書・技術書典の本）
    LeftToRight,
}

impl PageTurn {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RightToLeft => "right-to-left",
            Self::LeftToRight => "left-to-right",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "right-to-left" => Some(Self::RightToLeft),
            "left-to-right" => Some(Self::LeftToRight),
            _ => None,
        }
    }

    pub fn is_right_to_left(self) -> bool {
        matches!(self, Self::RightToLeft)
    }
}

pub fn insert(pool: &SqlitePool, book: &Book) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(&format!(
            "INSERT INTO books ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, \
             ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27)"
        ))
        .bind(&book.id)
        .bind(&book.title)
        .bind(&book.author)
        .bind(&book.circle_name)
        .bind(&book.purchase_date)
        .bind(&book.file_name)
        .bind(book.file_size)
        .bind(&book.opfs_path)
        .bind(&book.cover_thumbnail)
        .bind(&book.tbf_product_id)
        .bind(&book.site_id)
        .bind(book.tags_fetched)
        .bind(&book.pack_id)
        .bind(book.is_favorite)
        .bind(book.is_hidden)
        .bind(&book.created_at)
        .bind(&book.updated_at)
        .bind(&book.media_category)
        .bind(&book.ai_type)
        .bind(book.is_drm)
        .bind(&book.release_date)
        .bind(&book.description)
        .bind(&book.theme)
        .bind(&book.maker_id)
        .bind(book.page_count)
        .bind(&book.age_rating)
        .bind(&book.series_name)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// Insert or replace an existing row by id (drive sync import uses this).
pub fn upsert(pool: &SqlitePool, book: &Book) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(&format!(
            "INSERT INTO books ({COLUMNS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, \
             ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27)
             ON CONFLICT(id) DO UPDATE SET
               title = excluded.title,
               author = excluded.author,
               circle_name = excluded.circle_name,
               purchase_date = excluded.purchase_date,
               file_name = excluded.file_name,
               file_size = excluded.file_size,
               opfs_path = excluded.opfs_path,
               cover_thumbnail = excluded.cover_thumbnail,
               tbf_product_id = excluded.tbf_product_id,
               site_id = excluded.site_id,
               tags_fetched = excluded.tags_fetched,
               pack_id = excluded.pack_id,
               updated_at = excluded.updated_at,
               media_category = excluded.media_category,
               ai_type = excluded.ai_type,
               is_drm = excluded.is_drm,
               release_date = excluded.release_date,
               description = excluded.description,
               theme = excluded.theme,
               maker_id = excluded.maker_id,
               page_count = excluded.page_count,
               age_rating = excluded.age_rating,
               series_name = excluded.series_name"
        ))
        .bind(&book.id)
        .bind(&book.title)
        .bind(&book.author)
        .bind(&book.circle_name)
        .bind(&book.purchase_date)
        .bind(&book.file_name)
        .bind(book.file_size)
        .bind(&book.opfs_path)
        .bind(&book.cover_thumbnail)
        .bind(&book.tbf_product_id)
        .bind(&book.site_id)
        .bind(book.tags_fetched)
        .bind(&book.pack_id)
        .bind(book.is_favorite)
        .bind(book.is_hidden)
        .bind(&book.created_at)
        .bind(&book.updated_at)
        .bind(&book.media_category)
        .bind(&book.ai_type)
        .bind(book.is_drm)
        .bind(&book.release_date)
        .bind(&book.description)
        .bind(&book.theme)
        .bind(&book.maker_id)
        .bind(book.page_count)
        .bind(&book.age_rating)
        .bind(&book.series_name)
        .execute(pool)
        .await?;
        Ok(())
    })
}

pub fn get(pool: &SqlitePool, id: &str) -> Result<Option<Book>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, Book>(&format!("SELECT {COLUMNS} FROM books WHERE id = ?1"))
            .bind(id)
            .fetch_optional(pool)
            .await
    })
}

pub fn list(pool: &SqlitePool) -> Result<Vec<Book>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, Book>(&format!(
            "SELECT {COLUMNS} FROM books ORDER BY created_at DESC"
        ))
        .fetch_all(pool)
        .await
    })
}

/// インポート後に本のメタ（タイトル・作者・サークル・購入日）を上書きする。
/// インポートはファイル名由来で作るため、FANZA 等はリモート値で補完する。
pub fn set_metadata(
    pool: &SqlitePool,
    id: &str,
    title: &str,
    author: &str,
    circle_name: &str,
    purchase_date: Option<String>,
) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "UPDATE books SET title = ?1, author = ?2, circle_name = ?3, purchase_date = ?4, \
             updated_at = CURRENT_TIMESTAMP WHERE id = ?5",
        )
        .bind(title)
        .bind(author)
        .bind(circle_name)
        .bind(purchase_date)
        .bind(id)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// Link a locally downloaded book to its techbookfest shelf item
/// (`bookshelf_items.database_id`), so the shelf card resolves as downloaded.
pub fn set_tbf_product_id(
    pool: &SqlitePool,
    book_id: &str,
    tbf_product_id: &str,
) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query("UPDATE books SET tbf_product_id = ?1 WHERE id = ?2")
            .bind(tbf_product_id)
            .bind(book_id)
            .execute(pool)
            .await?;
        Ok(())
    })
}

/// インポート後にソース側の共有メタ列（media_category / ai_type / is_drm / release_date /
/// description / theme / maker_id / page_count / age_rating / series_name）を上書きする。
/// FANZA同人 / DLsite のリッチメタ補完に使う。
#[allow(clippy::too_many_arguments)]
pub fn set_source_metadata(
    pool: &SqlitePool,
    id: &str,
    media_category: Option<&str>,
    ai_type: Option<&str>,
    is_drm: i64,
    release_date: Option<&str>,
    description: Option<&str>,
    theme: Option<&str>,
    maker_id: Option<&str>,
    page_count: Option<i64>,
    age_rating: Option<&str>,
    series_name: Option<&str>,
) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "UPDATE books SET media_category = ?1, ai_type = ?2, is_drm = ?3, release_date = ?4, \
             description = ?5, theme = ?6, maker_id = ?7, page_count = ?8, age_rating = ?9, \
             series_name = ?10, updated_at = CURRENT_TIMESTAMP WHERE id = ?11",
        )
        .bind(media_category)
        .bind(ai_type)
        .bind(is_drm)
        .bind(release_date)
        .bind(description)
        .bind(theme)
        .bind(maker_id)
        .bind(page_count)
        .bind(age_rating)
        .bind(series_name)
        .bind(id)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// Set the (encrypted) owner sub for a book. `Some` = その sub で暗号化された pack、
/// `None` = 未所属（未暗号化）。呼び出し側で暗号化済み blob を渡す（不透明な文字列）。
pub fn set_owner_sub(
    pool: &SqlitePool,
    id: &str,
    owner_sub: Option<String>,
) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "UPDATE books SET owner_sub = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
        )
        .bind(&owner_sub)
        .bind(id)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// Read the (encrypted) owner sub of a book. `None` = 未所属 or 行なし。
pub fn get_owner_sub(pool: &SqlitePool, id: &str) -> Result<Option<String>, sqlx::Error> {
    crate::db::block_on(async {
        let row: Option<Option<String>> =
            sqlx::query_scalar("SELECT owner_sub FROM books WHERE id = ?1")
                .bind(id)
                .fetch_optional(pool)
                .await?;
        Ok(row.flatten())
    })
}

/// `(book_id, owner_sub)` の一覧。表示・アップロード・バックアップの所有者フィルタに使う。
pub fn list_owner_subs(pool: &SqlitePool) -> Result<Vec<(String, Option<String>)>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, (String, Option<String>)>("SELECT id, owner_sub FROM books")
            .fetch_all(pool)
            .await
    })
}

/// 現在の表示/同期/バックアップ対象の book id 集合（P2）。
/// - `current_sub = Some(s)`：`s` に帰属する本。
/// - `current_sub = None`（未ログイン）：未所属（owner_sub IS NULL）の本。
///
/// 他アカウント・復号不能（未知）の本は除外する。
pub fn owned_book_ids(
    pool: &SqlitePool,
    key: &[u8; 32],
    current_sub: Option<&str>,
) -> Result<std::collections::HashSet<String>, sqlx::Error> {
    let rows = list_owner_subs(pool)?;
    let mut out = std::collections::HashSet::new();
    for (id, owner_sub) in rows {
        let owned = match current_sub {
            Some(s) => owner_sub
                .as_deref()
                .is_some_and(|b| crate::owner::decrypt(key, b).as_deref() == Some(s)),
            None => owner_sub.is_none(),
        };
        if owned {
            out.insert(id);
        }
    }
    Ok(out)
}

/// `(book_id, owner_sub)` で source（`site_id + tbf_product_id`）に一致する本（重複抑止用）。
pub fn find_by_source(
    pool: &SqlitePool,
    site_id: &str,
    tbf_product_id: &str,
) -> Result<Vec<(String, Option<String>)>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, (String, Option<String>)>(
            "SELECT id, owner_sub FROM books WHERE site_id = ?1 AND tbf_product_id = ?2",
        )
        .bind(site_id)
        .bind(tbf_product_id)
        .fetch_all(pool)
        .await
    })
}

/// 再DL時に使い回す既存 book id（P5）。
/// - `sub = Some(s)`：同一 source で `s` に帰属する行の id。
/// - `sub = None`（未ログイン）：同一 source で未所属（owner_sub IS NULL）の行の id。
///
/// **他の owner の行は更新対象にしない**（別途追加。自動変換はしない）。
pub fn resolve_reuse_id(
    pool: &SqlitePool,
    key: &[u8; 32],
    site_id: &str,
    tbf_product_id: &str,
    sub: Option<&str>,
) -> Result<Option<String>, sqlx::Error> {
    let rows = find_by_source(pool, site_id, tbf_product_id)?;
    for (id, owner_sub) in rows {
        let matched = match sub {
            Some(s) => owner_sub
                .as_deref()
                .is_some_and(|b| crate::owner::decrypt(key, b).as_deref() == Some(s)),
            None => owner_sub.is_none(),
        };
        if matched {
            return Ok(Some(id));
        }
    }
    Ok(None)
}

/// 本を削除する。
///
/// `book_tags` / `imported_documents`（およびその孫の `document_images` /
/// `document_text` / `token_analysis`）/ `book_contents`（孫の `content_formats`）は
/// `ON DELETE CASCADE` が無い（または既存 DB で付いていない）ため、**先に消す**。
/// これをしないと FK 制約で DELETE が失敗し、本が消えない
/// （呼び出し側が `let _ =` で握り潰すと無言で失敗する）。
pub fn delete(pool: &SqlitePool, id: &str) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        let mut tx = pool.begin().await?;
        // 孫 → 子 → 親 の順で消す
        for sql in [
            "DELETE FROM document_images WHERE document_id IN \
             (SELECT id FROM imported_documents WHERE book_id = ?1)",
            "DELETE FROM document_text WHERE document_id IN \
             (SELECT id FROM imported_documents WHERE book_id = ?1)",
            "DELETE FROM token_analysis WHERE document_id IN \
             (SELECT id FROM imported_documents WHERE book_id = ?1)",
            "DELETE FROM imported_documents WHERE book_id = ?1",
            "DELETE FROM content_formats WHERE content_id IN \
             (SELECT content_id FROM book_contents WHERE book_id = ?1)",
            "DELETE FROM book_contents WHERE book_id = ?1",
            "DELETE FROM book_tags WHERE book_id = ?1",
            "DELETE FROM books WHERE id = ?1",
        ] {
            sqlx::query(sql).bind(id).execute(&mut *tx).await?;
        }
        tx.commit().await?;
        Ok(())
    })
}

/// Set the favorite flag on a downloaded book.
pub fn set_favorite(pool: &SqlitePool, id: &str, favorite: bool) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "UPDATE books SET is_favorite = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
        )
        .bind(favorite as i64)
        .bind(id)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// Set the hidden flag on a downloaded book.
pub fn set_hidden(pool: &SqlitePool, id: &str, hidden: bool) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "UPDATE books SET is_hidden = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
        )
        .bind(hidden as i64)
        .bind(id)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// Drive のバックアップ（pack のアップロード）の対象から外す / 戻す。
///
/// 大きい pack は Drive の容量と転送時間を食うので、対象外にできる（同期の
/// アップロード方向がこの印を見て skip する）。取り込み時に一定サイズを超えたら
/// 自動で立てる（`import::MAX_BACKUP_PACK_BYTES`）。
pub fn set_backup_excluded(pool: &SqlitePool, id: &str, excluded: bool) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "UPDATE books SET backup_excluded = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
        )
        .bind(excluded as i64)
        .bind(id)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// Drive バックアップ対象外になっている本の id（同期のアップロード判定で使う）。
pub fn backup_excluded_ids(
    pool: &SqlitePool,
) -> Result<std::collections::HashSet<String>, sqlx::Error> {
    crate::db::block_on(async {
        let rows: Vec<(String,)> =
            sqlx::query_as("SELECT id FROM books WHERE backup_excluded = 1")
                .fetch_all(pool)
                .await?;
        Ok(rows.into_iter().map(|(id,)| id).collect())
    })
}

/// 1 冊が Drive バックアップ対象外か。
pub fn is_backup_excluded(pool: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
    crate::db::block_on(async {
        let excluded: Option<i64> =
            sqlx::query_scalar("SELECT backup_excluded FROM books WHERE id = ?1")
                .bind(id)
                .fetch_optional(pool)
                .await?;
        Ok(excluded.unwrap_or(0) != 0)
    })
}

/// ダウンロード元のサイト（techbookfest / booth）を books に記録する。
/// ビューアー設定のサイト別キー（viewer.mode.{site} 等）の解決に使う。
pub fn set_site_id(pool: &SqlitePool, id: &str, site_id: &str) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query("UPDATE books SET site_id = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2")
            .bind(site_id)
            .bind(id)
            .execute(pool)
            .await?;
        Ok(())
    })
}

/// 本ごとの綴じ方向の指定（`None` = 未設定 = サイト別設定に従う）。
///
/// `Book` には載せない（`owner_sub` と同じく、本棚の一覧では使わないビューアー専用の列）。
pub fn page_turn(pool: &SqlitePool, id: &str) -> Result<Option<PageTurn>, sqlx::Error> {
    crate::db::block_on(async {
        let raw: Option<Option<String>> =
            sqlx::query_scalar("SELECT page_turn FROM books WHERE id = ?1")
                .bind(id)
                .fetch_optional(pool)
                .await?;
        Ok(raw.flatten().as_deref().and_then(PageTurn::parse))
    })
}

/// 本ごとの綴じ方向を保存する（`None` で指定を消してサイト別設定に戻す）。
pub fn set_page_turn(
    pool: &SqlitePool,
    id: &str,
    page_turn: Option<PageTurn>,
) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query(
            "UPDATE books SET page_turn = ?1, updated_at = CURRENT_TIMESTAMP WHERE id = ?2",
        )
        .bind(page_turn.map(PageTurn::as_str))
        .bind(id)
        .execute(pool)
        .await?;
        Ok(())
    })
}

/// List favorite books (for the auto-download on startup).
pub fn list_favorites(pool: &SqlitePool) -> Result<Vec<Book>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, Book>(&format!(
            "SELECT {COLUMNS} FROM books WHERE is_favorite = 1 ORDER BY created_at DESC"
        ))
        .fetch_all(pool)
        .await
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seed_book(pool: &SqlitePool, id: &str) {
        insert(
            pool,
            &Book {
                id: id.into(),
                title: "t".into(),
                author: String::new(),
                circle_name: String::new(),
                purchase_date: None,
                file_name: "t.pdf".into(),
                file_size: 1,
                opfs_path: format!("{id}.opfspack"),
                cover_thumbnail: None,
                tbf_product_id: None,
                site_id: None,
                tags_fetched: 1,
                pack_id: None,
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
            },
        )
        .unwrap();
    }

    /// 本ごとの綴じ方向を上書きできる（未設定は `None` = サイト別設定に従う）。
    #[test]
    fn page_turn_override_round_trips_per_book() {
        let pool = crate::db::test_pool();
        seed_book(&pool, "b1");
        seed_book(&pool, "b2");
        assert_eq!(page_turn(&pool, "b1").unwrap(), None);
        set_page_turn(&pool, "b1", Some(PageTurn::RightToLeft)).unwrap();
        assert_eq!(page_turn(&pool, "b1").unwrap(), Some(PageTurn::RightToLeft));
        // 同じサイトの別の本には影響しない
        assert_eq!(page_turn(&pool, "b2").unwrap(), None);
        // 指定を消せる（サイト別設定に戻る）
        set_page_turn(&pool, "b1", None).unwrap();
        assert_eq!(page_turn(&pool, "b1").unwrap(), None);
    }

    /// 保存文字列はビューアの切替・サイト別設定と共有する。
    #[test]
    fn page_turn_round_trips() {
        for turn in [PageTurn::RightToLeft, PageTurn::LeftToRight] {
            assert_eq!(PageTurn::parse(turn.as_str()), Some(turn));
        }
        assert_eq!(PageTurn::parse("up"), None);
        assert!(PageTurn::RightToLeft.is_right_to_left());
        assert!(!PageTurn::LeftToRight.is_right_to_left());
    }

    /// メタ更新（Drive 復元・再取り込みで使う upsert）で綴じ方向の指定が消えないこと。
    #[test]
    fn upsert_keeps_the_page_turn_override() {
        let pool = crate::db::test_pool();
        seed_book(&pool, "b1");
        set_page_turn(&pool, "b1", Some(PageTurn::LeftToRight)).unwrap();
        let mut book = get(&pool, "b1").unwrap().unwrap();
        book.title = "更新後".into();
        upsert(&pool, &book).unwrap();
        assert_eq!(
            page_turn(&pool, "b1").unwrap(),
            Some(PageTurn::LeftToRight),
            "メタ更新で本ごとの綴じ方向が消えている"
        );
    }
}
