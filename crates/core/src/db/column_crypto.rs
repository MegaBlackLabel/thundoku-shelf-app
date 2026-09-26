//! 機密列（ページ本文 / 形態素解析 / 付箋メモ）の暗号化。
//!
//! DB ファイル全体（SQLite）は暗号化しない方針（`docs/spec/10-pack-keys.md` §11.1 の
//! 線引き）だが、`.opfspack` を暗号化しても**抽出した本文や付箋の中身**が平文で残れば、
//! データディレクトリや WAL のコピーから読書内容が漏れる（セキュリティ評価 F02）。
//! そこで**機密列だけ**を、`books.owner_sub` と同じ keyring の DB 鍵
//! （[`crate::secrets::SecretStore::db_key`]）で AES-256-GCM にして保存する。
//!
//! - 保存形式は `owner.rs` と同じ `BASE64(IV(12) || ciphertext || tag)` に接頭辞
//!   [`PREFIX`] を付けたもの。**接頭辞の無い値は移行前の平文**として読む（読み取りで
//!   データを失わない）。既存行は起動時の [`crate::db::migrate_column_crypto_once`] が
//!   置き換え、**書き込みは常に暗号化する**。
//! - AAD は「用途 + テーブル名 + 行キー + 列名」。**列の入れ替え・行の入れ替え・別
//!   テーブルへのコピー**はタグ検証で落ちる。
//! - 復号できない値（鍵違い・改ざん・壊れた base64）は `None`。呼び出し側はそれを
//!   「空」として扱い、**暗号文を本文として画面やログに出さない**（fail-closed）。
//! - 鍵が取れないときは**平文で書かない**（`Err` にする）。

use aes_gcm::aead::{Aead, Payload};
use aes_gcm::{Aes256Gcm, KeyInit, Nonce};
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use rand::RngCore;

use crate::db::SqlitePool;
use crate::secrets::{SecretError, SecretStore};

/// 暗号化された値の接頭辞。**これが無い値は移行前の平文**として扱う。
pub const PREFIX: &str = "enc:v1:";

/// AAD の用途名（形式版込み）。用途や版が変われば別の暗号文として扱われる。
const PURPOSE: &str = "thundoku-shelf/column/v1";

/// 列の暗号化で起きたエラー。
#[derive(Debug, thiserror::Error)]
pub enum ColumnCryptoError {
    #[error("column encryption failed: {0}")]
    Encrypt(String),
    #[error("secret store error: {0}")]
    Secret(#[from] SecretError),
}

/// 値が暗号文か（移行の判定に使う。二重に暗号化しない）。
pub fn is_encrypted(value: &str) -> bool {
    value.starts_with(PREFIX)
}

/// 行の AAD。`table` + 行を一意にするキー + `column`。
pub fn aad(table: &str, row: &str, column: &str) -> String {
    format!("{PURPOSE}/{table}/{row}/{column}")
}

/// `document_text` の行（主キー `id`）の列 `column` の AAD。
pub fn aad_document_text(id: &str, column: &str) -> String {
    aad("document_text", id, column)
}

/// `token_analysis` の行（主キー `id`）の列 `column` の AAD。
pub fn aad_token_analysis(id: &str, column: &str) -> String {
    aad("token_analysis", id, column)
}

/// `page_notes` の行の列 `column` の AAD。
///
/// 主キー `id` ではなく **`book_id` + `content_id` + `page`（UNIQUE の自然キー）** を使う。
/// 同じページへの付箋は `ON CONFLICT(book_id, content_id, page)` で既存行を更新するため、
/// 渡された `id` と保存済み行の `id` が一致しないことがある（バックアップからの復元で
/// 別端末の id が来た場合も同様）。`id` を AAD にすると、その行のメモが復号できなくなる。
/// 本文と違い、この 3 つは書き込み側も読み出し側も常に持っている。
pub fn aad_page_notes(book_id: &str, content_id: &str, page: i64, column: &str) -> String {
    aad(
        "page_notes",
        &format!("{book_id}/{content_id}/{page}"),
        column,
    )
}

/// 平文を `enc:v1:BASE64(IV(12) || ciphertext || tag)` にする。
pub fn encrypt(key: &[u8; 32], aad: &str, plain: &str) -> Result<String, ColumnCryptoError> {
    let cipher = Aes256Gcm::new_from_slice(key)
        .map_err(|e| ColumnCryptoError::Encrypt(e.to_string()))?;
    let mut iv = [0u8; 12];
    rand::rngs::OsRng.fill_bytes(&mut iv);
    let ciphertext = cipher
        .encrypt(
            Nonce::from_slice(&iv),
            Payload {
                msg: plain.as_bytes(),
                aad: aad.as_bytes(),
            },
        )
        .map_err(|e| ColumnCryptoError::Encrypt(e.to_string()))?;
    let mut out = Vec::with_capacity(12 + ciphertext.len());
    out.extend_from_slice(&iv);
    out.extend_from_slice(&ciphertext);
    Ok(format!("{PREFIX}{}", B64.encode(&out)))
}

/// 保存値を平文に戻す。
///
/// - 接頭辞の無い値: 移行前の平文として**そのまま返す**（移行が失敗していても表示は壊さない）
/// - 接頭辞はあるが復号できない値（鍵違い・改ざん・壊れた base64・非 UTF-8）: `None`
pub fn decrypt(key: &[u8; 32], aad: &str, stored: &str) -> Option<String> {
    let Some(encoded) = stored.strip_prefix(PREFIX) else {
        // 接頭辞が無い = 移行前の平文（またはこの形式を知らない古い値）。
        // 読み取りでデータを失わないよう、そのまま返す（書き込みは常に暗号化し、
        // 既存行は起動時の移行が置き換える）。
        return Some(stored.to_string());
    };
    let data = B64.decode(encoded.trim()).ok()?;
    if data.len() < 12 {
        return None;
    }
    let (iv, ciphertext) = data.split_at(12);
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let plain = cipher
        .decrypt(
            Nonce::from_slice(iv),
            Payload {
                msg: ciphertext,
                aad: aad.as_bytes(),
            },
        )
        .ok()?;
    String::from_utf8(plain).ok()
}

/// keyring の DB 鍵（`books.owner_sub` と同じスロット）。無ければ新規生成される。
///
/// **keyring が使えない環境では `Err`**。呼び出し側は平文で書かずにエラーにする
/// （fail-closed: 平文のまま保存するくらいなら書き込みを失敗させる）。
pub fn db_key() -> Result<[u8; 32], sqlx::Error> {
    SecretStore::new()
        .db_key()
        .map_err(|e| sqlx::Error::Protocol(format!("column crypto: DB 鍵を取得できない: {e}")))
}

/// リポジトリ向け: 列を暗号化する（`sqlx::Error` に包む）。
pub fn encrypt_str(key: &[u8; 32], aad: &str, plain: &str) -> Result<String, sqlx::Error> {
    encrypt(key, aad, plain).map_err(|e| sqlx::Error::Protocol(format!("column crypto: {e}")))
}

/// リポジトリ向け: `Option` 列を暗号化する（`None` = NULL はそのまま）。
pub fn encrypt_opt_str(
    key: &[u8; 32],
    aad: &str,
    plain: Option<&str>,
) -> Result<Option<String>, sqlx::Error> {
    match plain {
        Some(plain) => Ok(Some(encrypt_str(key, aad, plain)?)),
        None => Ok(None),
    }
}

/// リポジトリ向け: 列を復号する。復号できない値は `Ok(None)`
/// （呼び出し側は空として扱い、**暗号文を返さない**）。
pub fn decrypt_str(key: &[u8; 32], aad: &str, stored: &str) -> Result<Option<String>, sqlx::Error> {
    Ok(decrypt(key, aad, stored))
}

/// 平文（接頭辞の無い値）で残っている機密列が 1 つでもあるか。
///
/// 移行の入口で使う。**無ければ鍵（keyring）に触れずに済む**ので、新規 DB や移行済みの
/// DB で余計な keyring アクセス（OS の許可ダイアログ）を起こさない。接頭辞は `substr` で見る
/// （SQLite の `LIKE` は ASCII の大文字小文字を無視するため）。
/// `token_analysis` は 4 列を 1 文で書き、`token` が NOT NULL なので `token` を目印にする。
pub(crate) async fn has_plaintext_rows(pool: &SqlitePool) -> Result<bool, sqlx::Error> {
    for (table, column) in [
        ("document_text", "text_content"),
        ("token_analysis", "token"),
        ("page_notes", "memo"),
    ] {
        let found: Option<i64> = sqlx::query_scalar(&format!(
            "SELECT 1 FROM {table} WHERE substr({column}, 1, ?1) <> ?2 LIMIT 1"
        ))
        .bind(PREFIX.len() as i64)
        .bind(PREFIX)
        .fetch_optional(pool)
        .await?;
        if found.is_some() {
            return Ok(true);
        }
    }
    Ok(false)
}

/// 移行対象の行（`token_analysis`）。
#[derive(sqlx::FromRow)]
struct TokenPlainRow {
    id: String,
    token: String,
    pos: String,
    base_form: Option<String>,
    reading: Option<String>,
}

/// 移行対象の行（`page_notes`）。
#[derive(sqlx::FromRow)]
struct NotePlainRow {
    id: String,
    book_id: String,
    content_id: String,
    page: i64,
    memo: String,
}

/// 平文で残っている機密列を暗号化して置き換える（一度きりの移行から呼ぶ）。
///
/// 接頭辞の付いた値は触らない（何度呼んでも安全）。1 列でも失敗したらその場で返し、
/// 呼び出し側はフラグを立てないので**次回起動で再試行**される。戻り値は書き換えた行数。
pub(crate) async fn encrypt_plaintext_rows(
    conn: &mut sqlx::SqliteConnection,
    key: &[u8; 32],
) -> Result<u64, sqlx::Error> {
    let mut updated = 0u64;

    // ページ本文（1 行 = 1 ページ）
    let rows: Vec<(String, String)> = sqlx::query_as("SELECT id, text_content FROM document_text")
        .fetch_all(&mut *conn)
        .await?;
    for (id, text) in rows {
        if is_encrypted(&text) {
            continue;
        }
        let aad = aad_document_text(&id, "text_content");
        sqlx::query("UPDATE document_text SET text_content = ?1 WHERE id = ?2")
            .bind(encrypt_str(key, &aad, &text)?)
            .bind(&id)
            .execute(&mut *conn)
            .await?;
        updated += 1;
    }

    // 形態素解析（1 行 = 1 ページ 1 名詞）。`token` / `pos` は NOT NULL なので
    // 接頭辞の有無だけで判定できる。
    let rows: Vec<TokenPlainRow> =
        sqlx::query_as("SELECT id, token, pos, base_form, reading FROM token_analysis")
            .fetch_all(&mut *conn)
            .await?;
    for row in rows {
        if is_encrypted(&row.token) {
            continue;
        }
        let aad = |column: &str| aad_token_analysis(&row.id, column);
        sqlx::query(
            "UPDATE token_analysis SET token = ?1, pos = ?2, base_form = ?3, reading = ?4 \
             WHERE id = ?5",
        )
        .bind(encrypt_str(key, &aad("token"), &row.token)?)
        .bind(encrypt_str(key, &aad("pos"), &row.pos)?)
        .bind(encrypt_opt_str(key, &aad("base_form"), row.base_form.as_deref())?)
        .bind(encrypt_opt_str(key, &aad("reading"), row.reading.as_deref())?)
        .bind(&row.id)
        .execute(&mut *conn)
        .await?;
        updated += 1;
    }

    // 付箋のメモ（1 行 = 1 ページ 1 件）
    let rows: Vec<NotePlainRow> =
        sqlx::query_as("SELECT id, book_id, content_id, page, memo FROM page_notes")
            .fetch_all(&mut *conn)
            .await?;
    for row in rows {
        if is_encrypted(&row.memo) {
            continue;
        }
        let aad = aad_page_notes(&row.book_id, &row.content_id, row.page, "memo");
        sqlx::query("UPDATE page_notes SET memo = ?1 WHERE id = ?2")
            .bind(encrypt_str(key, &aad, &row.memo)?)
            .bind(&row.id)
            .execute(&mut *conn)
            .await?;
        updated += 1;
    }

    Ok(updated)
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: [u8; 32] = [7u8; 32];

    fn aad_document(id: &str) -> String {
        aad_document_text(id, "text_content")
    }

    #[test]
    fn round_trip() {
        let blob = encrypt(&KEY, &aad_document("dt-1"), "ページの本文").unwrap();
        assert!(blob.starts_with(PREFIX));
        assert!(!blob.contains("ページの本文"), "平文が残っている");
        assert_eq!(
            decrypt(&KEY, &aad_document("dt-1"), &blob).as_deref(),
            Some("ページの本文")
        );
    }

    #[test]
    fn empty_string_round_trips() {
        let blob = encrypt(&KEY, &aad_document("dt-1"), "").unwrap();
        assert_ne!(blob, PREFIX, "空文字でも暗号文になる");
        assert_eq!(decrypt(&KEY, &aad_document("dt-1"), &blob).as_deref(), Some(""));
    }

    #[test]
    fn wrong_key_is_none() {
        let blob = encrypt(&KEY, &aad_document("dt-1"), "秘密").unwrap();
        assert!(decrypt(&[9u8; 32], &aad_document("dt-1"), &blob).is_none());
    }

    /// 行・列・テーブルを入れ替えた暗号文は復号できない（AAD に含めているため）。
    #[test]
    fn moved_value_is_none() {
        let blob = encrypt(&KEY, &aad_document("dt-1"), "秘密").unwrap();
        // 別の行へコピー
        assert!(decrypt(&KEY, &aad_document("dt-2"), &blob).is_none());
        // 別のテーブルへコピー（行キーと列名は同じ）
        assert!(
            decrypt(&KEY, &aad_token_analysis("dt-1", "text_content"), &blob).is_none(),
            "テーブル名が違えば復号できない"
        );
        // 別の列へコピー（同じテーブル・同じ行）
        assert!(
            decrypt(&KEY, &aad_document_text("dt-1", "page_number"), &blob).is_none(),
            "列名が違えば復号できない"
        );
        // 付箋は自然キー（book_id / content_id / page）で決まる
        let note = encrypt(&KEY, &aad_page_notes("b1", "", 3, "memo"), "メモ").unwrap();
        assert_eq!(
            decrypt(&KEY, &aad_page_notes("b1", "", 3, "memo"), &note).as_deref(),
            Some("メモ")
        );
        assert!(decrypt(&KEY, &aad_page_notes("b1", "", 4, "memo"), &note).is_none());
    }

    #[test]
    fn tampered_ciphertext_is_none() {
        let blob = encrypt(&KEY, &aad_document("dt-1"), "秘密").unwrap();
        // 接頭辞の後ろ（IV の後ろ）を 1 文字だけ別の base64 文字に差し替える
        let mut chars: Vec<char> = blob.chars().collect();
        let index = PREFIX.len() + 20;
        chars[index] = if chars[index] == 'A' { 'B' } else { 'A' };
        let tampered: String = chars.into_iter().collect();
        assert!(decrypt(&KEY, &aad_document("dt-1"), &tampered).is_none());
    }

    #[test]
    fn broken_base64_is_none() {
        for broken in [
            PREFIX.to_string(),
            format!("{PREFIX}!!!!"),
            format!("{PREFIX}AAA"),
            // IV（12 バイト）に満たない
            format!("{PREFIX}{}", B64.encode([1u8, 2, 3])),
        ] {
            assert!(
                decrypt(&KEY, &aad_document("dt-1"), &broken).is_none(),
                "壊れた値が復号された: {broken}"
            );
        }
    }

    /// 移行前の平文は読める（読み取りでデータを失わない）。書き込みは常に暗号化する。
    #[test]
    fn plaintext_is_read_as_is() {
        assert!(!is_encrypted("ふつうのメモ"));
        assert!(is_encrypted(&format!("{PREFIX}xxx")));
        assert_eq!(
            decrypt(&KEY, &aad_document("dt-1"), "ふつうのメモ").as_deref(),
            Some("ふつうのメモ")
        );
    }
}
