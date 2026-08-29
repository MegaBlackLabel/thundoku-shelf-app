//! Web 版（thundoku-web）と同じ lucide アイコンを提供するアセットソース。
//!
//! Web 版は `lucide-react` のアイコンを使っている。gpui-component にも
//! lucide 由来のアイコンはあるが、Web 版で使われていて gpui-component に
//! 無いもの（book-marked / library-big / list-checks / refresh-cw /
//! layout-grid / list / circle-user-round / log-in / log-out）は
//! `assets/icons/*.svg` に lucide-react から生成した SVG を埋め込んで提供する。

use std::borrow::Cow;

use gpui::{AssetSource, Result, SharedString};
use gpui_component::IconNamed;

/// Web 版で使われている lucide アイコンのうち、gpui-component に無いもの。
#[derive(Clone, Copy)]
pub enum AppIcon {
    /// ロゴ（BookMarkedIcon）
    BookMarked,
    /// 本棚（LibraryBigIcon）
    LibraryBig,
    /// チェックリスト（ListChecksIcon）
    ListChecks,
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
}

impl IconNamed for AppIcon {
    fn path(self) -> SharedString {
        match self {
            Self::BookMarked => "icons/book-marked.svg".into(),
            Self::LibraryBig => "icons/library-big.svg".into(),
            Self::ListChecks => "icons/list-checks.svg".into(),
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
        gpui_component_assets::Assets.load(path)
    }

    fn list(&self, path: &str) -> Result<Vec<SharedString>> {
        gpui_component_assets::Assets.list(path)
    }
}

fn custom_icon(path: &str) -> Option<&'static [u8]> {
    match path {
        "icons/book-marked.svg" => Some(include_bytes!("../assets/icons/book-marked.svg")),
        "icons/library-big.svg" => Some(include_bytes!("../assets/icons/library-big.svg")),
        "icons/list-checks.svg" => Some(include_bytes!("../assets/icons/list-checks.svg")),
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
        _ => None,
    }
}
