# 01. アーキテクチャ / 実行モデル / データ配置

> このファイルは `docs/spec/README.md`（索引）から参照される設計仕様書の一部。
> 対象: thundoku-shelf-app（Rust + GPUI デスクトップアプリ）。読み手は AI（別言語での再実装を想定）。
> 情報源はコード（アンカー付き）と既存 docs。**記載のない意図は推測しない**（推測は明記）。

## 1. リポジトリ構成

| パス | 役割 | 主な言語/依存 |
|---|---|---|
| `crates/opfspack` | 書籍ファイル形式 `.opfspack` の読み書き（自前形式） | Rust / aes-gcm, sha2 |
| `crates/core` | DB・取り込み・各ストア同期・Google/Drive・タグ・暗号（UI 非依存） | Rust / sqlx, tokio, reqwest 相当は ureq, lindera, pdfium/mupdf |
| `crates/app` | GPUI デスクトップ UI（画面・ビューアー・通知・アイコン） | Rust / gpui-kit, gpui-wry, image, resvg, usvg |

- ワークスペース: `resolver = "3"`, `edition = "2024"`, version 0.0.1, license MIT（`Cargo.toml:1-13`）
- リリースプロファイルは `debug = false`（`Cargo.toml:20-22`）
- `mupdf-sys` は dev プロファイルでも `opt-level = 3`（デバッグビルドだと 1 ページ数秒かかるため。`Cargo.toml:15-17`）

### 1.1 主要モジュール（アプリ層）

| ファイル | 役割 |
|---|---|
| `crates/app/src/main.rs` | 起動（単一インスタンス → Application → window → Workspace） |
| `crates/app/src/app_state.rs` | プロセス全体の状態（Global）。データディレクトリ・DB プール・通知のキュー |
| `crates/app/src/workspace.rs` | 常駐する画面の生成/切替、`App::on_action` の全体アクション、通知レイヤー |
| `crates/app/src/actions.rs` | 画面横断のアクション定義（ペイロード型） |
| `crates/app/src/icons.rs` | `AppIcon`（lucide 由来の自前 SVG を埋め込み）+ `AssetSource` |
| `crates/app/src/components/image_viewer/mod.rs` | ビューアー本体（単一/見開き/スクロール、付箋、キャッシュ） |
| `crates/app/src/views/*.rs` | 各画面（bookshelf / reader / history / notes / checklist / settings / about / *login） |

（網羅的な画面仕様は `docs/spec/04-ui.md`、ビューアーは `docs/spec/05-viewer-and-domain.md`）

### 1.2 主要モジュール（コア層）

`crates/core/src/` 直下に、ストア/サービスごとのモジュールと `db/`（テーブル別）、`import/`（取り込み）、`drive/`（Google Drive）を持つ。

| モジュール | 役割 |
|---|---|
| `db/` | SQLite アクセス（`schema.sql`, `desktop.sql`, `migrations/` + 各テーブルモジュール） |
| `import/` | 取り込みパイプライン（PDF / EPUB / 画像 ZIP → pack + DB 行） |
| `tbf/`, `booth.rs`, `fanza/`, `dlsite/` | 各ストアの同期（取得・解析・DB 反映） |
| `google.rs`, `owner.rs`, `drive/` | Google アカウント（OAuth/属性付け）、Google Drive 同期 |
| `secrets.rs` | 認証情報の保存（OS keyring / DB） |
| `tags.rs` | タグ生成（形態素解析 + Zenn 照合）とプロセス内キャッシュ |
| `single_instance.rs` | 多重起動の抑止（`instance.lock`） |

## 2. 起動シーケンス

1. `single_instance::InstanceGuard` を取得（既に起動中なら終了。`crates/app/src/main.rs:9,14-`）
2. `Application::run`（`crates/app/src/main.rs:98`）
3. ウィンドウオプションを構築して `cx.open_window`（`crates/app/src/main.rs:122`）
4. `Workspace::new` を生成（`crates/app/src/main.rs:154`）
5. `Root::new(workspace, window, cx)` をウィンドウのルートビューにする（`crates/app/src/main.rs:160`）
   - **通知レイヤーは `Root` ではなく `Workspace` が `Root::render_notification_layer(window, cx)` で描画する**（`crates/app/src/workspace.rs` の render。ライブラリの `Root` は通知レイヤーを含まないため）

### 2.1 起動時に生成・保持されるもの

- `Workspace` が**全画面（本棚 / 履歴 / 付箋 / チェックリスト / 設定 / 説明）を起動時に生成して保持**し、表示だけを切り替える（常駐。`crates/app/src/workspace.rs`）
- リーダー（`ReaderView`）だけは**本を開くたびに生成**され、閉じると破棄される
  - **注意（過去の不具合）**: 生成時に `App::on_action` へ登録するリスナーが強い `Entity` を持つと、閉じても解放されない。**弱参照（`WeakEntity`）にすること**（`crates/app/src/views/reader.rs` の `for_book`。回帰テスト `reader_is_dropped_when_closed`）

## 3. 実行モデル（プロセス / スレッド / 非同期）

| 項目 | 値 | 出典 |
|---|---|---|
| UI | 単一プロセス・単一ウィンドウ・GPUI のメインスレッド | `crates/app/src/main.rs` |
| 多重起動 | `instance.lock` により 1 プロセスのみ | `crates/core/src/single_instance.rs` |
| 非同期ランタイム | Tokio マルチスレッド、**worker_threads = 2**（`OnceLock` で 1 つだけ作る） | `crates/core/src/db/mod.rs` |
| 同期処理のブロック | `crate::db::block_on(...)` で同期 API から非同期 SQLx を回す | `crates/core/src/db/mod.rs` |
| 画像変換の並列度 | `min(コア数, 8)`、64 ページ単位のチャンク | `crates/core/src/import/mod.rs` |
| PDF レンダリング | **Windows = pdfium（`static Mutex` で直列化）** / 非 Windows = mupdf（8 スレッド、スレッドごとに `thread_local` コンテキスト） | `crates/core/src/import/pdf.rs` |
| 重い処理の分離 | 取り込み・ダウンロードは専用スレッド/バックグラウンド実行し、UI スレッドでは待たない | `crates/app/src/views/bookshelf.rs` |

**PDFium の直列化は必須**（理由は `docs/spec/07-decisions.md`）: PDFium はプロセスで 1 つのライブラリ状態（フォントキャッシュ等）を共有し、同時利用がスレッドセーフではない。実測でテキスト入り PDF を 8 スレッド同時描画すると `STATUS_ACCESS_VIOLATION` で落ちる。

## 4. データ配置（永続化）

`AppState`（Global）が保持する（`crates/app/src/app_state.rs:37,96,129-149`）。

| パス | 内容 | 備考 |
|---|---|---|
| `<data_dir>/thundoku-shelf.db` | SQLite（WAL） | `packs` / `downloads` / `thumbnails` への参照を持つ |
| `<data_dir>/packs/{book_id}.opfspack` | 書籍ファイル（自前形式） | 1 冊 1 ファイル |
| `<data_dir>/downloads/` | ダウンロード直後の一時ファイル | 取り込み後に pack へ変換 |
| `<data_dir>/thumbnails/` | 表紙サムネイル（PNG） | 命名規則は `docs/spec/03-import-and-pack.md` |
| `<data_dir>/instance.lock` | 単一インスタンス用 | |

- 既定の `data_dir` = `dirs::data_dir()/thundoku-shelf`（Windows: `%APPDATA%`、macOS: `~/Library/Application Support`）
- **保存先は設定で変更でき、変更時に既存ファイルを移動する**（`loaded_data_path()` / `init_with_data_dir`。`crates/app/src/app_state.rs:96,129-138`）
- テストは `std::env::temp_dir()/thundoku-shelf-test` を使う（`crates/app/src/app_state.rs:282-284`）

## 5. 依存ライブラリと選定理由（コード内コメント由来）

| 依存 | 用途 | コメントに残っている意図 |
|---|---|---|
| `gpui-kit`（git rev 固定 `84f57fdf...`） | UI コンポーネント（Button/Dialog/Popover/Notification/Radio/Icon 等） | crates.io 0.6.1 に Carousel が無く、上流 main を rev 固定で使用（`crates/app/Cargo.toml`） |
| `gpui-wry` / `lb-wry` | ログイン用 WebView | |
| `sqlx`（sqlite, runtime-tokio, migrate） | DB | |
| `image` / `webp` / `libwebp-sys`（sse41） | WebP エンコード/デコード | `sse41` は xwin クロスビルド対策 |
| `resvg` / `usvg` / `tiny-skia` | SVG（プレースホルダ表紙）のラスタライズ | `image` クレートは SVG を復号できないため |
| `pdfium-render`（thread_safe, pdfium_7881） | Windows の PDF レンダリング | `thread_safe` は `unsafe impl Send/Sync` を足すだけで**ロックはしない**（自前で Mutex が必要） |
| `mupdf`（非 Windows のみ） | PDF レンダリング | MSVC 前提のため xwin クロスビルドでは使えない |
| `lindera`（ipadic） | 日本語形態素解析（タグ自動生成） | |
| `keyring`（apple-native / windows-native） | 認証情報の保存 | 値の長さ上限 2560 UTF-16 文字 → 長いセッションは DB 側へ |
| `encoding_rs` / `zip` | Shift-JIS(CP932) のエントリ名デコード | `zip` は UTF-8 フラグ無しを CP437 と解釈するため自前デコード |
| `aes-gcm` / `sha2` / `md5` | `owner_sub` の暗号化 / ハッシュ | |

## 6. ビルドと検証（現行環境）

| 目的 | コマンド |
|---|---|
| テスト（アプリ） | `mise exec -- cargo test -p thundoku-shelf --lib` |
| テスト（コア） | `mise exec -- cargo test -p thundoku-core --lib` |
| 取り込み統合テスト | `mise exec -- cargo test -p thundoku-core --test import`（PDF 並列の回帰テストを含む） |
| Lint | `mise exec -- cargo clippy --workspace --all-targets -- -D warnings` |
| 整形 | `mise exec -- cargo fmt -p thundoku-shelf`（`-p thundoku-core` も） |
| リリースビルド | `mise exec -- cargo build --release` → `target/release/thundoku-shelf.exe` |

- 実行時ツールは **mise 管理**（`mise exec -- ...`）。システムの python/node は使わない
- リリース exe は**アプリ起動中は差し替えできない**（os error 5）。ビルド前にアプリを閉じる

## 7. 不明点

- `crates/opfspack` の言語非依存な仕様（バイトオーダ等）は `docs/spec/03-import-and-pack.md` を参照（本ファイルでは扱わない）
- macOS / Linux 固有のウィンドウ制御（タイトルバー等）の分岐はコード上に散在（`cfg(target_os)`）。本ファイルでは列挙しない
