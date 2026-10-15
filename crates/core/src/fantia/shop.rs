//! Fantia の**ショップ購入（商品）**のページ解析。
//!
//! 公式 API は無く、自分の購入履歴ページ（`/mypage/users/purchases`）の HTML を読む。
//! ページの形は third-party 実装（`ryoctrl/fc-downloader` の `fantia/purchases.ts`。2026-07 に
//! 実サイトで確認）と、そのテストの HTML フィクスチャに合わせている。
//!
//! **実セッションでの VERIFY が必要**（Fantia 側の変更で崩れうる）。
//!
//! ページの関係:
//! - `/mypage/users/purchases?page=N` … 注文の一覧（注文 id と、DL できる商品の id）
//! - `/mypage/users/purchases/<orderId>` … その注文の商品（タイトル + `ファイル名:` + 注文日時）
//! - `/products/<productId>` … 商品ページ（売り手のファンクラブ id、商品画像）
//! - `/products/<productId>/download` … 本体（302 → CDN）

use regex::Regex;

/// サイト。
const BASE: &str = "https://fantia.jp";

/// 注文一覧の URL（`page` は 1 始まり）。
pub fn purchases_page_url(page: usize) -> String {
    format!("{BASE}/mypage/users/purchases?page={page}")
}

/// 注文ページの URL。
pub fn order_page_url(order_id: &str) -> String {
    format!("{BASE}/mypage/users/purchases/{order_id}")
}

/// 商品ページの URL。
pub fn product_page_url(product_id: &str) -> String {
    format!("{BASE}/products/{product_id}")
}

/// 商品のダウンロード URL（302 で CDN のファイルへ飛ぶ）。
pub fn product_download_url(product_id: &str) -> String {
    format!("{BASE}/products/{product_id}/download")
}

/// 注文ページに並ぶ商品（**DL できるものだけ**）。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OrderedItem {
    /// 商品 id（`/products/<id>`）。
    pub product_id: String,
    /// 商品名（リンクのテキスト。取れなければ id で代用）。
    pub title: String,
    /// 元のファイル名（`ファイル名: ...`）。
    pub file_name: String,
}

/// 正規表現にマッチした 1 つ目のグループを、現れた順・重複なしで集める。
fn collect_unique(html: &str, pattern: &str) -> Vec<String> {
    let re = Regex::new(pattern).expect("正規表現は固定");
    let mut out: Vec<String> = Vec::new();
    for captures in re.captures_iter(html) {
        let id = captures[1].to_string();
        if !out.contains(&id) {
            out.push(id);
        }
    }
    out
}

/// 注文一覧ページから注文 id を拾う（現れた順・重複なし）。
pub fn parse_order_ids(html: &str) -> Vec<String> {
    collect_unique(html, r"/mypage/users/purchases/(\d+)")
}

/// 注文一覧ページから、**DL できる商品**の id を拾う（`/download` の付いたリンクだけ）。
///
/// `/products/<id>` だけのリンク（売り場・未購入）は拾わない。
pub fn parse_purchased_product_ids(html: &str) -> Vec<String> {
    collect_unique(html, r"/products/(\d+)/download\b")
}

/// 注文ページの商品を、`list-group-item` のブロック単位で対応づけて拾う。
///
/// ファイル名（`ファイル名:`）の無いブロックは DL できないので落とす（物販など）。
/// 想定外の HTML では空を返す（落とさない）。
pub fn parse_ordered_items(html: &str) -> Vec<OrderedItem> {
    let product = Regex::new(r"/products/(\d+)").expect("正規表現は固定");
    let file_name = Regex::new(r"ファイル名:\s*([^<\r\n]+)").expect("正規表現は固定");
    // サムネイルのリンク（中身が `<img>`）ではなく、テキストを持つリンクを商品名として拾う
    let title = Regex::new(r#"/products/\d+"[^>]*>\s*([^<\s][^<]*?)\s*<"#).expect("正規表現は固定");
    html.split("list-group-item")
        .skip(1)
        .filter_map(|block| {
            let product_id = product.captures(block)?.get(1)?.as_str().to_string();
            let file_name = file_name
                .captures(block)?
                .get(1)?
                .as_str()
                .trim()
                .to_string();
            let title = title
                .captures(block)
                .and_then(|captures| captures.get(1))
                .map(|matched| matched.as_str().trim().to_string())
                .filter(|title| !title.is_empty())
                .unwrap_or_else(|| product_id.clone());
            Some(OrderedItem {
                product_id,
                title,
                file_name,
            })
        })
        .collect()
}

/// 注文ページの注文日時を `YYYY/MM/DD` として読む（時刻は落とす。他のストアと同じ表記）。
pub fn parse_order_date(html: &str) -> Option<String> {
    let re = Regex::new(r"注文日時[\s\S]{0,120}?(\d{4})/(\d{2})/(\d{2})").expect("正規表現は固定");
    let captures = re.captures(html)?;
    Some(format!(
        "{}/{}/{}",
        &captures[1], &captures[2], &captures[3]
    ))
}

/// 商品ページから、売り手のファンクラブ id（最初の `/fanclubs/<id>`）を読む。
pub fn parse_seller_fanclub_id(html: &str) -> Option<String> {
    Regex::new(r"/fanclubs/(\d+)")
        .expect("正規表現は固定")
        .captures(html)
        .map(|captures| captures[1].to_string())
}

/// 商品ページから、**この商品の 1 枚目**の画像（表紙に使う URL）を選ぶ。
///
/// 実ページ（2026-10-15 実測 / product 1008246）の形:
/// - `application/ld+json` の `Product.image[0]` が**原寸の 1 枚目**
/// - ギャラリー（`product-gallery-item`）は `micro_<name>` の小さいサムネを並べる
/// - `blurred_ogp_<name>` は R18 の**ぼかし** OGP（表紙に使うとぼける）
///
/// そのため ld+json の 1 枚目を優先し、無ければギャラリー先頭の `micro_` / `thumb_` を
/// 外した URL（同じディレクトリにある原寸）へ落とす。`blurred_ogp_` は使わない。
pub fn parse_product_image_url(html: &str) -> Option<String> {
    if let Some(url) = ld_json_first_product_image(html)
        && !url.contains("blurred_ogp_")
    {
        return Some(url);
    }
    let gallery = Regex::new(r#"<div class="product-gallery-item">\s*<img[^>]*src="([^"]+)""#)
        .expect("正規表現は固定");
    let url = gallery.captures(html)?.get(1)?.as_str().to_string();
    if url.contains("blurred_ogp_") {
        return None;
    }
    Some(strip_thumbnail_prefix(&url))
}

/// `.../micro_<name>` / `.../thumb_<name>` を原寸の名前へ戻す（同じディレクトリにある）。
fn strip_thumbnail_prefix(url: &str) -> String {
    let Some((head, name)) = url.rsplit_once('/') else {
        return url.to_string();
    };
    for prefix in ["micro_", "thumb_"] {
        if let Some(rest) = name.strip_prefix(prefix) {
            return format!("{head}/{rest}");
        }
    }
    url.to_string()
}

/// `application/ld+json` の Product から 1 枚目の画像 URL を取り出す。
fn ld_json_first_product_image(html: &str) -> Option<String> {
    let script = Regex::new(r#"(?s)<script type="application/ld\+json">(.*?)</script>"#)
        .expect("正規表現は固定");
    let json = script.captures(html)?.get(1)?.as_str();
    let value: serde_json::Value = serde_json::from_str(json).ok()?;
    let entries = value.as_array().cloned().unwrap_or_else(|| vec![value]);
    entries.iter().find_map(|entry| {
        entry
            .get("image")
            .and_then(|image| image.as_array())
            .and_then(|images| images.first())
            .and_then(|url| url.as_str())
            .map(str::to_string)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 注文履歴の一覧ページ相当（third-party のテストの HTML。id/名前は作り物）。
    const LIST_HTML: &str = r#"
<div class="list-group-item">
  <a href="/mypage/users/purchases/900001">Sample product one</a>
  <a class="btn" href="/products/111111/download">商品をダウンロード</a>
</div>
<div class="list-group-item">
  <a href="/mypage/users/purchases/900002">Sample product two</a>
  <a class="btn" href="/products/222222/download">商品をダウンロード</a>
</div>"#;

    /// 注文ページ相当（商品・タイトル・ファイル名・注文日時）。
    const ORDER_HTML: &str = r#"
<div>注文日時 <span class="time">2026/07/30 22:52</span></div>
<div class="list-group-item">
  <a class="module-thumbnail" href="/products/111111"><img src="thumb.jpg" /></a>
  <p><strong><a href="/products/111111">Sample product one</a></strong></p>
  <small>ファイル名: sample-one.mp4</small>
  <a href="/products/555555/content_download">ダウンロード</a>
</div>
<div class="list-group-item">
  <a href="/products/222222">Sample product two</a>
  <small>ファイル名: sample-two.zip</small>
</div>"#;

    /// 一覧ページから注文 id を拾う。
    #[test]
    fn order_ids_are_collected_from_the_list() {
        assert_eq!(parse_order_ids(LIST_HTML), vec!["900001", "900002"]);
        assert!(parse_order_ids("<p>no orders</p>").is_empty());
    }

    /// 一覧ページから **DL できる商品** の id を拾う（`/download` の付いたリンクだけ）。
    #[test]
    fn purchased_product_ids_require_a_download_link() {
        assert_eq!(
            parse_purchased_product_ids(LIST_HTML),
            vec!["111111", "222222"]
        );
        // `/download` の付かない商品リンク（未購入の売り場）は拾わない
        assert!(parse_purchased_product_ids(r#"<a href="/products/333333">shop</a>"#).is_empty());
        // 重複は畳む
        let dup =
            r#"<a href="/products/111111/download">a</a><a href="/products/111111/download">b</a>"#;
        assert_eq!(parse_purchased_product_ids(dup), vec!["111111"]);
    }

    /// 注文ページの商品（商品 id + タイトル + ファイル名）を、ブロック単位で対応づける。
    #[test]
    fn ordered_items_pair_product_title_and_file_name() {
        assert_eq!(
            parse_ordered_items(ORDER_HTML),
            vec![
                OrderedItem {
                    product_id: "111111".into(),
                    title: "Sample product one".into(),
                    file_name: "sample-one.mp4".into(),
                },
                OrderedItem {
                    product_id: "222222".into(),
                    title: "Sample product two".into(),
                    file_name: "sample-two.zip".into(),
                },
            ]
        );
    }

    /// ファイル名の無いブロック（物販など）は DL できないので拾わない。壊れた HTML でも落ちない。
    #[test]
    fn ordered_items_skip_non_downloadable_and_broken_markup() {
        let physical =
            r#"<div class="list-group-item"><a href="/products/444444">physical</a></div>"#;
        assert!(parse_ordered_items(physical).is_empty());
        assert!(parse_ordered_items("").is_empty());
        assert!(parse_ordered_items("<p>no items</p>").is_empty());
    }

    /// 注文日時を `YYYY/MM/DD` として読む（他のストアと同じ表記に合わせる）。
    #[test]
    fn order_date_is_read_as_a_day() {
        assert_eq!(parse_order_date(ORDER_HTML).as_deref(), Some("2026/07/30"));
        assert_eq!(parse_order_date("<p>no date</p>"), None);
    }

    /// 商品ページの実物（2026-10-15 実測 / product 1008246）の形から、**1 枚目 =
    /// ld+json の `Product.image[0]`（原寸）** を表紙に選ぶこと。ギャラリーの `micro_` と
    /// OGP の `blurred_ogp_` は使わない。売り手は最初の `/fanclubs/<id>`。
    #[test]
    fn product_page_uses_the_first_ld_json_image_as_the_cover() {
        let html = r#"
            <meta content="https://c.fantia.jp/uploads/product/image/1008246/blurred_ogp_dc65e946.jpg" property="og:image" />
            <script type="application/ld+json">[{"@type":"Product","name":"x","image":["https://c.fantia.jp/uploads/product/image/1008246/dc65e946.jpg","https://c.fantia.jp/uploads/product_image/file/897993/micro_5949e477_x.jpg"]}]</script>
            <div class="product-gallery"><div class="product-gallery-item"><img class="img-fluid lazyload img-blurred" alt="x" src="https://c.fantia.jp/uploads/product/image/1008246/micro_dc65e946.jpg" /></div></div>
            <a href="/fanclubs/374068">seller</a>
            <a href="/fanclubs/999999">other</a>"#;
        assert_eq!(
            parse_product_image_url(html).as_deref(),
            Some("https://c.fantia.jp/uploads/product/image/1008246/dc65e946.jpg"),
            "1 枚目（ld+json の原寸）を表紙にしていない"
        );
        assert_eq!(parse_seller_fanclub_id(html).as_deref(), Some("374068"));
    }

    /// ld+json が無いときは、ギャラリー先頭の `micro_` / `thumb_` を外した原寸へ落とす。
    /// ぼかし OGP しか無いページでは表紙を作らない。
    #[test]
    fn product_page_falls_back_to_the_gallery_thumbnail() {
        let gallery = r#"<div class="product-gallery-item"><img src="https://c.fantia.jp/uploads/product/image/1008246/micro_dc65e946.jpg" /></div>"#;
        assert_eq!(
            parse_product_image_url(gallery).as_deref(),
            Some("https://c.fantia.jp/uploads/product/image/1008246/dc65e946.jpg"),
            "サムネの接頭辞を外して原寸にしていない"
        );
        let blurred = r#"<meta content="https://c.fantia.jp/uploads/product/image/1/blurred_ogp_a.jpg" property="og:image" />"#;
        assert_eq!(
            parse_product_image_url(blurred),
            None,
            "ぼかし OGP を表紙にしている"
        );
        assert_eq!(parse_product_image_url("<p>none</p>"), None);
        assert_eq!(parse_seller_fanclub_id("<p>none</p>"), None);
    }

    /// ダウンロード URL は商品 id から組み立てる（302 で CDN のファイルへ）。
    #[test]
    fn download_url_is_built_from_the_product_id() {
        assert_eq!(
            product_download_url("111111"),
            "https://fantia.jp/products/111111/download"
        );
    }

    /// 注文一覧のページ URL（1 始まり）。
    #[test]
    fn orders_list_url_takes_a_page() {
        assert_eq!(
            purchases_page_url(1),
            "https://fantia.jp/mypage/users/purchases?page=1"
        );
    }
}
