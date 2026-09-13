//! Global actions for the Thundoku Shelf desktop app.

use gpui_kit::{SharedString, actions};

actions!(
    thundoku,
    [
        /// Switch the main area to the bookshelf.
        NavBookshelf,
        /// Switch the main area to the viewing history.
        NavHistory,
        /// Switch the main area to the checklist.
        NavChecklist,
        /// Switch the main area to the settings.
        NavSettings,
        /// Toggle light/dark theme.
        ToggleTheme,
        /// Trigger a Drive sync pass.
        SyncDrive,
        /// Show the login overlay in the workspace.
        OpenAuth,
        /// Hide the login overlay.
        CloseAuth,
        /// Close the reader and return to the previous view.
        CloseReader,
        /// Quit the application (macOS アプリメニューの「終了」).
        QuitApp,
        /// Toggle the sidebar open/collapsed.
        ToggleSidebar,
        /// Switch the main area to the bookshelf (メニュー用)。
        ShowBookshelf,
        /// Switch the main area to the viewing history (メニュー用)。
        ShowHistory,
        /// Switch the main area to the checklist (メニュー用)。
        ShowChecklist,
        /// Switch the main area to the settings (メニュー用)。
        ShowSettings,
        /// Switch the main area to the about (メニュー用)。
        ShowAbout,
        /// Open the about view (「〜について」メニュー)。
        OpenAbout,
    ]
);

/// メニュー表示専用の何もしないアクション（閲覧回数の情報行など）。
#[derive(Clone, Debug, PartialEq, serde::Deserialize, gpui_kit::Action)]
pub struct Noop;

/// Open the reader for a book (payload = book id).
#[derive(Clone, Debug, PartialEq, serde::Deserialize, gpui_kit::Action)]
#[action(namespace = thundoku, no_json)]
pub struct OpenReader {
    pub book_id: SharedString,
}

/// Open the sample-page reader for a checklist item (payload = item id).
#[derive(Clone, Debug, PartialEq, serde::Deserialize, gpui_kit::Action)]
#[action(namespace = thundoku, no_json)]
pub struct OpenSampleReader {
    pub item_id: SharedString,
}

/// 表示中のページに付箋を付ける（ビューアのページ右上の付箋アイコンから）。
/// ダイアログを開くところまでで、保存はダイアログの OK で行う。
#[derive(Clone, Debug, PartialEq, serde::Deserialize, gpui_kit::Action)]
#[action(namespace = thundoku, no_json)]
pub struct NotePageRequest {
    /// 1-indexed のページ番号（`reading_progress` と同じ）。
    pub page: i64,
    /// 見開きの左右（"left" / "right"。単一表示は None）。
    pub side: Option<SharedString>,
}

/// Edit the manual tags of a book.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, gpui_kit::Action)]
#[action(namespace = thundoku, no_json)]
pub struct EditBookTags {
    pub book_id: SharedString,
}

/// Delete a book locally (Drive is never touched).
#[derive(Clone, Debug, PartialEq, serde::Deserialize, gpui_kit::Action)]
#[action(namespace = thundoku, no_json)]
pub struct DeleteBook {
    pub book_id: SharedString,
}

/// 本を再ダウンロードする（コンテキストメニューの「再取得」）。
/// 既にローカルに持っている場合は pack ごと削除してから再取得する。
#[derive(Clone, Debug, PartialEq, serde::Deserialize, gpui_kit::Action)]
#[action(namespace = thundoku, no_json)]
pub struct RedownloadBook {
    pub database_id: SharedString,
    pub site_id: SharedString,
}

/// Hide a book from the shelf.
#[derive(Clone, Debug, PartialEq, serde::Deserialize, gpui_kit::Action)]
#[action(namespace = thundoku, no_json)]
pub struct HideBook {
    pub database_id: SharedString,
    pub site_id: SharedString,
}

/// Open the login overlay with a specific provider (payload = provider).
/// 設定画面などから「技術書典のログイン」を押したとき、プロバイダ選択画面
/// （Popover に見える画面）を経由せず、そのプロバイダのログイン画面を直接開く。
#[derive(Clone, Debug, PartialEq, gpui_kit::Action)]
#[action(namespace = thundoku, no_json)]
pub struct OpenAuthProvider {
    pub provider: crate::views::auth::AuthProvider,
}

/// Google ログイン成功後に Drive バックアップ有効化の確認を表示する。
#[derive(Clone, Debug, PartialEq, gpui_kit::Action)]
#[action(namespace = thundoku, no_json)]
pub struct PromptDriveEnable;
