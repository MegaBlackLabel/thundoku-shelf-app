//! DLsite の購入済み作品を本棚（`bookshelf_items`）へ保存する。
//!
//! 一覧（`purchased`）の各作品を 2 軸分類し、ビューアー対象（画像系 = 漫画 / CG・イラスト。
//! AI バリアント含む）のものだけを `site_id="dlsite"` で upsert する。ボイス / ゲーム /
//! ノベル / 動画は保存しない。リッチメタ（`release_date`/`maker_id`/`age_rating`/
//! `series_name`/`custom_genres`）は `product_info`（一括）からベストエフォートで反映する。

use crate::db::SqlitePool;
use crate::db::bookshelf::{self, BookshelfItem};
use crate::dlsite::client::{DlsiteClient, DlsiteError};
use crate::dlsite::{ai_to_str, classify, is_viewable_included, media_to_str};

pub const SITE_ID_DLSITE: &str = "dlsite";

fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 購入済み作品のうち画像系（comic / cg）だけを `bookshelf_items(site_id='dlsite')` に
/// 保存し、保存件数を返す。除外カテゴリ（voice / game / novel / video）は upsert しない。
/// リッチメタは `product_info` から取得して `release_date`/`maker_id`/`age_rating`/
/// `series_name`/`tags_json` に反映する（取得失敗はベストエフォートで一覧の値のみ）。
pub fn save_purchases(pool: &SqlitePool, client: &mut DlsiteClient) -> Result<usize, DlsiteError> {
    let items = client.purchased()?;
    // リッチメタを一括取得（ベストエフォート。失敗時は一覧の値のみで続行）
    let ids: Vec<&str> = items.iter().map(|p| p.content_id.as_str()).collect();
    let metas = client.product_info(&ids).unwrap_or_default();
    let mut saved = 0usize;
    for p in items {
        let meta = metas.get(&p.content_id);
        let site_id = meta
            .map(|m| m.site_id.as_str())
            .unwrap_or(p.site_id.as_str());
        let age_category = meta.and_then(|m| m.age_category);
        let cfg = classify(&p.work_type, &p.genre_icons, site_id, age_category);
        if !is_viewable_included(&cfg, true) {
            continue;
        }
        let ts = now();
        let tags_json = meta.and_then(|m| {
            if m.custom_genres.is_empty() {
                None
            } else {
                serde_json::to_string(&m.custom_genres).ok()
            }
        });
        // 表紙は「作品画像」（product/info/ajax の work_image、//img.dlsite.jp/...）を優先。
        // ユーザーページの静的 HTML は data: プレースホルダしか持たないため。
        let thumbnail_url = meta
            .and_then(|m| m.work_image.clone())
            .or_else(|| p.thumbnail_url.clone().filter(|u| !u.starts_with("data:")))
            .map(|u| {
                if u.starts_with("//") {
                    format!("https:{u}")
                } else {
                    u
                }
            });
        let item = BookshelfItem {
            site_id: SITE_ID_DLSITE.into(),
            database_id: p.content_id.clone(),
            title: p.title,
            circle_name: p.maker_name,
            author: String::new(),
            thumbnail_url,
            format: "ZIP".into(),
            caused_at: p.purchase_date,
            event_name: None,
            event_slug: None,
            event_id: None,
            file_name: None,
            download_url: p.down_url,
            is_downloadable: 1,
            is_checked: 0,
            is_purchased: 1,
            is_new: 0,
            is_active: 1,
            is_favorite: 0,
            is_hidden: 0,
            hidden_at: None,
            tags_json,
            synced_at: ts.clone(),
            created_at: ts.clone(),
            updated_at: ts,
            media_category: Some(media_to_str(cfg.media).into()),
            ai_type: Some(ai_to_str(cfg.ai).into()),
            is_drm: 0,
            release_date: meta.and_then(|m| m.regist_date.clone()),
            description: None,
            theme: None,
            maker_id: meta.and_then(|m| m.maker_id.clone()),
            page_count: None,
            age_rating: cfg.age.map(String::from),
            series_name: meta.and_then(|m| m.title_name.clone()),
        };
        bookshelf::upsert(pool, &item)?;
        saved += 1;
    }
    Ok(saved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::bookshelf;
    use crate::dlsite::client::DlsiteSession;
    use crate::tbf::TbfError;
    use crate::tbf::transport::{RequestSpec, ResponseSpec};
    use serde_json::json;
    use std::collections::HashMap;

    struct MockTransport {
        handler: Box<dyn FnMut(RequestSpec) -> Result<ResponseSpec, TbfError> + Send>,
    }
    impl crate::tbf::transport::Transport for MockTransport {
        fn send(&mut self, spec: RequestSpec) -> Result<ResponseSpec, TbfError> {
            (self.handler)(spec)
        }
    }

    /// 1 行分の購入履歴 HTML。`store` はダウンロード URL のストアセグメント（下り URL から site_id を導出）。
    fn row_html(
        store: &str,
        content_id: &str,
        title: &str,
        icon_spans: &str,
        maker: &str,
        maker_id: &str,
    ) -> String {
        format!(
            r#"<tr><td class="buy_date">2026/05/04 17:03</td>
<td class="work_1col_thumb"><img src="//img.dlsite.jp/resize/images2/work/doujin/RJ{base}/{content_id}_img_main_240x240.webp"></td>
<td class="work_content">
<dl class="work_1col">
<dt class="work_name"><a href="https://www.dlsite.com/{store}/work/=/product_id/{content_id}.html">{title}</a></dt>
<dd class="maker_name"><a href="https://www.dlsite.com/{store}/circle/profile/=/maker_id/{maker_id}.html">{maker}</a></dd>
<dd class="work_genre"><span class="icon_GEN" title="全年齢">全年齢</span>{icon_spans}</dd>
</dl></td>
<td class="re_dl"><a href="https://www.dlsite.com/{store}/download/=/product_id/{content_id}.html">DL</a></td>
<td class="work_price">440円</td></tr>"#,
            base = &content_id[..8]
        )
    }

    /// 混在カテゴリの一覧ページ（maniax の page/1）。
    fn mixed_page() -> String {
        let rows = [
            row_html(
                "maniax",
                "RJ00000001",
                "漫画本",
                r#"<span class="icon_MNG" title="マンガ">マンガ</span>"#,
                "C1",
                "RG1",
            ),
            row_html(
                "maniax",
                "RJ00000002",
                "漫画AI一部",
                r#"<span class="icon_MNG" title="マンガ">マンガ</span><span class="icon_AIP" title="AI一部利用">AI一部利用</span>"#,
                "C2",
                "RG2",
            ),
            row_html(
                "maniax",
                "RJ00000003",
                "CGイラスト",
                r#"<span class="icon_ICG" title="CG・イラスト">CG・イラスト</span>"#,
                "C3",
                "RG3",
            ),
            row_html(
                "ai",
                "RJ00000004",
                "CGフルAI",
                r#"<span class="icon_ICG" title="CG・イラスト">CG・イラスト</span><span class="icon_AIG" title="AI生成作品">AI生成作品</span>"#,
                "C4",
                "RG4",
            ),
            row_html(
                "maniax",
                "RJ00000005",
                "ボイス",
                r#"<span class="icon_SOU" title="音声・ASMR">音声・ASMR</span>"#,
                "C5",
                "RG5",
            ),
            row_html(
                "maniax",
                "RJ00000006",
                "ゲーム",
                r#"<span class="icon_ACN" title="アクション">アクション</span>"#,
                "C6",
                "RG6",
            ),
            row_html(
                "maniax",
                "RJ00000007",
                "ノベル",
                r#"<span class="icon_NRE" title="ノベル">ノベル</span>"#,
                "C7",
                "RG7",
            ),
            row_html(
                "maniax",
                "RJ00000008",
                "ボイスコミック",
                r#"<span class="icon_VCM" title="ボイスコミック">ボイスコミック</span>"#,
                "C8",
                "RG8",
            ),
            row_html(
                "maniax",
                "RJ00000009",
                "Webtoon",
                r#"<span class="icon_WBT" title="Webtoon">Webtoon</span>"#,
                "C9",
                "RG9",
            ),
        ];
        format!(
            r#"<div id="buy_history_this"><table class="work_list_main"><tr class="item_name"><td>..</td></tr>{}</table><table class="global_pagination"><td class="page_no"><a href="/page/1">1</a></td></table></div>"#,
            rows.join("")
        )
    }

    fn empty_page() -> String {
        r#"<div id="buy_history_this"><table class="work_list_main"><tr class="item_name"><td>..</td></tr></table></div>"#.to_string()
    }

    /// 各作品の `product/info/ajax` メタ（全 ID 分）。site_id / work_type / age_category /
    /// custom_genres / title_name を含む。
    fn product_body() -> Vec<u8> {
        fn meta(id: &str, site: &str, wt: &str, tags: &[&str], title: &str) -> serde_json::Value {
            json!({
                "site_id": site, "work_type": wt, "maker_id": "RG1", "work_name": id,
                "regist_date": "2025-06-17 16:00:00", "price": 440,
                "down_url": format!("https://www.dlsite.com/{site}/download/=/product_id/{id}.html"),
                "custom_genres": tags, "options": "JPN", "age_category": 1,
                "title_name": title,
            })
        }
        let mut m = serde_json::Map::new();
        m.insert(
            "RJ00000001".into(),
            meta("RJ00000001", "maniax", "MNG", &["タグA"], "シリーズ1"),
        );
        m.insert(
            "RJ00000002".into(),
            meta("RJ00000002", "maniax", "MNG", &["タグB"], "シリーズ2"),
        );
        m.insert(
            "RJ00000003".into(),
            meta("RJ00000003", "maniax", "ICG", &[], "シリーズ3"),
        );
        m.insert(
            "RJ00000004".into(),
            meta("RJ00000004", "ai", "ICG", &["タグC"], "シリーズ4"),
        );
        m.insert(
            "RJ00000005".into(),
            meta("RJ00000005", "maniax", "SOU", &[], "シリーズ5"),
        );
        m.insert(
            "RJ00000006".into(),
            meta("RJ00000006", "maniax", "ACN", &[], "シリーズ6"),
        );
        m.insert(
            "RJ00000007".into(),
            meta("RJ00000007", "maniax", "NRE", &[], "シリーズ7"),
        );
        m.insert(
            "RJ00000008".into(),
            meta("RJ00000008", "maniax", "VCM", &[], "シリーズ8"),
        );
        m.insert(
            "RJ00000009".into(),
            meta("RJ00000009", "maniax", "WBT", &[], "シリーズ9"),
        );
        serde_json::to_vec(&serde_json::Value::Object(m)).unwrap()
    }

    /// 画像系（漫画 / CG・AI 含む）だけ保存され、ボイス / ゲーム / ノベル / 動画 /
    /// Webtoon は保存されないこと、および 2 軸分類（media_category / ai_type）と
    /// リッチメタ（release_date / maker_id / age_rating / series_name / tags_json）が
    /// 永続化されることを検証する。
    #[test]
    fn save_purchases_filters_to_image_only_and_enriches_meta() {
        let pool = crate::db::test_pool();
        let body = product_body();
        let transport = MockTransport {
            handler: Box::new(move |spec: RequestSpec| {
                let url = spec.url;
                if url.contains("/product/info/ajax") {
                    Ok(ResponseSpec {
                        status: 200,
                        headers: vec![],
                        body: body.clone(),
                    })
                } else if url.contains("/mypage/userbuy/") {
                    let page = if url.contains("maniax") && url.contains("page/1") {
                        mixed_page()
                    } else {
                        empty_page()
                    };
                    Ok(ResponseSpec {
                        status: 200,
                        headers: vec![],
                        body: page.into_bytes(),
                    })
                } else {
                    Ok(ResponseSpec {
                        status: 404,
                        headers: vec![],
                        body: vec![],
                    })
                }
            }),
        };
        let session = DlsiteSession::new(HashMap::from([("__DLsite_SID".into(), "abc".into())]));
        let mut client = DlsiteClient::with_transport(Box::new(transport), session);

        let saved = save_purchases(&pool, &mut client).unwrap();
        assert_eq!(saved, 4);

        let rows = bookshelf::list(&pool, SITE_ID_DLSITE).unwrap();
        assert_eq!(rows.len(), 4);
        let ids: std::collections::HashSet<_> =
            rows.iter().map(|r| r.database_id.clone()).collect();
        assert!(ids.contains("RJ00000001"));
        assert!(ids.contains("RJ00000002"));
        assert!(ids.contains("RJ00000003"));
        assert!(ids.contains("RJ00000004"));
        assert!(!ids.contains("RJ00000005"));
        assert!(!ids.contains("RJ00000006"));
        assert!(!ids.contains("RJ00000007"));
        assert!(!ids.contains("RJ00000008"));
        assert!(!ids.contains("RJ00000009"));

        let d1 = rows.iter().find(|r| r.database_id == "RJ00000001").unwrap();
        assert_eq!(d1.media_category.as_deref(), Some("comic"));
        assert_eq!(d1.ai_type.as_deref(), Some("none"));
        assert_eq!(d1.release_date.as_deref(), Some("2025-06-17 16:00:00"));
        assert_eq!(d1.maker_id.as_deref(), Some("RG1"));
        assert_eq!(d1.age_rating.as_deref(), Some("all"));
        assert_eq!(d1.series_name.as_deref(), Some("シリーズ1"));
        assert_eq!(d1.tags_json.as_deref(), Some(r#"["タグA"]"#));

        let d2 = rows.iter().find(|r| r.database_id == "RJ00000002").unwrap();
        assert_eq!(d2.media_category.as_deref(), Some("comic"));
        assert_eq!(d2.ai_type.as_deref(), Some("partial"));

        let d4 = rows.iter().find(|r| r.database_id == "RJ00000004").unwrap();
        assert_eq!(d4.media_category.as_deref(), Some("cg"));
        assert_eq!(d4.ai_type.as_deref(), Some("full"));
    }
}
