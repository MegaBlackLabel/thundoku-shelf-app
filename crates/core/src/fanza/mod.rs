//! FANZA同人（`www.dmm.co.jp/dc/doujin/...`）の取り込み。
//!
//! 現段階はソースメタ分類（メディア種別 + AI 生成状態の 2 軸）のみ。
//! クライアント（JSON API）・同期・インポートは後続スライス。
//!
//! 分類は FANZA の `genre` カテゴリ / `imageSrc` パスを正規化し、画像系
//! （コミック / CG。AI バリアント含む）だけをビューアー対象にする。

pub mod client;
pub mod sync;

/// メディア種別（FANZA の `genre` カテゴリ / `imageSrc` パス由来、正規化した enum）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaCategory {
    Comic,
    Cg,
    Voice,
    Game,
    Video,
}

/// AI 生成状態（FANZA の `genre` サフィックス `・一部AI` / `・AI` 由来）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AiType {
    None,
    PartialAi,
    FullAi,
}

/// 2 軸分類（メディア種別 + AI 生成状態）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FanzaMeta {
    pub media: MediaCategory,
    pub ai: AiType,
}

/// `image_src` の `/digital/{comic,cg,voice,game,video}/` セグメントからメディア種別を判定する。
fn media_from_image_src(image_src: &str) -> Option<MediaCategory> {
    let seg = image_src.split('/').find(|s| {
        matches!(*s, "comic" | "cg" | "voice" | "game" | "video")
    })?;
    Some(match seg {
        "comic" => MediaCategory::Comic,
        "cg" => MediaCategory::Cg,
        "voice" => MediaCategory::Voice,
        "game" => MediaCategory::Game,
        _ => MediaCategory::Video,
    })
}

/// `genre` 文字列の先頭（`・` より前）からメディア種別を判定する（日本語ラベルでのフォールバック）。
fn media_from_genre(genre: &str) -> Option<MediaCategory> {
    let base = genre.split('・').next().unwrap_or(genre);
    match base {
        "コミック" => Some(MediaCategory::Comic),
        "CG" => Some(MediaCategory::Cg),
        "ボイス" => Some(MediaCategory::Voice),
        "ゲーム" => Some(MediaCategory::Game),
        "動画" => Some(MediaCategory::Video),
        _ => None,
    }
}

/// `genre` 末尾のサフィックス `・一部AI` / `・AI` から AI 生成状態を判定する。
/// `一部AI` を先に判定する（`コミック・一部AI` は `・AI` とも末尾一致するため）。
fn ai_from_genre(genre: &str) -> AiType {
    if genre.ends_with("・一部AI") {
        AiType::PartialAi
    } else if genre.ends_with("・AI") {
        AiType::FullAi
    } else {
        AiType::None
    }
}

/// 2 軸分類。`image_src` パスの `/digital/...` セグメントを優先し、判読不能なら
/// `genre` 文字列にフォールバックする。両方で判読不能な場合は安全側（除外される
/// `Video`）へ寄せる。
pub fn classify(image_src: &str, genre: &str) -> FanzaMeta {
    let media = media_from_image_src(image_src)
        .or_else(|| media_from_genre(genre))
        .unwrap_or(MediaCategory::Video);
    FanzaMeta { media, ai: ai_from_genre(genre) }
}

/// ビューアー（画像系）対象か。画像系（`Comic` / `Cg`。AI バリアント含む）かつ
/// DRM 無しだけを許可する。ボイス / ゲーム / 動画は含めない。
pub fn is_viewable_included(meta: &FanzaMeta, drm_ok: bool) -> bool {
    drm_ok && matches!(meta.media, MediaCategory::Comic | MediaCategory::Cg)
}

/// `MediaCategory` → 正規化文字列（`bookshelf_items.media_category` の値）。
pub fn media_to_str(media: MediaCategory) -> &'static str {
    match media {
        MediaCategory::Comic => "comic",
        MediaCategory::Cg => "cg",
        MediaCategory::Voice => "voice",
        MediaCategory::Game => "game",
        MediaCategory::Video => "video",
    }
}

/// `AiType` → 正規化文字列（`bookshelf_items.ai_type` の値）。
pub fn ai_to_str(ai: AiType) -> &'static str {
    match ai {
        AiType::None => "none",
        AiType::PartialAi => "partial",
        AiType::FullAi => "full",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 画像系（コミック / CG）かつ DRM 無しだけをビューアー対象にすること。AI バリアント
    /// （コミック・一部AI / CG・AI）は画像系として含め、ボイス / ゲーム / 動画は除外する。
    #[test]
    fn classicify_media_and_ai_axes() {
        assert_eq!(
            classify("https://doujin-assets.dmm.co.jp/digital/comic/d_1/d_1pl.jpg", "コミック"),
            FanzaMeta { media: MediaCategory::Comic, ai: AiType::None }
        );
        assert_eq!(
            classify("https://assets/digital/comic/d_1/x.jpg", "コミック・一部AI"),
            FanzaMeta { media: MediaCategory::Comic, ai: AiType::PartialAi }
        );
        assert_eq!(
            classify("https://assets/digital/cg/d_2/x.jpg", "CG・AI"),
            FanzaMeta { media: MediaCategory::Cg, ai: AiType::FullAi }
        );
        assert_eq!(
            classify("https://assets/digital/voice/d_3/x.jpg", "ボイス"),
            FanzaMeta { media: MediaCategory::Voice, ai: AiType::None }
        );
        assert_eq!(
            classify("https://assets/digital/game/d_4/x.jpg", "ゲーム"),
            FanzaMeta { media: MediaCategory::Game, ai: AiType::None }
        );
        assert_eq!(
            classify("https://assets/digital/video/d_5/x.jpg", "動画"),
            FanzaMeta { media: MediaCategory::Video, ai: AiType::None }
        );
        // AI でもメディア種別で除外される（ボイス・AI）
        assert_eq!(
            classify("https://assets/digital/voice/d_6/x.jpg", "ボイス・AI"),
            FanzaMeta { media: MediaCategory::Voice, ai: AiType::FullAi }
        );
    }

    /// `imageSrc` パスに判読可能なセグメントが無いときは `genre` 文字列にフォールバックする。
    #[test]
    fn classify_falls_back_to_genre_when_no_image_path() {
        assert_eq!(
            classify("", "ボイス・一部AI"),
            FanzaMeta { media: MediaCategory::Voice, ai: AiType::PartialAi }
        );
        assert_eq!(
            classify("https://example.com/thumb.jpg", "コミック"),
            FanzaMeta { media: MediaCategory::Comic, ai: AiType::None }
        );
    }

    /// 画像系（comic/cg。AI 含む）+ DRM 無しのみ viewable。未知メディアは安全側（除外）。
    #[test]
    fn is_viewable_included_filters_image_only_and_drm() {
        assert!(is_viewable_included(&FanzaMeta { media: MediaCategory::Comic, ai: AiType::None }, true));
        assert!(is_viewable_included(&FanzaMeta { media: MediaCategory::Comic, ai: AiType::FullAi }, true));
        assert!(is_viewable_included(&FanzaMeta { media: MediaCategory::Cg, ai: AiType::PartialAi }, true));
        assert!(!is_viewable_included(&FanzaMeta { media: MediaCategory::Voice, ai: AiType::None }, true));
        assert!(!is_viewable_included(&FanzaMeta { media: MediaCategory::Game, ai: AiType::None }, true));
        assert!(!is_viewable_included(&FanzaMeta { media: MediaCategory::Video, ai: AiType::None }, true));
        assert!(!is_viewable_included(&FanzaMeta { media: MediaCategory::Comic, ai: AiType::None }, false)); // DRM 付き
    }
}
