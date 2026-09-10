//! ZIP エントリ名のデコード。
//!
//! `zip` crate の `ZipFile::name()` は、UTF-8 フラグ（general purpose bit 11）が
//! 立っていない名前を **CP437** として復号する。FANZA / BOOTH / DLsite の ZIP は
//! Shift-JIS（CP932）の名前を持つものが大半なので、そのまま使うと文字化けする
//! （例: `ギザ歯でかわいい葉子さん.pdf` → `âMâUÄòé┼é⌐éφéóéóùtÄqé│é±.pdf`）。
//!
//! `ZipFile::name_raw()` で生バイトを取り出し、ここでデコードする。

use encoding_rs::SHIFT_JIS;

/// ZIP エントリの生バイト列を名前へ復号する。
///
/// UTF-8 として妥当ならそのまま返す（ASCII も UTF-8 として通るため、
/// UTF-8 フラグの有無を別途見る必要は無い）。妥当でなければ CP932 として復号する。
pub fn decode_entry_name(raw: &[u8]) -> String {
    decode_bytes(raw)
}

/// `_export.txt` などの本文バイト列を復号する（判定規則はエントリ名と同じ）。
pub fn decode_text_bytes(raw: &[u8]) -> String {
    decode_bytes(raw)
}

/// UTF-8 を優先し、不正なら CP932（Shift-JIS）として復号する。
fn decode_bytes(raw: &[u8]) -> String {
    match std::str::from_utf8(raw) {
        Ok(text) => text.to_string(),
        Err(_) => SHIFT_JIS.decode(raw).0.into_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::decode_entry_name;

    #[test]
    fn decodes_cp932_names() {
        // 日本語（CP932）
        assert_eq!(
            decode_entry_name(&[0x93, 0xfa, 0x96, 0x7b, 0x8c, 0xea]),
            "日本語"
        );
        // 表紙.epub（CP932 + ASCII の拡張子）
        assert_eq!(
            decode_entry_name(&[0x95, 0x5c, 0x8e, 0x86, 0x2e, 0x65, 0x70, 0x75, 0x62]),
            "表紙.epub"
        );
    }

    #[test]
    fn keeps_utf8_and_ascii_names() {
        assert_eq!(decode_entry_name(b"pages/001.jpg"), "pages/001.jpg");
        assert_eq!(decode_entry_name("日本語.pdf".as_bytes()), "日本語.pdf");
    }
}
