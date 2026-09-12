//! 単一インスタンスガード（macOS / Windows / Linux 共通）。
//!
//! ロックファイルを OS のファイルロックで排他ロックし、2 重起動を防ぐ。
//! ロックはプロセスに紐づき、クラッシュや強制終了でも OS が解放するため
//! 「古いロックが残って起動できなくなる」ことはない（ロックファイル自体は残る）。
//!
//! - Unix: `flock(2)`
//! - Windows: `LockFileEx`
//!
//! どちらも [`std::fs::File::try_lock`] が吸収するので、プラットフォーム分岐は無い。

use std::fs::{File, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

/// 単一インスタンスガードの取得失敗。
#[derive(Debug, thiserror::Error)]
pub enum InstanceError {
    /// 既に別のインスタンスがロックを保持している。
    #[error("another instance is already running (lock: {0})")]
    AlreadyRunning(PathBuf),
    /// ロックファイルを開けなかった（ディレクトリ作成・権限・I/O エラー）。
    #[error("failed to open the lock file {path}: {source}")]
    Io {
        /// 開けなかったロックファイル。
        path: PathBuf,
        /// 元の I/O エラー。
        source: io::Error,
    },
}

/// ロックファイルの排他ロックを保持するガード。
///
/// この値が drop される（= プロセスが終了する）とロックが解放され、
/// 次の起動がロックを取得できる。
#[derive(Debug)]
pub struct InstanceGuard {
    /// ロックを保持しているファイル。保持中は閉じてはいけない。
    _file: File,
    path: PathBuf,
}

impl InstanceGuard {
    /// `lock_path` の排他ロックを取得する。親ディレクトリは無ければ作成する。
    ///
    /// 既に別のインスタンスがロックを保持していれば
    /// [`InstanceError::AlreadyRunning`] を返す。
    pub fn acquire(lock_path: &Path) -> Result<Self, InstanceError> {
        if let Some(parent) = lock_path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent).map_err(|source| InstanceError::Io {
                path: lock_path.to_path_buf(),
                source,
            })?;
        }

        let file = File::options()
            .create(true)
            .read(true)
            .write(true)
            // 既存のロックファイルの中身は触らない（truncate すると
            // 起動中の他インスタンスの状態を壊しかねないため）
            .truncate(false)
            .open(lock_path)
            .map_err(|source| InstanceError::Io {
                path: lock_path.to_path_buf(),
                source,
            })?;

        match file.try_lock() {
            Ok(()) => Ok(Self {
                _file: file,
                path: lock_path.to_path_buf(),
            }),
            // 別のハンドル（= 別インスタンス）がロックを保持している
            Err(TryLockError::WouldBlock) => {
                Err(InstanceError::AlreadyRunning(lock_path.to_path_buf()))
            }
            Err(TryLockError::Error(source)) => Err(InstanceError::Io {
                path: lock_path.to_path_buf(),
                source,
            }),
        }
    }

    /// ロックファイルのパス。
    pub fn path(&self) -> &Path {
        &self.path
    }
}
