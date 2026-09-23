//! DLsite の購入済み作品を本棚（`bookshelf_items`）へ保存する。
//!
//! 一覧（`purchased`）の各作品を 2 軸分類し、ビューアー対象（画像系 = 漫画 / CG・イラスト。
//! AI バリアント含む）のものだけを `site_id="dlsite"` で upsert する。ボイス / ゲーム /
//! ノベル / 動画は保存しない。リッチメタ（`release_date`/`maker_id`/`age_rating`/
//! `series_name`/`custom_genres`）は `product_info`（一括）からベストエフォートで反映する。

use crate::db::SqlitePool;
use crate::db::bookshelf::{self, BookshelfItem};
use crate::dlsite::client::{
    DlsiteClient, DlsiteError, DlsitePurchase, DlsiteWorkMeta, MAX_PAGES_PER_STORE, STORES,
};
use crate::dlsite::{ai_to_str, classify, is_viewable_included, media_to_str};

pub const SITE_ID_DLSITE: &str = "dlsite";

fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 1 回の分割同期で取り込むページ数（ユーザーの操作を挟んで少しずつ進める）。
pub const PURCHASE_PAGES_PER_RUN: usize = 5;

/// 分割同期の続き位置。
///
/// ストアをまたいで**ページ優先**で進む（全ストアの 1 ページ目 → 2 ページ目…）。
/// こうすると最初の 1 回で全ストアの最終ページ番号が分かり、「あと何回か」を出せる。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PurchaseCursor {
    /// 次に取るページ（1 始まり）。
    pub page: usize,
    /// `STORES` のうち次に取るストアの添字。
    pub store_index: usize,
    /// 各ストアの最終ページ番号（未取得は `None`）。
    pub last_pages: [Option<usize>; STORES.len()],
}

impl Default for PurchaseCursor {
    fn default() -> Self {
        Self {
            page: 1,
            store_index: 0,
            last_pages: [None; STORES.len()],
        }
    }
}

impl PurchaseCursor {
    /// まだ取っていないページ数（最終ページが分かっているストアだけ数える）。
    fn pages_left(&self) -> usize {
        self.last_pages
            .iter()
            .enumerate()
            .filter_map(|(index, last)| {
                let last = (*last)?;
                // 同じページ内では、既に取ったストアの次から始まる
                let from = if index < self.store_index {
                    self.page + 1
                } else {
                    self.page
                };
                (last >= from).then(|| last - from + 1)
            })
            .sum()
    }

    /// 全ストアの最終ページが分かっていて、残りが無いか。
    fn is_finished(&self) -> bool {
        self.last_pages.iter().all(Option::is_some) && self.pages_left() == 0
    }
}

/// 1 回の分割同期の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurchaseBatch {
    /// 取り込んだ件数（画像系のみ。upsert なので既存行の更新も含む）。
    pub saved: usize,
    /// 取得したページ数。
    pub fetched_pages: usize,
    /// 分かっている範囲の残りページ数（未取得のストアは数えない）。
    pub pages_left: usize,
    /// 続きの位置（`None` = 完了）。
    pub next: Option<PurchaseCursor>,
    /// 完了までにあと何回この操作が必要か（0 = 完了）。
    pub remaining_runs: usize,
}

/// ページ間の待機。DLsite の `robots.txt` は `Crawl-delay: 10` を指定しているので、
/// 自動で購入履歴を辿るときはそれを守る。
pub const PAGE_INTERVAL: std::time::Duration = std::time::Duration::from_secs(10);

/// 購入一覧を `pages` ページ分だけ取り込み、続きの位置と残り回数を返す。
///
/// 1 回で全ページ取ると、購入数の多いアカウントでは 1 操作で大量のリクエストになる。
/// ユーザーの操作を挟んで少しずつ進めるための分割版（続きは `next` を次の呼び出しに渡す）。
/// 画像系（comic / cg）だけを upsert する（voice / game / novel / video は保存しない）。
/// `owner` は暗号化済み sub（ログイン中のみ `Some`）。
pub fn save_purchases_batch(
    pool: &SqlitePool,
    client: &mut DlsiteClient,
    owner: Option<&str>,
    cursor: PurchaseCursor,
    pages: usize,
    page_interval: std::time::Duration,
) -> Result<PurchaseBatch, DlsiteError> {
    let pages = pages.max(1);
    let mut cursor = cursor;
    let mut fetched_pages = 0usize;
    let mut saved = 0usize;
    // 同一作品は複数ストアの一覧に重複して現れるため、初出のみ採る。
    let mut seen = std::collections::HashSet::new();
    while fetched_pages < pages {
        if cursor.store_index >= STORES.len() {
            cursor.page += 1;
            cursor.store_index = 0;
        }
        // 全ストアの最終ページが分かっていて残りが無い / 上限に達したら終わり
        if cursor.is_finished() || cursor.page > MAX_PAGES_PER_STORE {
            break;
        }
        // 既に終端と分かっているストアは叩かない
        if cursor.last_pages[cursor.store_index].is_some_and(|last| cursor.page > last) {
            cursor.store_index += 1;
            continue;
        }
        // 2 ページ目以降は間隔をあける。DLsite の `robots.txt` は `Crawl-delay: 10` を
        // 指定しているので、自動で購入履歴を辿るときはそれを守る
        // （`fanza::sync::TAG_FETCH_INTERVAL` と同じ流儀で、テストからは 0 を渡せる）。
        if fetched_pages > 0 && !page_interval.is_zero() {
            std::thread::sleep(page_interval);
        }
        let page = client.purchased_page(STORES[cursor.store_index], cursor.page)?;
        fetched_pages += 1;
        if let Some(last) = page.last_page {
            cursor.last_pages[cursor.store_index] = Some(last);
        } else if page.items.is_empty() {
            // ページャが取れないときは空ページを終端とみなす（同じページを叩き続けない）
            cursor.last_pages[cursor.store_index] = Some(cursor.page);
        }
        // リッチメタを一括取得（ベストエフォート。失敗時は一覧の値のみで続行）
        let ids: Vec<&str> = page.items.iter().map(|p| p.content_id.as_str()).collect();
        let metas = client.product_info(&ids).unwrap_or_default();
        for p in page.items {
            if !seen.insert(p.content_id.clone()) {
                continue;
            }
            let meta = metas.get(&p.content_id);
            if save_purchase(pool, p, meta)? {
                saved += 1;
            }
        }
        cursor.store_index += 1;
    }
    bookshelf::attribute_owner(pool, SITE_ID_DLSITE, owner)?;
    let pages_left = cursor.pages_left();
    // 全ストアを取り切った / 1 ストアの上限（`MAX_PAGES_PER_STORE`）に達したら続きを出さない
    // （上限で止まったまま「続き」を出し続けると、押しても何も進まなくなる）。
    let next = if cursor.is_finished() || cursor.page > MAX_PAGES_PER_STORE {
        None
    } else {
        Some(cursor)
    };
    // 未取得のストアがあると残りページは過小評価になるので、最低 1 回は残す
    let remaining_runs = if next.is_some() {
        pages_left.div_ceil(pages).max(1)
    } else {
        0
    };
    Ok(PurchaseBatch {
        saved,
        fetched_pages,
        pages_left,
        next,
        remaining_runs,
    })
}

/// 購入 1 件を本棚へ upsert する（画像系以外は保存しない）。保存したら `true`。
fn save_purchase(
    pool: &SqlitePool,
    p: DlsitePurchase,
    meta: Option<&DlsiteWorkMeta>,
) -> Result<bool, DlsiteError> {
    let site_id = meta
        .map(|m| m.site_id.as_str())
        .unwrap_or(p.site_id.as_str());
    let age_category = meta.and_then(|m| m.age_category);
    let cfg = classify(&p.work_type, &p.genre_icons, site_id, age_category);
    if !is_viewable_included(&cfg, true) {
        return Ok(false);
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
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::bookshelf;
    use crate::dlsite::client::DlsiteSession;
    use crate::tbf::TbfError;
    use crate::tbf::transport::{RequestSpec, ResponseSpec};
    use serde_json::json;
    use std::collections::BTreeMap;

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

    /// ページ間の待機が入ること（DLsite の `robots.txt` は `Crawl-delay: 10` を指定して
    /// いる）。実時間を測るので、待機を消すと落ちる。閾値は sleep の粒度ぶん緩めてある。
    #[test]
    fn waits_between_pages() {
        let pool = crate::db::test_pool();
        let times: std::sync::Arc<std::sync::Mutex<Vec<std::time::Instant>>> = Default::default();
        let recorder = times.clone();
        let transport = MockTransport {
            handler: Box::new(move |spec: RequestSpec| {
                if spec.url.contains("/mypage/userbuy/") {
                    recorder.lock().unwrap().push(std::time::Instant::now());
                    Ok(ResponseSpec {
                        status: 200,
                        headers: vec![],
                        body: empty_page().into_bytes(),
                    })
                } else {
                    Ok(ResponseSpec {
                        status: 200,
                        headers: vec![],
                        body: b"{}".to_vec(),
                    })
                }
            }),
        };
        let session = DlsiteSession::from_site_cookies(BTreeMap::from([(
            "__DLsite_SID".to_string(),
            "abc".to_string(),
        )]));
        let mut client = DlsiteClient::with_transport(Box::new(transport), session);

        save_purchases_batch(
            &pool,
            &mut client,
            None,
            PurchaseCursor::default(),
            2,
            std::time::Duration::from_millis(60),
        )
        .unwrap();

        let times = times.lock().unwrap();
        assert!(times.len() >= 2, "2 ページ叩いていない: {}", times.len());
        let gap = times[1].duration_since(times[0]);
        assert!(
            gap >= std::time::Duration::from_millis(40),
            "ページ間の待機が入っていない: {gap:?}"
        );
    }

    /// セッションが切れている（購入履歴がログインページを返す）ときは、**0 件で成功に
    /// しない**。黙って何も取り込まないまま「同期完了（0 件）」に見えるのが一番まずく、
    /// 利用者は同期できたと思い込む。
    ///
    /// マーカーは実物（`login.dlsite.com/register?user=self`）から採っている。
    #[test]
    fn expired_session_is_reported_instead_of_saved_as_zero_items() {
        let pool = crate::db::test_pool();
        let transport = MockTransport {
            handler: Box::new(|spec: RequestSpec| {
                if spec.url.contains("/mypage/userbuy/") {
                    Ok(ResponseSpec {
                        status: 200,
                        headers: vec![],
                        body: r#"<div class="contentLoginRegist-item"><p class="contentLoginLogin-text">viviON IDに登録済みの方はこちらから</p></div>"#
                            .as_bytes()
                            .to_vec(),
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
        let session = DlsiteSession::from_site_cookies(BTreeMap::from([(
            "__DLsite_SID".to_string(),
            "abc".to_string(),
        )]));
        let mut client = DlsiteClient::with_transport(Box::new(transport), session);

        let error = save_purchases_batch(
            &pool,
            &mut client,
            None,
            PurchaseCursor::default(),
            20,
            std::time::Duration::ZERO,
        )
        .expect_err("セッション切れを 0 件の成功にしてはいけない");
        assert!(
            matches!(error, DlsiteError::SessionExpired),
            "セッション切れとして返っていない: {error}"
        );
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
        let session = DlsiteSession::from_site_cookies(BTreeMap::from([(
            "__DLsite_SID".to_string(),
            "abc".to_string(),
        )]));
        let mut client = DlsiteClient::with_transport(Box::new(transport), session);

        let batch = save_purchases_batch(
            &pool,
            &mut client,
            None,
            PurchaseCursor::default(),
            20,
            std::time::Duration::ZERO,
        )
        .unwrap();
        assert_eq!(batch.saved, 4);

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

    // ---- 分割同期（1 回で少しずつ取り込む） ---------------------------------------

    /// 購入履歴 1 件分の行 HTML（画像系 = マンガ）。
    fn manga_row(store: &str, content_id: &str) -> String {
        row_html(
            store,
            content_id,
            "タイトル",
            r#"<span class="icon_MNG" title="マンガ">マンガ</span>"#,
            "C1",
            "RG1",
        )
    }

    /// ストア × ページごとの一覧モック。各ストアは `last_page` ページまである。
    /// 叩いた一覧 URL を記録する（メタ取得は数えない）。
    fn paged_transport(
        calls: std::sync::Arc<parking_lot::Mutex<Vec<String>>>,
        last_page: usize,
    ) -> MockTransport {
        const STORE_NAMES: [&str; 4] = ["maniax", "home", "books", "ai"];
        MockTransport {
            handler: Box::new(move |spec: RequestSpec| {
                let url = spec.url;
                if url.contains("/product/info/ajax") {
                    return Ok(ResponseSpec {
                        status: 200,
                        headers: vec![],
                        body: b"{}".to_vec(),
                    });
                }
                calls.lock().push(url.clone());
                let store = STORE_NAMES
                    .into_iter()
                    .find(|s| url.contains(&format!("/{s}/mypage/userbuy/")))
                    .unwrap_or("maniax");
                let page: usize = url
                    .split("/page/")
                    .nth(1)
                    .and_then(|p| p.trim_end_matches('/').parse().ok())
                    .unwrap_or(1);
                let index = STORE_NAMES.iter().position(|s| *s == store).unwrap_or(0);
                let content_id = format!("RJ{index:02}{page:04}00");
                let links: String = (1..=last_page)
                    .map(|p| format!(r#"<a href="/{store}/mypage/userbuy/.../page/{p}">{p}</a>"#))
                    .collect();
                let html = format!(
                    r#"<div id="buy_history_this"><table class="work_list_main"><tr class="item_name"><td>..</td></tr>{}</table><table class="global_pagination"><td class="page_no">{links}</td></table></div>"#,
                    manga_row(store, &content_id)
                );
                Ok(ResponseSpec {
                    status: 200,
                    headers: vec![],
                    body: html.into_bytes(),
                })
            }),
        }
    }

    fn session() -> DlsiteSession {
        DlsiteSession::from_site_cookies(BTreeMap::from([(
            "__DLsite_SID".to_string(),
            "abc".to_string(),
        )]))
    }

    /// 分割同期: 1 回で `pages` ページだけ取り込み、続きの位置と「あと何回か」を返す。
    /// ストアをまたいでページ優先で進むので、最初の 1 回で全ストアの最終ページが分かる。
    #[test]
    fn batch_takes_only_the_given_pages_and_reports_the_remaining_runs() {
        let pool = crate::db::test_pool();
        let calls = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        // 全ストア 2 ページ
        let transport = paged_transport(calls.clone(), 2);
        let mut client = DlsiteClient::with_transport(Box::new(transport), session());

        let batch = save_purchases_batch(
            &pool,
            &mut client,
            None,
            PurchaseCursor::default(),
            5,
            std::time::Duration::ZERO,
        )
        .unwrap();
        // 4 ストアの 1 ページ目 + maniax の 2 ページ目 = 5 ページ
        assert_eq!(batch.fetched_pages, 5);
        assert_eq!(calls.lock().len(), 5, "5 ページだけ叩く");
        assert_eq!(batch.saved, 5);
        let next = batch.next.expect("続きがある");
        assert_eq!(next.page, 2);
        assert_eq!(next.store_index, 1);
        assert_eq!(next.last_pages, [Some(2), Some(2), Some(2), Some(2)]);
        assert_eq!(batch.pages_left, 3);
        assert_eq!(batch.remaining_runs, 1, "残り 3 ページ = あと 1 回");

        // 続きから再開すると各ストアの 2 ページ目を取って完了する
        let rest =
            save_purchases_batch(&pool, &mut client, None, next, 5, std::time::Duration::ZERO)
                .unwrap();
        assert_eq!(rest.fetched_pages, 3);
        assert_eq!(rest.pages_left, 0);
        assert_eq!(rest.next, None);
        assert_eq!(rest.remaining_runs, 0);
        assert_eq!(calls.lock().len(), 8);
    }

    /// 各ストア 1 ページなら 1 回で完了する（続きなし・残り 0）。
    #[test]
    fn batch_finishes_in_one_run_when_every_store_has_a_single_page() {
        let pool = crate::db::test_pool();
        let calls = std::sync::Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let transport = paged_transport(calls.clone(), 1);
        let mut client = DlsiteClient::with_transport(Box::new(transport), session());

        let batch = save_purchases_batch(
            &pool,
            &mut client,
            None,
            PurchaseCursor::default(),
            5,
            std::time::Duration::ZERO,
        )
        .unwrap();
        assert_eq!(batch.fetched_pages, 4, "4 ストア × 1 ページ");
        assert_eq!(batch.pages_left, 0);
        assert_eq!(batch.next, None);
        assert_eq!(batch.remaining_runs, 0);
    }
}
