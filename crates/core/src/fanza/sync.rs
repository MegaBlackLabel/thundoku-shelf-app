//! FANZA同人 の購入済み作品を本棚（`bookshelf_items`）へ保存する。
//!
//! 一覧 API から取得した各作品を 2 軸分類し、ビューアー対象（画像系 = コミック / CG。
//! AI バリアント含む）のものだけを `site_id="fanza"` で upsert する。ボイス / ゲーム /
//! 動画は保存しない（カードに出さない）。DRM 判定はダウンロード時（`details`）に行うため、
//! ここではメディア種別でのみ絞る。

use crate::db::SqlitePool;
use crate::db::bookshelf::{self, BookshelfItem};
use crate::fanza::client::{FanzaClient, FanzaError};
use crate::fanza::{ai_to_str, classify, is_viewable_included, media_to_str};

pub const SITE_ID_FANZA: &str = "fanza";

fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 一覧の `imageSrc` は 100x75 と小さいため、高解像度サムネイル
/// （`pl-200x150`、詳細 API の `imagePath` で確認済み）に差し替える。
fn widen_thumb(url: &str) -> String {
    url.replace("pl-100x75", "pl-200x150")
}

/// サムネイル URL から**サイズ指定を外して原寸**にする（`..._pl-200x150.jpg` → `..._pl.jpg`）。
///
/// DMM の画像は `-幅x高` を付けると縮小版が返る。実測（2026-09-11）:
/// `d_815503pl-200x150.jpg` = 200x150 (13KB) → `d_815503pl.jpg` = **560x420** (71KB)、
/// `d_100588pl-200x150.jpg` = 107x150 (5KB) → `d_100588pl.jpg` = **290x408** (23KB)。
/// カードの表紙に使うので、取得時に原寸へ差し替える（失敗したら保存済み URL に戻す）。
pub fn full_size_thumb(url: &str) -> String {
    let Some((base, extension)) = url.rsplit_once('.') else {
        return url.to_string();
    };
    let Some((stem, size)) = base.rsplit_once('-') else {
        return url.to_string();
    };
    let Some((width, height)) = size.split_once('x') else {
        return url.to_string();
    };
    let digits = |value: &str| !value.is_empty() && value.chars().all(|c| c.is_ascii_digit());
    if digits(width) && digits(height) {
        format!("{stem}.{extension}")
    } else {
        url.to_string()
    }
}

/// 購入済み作品のうち画像系（comic / cg）だけを `bookshelf_items(site_id='fanza')` に
/// 保存し、保存件数を返す。除外カテゴリ（voice / game / video）は upsert しない。
pub fn save_purchases(pool: &SqlitePool, client: &mut FanzaClient) -> Result<usize, FanzaError> {
    let items = client.purchased()?;
    let mut saved = 0usize;
    for p in items {
        let meta = classify(&p.image_src, &p.genre);
        if !is_viewable_included(&meta, true) {
            continue;
        }
        let ts = now();
        let item = BookshelfItem {
            site_id: SITE_ID_FANZA.into(),
            database_id: p.content_id.clone(),
            title: p.title,
            circle_name: p.maker_name,
            author: String::new(),
            thumbnail_url: Some(widen_thumb(&p.image_src)),
            format: "ZIP".into(),
            caused_at: p.purchase_date,
            event_name: None,
            event_slug: None,
            event_id: None,
            file_name: None,
            download_url: None,
            is_downloadable: 1,
            is_checked: 0,
            is_purchased: 1,
            is_new: 0,
            is_active: 1,
            is_favorite: 0,
            is_hidden: 0,
            hidden_at: None,
            tags_json: None,
            synced_at: ts.clone(),
            created_at: ts.clone(),
            updated_at: ts,
            media_category: Some(media_to_str(meta.media).into()),
            ai_type: Some(ai_to_str(meta.ai).into()),
            is_drm: 0,
            release_date: None,
            description: None,
            theme: None,
            maker_id: None,
            page_count: None,
            age_rating: None,
            series_name: None,
        };
        bookshelf::upsert(pool, &item)?;
        saved += 1;
    }
    Ok(saved)
}

/// 1 回の実行で取る未取得タグの件数（同期のたびに少しずつ進める）。
///
/// タグは作品ページの HTML にしか無く **1 件 = 1 リクエスト**になる。数百件を
/// まとめて叩くと DMM 側の bot 判定に引っかかり、同期そのものが失敗しうる。
pub const TAG_FETCH_PER_RUN: usize = 20;

/// 連続で叩くときの間隔（サーバーに負荷をかけない）。
pub const TAG_FETCH_INTERVAL: std::time::Duration = std::time::Duration::from_millis(300);

/// 未取得タグの遅延取得の結果。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct TagFetchOutcome {
    /// タグを取り込んだ件数（タグ 0 件も「取得済み」として数える）。
    pub fetched: usize,
    /// 失敗して次回に回した件数。
    pub skipped: usize,
    /// セッション切れ（401/403）で打ち切ったか。
    pub stopped: bool,
    /// この実行のあとに残っている未取得の件数（通知に出す）。
    pub remaining: usize,
}

/// タグ未取得の**未ダウンロード**作品のタグを最大 `limit` 件、直列 + `interval` 間隔で取る。
///
/// 同期のたびに小さめの `limit` で呼び、`tags_fetched` が立った作品は次回の対象から
/// 外れる（＝何回かの同期で徐々に埋まる）。**失敗した作品は印を立てない**ので次回に
/// 再試行される。セッション切れは叩き続けずに即打ち切る（ブロックを避ける）。
pub fn fetch_pending_tags(
    pool: &SqlitePool,
    client: &mut FanzaClient,
    limit: usize,
    interval: std::time::Duration,
) -> Result<TagFetchOutcome, FanzaError> {
    let ids = bookshelf::pending_tag_fetch(pool, SITE_ID_FANZA, limit)?;
    let mut outcome = TagFetchOutcome::default();
    for (index, database_id) in ids.iter().enumerate() {
        if index > 0 && !interval.is_zero() {
            std::thread::sleep(interval);
        }
        match client.product_page(database_id) {
            Ok(page) => {
                // タグが 0 件でも「取得済み」にする（毎回同じ作品を叩かない）
                bookshelf::update_tags(pool, SITE_ID_FANZA, database_id, &page.genre_tags)?;
                outcome.fetched += 1;
            }
            Err(error @ (FanzaError::Unauthorized(_) | FanzaError::SessionExpired)) => {
                log::warn!("タグ取得を打ち切ります（セッション無効）: {error}");
                outcome.stopped = true;
                break;
            }
            Err(error) => {
                log::warn!("タグ取得に失敗（次回に再試行します）: {database_id}: {error}");
                outcome.skipped += 1;
            }
        }
    }
    // 通知に出す残り件数（この実行で取得済みにしたぶんは減っている）
    outcome.remaining = bookshelf::pending_tag_fetch_count(pool, SITE_ID_FANZA)? as usize;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fanza::client::FanzaSession;
    use crate::tbf::TbfError;
    use crate::tbf::transport::{RequestSpec, ResponseSpec};
    use serde_json::Value;
    use std::collections::HashMap;

    #[test]
    fn widen_thumb_uses_the_larger_listing_variant() {
        assert_eq!(
            widen_thumb("https://doujin-assets.dmm.co.jp/digital/comic/d_1/d_1pl-100x75.jpg"),
            "https://doujin-assets.dmm.co.jp/digital/comic/d_1/d_1pl-200x150.jpg"
        );
    }

    #[test]
    fn full_size_thumb_drops_the_size_suffix() {
        assert_eq!(
            full_size_thumb("https://doujin-assets.dmm.co.jp/digital/comic/d_1/d_1pl-200x150.jpg"),
            "https://doujin-assets.dmm.co.jp/digital/comic/d_1/d_1pl.jpg"
        );
        // サイズ指定が無い / 数字でないものはそのまま
        assert_eq!(
            full_size_thumb("https://doujin-assets.dmm.co.jp/digital/comic/d_1/d_1pl.jpg"),
            "https://doujin-assets.dmm.co.jp/digital/comic/d_1/d_1pl.jpg"
        );
        // DLsite の `_240x240` はアンダースコア区切りなので触らない（work_image = 原寸を使う）
        assert_eq!(
            full_size_thumb(
                "https://img.dlsite.jp/modpub/images2/work/doujin/RJ1/RJ1_img_main_240x240.webp"
            ),
            "https://img.dlsite.jp/modpub/images2/work/doujin/RJ1/RJ1_img_main_240x240.webp"
        );
        assert_eq!(
            full_size_thumb("https://example.com/no-extension"),
            "https://example.com/no-extension"
        );
    }

    struct MockTransport {
        handler: Box<dyn FnMut(RequestSpec) -> Result<ResponseSpec, TbfError> + Send>,
    }
    impl crate::tbf::transport::Transport for MockTransport {
        fn send(&mut self, spec: RequestSpec) -> Result<ResponseSpec, TbfError> {
            (self.handler)(spec)
        }
    }

    fn json_body(v: Value) -> Vec<u8> {
        serde_json::to_vec(&v).unwrap()
    }

    fn page_json(items: Vec<Value>) -> Value {
        let mut m = serde_json::Map::new();
        m.insert("2026年09月03日".into(), Value::Array(items));
        serde_json::json!({ "error_code": 0, "data": { "items": m, "total": 6, "hasNext": false } })
    }

    fn purchase(content: &str, genre: &str, image_path: &str) -> Value {
        serde_json::json!({
            "contentId": content, "title": "タイトル",
            "imageSrc": format!("https://dmm/digital/{image_path}/d/x.jpg"),
            "genre": genre, "makerName": "サークル",
            "isStreaming": true, "isUnavailable": false,
        })
    }

    /// 画像系（コミック / CG・AI 等）だけ保存され、ボイス / ゲーム / 動画は保存されないこと、
    /// および 2 軸分類（media_category / ai_type）が永続化されることを検証する。
    #[test]
    fn save_purchases_filters_to_image_only() {
        let pool = crate::db::test_pool();
        let body = json_body(page_json(vec![
            purchase("d_1", "コミック", "comic"),
            purchase("d_2", "コミック・一部AI", "comic"),
            purchase("d_3", "CG・AI", "cg"),
            purchase("d_4", "ボイス", "voice"),
            purchase("d_5", "ゲーム", "game"),
            purchase("d_6", "動画", "video"),
        ]));
        let transport = MockTransport {
            handler: Box::new(move |_| {
                Ok(ResponseSpec {
                    status: 200,
                    headers: vec![],
                    body: body.clone(),
                })
            }),
        };
        let session = FanzaSession::new(HashMap::from([("login_id".into(), "abc".into())]));
        let mut client = FanzaClient::with_transport(Box::new(transport), session);

        let saved = save_purchases(&pool, &mut client).unwrap();
        assert_eq!(saved, 3);

        let rows = bookshelf::list(&pool, SITE_ID_FANZA).unwrap();
        assert_eq!(rows.len(), 3);
        let ids: Vec<_> = rows.iter().map(|r| r.database_id.clone()).collect();
        assert!(ids.contains(&"d_1".to_string()));
        assert!(ids.contains(&"d_2".to_string()));
        assert!(ids.contains(&"d_3".to_string()));
        assert!(!ids.contains(&"d_4".to_string()));
        assert!(!ids.contains(&"d_5".to_string()));
        assert!(!ids.contains(&"d_6".to_string()));

        let d2 = rows.iter().find(|r| r.database_id == "d_2").unwrap();
        assert_eq!(d2.media_category.as_deref(), Some("comic"));
        assert_eq!(d2.ai_type.as_deref(), Some("partial"));
        let d3 = rows.iter().find(|r| r.database_id == "d_3").unwrap();
        assert_eq!(d3.media_category.as_deref(), Some("cg"));
        assert_eq!(d3.ai_type.as_deref(), Some("full"));
        let d1 = rows.iter().find(|r| r.database_id == "d_1").unwrap();
        assert_eq!(d1.media_category.as_deref(), Some("comic"));
        assert_eq!(d1.ai_type.as_deref(), Some("none"));
    }

    // ---- 未取得タグの遅延取得（1 回の同期で少しずつ） ----------------------------

    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    /// 未取得（未ダウンロード）の FANZA アイテムを N 件用意する（同期で保存した状態）。
    fn seed_pending_items(pool: &SqlitePool, count: usize) {
        let items: Vec<Value> = (1..=count)
            .map(|i| purchase(&format!("d_{i}"), "コミック", "comic"))
            .collect();
        let body = json_body(page_json(items));
        let transport = MockTransport {
            handler: Box::new(move |_| {
                Ok(ResponseSpec {
                    status: 200,
                    headers: vec![],
                    body: body.clone(),
                })
            }),
        };
        let session = FanzaSession::new(HashMap::from([("login_id".into(), "abc".into())]));
        let mut client = FanzaClient::with_transport(Box::new(transport), session);
        save_purchases(pool, &mut client).unwrap();
    }

    /// 商品ページ（`genreTag__txt` 入り HTML）を返す。叩かれた回数を数える。
    fn tag_transport(
        calls: Arc<AtomicUsize>,
        tags: Vec<&'static str>,
        status: u16,
    ) -> MockTransport {
        MockTransport {
            handler: Box::new(move |spec: RequestSpec| {
                assert!(
                    spec.url.contains("/detail/=/cid="),
                    "商品ページ以外を叩いた: {}",
                    spec.url
                );
                calls.fetch_add(1, Ordering::SeqCst);
                if status != 200 {
                    return Ok(ResponseSpec {
                        status,
                        headers: vec![],
                        body: Vec::new(),
                    });
                }
                let items: Vec<String> = tags
                    .iter()
                    .map(|tag| format!(r#"<li><a href="/x" class="genreTag__txt">{tag}</a></li>"#))
                    .collect();
                Ok(ResponseSpec {
                    status: 200,
                    headers: vec![],
                    body: format!(r#"<ul class="genreTagList">{}</ul>"#, items.join(""))
                        .into_bytes(),
                })
            }),
        }
    }

    fn session() -> FanzaSession {
        FanzaSession::new(HashMap::from([("login_id".into(), "abc".into())]))
    }

    /// 保存済みのタグと取得済みフラグ。
    fn tags_of(pool: &SqlitePool, database_id: &str) -> (Option<String>, i64) {
        crate::db::block_on(async {
            sqlx::query_as::<_, (Option<String>, i64)>(
                "SELECT tags_json, tags_fetched FROM bookshelf_items WHERE database_id = ?1",
            )
            .bind(database_id)
            .fetch_one(pool)
            .await
            .unwrap()
        })
    }

    /// 1 回の実行で取るのは `limit` 件まで（同期のたびに少しずつ進める）。
    #[test]
    fn fetch_pending_tags_takes_at_most_the_limit_and_stores_them() {
        let pool = crate::db::test_pool();
        seed_pending_items(&pool, 5);
        let calls = Arc::new(AtomicUsize::new(0));
        let mut client = FanzaClient::with_transport(
            Box::new(tag_transport(calls.clone(), vec!["タグA", "タグB"], 200)),
            session(),
        );

        let outcome = fetch_pending_tags(&pool, &mut client, 2, Duration::ZERO).unwrap();

        assert_eq!(outcome.fetched, 2);
        assert_eq!(outcome.skipped, 0);
        assert!(!outcome.stopped);
        assert_eq!(outcome.remaining, 3, "残り件数を返していない");
        assert_eq!(calls.load(Ordering::SeqCst), 2, "limit を超えて叩いている");
        let (tags, fetched) = tags_of(&pool, "d_1");
        assert_eq!(tags.as_deref(), Some(r#"["タグA","タグB"]"#));
        assert_eq!(fetched, 1, "取得済みの印が立っていない");
        assert_eq!(
            bookshelf::pending_tag_fetch(&pool, SITE_ID_FANZA, 10)
                .unwrap()
                .len(),
            3,
            "残りは次回に回す"
        );
    }

    /// タグが 0 件でも「取得済み」にする（毎回同じ作品を叩かない）。
    #[test]
    fn fetch_pending_tags_marks_items_without_tags_as_fetched() {
        let pool = crate::db::test_pool();
        seed_pending_items(&pool, 1);
        let calls = Arc::new(AtomicUsize::new(0));
        let mut client = FanzaClient::with_transport(
            Box::new(tag_transport(calls.clone(), vec![], 200)),
            session(),
        );

        let outcome = fetch_pending_tags(&pool, &mut client, 1, Duration::ZERO).unwrap();

        assert_eq!(outcome.fetched, 1);
        let (tags, fetched) = tags_of(&pool, "d_1");
        assert_eq!(tags.as_deref(), Some("[]"));
        assert_eq!(fetched, 1);
        assert!(
            bookshelf::pending_tag_fetch(&pool, SITE_ID_FANZA, 10)
                .unwrap()
                .is_empty(),
            "タグ無しの作品を再取得しようとしている"
        );
    }

    /// 失敗した項目は「取得済み」にせず、次回に再試行する（1 件の失敗で全体を止めない）。
    #[test]
    fn fetch_pending_tags_retries_failures_next_run() {
        let pool = crate::db::test_pool();
        seed_pending_items(&pool, 3);
        let calls = Arc::new(AtomicUsize::new(0));
        let counter = calls.clone();
        let transport = MockTransport {
            handler: Box::new(move |_| {
                let nth = counter.fetch_add(1, Ordering::SeqCst);
                // 2 件目だけ失敗させる
                let status = if nth == 1 { 500 } else { 200 };
                let body = if status == 200 {
                    r#"<ul class="genreTagList"><li><a href="/x" class="genreTag__txt">タグ</a></li></ul>"#
                        .as_bytes()
                        .to_vec()
                } else {
                    Vec::new()
                };
                Ok(ResponseSpec {
                    status,
                    headers: vec![],
                    body,
                })
            }),
        };
        let mut client = FanzaClient::with_transport(Box::new(transport), session());

        let outcome = fetch_pending_tags(&pool, &mut client, 3, Duration::ZERO).unwrap();

        assert_eq!(outcome.fetched, 2);
        assert_eq!(outcome.skipped, 1);
        assert!(!outcome.stopped);
        let pending = bookshelf::pending_tag_fetch(&pool, SITE_ID_FANZA, 10).unwrap();
        assert_eq!(
            pending,
            vec!["d_2"],
            "失敗した項目が次回の対象から外れている"
        );
    }

    /// セッション切れ（401/403）は打ち切る（叩き続けてブロックされない）。
    #[test]
    fn fetch_pending_tags_stops_on_unauthorized() {
        let pool = crate::db::test_pool();
        seed_pending_items(&pool, 5);
        let calls = Arc::new(AtomicUsize::new(0));
        let mut client = FanzaClient::with_transport(
            Box::new(tag_transport(calls.clone(), vec!["タグA"], 403)),
            session(),
        );

        let outcome = fetch_pending_tags(&pool, &mut client, 5, Duration::ZERO).unwrap();

        assert!(outcome.stopped);
        assert_eq!(outcome.fetched, 0);
        assert_eq!(calls.load(Ordering::SeqCst), 1, "403 のあとも叩いている");
        assert_eq!(
            bookshelf::pending_tag_fetch(&pool, SITE_ID_FANZA, 10)
                .unwrap()
                .len(),
            5,
            "打ち切り分を取得済みにしてしまっている"
        );
    }

    /// ダウンロード済み・取得済みの項目は対象にしない（未DLのぶんだけ拾う）。
    #[test]
    fn pending_tag_fetch_excludes_downloaded_and_fetched_items() {
        let pool = crate::db::test_pool();
        seed_pending_items(&pool, 3);
        // d_1 は取得済み
        bookshelf::update_tags(&pool, SITE_ID_FANZA, "d_1", &["既存".to_string()]).unwrap();
        // d_2 はダウンロード済み（books に紐づく）
        crate::db::books::insert(
            &pool,
            &crate::db::books::Book {
                id: "b_2".into(),
                title: "本".into(),
                author: String::new(),
                circle_name: "サークル".into(),
                purchase_date: None,
                file_name: "b_2.pdf".into(),
                file_size: 10,
                opfs_path: "b_2.opfspack".into(),
                cover_thumbnail: None,
                tbf_product_id: Some("d_2".into()),
                site_id: Some(SITE_ID_FANZA.into()),
                tags_fetched: 0,
                pack_id: Some("b_2".into()),
                is_favorite: 0,
                is_hidden: 0,
                created_at: "2026-08-21 00:00:00".into(),
                updated_at: "2026-08-21 00:00:00".into(),
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

        let pending = bookshelf::pending_tag_fetch(&pool, SITE_ID_FANZA, 10).unwrap();

        assert_eq!(pending, vec!["d_3"], "未DL の未取得だけを対象にする");
    }
}
