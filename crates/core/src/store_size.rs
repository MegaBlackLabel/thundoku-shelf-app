//! ストアが返すファイルサイズ表記（`"57.15MB"` など）をバイト数へ直す。
//!
//! 用途は「ダウンロード前に 2 GiB 超かどうかを判定して確認を出す」なので、
//! 数 % の誤差は許容する（単位は 1024 進で解釈する = Windows の表示に合わせる）。
//! 判定に使えない値（空・不正・0）は `None` を返し、呼び出し側は「サイズ不明」として
//! 扱う（不明なら確認を出さない = 誤って止めない）。

/// ダウンロード前に確認を出す閾値（2 GiB）。
///
/// pack の上限は 10 GiB（`opfspack::MAX_TOTAL_SIZE` / `MAX_DOWNLOAD_BODY_BYTES`）なので
/// 「大きすぎて扱えない」線ではない。2 GiB を超える本は**転送に時間とディスクを使い、
/// 取り込みにもメモリを使う**ことを先に伝えるための確認として出す。
pub const LARGE_DOWNLOAD_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// バイト数を「約 2.4 GB」の形にする（確認ダイアログの文言用）。
///
/// 1 GB 未満は MB で出す（小さい値を「約 0.0 GB」にしない）。
pub fn format_size_gb(size: u64) -> String {
    const GB: f64 = 1024.0 * 1024.0 * 1024.0;
    const MB: f64 = 1024.0 * 1024.0;
    let size = size as f64;
    if size >= GB {
        format!("約 {:.1} GB", size / GB)
    } else {
        format!("約 {:.0} MB", size / MB)
    }
}

/// ダウンロード前に確認を出すか。**サイズ不明（`None`）なら出さない**
/// （読めないことを理由に正当な取り込みを止めない）。
pub fn needs_large_download_confirmation(size: Option<u64>) -> bool {
    size.is_some_and(|size| size > LARGE_DOWNLOAD_BYTES)
}

/// `"57.15MB"` / `"1.2 GB"` / `"700KB"` / `"1234567890"` などをバイト数へ直す。
pub fn parse_store_size(text: &str) -> Option<u64> {
    // 空白（全角含む）・カンマ・アンダースコアを落として大文字化する
    // （`"12,345,678"` や `"1.2 GB"` のような表記ゆれを吸収する）。
    let cleaned: String = text
        .chars()
        .filter(|c| !c.is_whitespace() && *c != ',' && *c != '_')
        .collect::<String>()
        .to_uppercase();
    if cleaned.is_empty() {
        return None;
    }

    // 長い単位から見る（`B` が `KB` の末尾に一致するのを避ける）。
    const UNITS: [(&str, u64); 5] = [
        ("TB", 1024 * 1024 * 1024 * 1024),
        ("GB", 1024 * 1024 * 1024),
        ("MB", 1024 * 1024),
        ("KB", 1024),
        ("B", 1),
    ];
    let (number, multiplier) = match UNITS
        .iter()
        .find_map(|(suffix, multiplier)| cleaned.strip_suffix(suffix).map(|n| (n, *multiplier)))
    {
        Some((number, multiplier)) => (number, Some(multiplier)),
        // 単位が無い表記。桁が小さいと KB かバイトか判別できないので**判定に使わない**
        // （1 MiB 未満は確認を出す対象にならないので、不明として扱って困らない）。
        None => (cleaned.as_str(), None),
    };
    if number.is_empty() {
        return None;
    }
    let value: f64 = number.parse().ok()?;
    // `NaN` / `inf` は `f64::from_str` が受け付けるので明示的に弾く。
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    match multiplier {
        Some(multiplier) => {
            let bytes = value * multiplier as f64;
            if !bytes.is_finite() || bytes < 1.0 || bytes >= u64::MAX as f64 {
                return None;
            }
            Some(bytes as u64)
        }
        None if value >= (1024.0 * 1024.0) => {
            if value >= u64::MAX as f64 {
                return None;
            }
            Some(value as u64)
        }
        None => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_store_notations() {
        // FANZA の詳細は `"57.15MB"` の形で返る（実レスポンスの実測値）
        assert_eq!(parse_store_size("57.15MB"), Some((57.15 * 1024.0 * 1024.0) as u64));
        assert_eq!(parse_store_size("1.5GB"), Some((1.5 * 1024.0 * 1024.0 * 1024.0) as u64));
        assert_eq!(parse_store_size("700 KB"), Some(700 * 1024));
        assert_eq!(parse_store_size("2TB"), Some(2 * 1024 * 1024 * 1024 * 1024));
        assert_eq!(parse_store_size("512B"), Some(512));
        // 小文字・全角スペースも受ける
        assert_eq!(parse_store_size("1.2 gb"), Some((1.2 * 1024.0 * 1024.0 * 1024.0) as u64));
        assert_eq!(parse_store_size("1.2\u{3000}gb"), Some((1.2 * 1024.0 * 1024.0 * 1024.0) as u64));
    }

    #[test]
    fn parses_bytes_without_a_unit() {
        // 単位が無い大きな数はバイトとして扱う（桁が大きいときだけ）
        assert_eq!(parse_store_size("1234567890"), Some(1_234_567_890));
        assert_eq!(parse_store_size("12,345,678"), Some(12_345_678));
        // 小さい数は単位不明（KB かバイトか判別できない）→ 判定に使わない
        assert_eq!(parse_store_size("500"), None);
    }

    #[test]
    fn formats_sizes_for_the_confirmation_dialog() {
        assert_eq!(format_size_gb(3 * 1024 * 1024 * 1024), "約 3.0 GB");
        assert_eq!(format_size_gb(2 * 1024 * 1024 * 1024 + 512 * 1024 * 1024), "約 2.5 GB");
        // 1 GB 未満は MB（「約 0.5 GB」より読みやすい）
        assert_eq!(format_size_gb(512 * 1024 * 1024), "約 512 MB");
        assert_eq!(format_size_gb(0), "約 0 MB");
    }

    #[test]
    fn large_download_confirmation_uses_two_gib_and_ignores_unknown_sizes() {
        // ちょうど 2 GiB は確認しない（超えたときだけ）
        assert!(!needs_large_download_confirmation(Some(LARGE_DOWNLOAD_BYTES)));
        assert!(needs_large_download_confirmation(Some(LARGE_DOWNLOAD_BYTES + 1)));
        // サイズ不明なら確認しない（誤って止めない）
        assert!(!needs_large_download_confirmation(None));
        // 実データ例: 107.84MB は確認対象外
        assert!(!needs_large_download_confirmation(parse_store_size("107.84MB")));
    }

    #[test]
    fn rejects_values_that_cannot_be_trusted() {
        for text in ["", "   ", "abc", "MB", "0", "0MB", "-1GB", "1.2PB", "nanGB"] {
            assert_eq!(parse_store_size(text), None, "{text:?} を数値として扱っている");
        }
    }
}
