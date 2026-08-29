//! `book_tags` / `favorite_tags` repository.

use sqlx::Row;

use crate::db::SqlitePool;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct BookTag {
    pub id: String,
    pub book_id: String,
    pub tag_name: String,
    pub source: String,
    pub confidence: Option<f64>,
    pub created_at: String,
}

/// Replace all tags of a book with the given (tag_name, source) pairs.
pub fn set_for_book(
    pool: &SqlitePool,
    book_id: &str,
    tags: &[(&str, &str)],
) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        let mut tx = pool.begin().await?;
        sqlx::query("DELETE FROM book_tags WHERE book_id = ?1")
            .bind(book_id)
            .execute(&mut *tx)
            .await?;
        for (tag_name, source) in tags {
            sqlx::query(
                "INSERT INTO book_tags (id, book_id, tag_name, source) VALUES (?1, ?2, ?3, ?4)",
            )
            .bind(uuid::Uuid::new_v4().to_string())
            .bind(book_id)
            .bind(tag_name)
            .bind(source)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        Ok(())
    })
}

/// 生成タグ（source = 'generated'）だけを削除する。タグ取得が OFF の時に
/// ダウンロード直後の自動生成タグを取り除くために使う（手動タグは残す）。
pub fn delete_generated(pool: &SqlitePool, book_id: &str) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query("DELETE FROM book_tags WHERE book_id = ?1 AND source = 'generated'")
            .bind(book_id)
            .execute(pool)
            .await?;
        Ok(())
    })
}

pub fn list_for_book(pool: &SqlitePool, book_id: &str) -> Result<Vec<BookTag>, sqlx::Error> {
    crate::db::block_on(async {
        sqlx::query_as::<_, BookTag>(
            "SELECT id, book_id, tag_name, source, confidence, created_at FROM book_tags \
             WHERE book_id = ?1 ORDER BY tag_name",
        )
        .bind(book_id)
        .fetch_all(pool)
        .await
    })
}

/// Distinct tag names across all books (for the tag filter).
pub fn all_tags(pool: &SqlitePool) -> Result<Vec<String>, sqlx::Error> {
    crate::db::block_on(async {
        let rows = sqlx::query("SELECT DISTINCT tag_name FROM book_tags ORDER BY tag_name")
            .fetch_all(pool)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(row.get::<String, _>("tag_name"));
        }
        Ok(out)
    })
}

pub fn set_favorite(pool: &SqlitePool, tag_name: &str, favorite: bool) -> Result<(), sqlx::Error> {
    crate::db::block_on(async {
        if favorite {
            sqlx::query(
                "INSERT INTO favorite_tags (tag_name) VALUES (?1) ON CONFLICT(tag_name) DO NOTHING",
            )
            .bind(tag_name)
            .execute(pool)
            .await?;
        } else {
            sqlx::query("DELETE FROM favorite_tags WHERE tag_name = ?1")
                .bind(tag_name)
                .execute(pool)
                .await?;
        }
        Ok(())
    })
}

pub fn list_favorites(pool: &SqlitePool) -> Result<Vec<String>, sqlx::Error> {
    crate::db::block_on(async {
        let rows = sqlx::query("SELECT tag_name FROM favorite_tags ORDER BY tag_name")
            .fetch_all(pool)
            .await?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            out.push(row.get::<String, _>("tag_name"));
        }
        Ok(out)
    })
}

#[cfg(test)]
mod tag_crud_tests {
    use super::*;
    use crate::db::books;

    fn open_db() -> crate::db::SqlitePool {
        crate::db::test_pool()
    }

    fn seed_book(pool: &crate::db::SqlitePool, id: &str) {
        books::insert(
            pool,
            &books::Book {
                id: id.into(),
                title: "テスト本".into(),
                author: String::new(),
                circle_name: "サークル".into(),
                purchase_date: None,
                file_name: format!("{id}.pdf"),
                file_size: 10,
                opfs_path: format!("{id}.opfspack"),
                cover_thumbnail: None,
                tbf_product_id: None,
                site_id: None,
                tags_fetched: 1,
                pack_id: Some(id.into()),
                is_favorite: 0,
                is_hidden: 0,
                created_at: "2026-08-21 00:00:00".into(),
                updated_at: "2026-08-21 00:00:00".into(),
            },
        )
        .unwrap();
    }

    #[test]
    fn set_and_list_tags_for_book() {
        let pool = open_db();
        seed_book(&pool, "book-1");
        set_for_book(
            &pool,
            "book-1",
            &[("react", "manual"), ("rust", "generated")],
        )
        .unwrap();
        let tags = list_for_book(&pool, "book-1").unwrap();
        assert_eq!(tags.len(), 2);
        assert!(
            tags.iter()
                .any(|t| t.tag_name == "react" && t.source == "manual")
        );
        assert!(
            tags.iter()
                .any(|t| t.tag_name == "rust" && t.source == "generated")
        );
        // 再設定で置き換わる
        set_for_book(&pool, "book-1", &[("go", "manual")]).unwrap();
        let tags = list_for_book(&pool, "book-1").unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].tag_name, "go");
    }

    #[test]
    fn delete_generated_removes_only_generated() {
        let pool = open_db();
        seed_book(&pool, "book-1");
        set_for_book(
            &pool,
            "book-1",
            &[("react", "generated"), ("rust", "manual")],
        )
        .unwrap();
        delete_generated(&pool, "book-1").unwrap();
        let tags = list_for_book(&pool, "book-1").unwrap();
        assert_eq!(tags.len(), 1);
        assert_eq!(tags[0].tag_name, "rust");
        assert_eq!(tags[0].source, "manual");
    }

    #[test]
    fn favorite_tags_roundtrip() {
        let pool = open_db();
        set_favorite(&pool, "react", true).unwrap();
        set_favorite(&pool, "rust", true).unwrap();
        set_favorite(&pool, "react", false).unwrap();
        let favs = list_favorites(&pool).unwrap();
        assert_eq!(favs, vec!["rust".to_string()]);
    }

    #[test]
    fn all_tags_lists_distinct_tag_names() {
        let pool = open_db();
        seed_book(&pool, "book-1");
        seed_book(&pool, "book-2");
        set_for_book(&pool, "book-1", &[("react", "manual")]).unwrap();
        set_for_book(&pool, "book-2", &[("rust", "manual")]).unwrap();
        let tags = all_tags(&pool).unwrap();
        assert_eq!(tags.len(), 2);
        assert!(tags.contains(&"react".to_string()));
        assert!(tags.contains(&"rust".to_string()));
    }
}
