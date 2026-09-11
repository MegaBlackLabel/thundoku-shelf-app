//! `_export.txt`（画像のみ作品に同梱されるページ別セリフ本文）のパース。
//!
//! 実データ（`docs/import-patterns.md` §4）では `<<1Page>>` のようなマーカーで
//! 区切られ、ページ番号と画像が 1 対 1 で対応する（セリフの無いページは欠番になる）。

/// `_export.txt` を `(page_number, body)` の列へパースする。
///
/// - BOM（`\u{feff}`）と CRLF を除去する
/// - マーカー番号をそのままページ番号として使う（欠番・非連番を許容する）
/// - マーカーが 1 つも無ければ空を返す
pub fn parse_export_text(text: &str) -> Vec<(i64, String)> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut out: Vec<(i64, String)> = Vec::new();
    let mut current: Option<(i64, Vec<&str>)> = None;
    for raw_line in text.split('\n') {
        let line = raw_line.strip_suffix('\r').unwrap_or(raw_line);
        if let Some(page_number) = marker_number(line) {
            if let Some((page, body)) = current.take() {
                out.push((page, join_body(&body)));
            }
            current = Some((page_number, Vec::new()));
        } else if let Some((_, body)) = current.as_mut() {
            body.push(line);
        }
        // 最初のマーカーより前の行（ヘッダ等）は捨てる
    }
    if let Some((page, body)) = current {
        out.push((page, join_body(&body)));
    }
    out
}

/// 本文行を連結し、前後の空白・改行を落とす。
fn join_body(lines: &[&str]) -> String {
    lines.join("\n").trim().to_string()
}

/// `<<1Page>>` からページ番号を取り出す。マーカーでなければ `None`。
fn marker_number(line: &str) -> Option<i64> {
    let inner = line.trim().strip_prefix("<<")?.strip_suffix(">>")?;
    let digits = inner
        .strip_suffix("Page")
        .or_else(|| inner.strip_suffix("page"))
        .or_else(|| inner.strip_suffix("PAGE"))?;
    digits.trim().parse().ok()
}

#[cfg(test)]
mod tests {
    use super::parse_export_text;

    #[test]
    fn parses_markers_in_order() {
        let text = "<<1Page>>\n壊れた姉弟と壊れる僕\nこの物語はフィクションです\n<<2Page>>\n僕には好きな女性がいた\n<<3Page>>\n三人目\n";
        assert_eq!(
            parse_export_text(text),
            vec![
                (
                    1,
                    "壊れた姉弟と壊れる僕\nこの物語はフィクションです".to_string()
                ),
                (2, "僕には好きな女性がいた".to_string()),
                (3, "三人目".to_string()),
            ]
        );
    }

    #[test]
    fn keeps_marker_numbers_when_not_sequential() {
        // 実データには 2..18 のような欠番（セリフ無しページ）がある
        let text = "<<2Page>>\nイ\n<<5Page>>\nロ\n";
        assert_eq!(
            parse_export_text(text),
            vec![(2, "イ".to_string()), (5, "ロ".to_string())]
        );
    }

    #[test]
    fn strips_bom_and_crlf() {
        let text = "\u{feff}<<1Page>>\r\nあ\r\nい\r\n";
        assert_eq!(parse_export_text(text), vec![(1, "あ\nい".to_string())]);
    }

    #[test]
    fn ignores_text_before_first_marker() {
        let text = "ヘッダ\n<<1Page>>\n本文\n";
        assert_eq!(parse_export_text(text), vec![(1, "本文".to_string())]);
    }

    #[test]
    fn returns_empty_without_markers() {
        assert!(parse_export_text("本文だけ").is_empty());
    }
}
