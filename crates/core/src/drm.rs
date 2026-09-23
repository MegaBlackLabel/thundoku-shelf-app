//! 作品の DRM 状態（`is_drm` 列）の意味づけ。
//!
//! 以前は同期時に **`0` 固定**で保存していた。これは「DRM なしと確認できた」ではなく
//! 「判定していない」の意味だったので、本棚のメタデータが実態と食い違っていた
//! （DLsite / BOOTH / 技術書典 は DRM の情報を返さず、FANZA だけがダウンロード直前に
//! 詳細 API の `drm` を見て拒否している）。
//!
//! そこで 3 状態にする:
//!
//! | DB | 状態 | 意味 |
//! |---|---|---|
//! | `0` | [`DrmStatus::NoDrm`] | DRM なしと**確認できた**（FANZA の詳細 API に `drm` が無い） |
//! | `1` | [`DrmStatus::Protected`] | DRM ありと**確認できた**（同 API に `drm` がある＝取り込み対象外） |
//! | `2` | [`DrmStatus::Unknown`] | 判定していない（同期は DRM を見ない。既定はこちら） |
//!
//! 既存 DB の `0` は「未検証」の意味で書かれていたため、起動時に一度だけ
//! [`crate::db::migrate_drm_status_once`] が `2`（不明）へ移す。以降に書かれる `0` は
//! 「確認できた なし」だけになる。

/// 作品の DRM 状態。DB の `is_drm` 列（`INTEGER NOT NULL DEFAULT 0`）に対応する。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DrmStatus {
    /// DRM なしと確認できた。
    NoDrm,
    /// DRM ありと確認できた（取り込み対象外）。
    Protected,
    /// 判定していない（既定）。**「なし」ではない。**
    #[default]
    Unknown,
}

impl DrmStatus {
    /// DB の値へ。`NoDrm` = 0 / `Protected` = 1 / `Unknown` = 2。
    pub fn as_db(self) -> i64 {
        match self {
            Self::NoDrm => 0,
            Self::Protected => 1,
            Self::Unknown => 2,
        }
    }

    /// DB の値から。未知の値（将来追加・破損）は **`Unknown` として扱う**（fail-safe。
    /// 「なし」と誤って扱うと、取り込めない作品を取り込もうとする）。
    pub fn from_db(value: i64) -> Self {
        match value {
            0 => Self::NoDrm,
            1 => Self::Protected,
            _ => Self::Unknown,
        }
    }

    /// 画面・ログ用の短い日本語。
    pub fn label(self) -> &'static str {
        match self {
            Self::NoDrm => "DRM なし",
            Self::Protected => "DRM あり",
            Self::Unknown => "不明",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// DB との対応（書き出し → 読み直しで元に戻る）。
    #[test]
    fn round_trips_through_the_db_value() {
        for status in [DrmStatus::NoDrm, DrmStatus::Protected, DrmStatus::Unknown] {
            assert_eq!(DrmStatus::from_db(status.as_db()), status, "{status:?}");
        }
        assert_eq!(DrmStatus::NoDrm.as_db(), 0);
        assert_eq!(DrmStatus::Protected.as_db(), 1);
        assert_eq!(DrmStatus::Unknown.as_db(), 2);
    }

    /// 未知の値は「不明」として読む（「なし」と誤ると取り込めない作品を取り込もうとする）。
    #[test]
    fn unknown_db_values_are_treated_as_unknown() {
        for value in [-1, 3, 99, i64::MAX] {
            assert_eq!(
                DrmStatus::from_db(value),
                DrmStatus::Unknown,
                "value={value} を「なし」と解釈している"
            );
        }
    }

    /// 既定（`Default`）は「不明」。同期が値を入れ忘れても「なし」と嘘をつかない。
    #[test]
    fn defaults_to_unknown() {
        assert_eq!(DrmStatus::default(), DrmStatus::Unknown);
    }
}
