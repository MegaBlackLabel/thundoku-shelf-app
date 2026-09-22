//! ストアへ送る `User-Agent` の解決。
//!
//! 各ストアのクライアントは `ThundokuShelf/<version> (+リポジトリ URL)` を名乗る
//! （以前はブラウザを名乗っていた。DLsite の購入履歴が自認 UA で 200 になることは
//! 実機で確認済み）。
//!
//! **万一サイト側が非ブラウザ UA を弾くようになったとき**のために、そのストアだけ
//! `User-Agent` を差し替える逃げ道を用意している（コードを直して再ビルドせずに戻せる）:
//!
//! ```text
//! THUNDOKU_DLSITE_UA="Mozilla/5.0 … Chrome/126.0.0.0 Safari/537.36"
//! ```
//!
//! 既定は各クライアントの定数（＝自認 UA）で、環境変数を付けて起動したときだけ差し替わる。

/// DLsite のクライアントが使う UA を差し替える環境変数。
pub const ENV_DLSITE: &str = "THUNDOKU_DLSITE_UA";
/// FANZA のクライアントが使う UA を差し替える環境変数。
pub const ENV_FANZA: &str = "THUNDOKU_FANZA_UA";
/// BOOTH のクライアントが使う UA を差し替える環境変数。
pub const ENV_BOOTH: &str = "THUNDOKU_BOOTH_UA";
/// 表紙など CDN 画像の取得に使う UA を差し替える環境変数。
pub const ENV_COVER: &str = "THUNDOKU_COVER_UA";

/// 差し替え値から実際に送る UA を決める。
///
/// 環境変数はプロセス全体で共有され並列テストが互いに干渉するため、値は引数で受ける
/// （読み取りは [`for_store`] が行う）。
pub fn resolve(default: &str, override_value: Option<&str>) -> String {
    match override_value {
        Some(value) if !value.trim().is_empty() => value.trim().to_string(),
        _ => default.to_string(),
    }
}

/// 環境変数を見て、実際に送る UA を決める。
pub fn for_store(default: &str, env_key: &str) -> String {
    let override_value = std::env::var(env_key).ok();
    resolve(default, override_value.as_deref())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DEFAULT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) Chrome/126.0.0.0";

    /// 差し替えが無ければ、各クライアントの既定をそのまま使う。
    #[test]
    fn keeps_the_default_without_an_override() {
        assert_eq!(resolve(DEFAULT, None), DEFAULT);
    }

    /// 空文字・空白だけの差し替えは「未指定」として扱う（起動スクリプトの事故で
    /// 空 UA を送ってサイトに弾かれる、を防ぐ）。
    #[test]
    fn treats_blank_overrides_as_unspecified() {
        assert_eq!(resolve(DEFAULT, Some("")), DEFAULT);
        assert_eq!(resolve(DEFAULT, Some("   ")), DEFAULT);
        assert_eq!(resolve(DEFAULT, Some("\t\n")), DEFAULT);
    }

    /// 差し替え値があればそれを使う（前後の空白は落とす）。
    #[test]
    fn uses_the_override_value() {
        let app_ua = "ThundokuShelf/0.2.7 (+https://github.com/MegaBlackLabel/thundoku-shelf-app)";
        assert_eq!(resolve(DEFAULT, Some(app_ua)), app_ua);
        assert_eq!(resolve(DEFAULT, Some(&format!(" {app_ua} "))), app_ua);
    }
}
