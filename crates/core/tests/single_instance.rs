//! 単一インスタンスガードの検証。
//!
//! 実際にアプリを 2 回起動するのはテストでは困難なので、ロックファイルを直接
//! 使って「2 つ目の取得が失敗する」「保持者が消えたら再取得できる」ことを確認する。

use std::path::PathBuf;

use thundoku_core::single_instance::{InstanceError, InstanceGuard};

/// テストごとに独立したロックファイルのパス（親ディレクトリは無い状態から始める）。
fn lock_path(tag: &str) -> PathBuf {
    std::env::temp_dir()
        .join(format!(
            "thundoku-instance-test-{}-{tag}",
            std::process::id()
        ))
        .join("instance.lock")
}

fn fresh(tag: &str) -> PathBuf {
    let path = lock_path(tag);
    let _ = std::fs::remove_dir_all(path.parent().unwrap());
    path
}

#[test]
fn acquire_succeeds_when_no_instance_holds_the_lock() {
    let path = fresh("free");

    let guard = InstanceGuard::acquire(&path).expect("初回の取得は成功する");

    assert!(path.exists(), "ロックファイルが作成される");
    assert_eq!(guard.path(), path);
}

#[test]
fn second_acquire_fails_while_the_first_holds_the_lock() {
    let path = fresh("busy");
    let _first = InstanceGuard::acquire(&path).expect("初回の取得は成功する");

    match InstanceGuard::acquire(&path) {
        Err(InstanceError::AlreadyRunning(holder)) => assert_eq!(holder, path),
        Ok(_) => panic!("2 つ目の取得が成功した（2重起動を防げていない）"),
        Err(err) => panic!("想定外のエラー: {err}"),
    }
}

#[test]
fn acquire_succeeds_after_the_holder_is_dropped() {
    let path = fresh("released");
    {
        let _guard = InstanceGuard::acquire(&path).expect("初回の取得は成功する");
    }

    // 保持者の終了（guard の drop）でロックは OS が解放するので再起動できる
    assert!(
        InstanceGuard::acquire(&path).is_ok(),
        "保持者がいなくなれば再取得できる（再起動できる）"
    );
}

#[test]
fn locks_are_independent_per_path() {
    let first = InstanceGuard::acquire(&fresh("path-a")).expect("A の取得");
    let second = InstanceGuard::acquire(&fresh("path-b")).expect("B の取得");

    assert_ne!(first.path(), second.path());
}
