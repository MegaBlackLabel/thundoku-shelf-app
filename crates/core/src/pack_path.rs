//! pack ファイル（`packs/{id}.opfspack`）のパス解決と id の検証。
//!
//! `books.id` / `books.pack_id` は Drive の JSON バックアップから復元でき、復元時に
//! 値の検証が無かった。`packs_dir.join(format!("{id}.opfspack"))` へそのまま渡すと、
//! `../../other/book` のような id で保存領域の外のファイルを読み書き・削除できる
//! （CWE-22）。pack を触る経路（取り込み・閲覧・改名・削除・Drive 同期）は
//! **すべてこのモジュールを通す**。
//!
//! 判定は Drive のファイル名検証（旧 `drive::sync::pack_id_from_name`）と同じ
//! deny-list を基礎にし、最後に「解決後の親が `packs_dir` と一致すること」を
//! 確認する（区切り・絶対パス・Windows プレフィックスの取りこぼし対策）。

use std::path::{Path, PathBuf};

/// pack ファイルの拡張子。
pub const PACK_EXTENSION: &str = "opfspack";

#[derive(Debug, thiserror::Error)]
pub enum IdError {
    #[error("unsafe pack id: {0:?}")]
    Unsafe(String),
    #[error("pack path escapes the packs directory: {0:?}")]
    Escapes(String),
}

/// id を 1 つのファイル名として安全に使えるか。
///
/// 拒否するもの: 空 / 長すぎる / パス区切り（`/` `\`）/ 親参照（`..`）/ `:` /
/// 制御文字 / 先頭 `.`（隠しファイル・`.` `..` 自体）/ パスとして 1 成分でない
/// （絶対パス・Windows プレフィックスを含む）。
pub fn is_safe_id(id: &str) -> bool {
    if id.is_empty() || id.len() > 200 {
        return false;
    }
    if id.starts_with('.')
        || id.contains('/')
        || id.contains('\\')
        || id.contains("..")
        || id.contains(':')
        || id.chars().any(char::is_control)
    {
        return false;
    }
    // `Path` として見たときに 1 成分であること（ドライブ相対 `C:x` 等を弾く。
    // `/` `\` は上で弾いているので、ここは主にプレフィックス対策）。
    Path::new(id).components().count() == 1
}

/// `packs_dir/{id}.opfspack`。id が安全でなければエラー。
pub fn pack_path(packs_dir: &Path, id: &str) -> Result<PathBuf, IdError> {
    if !is_safe_id(id) {
        return Err(IdError::Unsafe(id.to_string()));
    }
    let path = packs_dir.join(format!("{id}.{PACK_EXTENSION}"));
    // `join` は絶対パスを渡されると置き換わる。上の検査と二重に、解決後の親が
    // 保存領域そのものであることを確認する。
    if path.parent() != Some(packs_dir) {
        return Err(IdError::Escapes(id.to_string()));
    }
    Ok(path)
}

/// `packs_dir/{id}.conflict-local.opfspack`（Drive 同期の競合退避先）。
pub fn conflict_backup_path(packs_dir: &Path, id: &str) -> Result<PathBuf, IdError> {
    if !is_safe_id(id) {
        return Err(IdError::Unsafe(id.to_string()));
    }
    let path = packs_dir.join(format!("{id}.conflict-local.{PACK_EXTENSION}"));
    if path.parent() != Some(packs_dir) {
        return Err(IdError::Escapes(id.to_string()));
    }
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_uuid_like_ids() {
        assert!(is_safe_id("0f8fad5b-d9cb-469f-a165-70867728950e"));
        assert!(is_safe_id("book-1"));
        assert!(is_safe_id("a"));
    }

    #[test]
    fn rejects_traversal_and_absolute_ids() {
        for id in [
            "",
            ".",
            "..",
            "../other/book",
            "..\\other\\book",
            "a/b",
            "a\\b",
            "C:\\evil",
            "C:evil",
            "/etc/passwd",
            ".hidden",
            "a:b",
            "a\u{0}b",
            "a\nb",
        ] {
            assert!(!is_safe_id(id), "危険な id を許可している: {id:?}");
        }
    }

    #[test]
    fn pack_path_stays_inside_packs_dir() {
        let dir = std::path::Path::new("/tmp/packs");
        let path = pack_path(dir, "book-1").unwrap();
        assert_eq!(path, dir.join("book-1.opfspack"));
        assert!(pack_path(dir, "../escape").is_err());
        assert!(pack_path(dir, "/etc/passwd").is_err());
    }

    #[test]
    fn conflict_backup_path_uses_the_same_rules() {
        let dir = std::path::Path::new("/tmp/packs");
        assert_eq!(
            conflict_backup_path(dir, "book-1").unwrap(),
            dir.join("book-1.conflict-local.opfspack")
        );
        assert!(conflict_backup_path(dir, "../escape").is_err());
    }
}
