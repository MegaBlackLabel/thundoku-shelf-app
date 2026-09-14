//! Web 版（thundoku-web）と同じ lucide アイコンを提供するアセットソース。
//!
//! Web 版は `lucide-react` のアイコンを使っている。gpui-component にも
//! lucide 由来のアイコンはあるが、Web 版で使われていて gpui-component に
//! 無いもの（book-marked / library-big / history / book-open / arrow-right /
//! bookmark / bookmark-filled / sticky-note /
//! list-checks /
//! refresh-cw / layout-grid / list / circle-user-round / log-in / log-out）は
//! `assets/icons/*.svg` に lucide-react から生成した SVG を埋め込んで提供する。
//!
//! 埋め込んでいる SVG は [Lucide](https://lucide.dev)（ISC License,
//! Copyright (c) Lucide Icons and Contributors）から取得したもの。ISC は
//! 再配布時に著作権表示の保持を求めるため、この注記を残すこと
//! （`assets/icons/*.svg` の生成元は lucide の各アイコン）。

use std::borrow::Cow;

use gpui_kit::component::IconNamed;
use gpui_kit::{AssetSource, Result, SharedString};

/// Web 版で使われている lucide アイコンのうち、gpui-component に無いもの。
#[derive(Clone, Copy)]
pub enum AppIcon {
    /// ロゴ（BookMarkedIcon）
    BookMarked,
    /// 本棚（LibraryBigIcon）
    LibraryBig,
    /// 閲覧履歴（HistoryIcon）
    History,
    /// ビューアー（BookOpenIcon: 開いて閲覧する）
    BookOpen,
    /// 関連書籍へのショートカット（ArrowRightIcon: 移動）
    ArrowRight,
    /// 付箋（しおり型の枠。BookmarkIcon）
    Bookmark,
    /// 付箋の塗り（しおり型。同じパスの `fill`。BookmarkFilledIcon）
    BookmarkFilled,
    /// 付箋（サイドバー用のふせん型。StickyNoteIcon）
    StickyNote,
    /// チェックリスト（ListChecksIcon）
    ListChecks,
    /// レポート（MegaphoneIcon: 問題・要望を送る）
    Megaphone,
    /// 同期（RefreshCw）
    RefreshCw,
    /// タイル表示（LayoutGrid）
    LayoutGrid,
    /// リスト表示（List）
    List,
    /// アカウント（CircleUserRound）
    CircleUserRound,
    /// ログイン（LogInIcon）
    LogIn,
    /// ログアウト（LogOutIcon）
    LogOut,
    /// ダウンロード（Download）
    Download,
    /// タグ（Tag）
    Tag,
    /// ハードドライブ（HardDriveIcon、gpui-component 標準）
    HardDrive,
    /// クラウド（CloudIcon）
    Cloud,
    /// ライトテーマ（SunIcon）
    Sun,
    /// ダークテーマ（MoonIcon）
    Moon,
    /// システムテーマ（MonitorIcon）
    Monitor,
    /// データベース（DatabaseIcon）
    Database,
    /// 名前の変更（Pencil）
    Pencil,
    /// お気に入り（lucide Heart、未登録）
    Heart,
    /// お気に入り（lucide Heart、登録済み = 塗りつぶし）
    HeartFilled,
}

impl IconNamed for AppIcon {
    fn path(self) -> SharedString {
        match self {
            Self::BookMarked => "icons/book-marked.svg".into(),
            Self::LibraryBig => "icons/library-big.svg".into(),
            Self::History => "icons/history.svg".into(),
            Self::BookOpen => "icons/book-open.svg".into(),
            Self::ArrowRight => "icons/arrow-right.svg".into(),
            Self::Bookmark => "icons/bookmark.svg".into(),
            Self::BookmarkFilled => "icons/bookmark-filled.svg".into(),
            Self::StickyNote => "icons/sticky-note.svg".into(),
            Self::ListChecks => "icons/list-checks.svg".into(),
            Self::Megaphone => "icons/megaphone.svg".into(),
            Self::RefreshCw => "icons/refresh-cw.svg".into(),
            Self::LayoutGrid => "icons/layout-grid.svg".into(),
            Self::List => "icons/list.svg".into(),
            Self::CircleUserRound => "icons/circle-user-round.svg".into(),
            Self::LogIn => "icons/log-in.svg".into(),
            Self::LogOut => "icons/log-out.svg".into(),
            Self::Download => "icons/download.svg".into(),
            Self::Tag => "icons/tag.svg".into(),
            Self::HardDrive => "icons/hard-drive.svg".into(),
            Self::Cloud => "icons/cloud.svg".into(),
            Self::Sun => "icons/sun.svg".into(),
            Self::Moon => "icons/moon.svg".into(),
            Self::Monitor => "icons/monitor.svg".into(),
            Self::Database => "icons/database.svg".into(),
            Self::Pencil => "icons/pencil.svg".into(),
            Self::Heart => "icons/heart.svg".into(),
            Self::HeartFilled => "icons/heart-filled.svg".into(),
        }
    }
}

/// gpui-component のアイコン + Web 版 lucide アイコンを提供するアセットソース。
pub struct AppAssets;

impl AssetSource for AppAssets {
    fn load(&self, path: &str) -> Result<Option<Cow<'static, [u8]>>> {
        if let Some(bytes) = custom_icon(path) {
            return Ok(Some(Cow::Borrowed(bytes)));
        }
        gpui_kit::assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        gpui_kit::assets::Assets.list(path)
    }
}

fn custom_icon(path: &str) -> Option<&'static [u8]> {
    match path {
        "icons/book-marked.svg" => Some(include_bytes!("../assets/icons/book-marked.svg")),
        "icons/library-big.svg" => Some(include_bytes!("../assets/icons/library-big.svg")),
        "icons/history.svg" => Some(include_bytes!("../assets/icons/history.svg")),
        "icons/book-open.svg" => Some(include_bytes!("../assets/icons/book-open.svg")),
        "icons/arrow-right.svg" => Some(include_bytes!("../assets/icons/arrow-right.svg")),
        "icons/bookmark.svg" => Some(include_bytes!("../assets/icons/bookmark.svg")),
        "icons/sticky-note.svg" => Some(include_bytes!("../assets/icons/sticky-note.svg")),
        "icons/bookmark-filled.svg" => Some(include_bytes!("../assets/icons/bookmark-filled.svg")),
        "icons/list-checks.svg" => Some(include_bytes!("../assets/icons/list-checks.svg")),
        "icons/megaphone.svg" => Some(include_bytes!("../assets/icons/megaphone.svg")),
        "icons/refresh-cw.svg" => Some(include_bytes!("../assets/icons/refresh-cw.svg")),
        "icons/layout-grid.svg" => Some(include_bytes!("../assets/icons/layout-grid.svg")),
        "icons/list.svg" => Some(include_bytes!("../assets/icons/list.svg")),
        "icons/circle-user-round.svg" => {
            Some(include_bytes!("../assets/icons/circle-user-round.svg"))
        }
        "icons/log-in.svg" => Some(include_bytes!("../assets/icons/log-in.svg")),
        "icons/log-out.svg" => Some(include_bytes!("../assets/icons/log-out.svg")),
        "icons/download.svg" => Some(include_bytes!("../assets/icons/download.svg")),
        "icons/tag.svg" => Some(include_bytes!("../assets/icons/tag.svg")),
        "icons/cloud.svg" => Some(include_bytes!("../assets/icons/cloud.svg")),
        "icons/database.svg" => Some(include_bytes!("../assets/icons/database.svg")),
        "icons/sun.svg" => Some(include_bytes!("../assets/icons/sun.svg")),
        "icons/moon.svg" => Some(include_bytes!("../assets/icons/moon.svg")),
        "icons/monitor.svg" => Some(include_bytes!("../assets/icons/monitor.svg")),
        "icons/pencil.svg" => Some(include_bytes!("../assets/icons/pencil.svg")),
        "icons/heart.svg" => Some(include_bytes!("../assets/icons/heart.svg")),
        "icons/heart-filled.svg" => Some(include_bytes!("../assets/icons/heart-filled.svg")),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// パス文字列（`IconNamed::path`）と `custom_icon` の対応がずれると
    /// アイコンが無言で出なくなるため、実アセットが引けることを確かめる。
    #[test]
    fn every_app_icon_resolves_to_an_asset() {
        let icons = [
            AppIcon::BookMarked,
            AppIcon::LibraryBig,
            AppIcon::History,
            AppIcon::BookOpen,
            AppIcon::ArrowRight,
            AppIcon::Bookmark,
            AppIcon::BookmarkFilled,
            AppIcon::StickyNote,
            AppIcon::ListChecks,
            AppIcon::Megaphone,
            AppIcon::RefreshCw,
            AppIcon::LayoutGrid,
            AppIcon::List,
            AppIcon::CircleUserRound,
            AppIcon::LogIn,
            AppIcon::LogOut,
            AppIcon::Download,
            AppIcon::Tag,
            AppIcon::HardDrive,
            AppIcon::Cloud,
            AppIcon::Sun,
            AppIcon::Moon,
            AppIcon::Monitor,
            AppIcon::Database,
            AppIcon::Pencil,
            AppIcon::Heart,
            AppIcon::HeartFilled,
        ];
        for icon in icons {
            let path = icon.path();
            // アプリ固有のアイコン（custom_icon）と gpui-kit 標準の両方が対象。
            // 実際の解決経路（AppAssets）で引けることを確かめる。
            let loaded = AppAssets.load(&path).expect("asset load");
            assert!(loaded.is_some(), "アセットが引けない: {path}");
        }
    }
}
