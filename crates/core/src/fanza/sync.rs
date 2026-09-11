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
}
