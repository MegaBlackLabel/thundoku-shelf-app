//! ZIP エントリの種別判定。
//!
//! 取り込み対象（画像・PDF・音声・動画）と、捨てるもの（junk）、
//! 別扱いするもの（表紙画像・`_export.txt`）を名前だけで分類する。
//! 実データ（FANZA 290 件）の語彙を根拠にしている（`docs/import-patterns.md` §5）。

/// エントリ 1 件の種別。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EntryKind {
    /// ページ画像。
    Image,
    /// PDF（描画してページにする）。
    Pdf,
    /// EPUB（ビューア非対応。1 エントリのまま保持する）。
    Epub,
    /// 音声（コンテンツになりうる）。
    Audio,
    /// 動画（コンテンツになりうる）。
    Video,
    /// `_export.txt`（ページ別セリフ本文。ページにはせず `document_text` へ入れる）。
    ExportText,
    /// 表紙・裏表紙の画像（ページ列からは除外し、カバーの材料にする）。
    Cover,
    /// 読めない・読む必要のないもの（`Thumbs.db` 等）。
    Junk,
}

/// 画像として扱う拡張子（小文字）。
///
/// `bmp` / `tif` / `tiff` は実データの拡張子事情に合わせて追加している
/// （`.bmp` のみの作品が実在: `docs/import-patterns.md` §2.3 G1）。
pub const IMAGE_EXTENSIONS: &[&str] = &["png", "jpg", "jpeg", "webp", "gif", "bmp", "tif", "tiff"];

/// 音声として扱う拡張子（小文字）。
const AUDIO_EXTENSIONS: &[&str] = &["mp3", "wav", "m4a", "ogg", "flac"];

/// 動画として扱う拡張子（小文字）。
const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "mkv", "webm", "avi"];

/// ファイル名そのものが junk なもの（小文字で比較）。
const JUNK_NAMES: &[&str] = &[
    "thumbs.db",
    ".ds_store",
    "desktop.ini",
    "readme.txt",
    "ご挨拶.txt",
    "メモ.txt",
    "あとがき.txt",
];

/// 表紙を表す日本語の語（`裏表紙` `背表紙` も部分一致で拾える）。
const COVER_SUBSTRINGS: &[&str] = &["表紙"];

/// 表紙を表す英字の語。単語として一致したときだけ表紙とみなす
/// （`discover` を `cover` と誤判定しないため）。
const COVER_TOKENS: &[&str] = &["cover", "omote", "ura", "back"];

/// ZIP エントリ名（パス）から種別を判定する。
pub fn classify_entry(path: &str) -> EntryKind {
    let components: Vec<&str> = path.split(['/', '\\']).filter(|c| !c.is_empty()).collect();
    let base = components.last().copied().unwrap_or(path);
    let lower_base = base.to_lowercase();

    if components.contains(&"__MACOSX") {
        return EntryKind::Junk;
    }
    if JUNK_NAMES.contains(&lower_base.as_str()) {
        return EntryKind::Junk;
    }

    let extension = base.rsplit_once('.').map(|(_, ext)| ext.to_lowercase());
    if extension.as_deref() == Some("txt") && lower_base.ends_with("_export.txt") {
        return EntryKind::ExportText;
    }
    match extension.as_deref() {
        Some(ext) if IMAGE_EXTENSIONS.contains(&ext) => {
            if is_cover_path(&components) {
                EntryKind::Cover
            } else {
                EntryKind::Image
            }
        }
        Some("pdf") => EntryKind::Pdf,
        Some("epub") => EntryKind::Epub,
        Some(ext) if AUDIO_EXTENSIONS.contains(&ext) => EntryKind::Audio,
        Some(ext) if VIDEO_EXTENSIONS.contains(&ext) => EntryKind::Video,
        // 上記以外（本文ではない txt、未知の拡張子、拡張子なし）は読まない
        _ => EntryKind::Junk,
    }
}

/// パスのいずれかの成分が表紙を表すか。
fn is_cover_path(components: &[&str]) -> bool {
    for (index, component) in components.iter().enumerate() {
        let is_file_name = index + 1 == components.len();
        let stem = if is_file_name {
            component
                .rsplit_once('.')
                .map(|(stem, _)| stem)
                .unwrap_or(component)
        } else {
            component
        };
        if COVER_SUBSTRINGS.iter().any(|key| stem.contains(key)) {
            return true;
        }
        let lower = stem.to_lowercase();
        if lower
            .split(|c: char| !c.is_alphanumeric())
            .any(|token| COVER_TOKENS.contains(&token))
        {
            return true;
        }
    }
    false
}

/// ページ（またはレンディション）として読める種別か。
///
/// 表紙（`Cover`）はカバーの材料であって本文ではない（決定 D4）ため含めない。
pub fn is_readable_kind(kind: EntryKind) -> bool {
    matches!(
        kind,
        EntryKind::Image | EntryKind::Pdf | EntryKind::Epub | EntryKind::Audio | EntryKind::Video
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_junk() {
        assert_eq!(classify_entry("Thumbs.db"), EntryKind::Junk);
        assert_eq!(classify_entry("readme.txt"), EntryKind::Junk);
        assert_eq!(classify_entry("ご挨拶.txt"), EntryKind::Junk);
        assert_eq!(classify_entry("メモ.txt"), EntryKind::Junk);
        assert_eq!(classify_entry("あとがき.txt"), EntryKind::Junk);
        assert_eq!(classify_entry("__MACOSX/._cover.jpg"), EntryKind::Junk);
        assert_eq!(classify_entry(".DS_Store"), EntryKind::Junk);
        assert_eq!(classify_entry("desktop.ini"), EntryKind::Junk);
        assert_eq!(classify_entry("dir/.DS_Store"), EntryKind::Junk);
        // ゲーム付属のタイルセット凡例など、本文ではない txt は junk
        assert_eq!(classify_entry("Outside_A1.txt"), EntryKind::Junk);
    }

    #[test]
    fn classifies_export_text() {
        assert_eq!(
            classify_entry("壊れた姉弟と壊れる僕_export.txt"),
            EntryKind::ExportText
        );
        assert_eq!(
            classify_entry("dir/btid01_EXPORT.TXT"),
            EntryKind::ExportText
        );
    }

    #[test]
    fn classifies_images_and_covers() {
        assert_eq!(classify_entry("pages/001.jpg"), EntryKind::Image);
        assert_eq!(classify_entry("mhszplum/01.BMP"), EntryKind::Image);
        assert_eq!(classify_entry("scan/001.tiff"), EntryKind::Image);
        // 表紙は画像だが本文ではない
        assert_eq!(classify_entry("表紙.png"), EntryKind::Cover);
        assert_eq!(classify_entry("5.表紙/001.jpg"), EntryKind::Cover);
        assert_eq!(classify_entry("裏表紙/01.jpg"), EntryKind::Cover);
        assert_eq!(classify_entry("omote/001.jpg"), EntryKind::Cover);
        assert_eq!(classify_entry("ura/01.jpg"), EntryKind::Cover);
        assert_eq!(classify_entry("cover.jpg"), EntryKind::Cover);
        // "discover" のような無関係な語を cover と誤判定しない
        assert_eq!(classify_entry("discover/001.jpg"), EntryKind::Image);
    }

    #[test]
    fn classifies_media() {
        assert_eq!(classify_entry("book.pdf"), EntryKind::Pdf);
        assert_eq!(classify_entry("book.epub"), EntryKind::Epub);
        assert_eq!(classify_entry("音声/01.mp3"), EntryKind::Audio);
        assert_eq!(classify_entry("voice/SEなし.wav"), EntryKind::Audio);
        assert_eq!(classify_entry("movie/op.mp4"), EntryKind::Video);
    }

    #[test]
    fn readable_kinds_exclude_covers_and_junk() {
        assert!(is_readable_kind(EntryKind::Image));
        assert!(is_readable_kind(EntryKind::Pdf));
        assert!(is_readable_kind(EntryKind::Epub));
        assert!(is_readable_kind(EntryKind::Audio));
        assert!(is_readable_kind(EntryKind::Video));
        assert!(!is_readable_kind(EntryKind::Cover));
        assert!(!is_readable_kind(EntryKind::ExportText));
        assert!(!is_readable_kind(EntryKind::Junk));
    }
}
