//! アプリのログ（`<データディレクトリ>/logs/thundoku.log`）。
//!
//! 出力先は**データディレクトリ配下**に固定する（issue #8）。レポート画面が同じ値を
//! 表示して「ログの格納先を開く」で開くので、`log_dir` / `log_file` を唯一の決定箇所にする。
//!
//! - GUI 起動では標準エラーが見えないので**ファイルにも出す**（ターミナル起動では
//!   標準エラーにも出す: その場で読めるように）。
//! - 追記（append）で開く: `File::create` は既存ログを切り詰めるため、2 個目の起動が
//!   起動中インスタンスのログを消してしまう。増え続けないよう、起動時に大きすぎたら捨てる。
//! - panic は `PANIC:` 行として同じファイルに追記する。

use std::fs::File;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

/// ログファイル名。
pub const LOG_FILE_NAME: &str = "thundoku.log";

/// 起動時にこれを超えていたらログを捨てる（増え続けないように）。
pub const MAX_LOG_BYTES: u64 = 4 * 1024 * 1024;

/// 既定のログフィルタ（`RUST_LOG` があればそちらが優先）。
pub const DEFAULT_FILTER: &str = "debug";

/// ログのディレクトリ（データディレクトリ配下）。
pub fn log_dir(data_dir: &Path) -> PathBuf {
    data_dir.join("logs")
}

/// ログファイルのパス。
pub fn log_file(data_dir: &Path) -> PathBuf {
    log_dir(data_dir).join(LOG_FILE_NAME)
}

/// ログを初期化する（ファイル + 標準エラー。開けなくても起動は止めない）。
pub fn init(data_dir: &Path) {
    let path = log_file(data_dir);
    let env = env_logger::Env::default().default_filter_or(DEFAULT_FILTER);
    let Some(mut builder) = builder(&path, env) else {
        // ファイルを開けない（権限・パス等）。標準エラーだけで続ける。
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or(DEFAULT_FILTER))
            .init();
        return;
    };
    set_panic_hook(&path);
    builder.init();
}

/// ログファイルを開く（ディレクトリ作成 → 大きすぎたら捨てる → 追記で開く）。
fn open_log_file(path: &Path) -> Option<File> {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    if let Ok(meta) = std::fs::metadata(path)
        && meta.len() > MAX_LOG_BYTES
    {
        let _ = File::create(path);
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .ok()
}

/// `env_logger` の Builder を作る（`env` はテストで固定するため引数）。
fn builder(path: &Path, env: env_logger::Env) -> Option<env_logger::Builder> {
    let file = open_log_file(path)?;
    let mut builder = env_logger::Builder::from_env(env);
    builder.target(env_logger::Target::Pipe(Box::new(TeeWriter {
        file,
        stderr: io::stderr(),
    })));
    Some(builder)
}

/// ログを**ファイルと標準エラーの両方**へ書く。
///
/// GUI 起動（Windows はサブシステム / macOS は .app）では標準エラーが見えないので
/// ファイルに残し、ターミナルから起動したときはその場でも読めるようにする。
struct TeeWriter {
    file: File,
    stderr: io::Stderr,
}

impl Write for TeeWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.file.write(buf)?;
        // 標準エラーに書けなくてもログ自体は失敗させない
        let _ = self.stderr.write_all(&buf[..written]);
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()?;
        let _ = self.stderr.flush();
        Ok(())
    }
}

/// ログに 1 行追記する（戻り値は「書けたか」）。
///
/// panic フックと、2 個目の起動を記録する側（`main.rs` の `note_second_launch`）で使う。
/// **親ディレクトリは作らない**: 起動時に [`init`] が作る（作れない状態では書けなかったと
/// 返すので、呼び出し側が標準エラーへ落とせる）。
pub fn append_line(path: &Path, text: &str) -> bool {
    if let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    {
        let _ = writeln!(file, "{text}");
        true
    } else {
        false
    }
}

/// panic を同じログファイルに追記するフックを付ける。
fn set_panic_hook(path: &Path) {
    let path = path.to_path_buf();
    std::panic::set_hook(Box::new(move |info| {
        let mut text = format!("PANIC: {info}");
        // スタックは `RUST_BACKTRACE=1` のときだけ残す（release は `strip = true` で
        // 関数名が出ないため、常用では情報が増えない）。
        if std::env::var_os("RUST_BACKTRACE").is_some() {
            text.push_str(&format!(
                "\nBACKTRACE:\n{}",
                std::backtrace::Backtrace::capture()
            ));
        }
        append_line(&path, &text);
    }));
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use super::*;

    /// テスト用のログファイル（プロセスとテスト名で分ける）。
    fn temp_log(tag: &str) -> PathBuf {
        std::env::temp_dir()
            .join(format!("thundoku-log-test-{}-{tag}", std::process::id()))
            .join(LOG_FILE_NAME)
    }

    fn cleanup(path: &Path) {
        if let Some(parent) = path.parent() {
            let _ = std::fs::remove_dir_all(parent);
        }
    }

    /// `RUST_LOG` に左右されないロガーを作る（テストは環境変数を見ない）。
    fn test_logger(path: &Path) -> env_logger::Logger {
        builder(path, env_logger::Env::new().default_filter_or("info"))
            .expect("ログファイルを開ける")
            .build()
    }

    /// 1 行書く（`format_args!` の一時値が生きるうちに `log` する）。
    fn log_line(logger: &env_logger::Logger, message: &str) {
        log::Log::log(
            logger,
            &log::Record::builder()
                .args(format_args!("{message}"))
                .level(log::Level::Info)
                .target("test")
                .build(),
        );
    }

    /// ログの置き場所はデータディレクトリ配下（レポート画面も同じ値を使う）。
    #[test]
    fn log_paths_live_under_the_data_dir() {
        let data_dir = Path::new("/tmp/thundoku-data");
        assert_eq!(log_dir(data_dir), data_dir.join("logs"));
        assert_eq!(
            log_file(data_dir),
            data_dir.join("logs").join(LOG_FILE_NAME),
            "ログファイルが logs/ の下に無い"
        );
    }

    /// 書いた行がファイルに残り、開き直しても消えない（追記）。
    #[test]
    fn records_are_appended_to_the_file() {
        let path = temp_log("append");
        cleanup(&path);

        let logger = test_logger(&path);
        log_line(&logger, "first line");
        log_line(&logger, "second line");
        let text = std::fs::read_to_string(&path).expect("ログが書かれている");
        assert!(text.contains("first line"), "1 行目が無い: {text}");
        assert!(text.contains("second line"), "2 行目が無い: {text}");

        // 起動し直し（別のハンドルで開き直す）でも前の行が残る
        drop(logger);
        let logger = test_logger(&path);
        log_line(&logger, "third line");
        let text = std::fs::read_to_string(&path).expect("ログが書かれている");
        assert!(text.contains("first line"), "開き直しで消えた: {text}");
        assert!(text.contains("third line"), "3 行目が無い: {text}");

        cleanup(&path);
    }

    /// 大きくなりすぎたログは起動時に捨てる（古い行が残らない）。
    #[test]
    fn oversized_log_is_discarded_on_open() {
        let path = temp_log("rotate");
        cleanup(&path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, vec![b'x'; MAX_LOG_BYTES as usize + 1]).unwrap();

        let logger = test_logger(&path);
        log_line(&logger, "after rotation");
        let text = std::fs::read_to_string(&path).expect("ログが書かれている");
        assert!(
            text.len() < 4096,
            "古いログが残っている（{} bytes）",
            text.len()
        );
        assert!(text.contains("after rotation"), "新しい行が無い: {text}");

        cleanup(&path);
    }

    /// 小さいログは捨てない（前の行が残る）。
    #[test]
    fn small_log_is_kept() {
        let path = temp_log("keep");
        cleanup(&path);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"old line\n").unwrap();

        let logger = test_logger(&path);
        log_line(&logger, "new line");
        let text = std::fs::read_to_string(&path).expect("ログが書かれている");
        assert!(text.contains("old line"), "小さいログを捨てている: {text}");
        assert!(text.contains("new line"), "新しい行が無い: {text}");

        cleanup(&path);
    }

    /// panic の行を同じファイルに追記する（開けなくても落ちない）。
    #[test]
    fn panic_lines_are_appended_to_the_file() {
        let path = temp_log("panic");
        cleanup(&path);
        // ロガーが起動時に作るディレクトリ（本番と同じ前提）
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        append_line(&path, "PANIC: boom");
        append_line(&path, "PANIC: boom2");
        let text = std::fs::read_to_string(&path).expect("ログが書かれている");
        assert!(text.contains("PANIC: boom"), "panic 行が無い: {text}");
        assert!(text.contains("PANIC: boom2"), "2 行目が無い: {text}");
        cleanup(&path);

        // 親ディレクトリが無いときは何もしない（panic フックで落ちない）
        let missing = std::env::temp_dir()
            .join("thundoku-log-test-no-such-dir")
            .join("nested")
            .join(LOG_FILE_NAME);
        append_line(&missing, "PANIC: no dir");
        assert!(
            !append_line(&missing, "PANIC: no dir"),
            "書けないのに成功を返している"
        );
        assert!(!missing.exists(), "親を作らずにファイルを作っている");
    }

    /// 標準エラーにも出す（ターミナル起動でその場でも読める）。
    #[test]
    fn log_goes_to_both_the_file_and_stderr() {
        let path = temp_log("tee");
        cleanup(&path);
        let mut writer = TeeWriter {
            file: open_log_file(&path).expect("ログファイルを開ける"),
            stderr: io::stderr(),
        };
        writer.write_all(b"tee line\n").unwrap();
        writer.flush().unwrap();
        let text = std::fs::read_to_string(&path).expect("ログが書かれている");
        assert_eq!(text, "tee line\n");
        cleanup(&path);
    }
}
