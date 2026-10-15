//! Fantia の**ショップ購入（商品）**を本棚（`bookshelf_items`）へ保存する。
//!
//! Fantia には公式 API が無いため、購入履歴ページの HTML を読む（解析は
//! [`crate::fantia::shop`]）。1 商品 = 1 ファイルで、中身の種類は注文ページの
//! `ファイル名:` でしか分からない。**ビューアーで開ける拡張子だけ**を取り込み
//! （[`VIEWABLE_EXTENSIONS`]）、動画 / 音声（`mp4` / `mp3` 等）は取り込まない。
//!
//! 一覧の HTML には総数の表記が無いため、分割同期の「あと何回か」は**下限**
//! （続きがある限り 1 回以上）として返す。

use std::time::Duration;

use crate::db::SqlitePool;
use crate::db::bookshelf::{self, BookshelfItem};
use crate::fantia::client::{FantiaClient, FantiaError};
use crate::fantia::shop::{self, OrderedItem};

/// サイト id（本棚の絞り込み・サイドバーのサイト行と同じ表記）。
pub const SITE_ID: &str = "fantia";

fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 1 回の分割同期で取り込むページ数（ユーザーの操作を挟んで少しずつ進める）。
pub const PURCHASE_PAGES_PER_RUN: usize = 3;

/// リクエスト間の待機。
///
/// Fantia の購入履歴は「一覧の 1 ページ = 注文の一覧」「1 注文 = 1 リクエスト」なので、
/// 1 ページぶんでも注文数ぶんのリクエストが増える。同期のたびに間隔を空けて
/// サーバーに負荷をかけない（DLsite は `robots.txt` の `Crawl-delay: 10` に従うが、
/// Fantia に同種の指定は見当たらないため、FANZA のタグ取得と同じ 300 ms に揃える）。
pub const PAGE_INTERVAL: Duration = Duration::from_millis(300);

/// ビューアーで開ける（= アプリが取り込める）拡張子。小文字で持つ。
///
/// Fantia のショップは同じ商品に本・音声・動画が混在し、種類は `ファイル名:` の
/// 拡張子でしか分からない（実測 2026-10-15。一覧は `zip` / `pdf` が主）。
/// **ここに載っている拡張子だけ**を保存する:
///
/// - 動画 / 音声（`mp4` / `mp3` 等）はビューアーで開けない。取り込むと
///   「本棚には並ぶが開けない」行ができるので落とす。
/// - `rar` / `7z` も Fantia の出品ではあり得るが、**アプリの取り込みが対応していない**
///   （`crate::import` は pdf / epub / zip / 画像のみ。`import_file` の `UnsupportedType`）。
///   入れると落とせない本が本棚に残るため落とす（対応したらここに足す）。
/// - ドットの無い名前や未知の拡張子も保存しない（中身を判定できない）。
const VIEWABLE_EXTENSIONS: [&str; 7] = ["zip", "pdf", "png", "jpg", "jpeg", "webp", "gif"];

/// ファイル名の小文字拡張子（ドットが無ければ空文字）。
pub fn file_extension(file_name: &str) -> String {
    file_name
        .rsplit_once('.')
        .map(|(_, extension)| extension.trim().to_ascii_lowercase())
        .unwrap_or_default()
}

/// 取り込む（ビューアーで開ける）ファイルか。
pub fn is_viewable_file(file_name: &str) -> bool {
    VIEWABLE_EXTENSIONS.contains(&file_extension(file_name).as_str())
}

/// 分割同期の続き位置（1 始まりのページ）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PurchaseCursor {
    pub page: usize,
}

impl Default for PurchaseCursor {
    fn default() -> Self {
        Self { page: 1 }
    }
}

/// 1 回の分割同期の結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurchaseBatch {
    /// 取り込んだ件数（開けない拡張子は数えない。upsert なので既存行の更新も含む）。
    pub saved: usize,
    /// 取得したページ数。
    pub fetched_pages: usize,
    /// 注文ページから読んだ商品の件数（開けない拡張子も含む）。
    ///
    /// Fantia の一覧 HTML には総数の表記が無いため、**全体の件数は分からない**
    /// （`fanza::sync::PurchaseBatch::total_items` の「一覧の総数」に相当する値は無い）。
    pub total_items: usize,
    /// 続きの位置（`None` = 完了）。
    pub next: Option<PurchaseCursor>,
    /// 完了までにあと何回この操作が必要か（0 = 完了。総数が無いので続きがある限り 1）。
    pub remaining_runs: usize,
}

/// リクエストの前に間隔を空ける（最初の 1 回は待たない）。
fn pace_requests(requests: &mut usize) {
    if *requests > 0 && !PAGE_INTERVAL.is_zero() {
        std::thread::sleep(PAGE_INTERVAL);
    }
    *requests += 1;
}

/// 表紙にする画像の URL を決める。
///
/// 商品ページ（`/products/<id>`）の**1 枚目**の画像を使う（一覧・注文ページには画像が無い）。
/// 既に表紙が入っている行は商品ページを叩き直さない（1 商品 = 1 リクエストを毎回払わない）。
/// 取れなかったときは表紙なしで返す（本の取り込みは続ける。次の同期で再試行される）。
fn cover_url(
    pool: &SqlitePool,
    client: &mut FantiaClient,
    requests: &mut usize,
    product_id: &str,
) -> Result<Option<String>, FantiaError> {
    if let Some(existing) = bookshelf::thumbnail_url(pool, SITE_ID, product_id)?
        && !existing.trim().is_empty()
    {
        return Ok(Some(existing));
    }
    pace_requests(requests);
    match client.product_page_html(product_id) {
        Ok(html) => Ok(shop::parse_product_image_url(&html)),
        Err(error) => {
            log::warn!("fantia: 商品ページを取得できません（表紙なし）: {product_id}: {error}");
            Ok(None)
        }
    }
}

/// 購入一覧を `pages` ページ分だけ取り込み、続きの位置と残り回数を返す。
///
/// 1 回で全ページ取ると、購入数の多いアカウントでは 1 操作で大量のリクエストになる。
/// ユーザーの操作を挟んで少しずつ進めるための分割版（続きは `next` を次の呼び出しに渡す）。
/// 注文の無いページで終端とみなす（一覧の総数が無いため、ページャの終わりはここでしか
/// 分からない）。`owner` は暗号化済み sub（ログイン中のみ `Some`）。
pub fn save_purchases_batch(
    pool: &SqlitePool,
    client: &mut FantiaClient,
    owner: Option<&str>,
    cursor: PurchaseCursor,
    pages: usize,
) -> Result<PurchaseBatch, FantiaError> {
    let pages = pages.max(1);
    let mut next_page = cursor.page.max(1);
    // 最後に取得したページ（続きの位置はここから出す）
    let mut last_fetched = next_page.saturating_sub(1);
    let mut fetched_pages = 0usize;
    let mut saved = 0usize;
    let mut total_items = 0usize;
    let mut requests = 0usize;
    // 最後に取得したページに注文があったか（＝一覧の終わりに達していない）
    let mut has_next = false;
    while fetched_pages < pages {
        pace_requests(&mut requests);
        let html = client.purchases_page_html(next_page)?;
        last_fetched = next_page;
        fetched_pages += 1;
        let order_ids = shop::parse_order_ids(&html);
        if order_ids.is_empty() {
            // 注文が無いページ = 一覧の終わり（次は無い）
            has_next = false;
            break;
        }
        has_next = true;
        for order_id in &order_ids {
            pace_requests(&mut requests);
            let order_html = client.order_page_html(order_id)?;
            let ordered_at = shop::parse_order_date(&order_html);
            for item in shop::parse_ordered_items(&order_html) {
                total_items += 1;
                // 開けない拡張子は保存しない（商品ページも取りに行かない）
                if !is_viewable_file(&item.file_name) {
                    save_purchase(pool, item, ordered_at.clone(), None)?;
                    continue;
                }
                let thumbnail_url = cover_url(pool, client, &mut requests, &item.product_id)?;
                if save_purchase(pool, item, ordered_at.clone(), thumbnail_url)? {
                    saved += 1;
                }
            }
        }
        next_page += 1;
    }
    bookshelf::attribute_owner(pool, SITE_ID, owner)?;
    let next = has_next.then(|| PurchaseCursor {
        page: last_fetched + 1,
    });
    // 総数が無いので残りページは割り出せない。続きがあるときは「少なくともあと 1 回」を
    // 返す（`dlsite::sync` の `pages_left.div_ceil(pages).max(1)` と同じ下限の考え方）。
    let remaining_runs = usize::from(next.is_some());
    Ok(PurchaseBatch {
        saved,
        fetched_pages,
        total_items,
        next,
        remaining_runs,
    })
}

/// 注文の商品 1 件を本棚へ upsert する（開けない拡張子は保存しない）。保存したら `true`。
fn save_purchase(
    pool: &SqlitePool,
    item: OrderedItem,
    ordered_at: Option<String>,
    thumbnail_url: Option<String>,
) -> Result<bool, FantiaError> {
    let extension = file_extension(&item.file_name);
    if !is_viewable_file(&item.file_name) {
        log::info!(
            "fantia: ビューアーで開けない拡張子を飛ばします: product={} extension={:?} file={}",
            item.product_id,
            extension,
            item.file_name
        );
        return Ok(false);
    }
    let ts = now();
    let shelf_item = BookshelfItem {
        site_id: SITE_ID.into(),
        database_id: item.product_id.clone(),
        title: item.title,
        // ショップの商品ページにしか売り手 / 作者は無く、同期では取りに行かない
        // （1 商品 = 1 リクエストが増える。ダウンロード時に埋める余地を残す）。
        circle_name: String::new(),
        author: String::new(),
        // 表紙は商品ページの 1 枚目（[`cover_url`]）
        thumbnail_url,
        format: extension.to_uppercase(),
        caused_at: ordered_at,
        event_name: None,
        event_slug: None,
        event_id: None,
        file_name: Some(item.file_name),
        download_url: Some(shop::product_download_url(&item.product_id)),
        download_options: None,
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
        media_category: None,
        ai_type: None,
        // 同期では DRM を判定できない（Fantia のショップは 302 → CDN の直リンクで、
        // DRM の有無を返す API が無い）ため、**「なし」ではなく「不明」**として保存する。
        is_drm: crate::drm::DrmStatus::Unknown.as_db(),
        release_date: None,
        description: None,
        theme: None,
        maker_id: None,
        page_count: None,
        age_rating: None,
        series_name: None,
    };
    bookshelf::upsert(pool, &shelf_item)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fantia::client::FantiaSession;
    use crate::tbf::TbfError;
    use crate::tbf::transport::{RequestSpec, ResponseSpec, Transport};
    use std::collections::BTreeMap;

    /// 応答を URL で振り分ける transport。叩いた URL を記録する。
    struct MockTransport {
        handler: Box<dyn FnMut(RequestSpec) -> Result<ResponseSpec, TbfError> + Send>,
    }

    impl Transport for MockTransport {
        fn send(&mut self, spec: RequestSpec) -> Result<ResponseSpec, TbfError> {
            (self.handler)(spec)
        }
    }

    fn html(body: &str) -> ResponseSpec {
        ResponseSpec {
            status: 200,
            headers: vec![],
            body: body.as_bytes().to_vec(),
        }
    }

    fn session() -> FantiaSession {
        FantiaSession::from_site_cookies(BTreeMap::from([(
            "_session_id".to_string(),
            "abc".to_string(),
        )]))
    }

    /// 注文一覧の 1 ページ相当（注文 id のリンクだけ）。
    fn list_html(order_ids: &[&str]) -> String {
        order_ids
            .iter()
            .map(|id| {
                format!(
                    r#"<div class="list-group-item"><a href="/mypage/users/purchases/{id}">注文</a></div>"#
                )
            })
            .collect::<Vec<_>>()
            .join("")
    }

    /// 注文ページ相当（注文日時 + 商品のブロック）。
    fn order_html(date: &str, items: &[(&str, &str, &str)]) -> String {
        let blocks: Vec<String> = items
            .iter()
            .map(|(product_id, title, file_name)| {
                format!(
                    r#"<div class="list-group-item">
                         <a href="/products/{product_id}">{title}</a>
                         <small>ファイル名: {file_name}</small>
                       </div>"#
                )
            })
            .collect();
        format!(
            r#"<div>注文日時 <span class="time">{date} 22:52</span></div>{}"#,
            blocks.join("")
        )
    }

    /// 開ける拡張子（zip / pdf）だけが保存され、mp4 は保存されないこと。
    /// 保存した行の内容（site / ファイル名 / DL URL / 購入日 / 形式）も確かめる。
    #[test]
    fn save_purchases_keeps_only_the_viewer_openable_files() {
        let pool = crate::db::test_pool();
        let transport = MockTransport {
            handler: Box::new(|spec: RequestSpec| {
                let body = if spec.url.contains("page=1") {
                    list_html(&["900001", "900002"])
                } else if spec.url.contains("/purchases/900001") {
                    order_html(
                        "2026/07/30",
                        &[
                            ("111111", "本1", "sample-one.zip"),
                            ("222222", "動画", "movie.mp4"),
                            ("333333", "本2", "sample-two.zip"),
                        ],
                    )
                } else if spec.url.contains("/purchases/900002") {
                    order_html("2026/07/31", &[("444444", "本3", "sample-three.pdf")])
                } else if spec.url.contains("/products/") {
                    // 商品ページ（実ページと同じ形: ld+json の 1 枚目が表紙）
                    let id = spec.url.rsplit('/').next().unwrap_or_default();
                    format!(
                        r#"<script type="application/ld+json">[{{"@type":"Product","image":["https://c.fantia.jp/uploads/product/image/{id}/cover.jpg"]}}]</script>"#
                    )
                } else {
                    // 2 ページ目は注文が無い（終端）
                    "<p>no orders</p>".to_string()
                };
                Ok(html(&body))
            }),
        };
        let mut client = FantiaClient::with_transport(Box::new(transport), session());

        let batch = save_purchases_batch(
            &pool,
            &mut client,
            None,
            PurchaseCursor::default(),
            PURCHASE_PAGES_PER_RUN,
        )
        .unwrap();

        // 2 ページ目が空なので、そこで終わり（続きは無い）
        assert_eq!(batch.fetched_pages, 2);
        assert_eq!(batch.saved, 3, "zip / zip / pdf の 3 件が保存される");
        assert_eq!(batch.total_items, 4, "読んだ商品の件数（mp4 も含む）");
        assert_eq!(batch.next, None);
        assert_eq!(batch.remaining_runs, 0);

        let rows = bookshelf::list(&pool, SITE_ID).unwrap();
        let ids: Vec<&str> = rows.iter().map(|row| row.database_id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["444444", "111111", "333333"],
            "購入日の新しい順（同じ日はタイトル順）"
        );
        assert!(
            !ids.contains(&"222222"),
            "ビューアーで開けない mp4 を保存している"
        );

        let first = rows.iter().find(|row| row.database_id == "111111").unwrap();
        assert_eq!(first.site_id, "fantia");
        assert_eq!(first.title, "本1");
        assert_eq!(first.format, "ZIP");
        assert_eq!(first.file_name.as_deref(), Some("sample-one.zip"));
        assert_eq!(
            first.download_url.as_deref(),
            Some("https://fantia.jp/products/111111/download"),
            "ダウンロード URL が商品 id から組み立てられていない"
        );
        assert_eq!(first.caused_at.as_deref(), Some("2026/07/30"));
        assert_eq!(
            first.thumbnail_url.as_deref(),
            Some("https://c.fantia.jp/uploads/product/image/111111/cover.jpg"),
            "商品ページの 1 枚目が表紙になっていない"
        );
        assert_eq!(first.is_downloadable, 1);
        assert_eq!(first.is_purchased, 1);
        assert_eq!(first.is_active, 1);
        assert_eq!(first.is_favorite, 0);
        assert_eq!(first.is_hidden, 0);
        assert_eq!(first.is_checked, 0);
        assert_eq!(first.is_new, 0);
        assert_eq!(first.tags_json, None);
        assert_eq!(
            first.is_drm,
            crate::drm::DrmStatus::Unknown.as_db(),
            "同期の DRM は「不明」で入れる"
        );

        let pdf = rows.iter().find(|row| row.database_id == "444444").unwrap();
        assert_eq!(pdf.format, "PDF");
        assert_eq!(pdf.caused_at.as_deref(), Some("2026/07/31"));
    }

    /// 分割同期: 1 回で `pages` ページだけ取り込み、続きの位置と「あと何回か」を返す。
    /// 注文の無いページで終端になること（そこから先は叩かない）も確かめる。
    #[test]
    fn batch_takes_only_the_given_pages_and_advances_the_cursor() {
        use std::sync::Arc;
        let pool = crate::db::test_pool();
        let calls = Arc::new(parking_lot::Mutex::new(Vec::<String>::new()));
        let transport = MockTransport {
            handler: Box::new({
                let calls = calls.clone();
                move |spec: RequestSpec| {
                    calls.lock().push(spec.url.clone());
                    // 注文ページ（`/purchases/<id>`）は商品 1 件を返す
                    if let Some(rest) = spec.url.split("/purchases/").nth(1)
                        && rest.chars().all(|c| c.is_ascii_digit())
                    {
                        let product_id = format!("{}001", rest.trim_start_matches('9'));
                        return Ok(html(&order_html(
                            "2026/07/30",
                            &[(&product_id, "本", &format!("{product_id}.zip"))],
                        )));
                    }
                    // 1 / 2 ページ目は注文が 1 件、3 ページ目は空（＝終端）
                    let page: usize = spec
                        .url
                        .split("page=")
                        .nth(1)
                        .and_then(|rest| rest.split('&').next())
                        .and_then(|rest| rest.parse().ok())
                        .unwrap_or(1);
                    if page <= 2 {
                        Ok(html(&list_html(&[&format!("90000{page}")])))
                    } else {
                        Ok(html("<p>no orders</p>"))
                    }
                }
            }),
        };
        let mut client = FantiaClient::with_transport(Box::new(transport), session());

        // 1 回目: 1 ページだけ取る（続きはあるが、ページ数は分からないので下限 1）
        let batch =
            save_purchases_batch(&pool, &mut client, None, PurchaseCursor::default(), 1).unwrap();
        assert_eq!(batch.fetched_pages, 1, "指定した 1 ページだけ取る");
        assert_eq!(batch.saved, 1);
        assert_eq!(batch.next, Some(PurchaseCursor { page: 2 }));
        assert_eq!(batch.remaining_runs, 1, "総数が無いので「あと 1 回」を出す");

        // 2 回目: 続きから再開する
        let rest = save_purchases_batch(&pool, &mut client, None, batch.next.unwrap(), 1).unwrap();
        assert_eq!(rest.fetched_pages, 1);
        assert_eq!(rest.saved, 1);
        assert_eq!(rest.next, Some(PurchaseCursor { page: 3 }));
        let urls = calls.lock().clone();
        assert!(
            urls.iter().any(|url| url.contains("page=2")),
            "続きの位置から取り直していない: {urls:?}"
        );

        // 3 回目: 注文の無いページで終端（続きは無い）
        let end = save_purchases_batch(&pool, &mut client, None, rest.next.unwrap(), 5).unwrap();
        assert_eq!(end.fetched_pages, 1, "空ページの先は叩かない");
        assert_eq!(end.saved, 0);
        assert_eq!(end.next, None);
        assert_eq!(end.remaining_runs, 0);
        let urls = calls.lock().clone();
        assert_eq!(
            urls,
            vec![
                "https://fantia.jp/mypage/users/purchases?page=1",
                "https://fantia.jp/mypage/users/purchases/900001",
                "https://fantia.jp/products/00001001",
                "https://fantia.jp/mypage/users/purchases?page=2",
                "https://fantia.jp/mypage/users/purchases/900002",
                "https://fantia.jp/products/00002001",
                "https://fantia.jp/mypage/users/purchases?page=3",
            ],
            "叩いた URL が想定と違う（1 ページ = 一覧 1 + 注文 1 + 商品 1）"
        );
        // 3 ページ目は注文が無いので、その先（page=4）は叩かない
        assert!(
            !urls.iter().any(|url| url.contains("page=4")),
            "終端の先を叩いている: {urls:?}"
        );
    }

    /// 拡張子の判定（大文字・複数ドット・ドット無し）。
    #[test]
    fn viewable_file_detection_uses_the_last_extension_case_insensitively() {
        assert!(is_viewable_file("book.zip"));
        assert!(is_viewable_file("BOOK.PDF"));
        assert!(is_viewable_file("book.tar.zip"));
        assert!(is_viewable_file("scan-01.JPEG"));
        assert!(!is_viewable_file("movie.mp4"));
        assert!(!is_viewable_file("voice.MP3"));
        // rar / 7z は取り込みが対応していないので保存しない（落とせない本を作らない）
        assert!(!is_viewable_file("archive.rar"));
        assert!(!is_viewable_file("archive.7z"));
        assert!(!is_viewable_file("no-extension"));
        // 許可リストに無い拡張子は保存しない（中身を判定できない）
        assert!(!is_viewable_file("book.epub"));
        assert_eq!(file_extension("BOOK.PDF"), "pdf");
        assert_eq!(file_extension("no-extension"), "");
    }
}
