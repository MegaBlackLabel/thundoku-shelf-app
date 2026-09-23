//! DLsite（`www.dlsite.com`）の取り込み。
//!
//! 分類は DLsite の `work_type`（作品形式コード）と `.work_genre` の `icon_*`
//! （`icon_MNG`/`icon_ICG`/`icon_SOU`/`icon_AIG`/`icon_AIP` 等）を正規化し、
//! 画像系（漫画 / CG・イラスト。AI バリアント含む）だけをビューアー対象にする。

pub mod client;
pub mod sync;

/// メディア種別（DLsite の `work_type` / `.work_genre` icon 由来、正規化した enum）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DlsiteMediaCategory {
    Comic,
    Cg,
    Voice,
    Game,
    Novel,
    Video,
    Other,
}

/// AI 生成状態（DLsite の `.work_genre` の `icon_AIG`（AI生成作品）/ `icon_AIP`
/// （AI一部利用）由来）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DlsiteAiType {
    None,
    PartialAi,
    FullAi,
}

/// 2 軸分類（メディア種別 + AI 生成状態）+ 年齢指定。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DlsiteMeta {
    pub media: DlsiteMediaCategory,
    pub ai: DlsiteAiType,
    pub age: Option<&'static str>,
}

/// `work_type` コード（`MNG` / `ICG` / `SOU` / ゲーム群 / `NRE` 等）からメディア種別を判定する。
pub(crate) fn media_from_work_type(work_type: &str) -> Option<DlsiteMediaCategory> {
    use DlsiteMediaCategory::*;
    Some(match work_type {
        "MNG" => Comic,
        "ICG" => Cg,
        "SOU" => Voice,
        "NRE" | "DNV" => Novel,
        "VCM" | "WBT" => Video,
        "ACN" | "ADV" | "QIZ" | "RPG" | "STG" | "SLN" | "TBL" | "TYP" | "PZL" | "ETC" => Game,
        _ => return None,
    })
}

/// `.work_genre` の `icon_*`（`icon_MNG` / `icon_ICG` / `icon_SOU` 等）からメディア種別を判定する。
/// `icon_` プレフィックスを剥がして `work_type` コードに合流させる（`icon_MNG` → `MNG`）。
fn media_from_genre_icons(icons: &[String]) -> Option<DlsiteMediaCategory> {
    for icon in icons {
        if let Some(code) = icon.strip_prefix("icon_")
            && let Some(m) = media_from_work_type(code)
        {
            return Some(m);
        }
    }
    None
}

/// `.work_genre` の `icon_AIG`（AI生成作品）→ full、`icon_AIP`（AI一部利用）→ partial。
fn ai_from_genre_icons(icons: &[String]) -> DlsiteAiType {
    use DlsiteAiType::*;
    if icons.iter().any(|i| i == "icon_AIG") {
        FullAi
    } else if icons.iter().any(|i| i == "icon_AIP") {
        PartialAi
    } else {
        DlsiteAiType::None
    }
}

/// 年齢指定を `age_category` / ストアフロアから判定する。
/// `age_category` が 1（実測）= 全年齢。2 以上（R18 想定）または R18 フロア（`maniax`）= R18。
/// どちらも無い場合は None。
fn age_from(site_id: &str, age_category: Option<i64>) -> Option<&'static str> {
    match age_category {
        Some(1) => Some("all"),
        Some(n) if n > 1 => Some("r18"),
        Some(_) => Some("all"),
        None => (site_id == "maniax").then_some("r18"),
    }
}

/// 2 軸分類。`work_type` を優先し、判読不能なら `.work_genre` icon にフォールバックする。
/// 両方で判読不能な場合は安全側（除外される `Other`）へ寄せる。AI は `site_id=="ai"`
/// （AI フロア）なら full、無ければ `.work_genre` icon から判定。
pub fn classify(
    work_type: &str,
    genre_icons: &[String],
    site_id: &str,
    age_category: Option<i64>,
) -> DlsiteMeta {
    let media = media_from_work_type(work_type)
        .or_else(|| media_from_genre_icons(genre_icons))
        .unwrap_or(DlsiteMediaCategory::Other);
    let ai = if site_id == "ai" {
        DlsiteAiType::FullAi
    } else {
        ai_from_genre_icons(genre_icons)
    };
    DlsiteMeta {
        media,
        ai,
        age: age_from(site_id, age_category),
    }
}

/// ビューアー（画像系）対象か。画像系（`Comic` / `Cg`。AI バリアント含む）のみ。
///
/// **DRM の有無はここでは見ない**。同期の時点では DRM を判定できない
/// （FANZA は取り込み直前に詳細 API で判定して拒否し、DLsite は取り込み時に
/// 読める形式でなければ失敗する）。`bookshelf_items.is_drm` も実データではなく
/// 同期が入れる既定値なので、判定材料にしない。
pub fn is_viewable_included(meta: &DlsiteMeta) -> bool {
    matches!(
        meta.media,
        DlsiteMediaCategory::Comic | DlsiteMediaCategory::Cg
    )
}

/// `DlsiteMediaCategory` → 正規化文字列（`bookshelf_items.media_category` の値）。
pub fn media_to_str(media: DlsiteMediaCategory) -> &'static str {
    use DlsiteMediaCategory::*;
    match media {
        Comic => "comic",
        Cg => "cg",
        Voice => "voice",
        Game => "game",
        Novel => "novel",
        Video => "video",
        Other => "other",
    }
}

/// `DlsiteAiType` → 正規化文字列（`bookshelf_items.ai_type` の値）。
pub fn ai_to_str(ai: DlsiteAiType) -> &'static str {
    use DlsiteAiType::*;
    match ai {
        DlsiteAiType::None => "none",
        PartialAi => "partial",
        FullAi => "full",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn icons(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    /// `work_type` コードのメディア種別マッピング。
    #[test]
    fn classify_work_type_axis() {
        use DlsiteMediaCategory::*;
        assert_eq!(
            classify("MNG", &icons(&["icon_MNG"]), "home", Some(1)).media,
            Comic
        );
        assert_eq!(
            classify("ICG", &icons(&["icon_ICG"]), "home", None).media,
            Cg
        );
        assert_eq!(
            classify("SOU", &icons(&["icon_SOU"]), "maniax", None).media,
            Voice
        );
        for g in [
            "ACN", "ADV", "QIZ", "RPG", "STG", "SLN", "TBL", "TYP", "PZL", "ETC",
        ] {
            assert_eq!(
                classify(g, &icons(&[]), "home", None).media,
                Game,
                "game {g}"
            );
        }
        assert_eq!(classify("NRE", &icons(&[]), "home", None).media, Novel);
        assert_eq!(classify("DNV", &icons(&[]), "home", None).media, Novel);
        assert_eq!(classify("VCM", &icons(&[]), "home", None).media, Video);
        assert_eq!(classify("WBT", &icons(&[]), "home", None).media, Video);
        assert_eq!(
            classify("", &icons(&["icon_MNG"]), "home", None).media,
            Comic
        );
        assert_eq!(classify("", &icons(&["icon_ICG"]), "home", None).media, Cg);
        // 不明は安全側（Other → 除外）
        assert_eq!(classify("UNKNOWN", &icons(&[]), "home", None).media, Other);
    }

    /// AI 判定: `site_id=="ai"`（AI フロア）→ full。`.work_genre` の `icon_AIG` → full、
    /// `icon_AIP` → partial。
    #[test]
    fn classify_ai_axis() {
        use DlsiteAiType::{FullAi, PartialAi};
        assert_eq!(classify("MNG", &icons(&[]), "ai", None).ai, FullAi);
        assert_eq!(
            classify("MNG", &icons(&["icon_AIG"]), "home", None).ai,
            FullAi
        );
        assert_eq!(
            classify("MNG", &icons(&["icon_AIP"]), "home", None).ai,
            PartialAi
        );
        assert_eq!(
            classify("MNG", &icons(&["icon_MNG"]), "home", None).ai,
            DlsiteAiType::None
        );
    }

    /// 年齢指定: `age_category=1` → all、R18 フロア（`maniax`）or 2 以上 → r18、無し → None。
    #[test]
    fn classify_age_axis() {
        assert_eq!(classify("MNG", &[], "home", Some(1)).age, Some("all"));
        assert_eq!(classify("MNG", &[], "maniax", Some(2)).age, Some("r18"));
        assert_eq!(classify("MNG", &[], "home", None).age, None);
    }

    /// 画像系（comic/cg。AI 含む）のみ viewable。ノベル / 音声 / ゲーム / 動画 / 不明は除外。
    /// **DRM は同期では判定できない**（取り込み時に読めなければ失敗する）ので判定に含めない。
    #[test]
    fn is_viewable_included_filters_image_media_only() {
        use DlsiteAiType::FullAi;
        use DlsiteMediaCategory::*;
        for (media, ai, expected) in [
            (Comic, DlsiteAiType::None, true),
            (Cg, FullAi, true),
            (Voice, DlsiteAiType::None, false),
            (Game, DlsiteAiType::None, false),
            (Novel, DlsiteAiType::None, false),
            (Video, DlsiteAiType::None, false),
            (Other, DlsiteAiType::None, false),
        ] {
            assert_eq!(
                is_viewable_included(&DlsiteMeta { media, ai, age: None }),
                expected,
                "{media:?} / {ai:?}"
            );
        }
    }

    /// 正規化文字列（DB 値）。
    #[test]
    fn media_and_ai_to_str() {
        use DlsiteAiType::{FullAi, PartialAi};
        use DlsiteMediaCategory::*;
        assert_eq!(media_to_str(Comic), "comic");
        assert_eq!(media_to_str(Cg), "cg");
        assert_eq!(media_to_str(Voice), "voice");
        assert_eq!(media_to_str(Game), "game");
        assert_eq!(media_to_str(Novel), "novel");
        assert_eq!(media_to_str(Video), "video");
        assert_eq!(media_to_str(Other), "other");
        assert_eq!(ai_to_str(DlsiteAiType::None), "none");
        assert_eq!(ai_to_str(PartialAi), "partial");
        assert_eq!(ai_to_str(FullAi), "full");
    }
}
