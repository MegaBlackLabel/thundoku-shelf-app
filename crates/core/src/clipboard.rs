//! クリップボードへ画像「ファイル」と文字列を置く。
//!
//! レポート画面の添付画像はアプリから GitHub へアップロードしない（GitHub のトークンを
//! 持たない）。代わりに「ファイル」としてクリップボードへ置き、利用者が GitHub の本文で
//! `Ctrl+V` するとアップロードされて本文に展開される（実測 2026-10-04。
//! `<img src="https://github.com/user-attachments/assets/…">` が挿入された。画像と文字列を
//! 同時に置いた場合、貼り付け先は**画像を優先**し、本文は `Ctrl+Shift+V` で入る）。
//!
//! Windows は `CF_HDROP`（`DROPFILES` + UTF-16 のパス一覧）と `CF_UNICODETEXT`。`gpui` の
//! `write_to_clipboard` は `ClipboardEntry::ExternalPaths` を書かず（`gpui-pre-windows` 0.3.2
//! が無視する）、`App` を要求するため **update の外**から使えない。[`crate::google::open_browser`]
//! と同じ考え方で自前で置く。**コマンドライン経由にしない**: 空白や引用符を含むパスが壊れる。
//!
//! 対応は **Windows**（`CF_HDROP`。実測 2026-10-04）と、文字列のみ **macOS**（`pbcopy`）。
//! 他 OS では画像の受け渡しができないので `Err` を返し、呼び出し側が「GitHub の画面で
//! 添付してください」と案内する。

use std::io;
use std::path::PathBuf;

/// 文字列をクリップボードへ置く（Windows: `CF_UNICODETEXT` / macOS: `pbcopy`）。
///
/// 画像（[`copy_files`]）と同じ理由で自前で置く: レポートの送信は **update の外**で
/// クリップボードを触る必要があり、`gpui` の `write_to_clipboard` は `App` を要求する。
pub fn copy_text(text: &str) -> io::Result<()> {
    #[cfg(target_os = "windows")]
    {
        windows_clipboard::put_text(text)
    }
    #[cfg(target_os = "macos")]
    {
        // macOS の標準 CLI。追加のクレートを増やさずに一般ペーストボードへ書ける。
        use std::io::Write as _;

        let mut child = std::process::Command::new("pbcopy")
            .stdin(std::process::Stdio::piped())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes())?;
        }
        let status = child.wait()?;
        if status.success() {
            Ok(())
        } else {
            Err(io::Error::other(format!(
                "pbcopy が失敗しました ({status})"
            )))
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos")))]
    {
        let _ = text;
        Err(io::Error::other(
            "この OS ではクリップボードへの書き込みに未対応です",
        ))
    }
}

/// 画像ファイルを「ファイル」としてクリップボードへ置く。
///
/// `text` を渡すと本文も一緒に置く（本文が長すぎて URL に載らないとき、利用者が GitHub の
/// 画面へ貼れるようにするため）。**画像と同時に置くと貼り付け先は画像を優先する**
/// （本文は `Ctrl+Shift+V` で取り出せる。実測 2026-10-04）。
///
/// **Windows のみ対応**。ファイル一覧を載せる汎用の API が他 OS に無く、失敗は呼び出し側が
/// 画面に出す（macOS / Linux では画像を GitHub の画面で添付してもらう）。
pub fn copy_files(paths: &[PathBuf], text: Option<&str>) -> io::Result<()> {
    if paths.is_empty() {
        return Ok(());
    }
    #[cfg(target_os = "windows")]
    {
        windows_clipboard::put_files(paths, text)
    }
    #[cfg(not(target_os = "windows"))]
    {
        let _ = (paths, text);
        Err(io::Error::other(
            "この OS ではクリップボードへのファイルコピーに未対応です",
        ))
    }
}

/// `CF_HDROP` に載せる中身: `DROPFILES` ヘッダー（20 バイト）+ UTF-16 のパス一覧。
///
/// - `pFiles` はヘッダー先頭からファイル一覧までのオフセット（= 20）
/// - `fWide` を 1 にして UTF-16 で並べる
/// - パスは NUL 区切りで、最後を NUL でもう一度終える（一覧の終端）
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn dropfiles_buffer(paths: &[PathBuf]) -> Vec<u8> {
    // DROPFILES { DWORD pFiles; POINT pt; BOOL fNC; BOOL fWide; } = 20 バイト。
    // POINT / fNC は 0 のままでよい（ドロップ先は使わない）。
    let mut header = [0u8; 20];
    header[0..4].copy_from_slice(&20u32.to_le_bytes());
    header[16..20].copy_from_slice(&1u32.to_le_bytes());

    let mut buffer = Vec::from(header);
    for path in paths {
        buffer.extend_from_slice(&wide(&path.to_string_lossy()));
    }
    // 各パスが NUL（2 バイト）で終わっているので、もう 1 つ足すと一覧の終端になる。
    buffer.extend_from_slice(&0u16.to_le_bytes());
    buffer
}

/// NUL 終端の UTF-16（リトルエンディアンのバイト列）。`CF_UNICODETEXT` と `CF_HDROP` 用。
#[cfg_attr(not(target_os = "windows"), allow(dead_code))]
fn wide(value: &str) -> Vec<u8> {
    value
        .encode_utf16()
        .chain(std::iter::once(0))
        .flat_map(u16::to_le_bytes)
        .collect()
}

/// Windows のクリップボード書き込み（`OpenClipboard` / `SetClipboardData`）。
#[cfg(target_os = "windows")]
mod windows_clipboard {
    use super::{dropfiles_buffer, wide};
    use std::io;
    use std::path::PathBuf;
    use windows_sys::Win32::Foundation::GlobalFree;
    use windows_sys::Win32::System::DataExchange::{
        CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData,
    };
    use windows_sys::Win32::System::Memory::{
        GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalUnlock,
    };
    use windows_sys::Win32::System::Ole::{CF_HDROP, CF_UNICODETEXT};

    /// バイト列をクリップボードへ載せる。成功したら所有権はシステムへ移る（解放しない）。
    ///
    /// # Safety
    ///
    /// クリップボードを開いている間だけ呼ぶこと（`replace` が保証する）。
    unsafe fn put(bytes: &[u8], format: u32) -> io::Result<()> {
        let handle = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes.len()) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let pointer = unsafe { GlobalLock(handle) };
        if pointer.is_null() {
            unsafe { GlobalFree(handle) };
            return Err(io::Error::last_os_error());
        }
        unsafe {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), pointer.cast::<u8>(), bytes.len());
            GlobalUnlock(handle);
        }
        if unsafe { SetClipboardData(format, handle) }.is_null() {
            let error = io::Error::last_os_error();
            unsafe { GlobalFree(handle) };
            return Err(error);
        }
        Ok(())
    }

    /// クリップボードを開いて空にし、`fill` が載せた内容で置き換える（必ず閉じる）。
    fn replace(fill: impl FnOnce() -> io::Result<()>) -> io::Result<()> {
        // SAFETY: `hwnd` は null（所有者なし）。開けなかった場合は何も触らずに返す。
        if unsafe { OpenClipboard(std::ptr::null_mut()) } == 0 {
            return Err(io::Error::last_os_error());
        }
        let result = (|| -> io::Result<()> {
            if unsafe { EmptyClipboard() } == 0 {
                return Err(io::Error::last_os_error());
            }
            fill()
        })();
        unsafe { CloseClipboard() };
        result
    }

    pub(super) fn put_text(text: &str) -> io::Result<()> {
        replace(|| unsafe { put(&wide(text), u32::from(CF_UNICODETEXT)) })
    }

    pub(super) fn put_files(paths: &[PathBuf], text: Option<&str>) -> io::Result<()> {
        replace(|| {
            unsafe { put(&dropfiles_buffer(paths), u32::from(CF_HDROP))? };
            if let Some(text) = text {
                unsafe { put(&wide(text), u32::from(CF_UNICODETEXT))? };
            }
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// パス一覧の UTF-16 を NUL で終える（`CF_HDROP` の取り決め）。
    #[test]
    fn dropfiles_buffer_lays_out_wide_paths_terminated_by_a_nul() {
        let paths = [
            PathBuf::from(r"C:\tmp\a.png"),
            PathBuf::from(r"C:\tmp\two words\b.png"),
        ];
        let buffer = dropfiles_buffer(&paths);

        // ヘッダー: pFiles = 20、fWide = 1（UTF-16）
        assert_eq!(u32::from_le_bytes(buffer[0..4].try_into().unwrap()), 20);
        assert_eq!(u32::from_le_bytes(buffer[16..20].try_into().unwrap()), 1);

        let expected: Vec<u16> = paths
            .iter()
            .flat_map(|path| {
                path.to_string_lossy()
                    .encode_utf16()
                    .chain(std::iter::once(0))
                    .collect::<Vec<_>>()
            })
            .collect();
        let written: Vec<u16> = buffer[20..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        assert_eq!(
            written.len(),
            expected.len() + 1,
            "末尾の終端 NUL が無い / 余分なバイトがある"
        );
        assert_eq!(&written[..expected.len()], expected.as_slice());
        assert_eq!(written[expected.len()], 0, "一覧の終端が NUL でない");
        assert_eq!(buffer.len(), 20 + (expected.len() + 1) * 2);
    }

    /// 空の一覧はそのまま何もしない（クリップボードを空にしない）。
    #[test]
    fn copy_files_with_no_paths_does_nothing() {
        assert!(copy_files(&[], None).is_ok());
    }
}
