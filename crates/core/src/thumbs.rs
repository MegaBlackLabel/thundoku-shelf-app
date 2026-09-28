//! 表紙バンドル（`thundoku-thumbs.json`）の平文を作る。
//!
//! Drive には封筒（[`opfspack::SealedEnvelope`] + [`opfspack::THUMBS_LABEL`]）で
//! 上げる。ここは**平文**（封筒の中身）の組み立てと、アップロード用画像の
//! エンコードだけを担当する。仕様は `docs/spec/10-pack-keys.md` §11.8。
//!
//! 平文の形:
//!
//! ```json
//! {"entries":[
//!   {"kind":"shelf","site_id":"dlsite","database_id":"RJ1","mime":"image/webp",
//!    "width":256,"height":384,"sha256":"…","data":"<base64>"},
//!   {"kind":"checklist","item_id":"…","mime":"image/jpeg",
//!    "width":256,"height":256,"sha256":"…","data":"<base64>"}
//! ],"format_version":1}
//! ```
//!
//! **時刻や mtime など実行ごとに変わる値を平文に入れない**（封筒の
//! `content_hmac` による変更検知が毎回「変わった」になり、毎回アップロードに
//! なるため）。`entries` は `(kind, key)` の順に固定する。キーの並びは
//! `serde_json` の既定（辞書順）に従うので、同じ内容なら同じバイト列になる。

use std::collections::HashMap;
use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use sha2::{Digest, Sha256};
use sqlx::SqlitePool;

use crate::db;

/// 平文の `format_version`。
pub const FORMAT_VERSION: u32 = 1;

/// 本棚の表紙を縮小する最大幅（カード表示 128〜160px の 2x）。
///
/// 2026-09-28 の実測（実データ 618 枚の 448px PNG キャッシュ → WebP q80）:
/// **合計 9.5MB / 平均 15.5KB / p95 24KB / 最大 41KB**（448px のままなら 21.8MB、
/// 源の PNG は 169MB）。256px でカード表示に十分で、バンドルが 10MB 級に収まる。
pub const SHELF_MAX_WIDTH: u32 = 256;

/// 本棚の表紙の WebP 品質。
pub const SHELF_QUALITY: u8 = 80;

/// 上限を超えた 1 枚を再エンコードするときの品質。
const FALLBACK_QUALITY: u8 = 60;

/// 1 枚のエンコード後サイズの目標上限（超えたら [`FALLBACK_QUALITY`] で作り直す）。
pub const MAX_IMAGE_BYTES: usize = 64 * 1024;

/// 再エンコードしてもこれを超える 1 枚は載せない（病的な画像でバンドルを太らせない）。
pub const HARD_MAX_IMAGE_BYTES: usize = 256 * 1024;

/// 平文の警告しきい値（超えても上げる。分割は §11.8 の「未実装」）。
pub const WARN_PLAINTEXT_BYTES: usize = 48 * 1024 * 1024;

/// バンドルを作った結果。
#[derive(Clone, Debug)]
pub struct BuildOutcome {
    /// 封筒に入れる平文（JSON）。
    pub plaintext: Vec<u8>,
    /// 平文に載った枚数。
    pub entries: usize,
    /// この回に新しくエンコードした枚数。
    pub encoded: usize,
    /// 所有しているが載せられなかった枚数（取得元が無い / エンコード失敗 / 今回の上限）。
    /// 次回の同期でまた試す。
    pub deferred: usize,
}

/// 所有する表紙を集めてバンドルの平文を作る。
///
/// - 既に `thumbnail_share` にある分はそのまま使う（再エンコードしない）
/// - 無い分は取得元から作って `thumbnail_share` へ入れる（1 回 `max_encode` 枚まで）
/// - 取得元が無い分（表紙キャッシュ未取得）は載せずに `deferred` へ回す
///   （**同期の中でネットワークを叩かない**: 取得は本棚表示時の
///   `fetch_remote_covers` が行う）
pub fn build_plaintext(
    pool: &SqlitePool,
    owner_key: &[u8; 32],
    sub: Option<&str>,
    thumbnails_dir: &Path,
    max_encode: usize,
) -> Result<BuildOutcome, sqlx::Error> {
    let owned = db::thumbs::owned_keys(pool, owner_key, sub)?;
    let mut entries = db::thumbs::entries_for_owner(pool, &owned)?;

    let mut missing: Vec<(String, String)> = owned
        .iter()
        .filter(|(kind, key)| {
            !entries
                .iter()
                .any(|entry| entry.kind == *kind && entry.key == *key)
        })
        .map(|(kind, key)| (kind.to_string(), key.to_string()))
        .collect();
    missing.sort();

    // チェックリストは DB に base64 JPEG を持っているので、必要な分だけまとめて読む
    let checklist_ids: Vec<String> = missing
        .iter()
        .filter(|(kind, _)| kind == db::thumbs::KIND_CHECKLIST)
        .map(|(_, key)| key.clone())
        .collect();
    let checklist_thumbs: HashMap<String, String> =
        db::checklist::thumbnails_of(pool, &checklist_ids)?
            .into_iter()
            .collect();

    let mut encoded = 0usize;
    let mut deferred = 0usize;
    for (kind, key) in missing {
        if encoded >= max_encode {
            deferred += 1;
            continue;
        }
        let source = match kind.as_str() {
            db::thumbs::KIND_SHELF => encode_shelf_cover(thumbnails_dir, &key),
            db::thumbs::KIND_CHECKLIST => checklist_thumbs
                .get(&key)
                .and_then(|data| decode_checklist_thumbnail(data)),
            other => {
                log::warn!("thumbs: 未知の kind {other} をスキップ");
                None
            }
        };
        let Some(source) = source else {
            deferred += 1;
            continue;
        };
        entries.push(store(pool, &kind, &key, source)?);
        encoded += 1;
    }

    let plaintext = serialize(&mut entries);
    if plaintext.len() > WARN_PLAINTEXT_BYTES {
        log::warn!(
            "thumbs: バンドルが大きい（{} bytes / {} 枚）。分割は未実装",
            plaintext.len(),
            entries.len()
        );
    }
    Ok(BuildOutcome {
        plaintext,
        entries: entries.len(),
        encoded,
        deferred,
    })
}

/// アップロードする 1 枚（エンコード済み）。
struct EncodedImage {
    bytes: Vec<u8>,
    mime: &'static str,
    width: u32,
    height: u32,
    source_mtime: Option<i64>,
    source_size: Option<i64>,
}

/// エンコード済みの 1 枚を共有キャッシュ（`thumbnail_share`）へ入れる。
fn store(
    pool: &SqlitePool,
    kind: &str,
    key: &str,
    image: EncodedImage,
) -> Result<db::thumbs::ThumbEntry, sqlx::Error> {
    let entry = db::thumbs::ThumbEntry {
        kind: kind.to_string(),
        key: key.to_string(),
        mime: image.mime.to_string(),
        width: image.width,
        height: image.height,
        sha256: sha256_hex(&image.bytes),
        bytes: image.bytes,
        source_mtime: image.source_mtime,
        source_size: image.source_size,
    };
    db::thumbs::upsert(pool, &entry)?;
    Ok(entry)
}

/// 448px の表紙キャッシュから共有キャッシュを作る（表紙を取得した直後に呼ぶ）。
///
/// これを呼んでおくと、次の同期は再エンコードせずにバンドルを作れる。
/// 戻り値は「作れたか」。取得元が無い・読めない・大きすぎるときは `false`
/// （同期側が後で拾う）。
pub fn cache_shelf_cover(
    pool: &SqlitePool,
    thumbnails_dir: &Path,
    site_id: &str,
    database_id: &str,
) -> Result<bool, sqlx::Error> {
    let key = db::thumbs::shelf_key(site_id, database_id);
    let Some(image) = encode_shelf_cover(thumbnails_dir, &key) else {
        return Ok(false);
    };
    store(pool, db::thumbs::KIND_SHELF, &key, image)?;
    Ok(true)
}

/// チェックリストのサムネイル（base64 の 256px JPEG）を共有キャッシュへ入れる。
///
/// 保存時点で 256px JPEG になっているので再エンコードしない。
/// 画像でないデータは `false`（保存しない）。
pub fn cache_checklist_thumbnail(
    pool: &SqlitePool,
    item_id: &str,
    data_base64: &str,
) -> Result<bool, sqlx::Error> {
    let Some(image) = decode_checklist_thumbnail(data_base64) else {
        return Ok(false);
    };
    store(pool, db::thumbs::KIND_CHECKLIST, item_id, image)?;
    Ok(true)
}

/// `{site_id}_{database_id}_448.png`（本棚の表紙キャッシュ）を 256px WebP にする。
///
/// 取得元が無い・読めない・大きすぎる場合は `None`（呼び出し側が次回へ回す）。
fn encode_shelf_cover(thumbnails_dir: &Path, key: &str) -> Option<EncodedImage> {
    let (site_id, database_id) = key.split_once(':')?;
    let path = thumbnails_dir.join(format!("{site_id}_{database_id}_448.png"));
    let source = std::fs::read(&path).ok()?;
    let image = match image::load_from_memory(&source) {
        Ok(image) => image,
        Err(error) => {
            log::warn!(
                "thumbs: 表紙キャッシュを読めない {}: {error}",
                path.display()
            );
            return None;
        }
    };
    let (width, height) = (image.width(), image.height());
    let resized = if width > SHELF_MAX_WIDTH {
        let scale = SHELF_MAX_WIDTH as f32 / width as f32;
        let new_width = (width as f32 * scale).max(1.0) as u32;
        let new_height = (height as f32 * scale).max(1.0) as u32;
        image.resize(new_width, new_height, image::imageops::FilterType::Lanczos3)
    } else {
        image
    };
    let mut bytes = crate::import::encode_webp(&resized, SHELF_QUALITY).ok()?;
    if bytes.len() > MAX_IMAGE_BYTES
        && let Ok(smaller) = crate::import::encode_webp(&resized, FALLBACK_QUALITY)
    {
        bytes = smaller;
    }
    if bytes.len() > HARD_MAX_IMAGE_BYTES {
        log::warn!(
            "thumbs: 表紙が大きすぎるので載せない {}（{} bytes）",
            path.display(),
            bytes.len()
        );
        return None;
    }
    let metadata = std::fs::metadata(&path).ok();
    Some(EncodedImage {
        bytes,
        mime: "image/webp",
        width: resized.width(),
        height: resized.height(),
        source_mtime: metadata.as_ref().and_then(modified_secs),
        source_size: metadata.as_ref().map(|meta| meta.len() as i64),
    })
}

/// `checked_items.thumbnail_data`（base64 の 256px JPEG）をそのまま使う。
fn decode_checklist_thumbnail(data: &str) -> Option<EncodedImage> {
    let bytes = B64.decode(data).ok()?;
    // ヘッダだけ読む（全デコードはしない）
    let reader = image::ImageReader::new(std::io::Cursor::new(&bytes))
        .with_guessed_format()
        .ok()?;
    let (width, height) = reader.into_dimensions().ok()?;
    Some(EncodedImage {
        bytes,
        mime: "image/jpeg",
        width,
        height,
        source_mtime: None,
        source_size: None,
    })
}

/// ファイルの mtime（UNIX 秒）。
fn modified_secs(metadata: &std::fs::Metadata) -> Option<i64> {
    let modified = metadata.modified().ok()?;
    let since = modified.duration_since(std::time::UNIX_EPOCH).ok()?;
    Some(since.as_secs() as i64)
}

/// エンコード後バイト列の SHA-256（小文字 hex）。
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// 平文の JSON にする（`(kind, key)` 順に固定する）。
fn serialize(entries: &mut [db::thumbs::ThumbEntry]) -> Vec<u8> {
    entries
        .sort_by(|a, b| (a.kind.as_str(), a.key.as_str()).cmp(&(b.kind.as_str(), b.key.as_str())));
    let list: Vec<serde_json::Value> = entries.iter().map(entry_json).collect();
    let mut root = serde_json::Map::new();
    root.insert("format_version".to_string(), FORMAT_VERSION.into());
    root.insert("entries".to_string(), serde_json::Value::Array(list));
    serde_json::to_vec(&serde_json::Value::Object(root))
        .expect("in-memory JSON serialization cannot fail")
}

/// 1 枚を JSON にする。本棚は `site_id` + `database_id`、チェックリストは `item_id`
/// と、識別子を種別ごとの明示フィールドで出す（Web 側で文字列を分割させない）。
fn entry_json(entry: &db::thumbs::ThumbEntry) -> serde_json::Value {
    let mut map = serde_json::Map::new();
    map.insert(
        "kind".to_string(),
        serde_json::Value::String(entry.kind.clone()),
    );
    match entry.kind.as_str() {
        db::thumbs::KIND_SHELF => match entry.key.split_once(':') {
            Some((site_id, database_id)) => {
                map.insert(
                    "site_id".to_string(),
                    serde_json::Value::String(site_id.to_string()),
                );
                map.insert(
                    "database_id".to_string(),
                    serde_json::Value::String(database_id.to_string()),
                );
            }
            None => {
                log::warn!("thumbs: 本棚のキーが壊れている: {}", entry.key);
            }
        },
        _ => {
            map.insert(
                "item_id".to_string(),
                serde_json::Value::String(entry.key.clone()),
            );
        }
    }
    map.insert(
        "mime".to_string(),
        serde_json::Value::String(entry.mime.clone()),
    );
    map.insert("width".to_string(), entry.width.into());
    map.insert("height".to_string(), entry.height.into());
    map.insert(
        "sha256".to_string(),
        serde_json::Value::String(entry.sha256.clone()),
    );
    map.insert(
        "data".to_string(),
        serde_json::Value::String(B64.encode(&entry.bytes)),
    );
    serde_json::Value::Object(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{self, block_on, test_pool};
    use std::path::{Path, PathBuf};

    const KEY: [u8; 32] = [31u8; 32];

    fn temp_dir(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("thundoku-thumbs-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 448px の表紙キャッシュ（実運用と同じ名前・PNG）を置く。
    fn write_cover_png(dir: &Path, site_id: &str, database_id: &str) {
        let image = image::RgbaImage::from_fn(448, 672, |x, y| {
            image::Rgba([(x % 251) as u8, (y % 241) as u8, 96, 255])
        });
        image
            .save(dir.join(format!("{site_id}_{database_id}_448.png")))
            .unwrap();
    }

    fn jpeg_bytes(width: u32, height: u32) -> Vec<u8> {
        let image = image::RgbImage::from_fn(width, height, |x, y| {
            image::Rgb([(x % 251) as u8, (y % 241) as u8, 96])
        });
        let mut out = Vec::new();
        image
            .write_to(
                &mut std::io::Cursor::new(&mut out),
                image::ImageFormat::Jpeg,
            )
            .unwrap();
        out
    }

    fn seed_shelf_item(pool: &db::SqlitePool, site_id: &str, database_id: &str) {
        block_on(async {
            sqlx::query(
                "INSERT INTO bookshelf_items (site_id, database_id, title) VALUES (?1, ?2, '本')",
            )
            .bind(site_id)
            .bind(database_id)
            .execute(pool)
            .await
            .unwrap();
        });
    }

    fn seed_checklist_item(pool: &db::SqlitePool, id: &str, thumbnail: &[u8]) {
        block_on(async {
            sqlx::query(
                "INSERT INTO tbf_events (id, site_id, event_name) \
                 VALUES ('tbf20', 'techbookfest', '技術書典20') ON CONFLICT(id) DO NOTHING",
            )
            .execute(pool)
            .await
            .unwrap();
            sqlx::query(
                "INSERT INTO checked_items (id, event_id, circle_name, thumbnail_data) \
                 VALUES (?1, 'tbf20', 'サークル', ?2)",
            )
            .bind(id)
            .bind(B64.encode(thumbnail))
            .execute(pool)
            .await
            .unwrap();
        });
    }

    fn entries(plaintext: &[u8]) -> Vec<serde_json::Value> {
        let value: serde_json::Value = serde_json::from_slice(plaintext).unwrap();
        assert_eq!(value["format_version"], 1);
        value["entries"].as_array().unwrap().clone()
    }

    #[test]
    fn an_empty_owner_gets_an_empty_bundle() {
        let pool = test_pool();
        let dir = temp_dir("empty");
        let outcome = build_plaintext(&pool, &KEY, None, &dir, 100).unwrap();

        assert_eq!(outcome.entries, 0);
        assert_eq!(outcome.encoded, 0);
        let value: serde_json::Value = serde_json::from_slice(&outcome.plaintext).unwrap();
        assert_eq!(value["format_version"], 1);
        assert!(value["entries"].as_array().unwrap().is_empty());
    }

    #[test]
    fn build_encodes_a_shelf_cover_and_caches_it() {
        let pool = test_pool();
        let dir = temp_dir("shelf");
        seed_shelf_item(&pool, "dlsite", "RJ1");
        write_cover_png(&dir, "dlsite", "RJ1");

        let outcome = build_plaintext(&pool, &KEY, None, &dir, 100).unwrap();
        assert_eq!(outcome.entries, 1);
        assert_eq!(outcome.encoded, 1, "初回はエンコードする");
        assert_eq!(outcome.deferred, 0);
        let entry = &entries(&outcome.plaintext)[0];
        assert_eq!(entry["kind"], "shelf");
        assert_eq!(entry["site_id"], "dlsite");
        assert_eq!(entry["database_id"], "RJ1");
        assert_eq!(entry["mime"], "image/webp");
        assert_eq!(entry["width"], 256, "448px のキャッシュを 256px へ縮小する");
        assert_eq!(entry["height"], 384, "縦横比を保つ");
        let bytes = B64.decode(entry["data"].as_str().unwrap()).unwrap();
        assert_eq!(&bytes[..4], b"RIFF", "WebP になっていない");
        assert!(
            bytes.len() <= MAX_IMAGE_BYTES,
            "1 枚の上限を超えている（{} bytes）",
            bytes.len()
        );
        assert_eq!(
            entry["sha256"].as_str().unwrap(),
            sha256_hex(&bytes),
            "sha256 は data と一致していなければならない"
        );
        assert!(
            db::thumbs::get(&pool, db::thumbs::KIND_SHELF, "dlsite:RJ1")
                .unwrap()
                .is_some(),
            "次回のために派生キャッシュへ入れる"
        );

        // 2 回目はキャッシュから作る（再エンコードしない・同じバイト列）
        let second = build_plaintext(&pool, &KEY, None, &dir, 100).unwrap();
        assert_eq!(second.encoded, 0, "2 回目はエンコードしない");
        assert_eq!(
            second.plaintext, outcome.plaintext,
            "同じ内容なら同じバイト列"
        );
    }

    #[test]
    fn build_ships_checklist_thumbnails_untouched() {
        let pool = test_pool();
        let dir = temp_dir("checklist");
        let jpeg = jpeg_bytes(256, 360);
        seed_checklist_item(&pool, "check-1", &jpeg);

        let outcome = build_plaintext(&pool, &KEY, None, &dir, 100).unwrap();
        assert_eq!(outcome.entries, 1);
        assert_eq!(outcome.encoded, 1);
        let entry = &entries(&outcome.plaintext)[0];
        assert_eq!(entry["kind"], "checklist");
        assert_eq!(entry["item_id"], "check-1");
        assert_eq!(
            entry["mime"], "image/jpeg",
            "チェックリストは既存の 256px JPEG をそのまま運ぶ"
        );
        assert_eq!(entry["width"], 256);
        assert_eq!(entry["height"], 360);
        assert_eq!(
            B64.decode(entry["data"].as_str().unwrap()).unwrap(),
            jpeg,
            "再エンコードで画質を落とさない"
        );
    }

    #[test]
    fn build_defers_missing_sources_instead_of_failing() {
        let pool = test_pool();
        let dir = temp_dir("missing");
        seed_shelf_item(&pool, "dlsite", "RJ-gone");
        seed_shelf_item(&pool, "fanza", "d_1");

        let outcome = build_plaintext(&pool, &KEY, None, &dir, 100).unwrap();
        assert_eq!(outcome.entries, 0);
        assert_eq!(outcome.encoded, 0);
        assert_eq!(outcome.deferred, 2, "取得元が無い分は次回に回す");
    }

    #[test]
    fn build_stops_encoding_at_the_limit() {
        let pool = test_pool();
        let dir = temp_dir("limit");
        for database_id in ["RJ1", "RJ2", "RJ3"] {
            seed_shelf_item(&pool, "dlsite", database_id);
            write_cover_png(&dir, "dlsite", database_id);
        }

        let outcome = build_plaintext(&pool, &KEY, None, &dir, 2).unwrap();
        assert_eq!(outcome.encoded, 2, "1 回のエンコードは上限まで");
        assert_eq!(outcome.entries, 2);
        assert_eq!(outcome.deferred, 1);

        // 残りは次の同期で載る
        let second = build_plaintext(&pool, &KEY, None, &dir, 2).unwrap();
        assert_eq!(second.encoded, 1);
        assert_eq!(second.entries, 3);
        assert_eq!(second.deferred, 0);
    }

    #[test]
    fn cache_shelf_cover_fills_the_share_table_immediately() {
        let pool = test_pool();
        let dir = temp_dir("cache-shelf");
        write_cover_png(&dir, "booth", "123");

        assert!(
            cache_shelf_cover(&pool, &dir, "booth", "123").unwrap(),
            "448px キャッシュがあれば共有キャッシュを作れる"
        );
        let entry = db::thumbs::get(&pool, db::thumbs::KIND_SHELF, "booth:123")
            .unwrap()
            .expect("行が入っていること");
        assert_eq!(entry.mime, "image/webp");
        assert!(entry.source_size.unwrap_or(0) > 0, "取得元のメタを残す");
        assert!(
            !cache_shelf_cover(&pool, &dir, "booth", "456").unwrap(),
            "取得元が無ければ何もしない"
        );
    }

    #[test]
    fn cache_checklist_thumbnail_keeps_the_jpeg() {
        let pool = test_pool();
        let jpeg = jpeg_bytes(256, 360);

        assert!(cache_checklist_thumbnail(&pool, "check-1", &B64.encode(&jpeg)).unwrap());
        let entry = db::thumbs::get(&pool, db::thumbs::KIND_CHECKLIST, "check-1")
            .unwrap()
            .expect("行が入っていること");
        assert_eq!(entry.mime, "image/jpeg");
        assert_eq!(entry.bytes, jpeg, "再エンコードしない");
        assert_eq!((entry.width, entry.height), (256, 360));
        assert!(
            !cache_checklist_thumbnail(&pool, "check-2", "not-base64!!").unwrap(),
            "画像でないデータは黙って無視する（保存はしない）"
        );
    }

    #[test]
    fn build_keeps_other_accounts_out() {
        let pool = test_pool();
        let dir = temp_dir("owner");
        block_on(async {
            sqlx::query(
                "INSERT INTO bookshelf_items (site_id, database_id, title, owner_sub) \
                 VALUES ('dlsite', 'RJ-a', 'A の本', ?1), ('dlsite', 'RJ-b', 'B の本', ?2)",
            )
            .bind(crate::owner::encrypt(&KEY, "A"))
            .bind(crate::owner::encrypt(&KEY, "B"))
            .execute(&pool)
            .await
            .unwrap();
        });
        write_cover_png(&dir, "dlsite", "RJ-a");
        write_cover_png(&dir, "dlsite", "RJ-b");

        let outcome = build_plaintext(&pool, &KEY, Some("A"), &dir, 100).unwrap();
        let entries = entries(&outcome.plaintext);
        assert_eq!(entries.len(), 1);
        assert_eq!(
            entries[0]["database_id"], "RJ-a",
            "他アカウントの表紙が混ざっている"
        );
    }
}
