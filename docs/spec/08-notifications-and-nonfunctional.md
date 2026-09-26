# 08. 通知・非機能・ログ・ストア詳細

> `docs/spec/README.md`（索引）から参照される設計仕様書の一部。
> 本章は **通知の送出元 / 非機能（並列度・上限値・直列化）/ ログ / 各ストア同期の詳細手順**。
> 事実にはアンカー付き。断定できない事項は章末の「不明点 / 推測」に分離してある。
> 情報源: crates/core/src/{tbf,booth,fanza,dlsite,drive,**}.rs, crates/app/src/{app_state.rs,views/*.rs}

## 4. 通知

### 4.1 仕組み

- 種別 `ToastKind { Info, Success, Error }`（`Default = Info`）`crates/app/src/app_state.rs:333-340`。
- 送出 API `crates/app/src/app_state.rs:342-363`:
  - `set_toast_kind(cx, kind, msg)` — 種別をセットしてから `set_toast`。
  - `set_toast(cx, msg)` — メッセージ格納 + `toast_generation` インクリメント + `workspace` を `cx.defer` で notify。**種別は変更しない**（直前の種別を引き継ぐ。未設定時の初期値は Info）。
  - `clear_toast(cx)` — メッセージのみクリア `crates/app/src/app_state.rs:366-369`。
- ホスト（`Workspace::render`）`crates/app/src/workspace.rs:1152-1174`: `toast_generation` が前回と変わったときだけ 1 回 `window.push_notification((NotificationType, message), cx)` を呼び、その後 `clear_toast`。表示は `gpui_kit::component::Root::render_notification_layer(window, cx)` `crates/app/src/workspace.rs:1271-1274`。種別→`NotificationType` の対応は Info/Success/Error の 1:1 `crates/app/src/workspace.rs:1165-1169`。
- 表示位置・消滅時間: gpui-kit の Notification（ウィンドウ右上、**既定 5 秒で自動消滅**、マウスを乗せるとカウントダウン停止）`docs/features.md:459-465`, `crates/app/src/workspace.rs:1271`。実装値は依存先 `gpui-kit` rev `84f57fdf…` のリポジトリ外ファイル `~/.cargo/git/checkouts/gpui-kit-ad7eb35d851fbd28/84f57fdf/crates/component/src/notification.rs:841`（`timeout = autohide.then_some(Duration::from_secs(5))`）、遷移 400 ms / 退出 200 ms / 進行間隔 50 ms（同ファイル `:23-27`）。
- 旧実装（本棚下部の自前メッセージバー、下部パディング 28px）は廃止済み `docs/features.md:478-482`。

### 4.2 送出元の一覧（種別・メッセージ・位置）

| 種別 | メッセージ（リテラル / 生成） | 送出元 |
|---|---|---|
| Info | `書籍情報を展開中です` | `crates/app/src/views/bookshelf.rs:1689` |
| Info | `BOOTH にログインしてから同期してください` | `crates/app/src/views/bookshelf.rs:2258` |
| Info | `BOOTH サイトのデータを取得中です` | `crates/app/src/views/bookshelf.rs:2271` |
| Info | `FANZA にログインしてから同期してください` | `crates/app/src/views/bookshelf.rs:2484` |
| Info | `FANZA サイトのデータを取得中です` | `crates/app/src/views/bookshelf.rs:2497` |
| Info | `DLsite にログインしてから同期してください` | `crates/app/src/views/bookshelf.rs:2562` |
| Info | `DLsite サイトのデータを取得中です` | `crates/app/src/views/bookshelf.rs:2575` |
| Info | `ログインしてから同期してください`（技術書典） | `crates/app/src/views/bookshelf.rs:2640` |
| Info | `技術書典サイトのデータを取得中です` | `crates/app/src/views/bookshelf.rs:2649` |
| Info | ダウンロード中/キャンセル等の案内（成功時は Success、それ以外 Info） `crates/app/src/views/bookshelf.rs:3208-3220` | |
| Info | `ダウンロードを中止しました`（中止したダウンロードの worker 完了時） | `crates/app/src/views/bookshelf.rs:1094-1096` |
| Info | `表紙の取得後に再取得します` / `再取得を開始しました` | `crates/app/src/views/bookshelf.rs:3378`, `:3383` |
| Info | `FANZA にログインしてください` | `crates/app/src/views/bookshelf.rs:3765` |
| Info | `ダウンロード中です。完了したら開きます` | `crates/app/src/views/bookshelf.rs:3818` |
| Success | `書籍情報の展開が完了しました` | `crates/app/src/views/bookshelf.rs:1893` |
| Success | `BOOTH サイトから {count} 件取得しました` | `crates/app/src/views/bookshelf.rs:2455` |
| Success | `FANZA サイトから {count} 件取得しました` | `crates/app/src/views/bookshelf.rs:2532` |
| Success | `DLsite サイトから {count} 件取得しました` | `crates/app/src/views/bookshelf.rs:2610` |
| Success | `技術書典サイトから {count} 件取得しました` | `crates/app/src/views/bookshelf.rs:2687` |
| Success | ダウンロード/取り込み成功（`download_messages`） | `crates/app/src/views/bookshelf.rs:3211-3213` |
| Success | `本を削除しました` | `crates/app/src/views/bookshelf.rs:3361` |
| Success | `ジャンルを再取得しました（{added} 件追加）` | `crates/app/src/views/bookshelf.rs:3784` |
| Error | `表紙を取得できませんでした（{fail} 件）` | `crates/app/src/views/bookshelf.rs:1887` |
| Error | 同期失敗メッセージ（BOOTH/FANZA/DLsite/技術書典、`not logged in` / `session expired` 等） | `crates/app/src/views/bookshelf.rs:2462`, `:2539`, `:2617`, `:2694` |
| Error | 取り込み失敗（`download_messages` のエラー側） | `crates/app/src/views/bookshelf.rs:3207-3209` |
| Error | `いまはダウンロードを開始できません（同期中など）。少し待ってからもう一度` | `crates/app/src/views/bookshelf.rs:5347` |
| Error | `本を取り込むには Google にログインしてください（本はアカウントごとの鍵で暗号化されます）`（未ログイン。`LOGIN_REQUIRED_FOR_IMPORT`。明示操作では Google の認証モーダルも開き、自動ダウンロードでは通知だけ） | `crates/app/src/views/bookshelf.rs:1356`, `:3822`（`require_import_login`） |
| Info（種別未指定 = 直前の種別を継承） | `Drive バックアップを復元しました` / `バックアップの復元に失敗しました` | `crates/app/src/workspace.rs:613`, `:615` |
| Info（種別未指定） | `Google Drive と同期しました` | `crates/app/src/workspace.rs:1449` |
| Info（種別未指定） | `技術書典からログアウトしました（サイト側のセッションも破棄しました / は残っています）` | `crates/app/src/views/settings.rs:632-642` |
| Info（種別未指定） | `Google からログアウトしました` | `crates/app/src/views/settings.rs:673` |
| Info（種別未指定） | `BOOTH からログアウトしました` / `FANZA からログアウトしました` / `DLsite からログアウトしました` | `crates/app/src/views/settings.rs:707-711`, `:733`, `:751` |
| Info（種別未指定） | `同期完了（DL n / UL n / スキップ n / 競合 n）…`、および同期エラー時の `error` 表示 | `crates/app/src/views/settings.rs:868-882`, `:884-895` |
| Info（種別未指定） | `同期情報をクリアしました` | `crates/app/src/views/settings.rs:2506` |
| Info（種別未指定） | `ローカルデータをすべて削除しました` | `crates/app/src/views/settings.rs:1062` |

- 用途の要約: `Info` = 進捗・案内、`Success` = 完了、`Error` = 失敗・開始不可 `docs/features.md:466-475`。

---

## 2. 非機能（数値・並列度・直列化）

### 5.1 SQLite / DB 接続

| 項目 | 値 | アンカー |
|---|---|---|
| ドライバ | sqlx（`SqlitePool`） | `crates/core/src/db/mod.rs:20` |
| `foreign_keys` | `true` | `crates/core/src/db/mod.rs:53` |
| `journal_mode` | `WAL` | `crates/core/src/db/mod.rs:54` |
| `busy_timeout` | 5 秒 | `crates/core/src/db/mod.rs:55` |
| `create_if_missing` | `true` | `crates/core/src/db/mod.rs:56` |
| 本番プールの最大接続数 | 明示指定なし（`SqlitePool::connect_with(options)` のみ）`crates/core/src/db/mod.rs:57` → sqlx 既定値（10）[推測] | `crates/core/src/db/mod.rs:50-58` |
| テスト用プール | `max_connections(1)`（メモリ DB を共有するため） | `crates/core/src/db/mod.rs:465-471`, `crates/app/src/app_state.rs:273-275` |
| マイグレーション | `sqlx::migrate!("./migrations")` + `_sqlx_migrations` 管理。加えて `PRAGMA table_info` で列の有無を確認して `ALTER TABLE ... ADD COLUMN` する冪等マイグレーション群（`bookshelf_items.hidden_at` / `author`、`books.owner_sub`、`tbf_events.poll_sync_enabled`、`page_notes.is_active` 等） | `crates/core/src/db/mod.rs:192-300`, `:62-84` |
| 同期 API | 全リポジトリ関数が `block_on` でプロセス共有ランタイム上で sqlx を実行 | `crates/core/src/db/mod.rs:1-5`, `:42-46` |
| DB ファイルの場所 | `{data_dir}/thundoku-shelf.db`（既定 `dirs::data_dir()/thundoku-shelf/`） | `crates/app/src/app_state.rs:138-147` |
| 同期中の DB アクセス | メインの DB プールを同期タスクでもそのまま使う（WAL により並行可能） | `crates/app/src/views/settings.rs:820-822` |

### 5.2 Tokio ランタイム

- プロセス全体で 1 つの `tokio::runtime::Runtime` を `OnceLock` で保持。`Builder::new_multi_thread().worker_threads(2).enable_all()` `crates/core/src/db/mod.rs:26-40`。
- 公開 API は `block_on(fut)` のみ。GPUI のバックグラウンドタスクから同期 API として使う `crates/core/src/db/mod.rs:1-5`, `:42-44`。

### 5.3 HTTP 通信（`UreqTransport`）

| 項目 | 値 | アンカー |
|---|---|---|
| 実装 | `ureq`（rustls） | `crates/core/src/tbf/transport.rs:70-90` |
| connect タイムアウト | 5 秒 | `crates/core/src/tbf/transport.rs:84` |
| read タイムアウト | 15 秒 | `crates/core/src/tbf/transport.rs:85` |
| エージェント | リダイレクト追跡用（既定 5 回）と、`redirects(0)` の手動追跡用の 2 つ（接続プール維持のため） | `crates/core/src/tbf/transport.rs:72-90`, `:124-130` |
| リクエストごとのリダイレクト数 | `RequestSpec.redirects`（0 = 手動。3xx をそのまま返す） | `crates/core/src/tbf/transport.rs:8-13`, `:126-130` |
| ダウンロード進捗 | `send_download` は 64 KiB バッファで読み、`content-length` に対して**1% 刻み**でのみコールバック（チャンク毎だとチャネルが溢れて UI 側で停止するため） | `crates/core/src/tbf/transport.rs:155-190` |
| HTTP ステータス | 4xx/5xx も `Ok(ResponseSpec{status,...})` として返し、呼び出し側が判定 | `crates/core/src/tbf/transport.rs:143-152` |
| ボディ | 常にメモリ上へ全読み（ストリーミング保存はしない） | `crates/core/src/tbf/transport.rs:113-117` |

### 5.4 並列度

| 対象 | 値 / 方式 | アンカー |
|---|---|---|
| 取り込み（ページ変換: デコード + WebP 再圧縮） | `page_render_workers = min(available_parallelism, 8, page_count)` | `crates/core/src/import/mod.rs:119-127` |
| 取り込みのチャンク（圧縮バイト保持量） | `PAGE_RENDER_CHUNK = 64` ページ ≒ 30MB（1 ページ約 0.5MB の想定） | `crates/core/src/import/mod.rs:116-117` |
| お気に入りの自動ダウンロード同時数 | `MAX_AUTO_DOWNLOADS = 2`（1 件ごとに DL 本体 + 最大 8 並列デコード + pack 出力を抱え、無制限だと数 GB のピークになるため） | `crates/app/src/views/bookshelf.rs:3236-3237` |
| 手動ダウンロード 1 件 | `std::thread::spawn` した専用スレッドで実行し、結果を `mpsc::channel`（unbounded）で UI スレッドへ返す。進捗は UI 側で 16 ms 間隔のタイマーで取り込む | `crates/app/src/views/bookshelf.rs:2750-2790`, `:3222-3226` |
| 技術書典チェックリストのポーリング | `tbf` クライアントの `Mutex` で手動同期と直列化。周期は `checklist.poll.interval_min`（`n.max(1)`、既定 5 分） | `crates/app/src/workspace.rs:245-250`, `:265-266` |
| Drive 同期 | 単一タスクで逐次（ダウンロード方向 → アップロード方向 → DB バックアップ）。並列化なし | `crates/core/src/drive/sync.rs:205-390` |
| Google ログイン | **システムブラウザ**で認可 + 別スレッドで `finish_authorize`（ブロッキング accept ループ）。WebView を作らない | `crates/app/src/views/google_login.rs` |

### 5.5 メモリ上限の定数

| 定数 | 値 | 用途 | アンカー |
|---|---|---|---|
| `SCROLL_CACHE_BUDGET_BYTES` | 384 MiB（384 × 1024 × 1024 byte） | スクロールモードで前後ページを保持する総バイト予算（超過分は `window.drop_image` で解放） | `crates/app/src/components/image_viewer/mod.rs:438-441` |
| `MAX_PAGE_THUMBS` | 120 枚（1 枚 ≒ 0.2MB として約 24MB） | ページ一覧のサムネイル保持数（古い順に破棄） | `crates/app/src/components/image_viewer/mod.rs:49-51` |
| `PAGE_THUMB_WIDTH` | 200.0 px | ページ一覧サムネイルの幅 | `crates/app/src/components/image_viewer/mod.rs:47-48` |
| `MAX_THUMB_CACHE` | 64 | チェックリストのデコード済みサムネイル LRU 上限 | `crates/app/src/views/checklist.rs:29-30`, `:161-165` |
| `MAX_THUMB_CACHE` | 64 | 付箋（notes）のページ画像 LRU 上限 | `crates/app/src/views/notes.rs:48-49`, `:66-69` |
| `MAX_NESTED_DEPTH` | 1 | 入れ子 ZIP の展開深さ上限 | `crates/core/src/import/mod.rs:184-185` |
| `MAX_NESTED_BYTES` | 512 MiB（512 × 1024 × 1024 byte） | 入れ子 ZIP から合流させるエントリの非圧縮合計サイズ上限（解凍爆弾対策） | `crates/core/src/import/mod.rs:187-191` |
| `MAX_NESTED_ENTRIES` | 2000 | 入れ子 ZIP から合流させるエントリ数上限 | `crates/core/src/import/mod.rs:193-197` |
| `MAX_LOG_BYTES` | 4 MiB（4 × 1024 × 1024 byte） | 起動時にこれを超えていたらログファイルを捨てる | `crates/app/src/main.rs:75-79` |

- 実測値（コードコメント / docs）:
  - 取り込み 1 ページあたり **0.8 秒（release ビルド）**、逐次だと 3,000 ページ級で **40 分超** `crates/core/src/import/mod.rs:119-123`。
  - メモリ使用量 **6,000MB** の原因 3 つ（リーク / スクロールモードの全ページ保持: 1 ページ平均 **16MiB** × 192 ページ = **5,631MiB** / 自動ダウンロード無制限）と対策（`WeakEntity` 化・**384MiB 予算**・同時 2 件）`docs/features.md:604-614`。スクロール予算のコメントにも「1 ページ 47MiB の本なら前後 8 ページ程度、16MiB の本なら前後 24 ページ程度」`crates/app/src/components/image_viewer/mod.rs:436-439`。
  - 画像の小さい元データは Lanczos3 で **1000px 幅**まで拡大してから保存、ページ用 WebP 品質 88、サムネイル WebP 品質 80 `crates/core/src/import/mod.rs:99-113`。
  - 技術書典チェックリストのポーリング既定 **5 分・下限 1 分** `docs/features.md:569`, `crates/app/src/workspace.rs:245-250`。

### 5.6 スレッド安全性のための直列化

- **PDFium はスレッドセーフでない**ため、PDFium を使う区間（初期化を含む）を `static PDFIUM_LOCK: std::sync::Mutex<()>` で直列化する（全プラットフォーム共通）`crates/core/src/import/pdf.rs:80`, `:91-95`。
  - 実測（Windows / pdfium-render 0.9.3）: テキスト入り 1 ページ PDF を 8 スレッドで同時レンダリングすると `STATUS_ACCESS_VIOLATION (0xc0000005)` でプロセスが落ちる。スレッドごとに `load_pdf_from_byte_slice` し直しても再現（ドキュメントを分けてもダメ）`crates/core/src/import/pdf.rs:14-21`。
  - `pdfium-render` の `thread_safe` feature は `unsafe impl Send/Sync` を足すだけでロックしないため呼び出し側の責任 `crates/core/src/import/pdf.rs:14-16`。
  - 1 冊の中の描画は元々逐次なので単冊の速度は変わらず、複数冊を並行取り込みするときだけ待ち合う `crates/core/src/import/pdf.rs:20-21`。
  - pdfium のグローバル `BINDINGS` はプロセスに 1 つだけなので、`LazyLock` で一度だけ初期化して再利用する（2 回目の `Pdfium::new` は panic）`crates/core/src/import/pdf.rs:57-79`。
  - PDFium は**実行時ロード**（静的リンクしない）で、探索順は ① ビルド時の `CARGO_MANIFEST_DIR`（= `crates/core`） ② 実行ファイルと同じディレクトリ ③ macOS は実行ファイルの `../Frameworks`（`.app` の `Contents/Frameworks`）。**CWD は探索しない**（DLL 配置攻撃対策）。prebuilt の static ライブラリは macOS で `FPDF_FORMFILL` を欠きリンクできないため `crates/core/src/import/pdf.rs`。
- その他の直列化: `AppState` の各クライアントは `parking_lot::Mutex`（`tbf`, `google`, 各セッション、`google_profile` 等）`crates/app/src/app_state.rs:39-60`。Drive 同期・TBF 同期は同じ Mutex を通るため相互排他。
- UI スレッドの再入回避: トースト通知・認証モーダル開閉は `cx.defer` / `AtomicBool` フラグ + 監視タスク経由（`cx.notify()` の RefCell 再入回避）`crates/app/src/app_state.rs:358-362`, `crates/app/src/workspace.rs:160-220`。

### 5.7 単一インスタンス

- 仕組み: ロックファイルを OS のファイルロックで排他。Unix は `flock(2)`、Windows は `LockFileEx` で、どちらも `std::fs::File::try_lock` が吸収するためプラットフォーム分岐なし `crates/core/src/single_instance.rs:1-8`。
- 実装 `InstanceGuard::acquire(lock_path)` `crates/core/src/single_instance.rs:52-95`:
  1. 親ディレクトリを `create_dir_all`（無ければ）。
  2. `File::options().create(true).read(true).write(true).truncate(false)` で開く（**truncate しない**: 起動中インスタンスの状態を壊さないため）。
  3. `try_lock()` 成功 → ガード返却。`TryLockError::WouldBlock` → `InstanceError::AlreadyRunning(path)`。その他 I/O エラー → `InstanceError::Io`。
- `InstanceGuard` は `drop`（= プロセス終了）でロック解放。クラッシュ・強制終了でも OS が解放するため「古いロックで起動できない」ことはない（ロックファイル自体は残る）`crates/core/src/single_instance.rs:9-12`, `:32-38`。
- ロックファイルの場所: `dirs::config_dir()/thundoku-shelf/instance.lock`。データ保存先（変更可能）ではなく config ディレクトリに置くことで、保存先を変えても「同時に動くアプリは 1 つ」を保つ `crates/app/src/main.rs:172-178`。
- `main` の挙動 `crates/app/src/main.rs:25-46`:
  - **ログ初期化より先に**ガード取得（ログ初期化が `File::create` で切り詰めるため、2 個目が起動中インスタンスのログを壊すのを防ぐ）。ガードは `main` の間ずっと保持。
  - `AlreadyRunning` → `note_second_launch(&lock_path)` でログに追記し、**何も表示せず終了**。
  - ロックファイルが開けない場合は起動を止めず `eprintln!` して続行。
  - config ディレクトリ解決不能時はガード無効で続行。
- 単一インスタンスにする理由（ドキュメント）: 多重起動すると DB・パック・ダウンロード先を共有して二重取り込みが起き、Google OAuth のループバックポート（`127.0.0.1:38387`）の取り合いが起きるため `docs/features.md:78-88`。

### 5.8 終了時の同期（`exit_checked` / `exit_uploading`）

- `AppState.exit_checked: AtomicBool`（終了確認を表示済みか、キャンセルで false に戻す）と `exit_uploading: AtomicBool`（「アップロードして終了」実行中。本棚のダウンロード・ビューアー起動をブロックするために共有）`crates/app/src/app_state.rs:64-71`。
- ウィンドウを閉じる時は保存済みウィンドウ状態（`window.bounds`）を DB に保存し、閉じてよいかの判断は `Workspace::handle_window_close_request` に集約する: アップロード中は常に閉じない（`false`）、未確認なら終了確認ダイアログを出して `false`、確認済みなら `true` `crates/app/src/workspace.rs:737-760`。
- アップロードを始めたら Info の通知（「バックアップをアップロード中です…」）を出す。確認ダイアログは押した時点で閉じるため、これが進行中の唯一の手がかりになる。通知の自動消滅は `AppState.toast_autohide` で切り替えられ、これだけは `false`（完了＝アプリ終了まで出し続ける。他は従来どおり 5 秒）`crates/app/src/workspace.rs:730-745,845-860`; `crates/app/src/app_state.rs:86-88,551-570`。
- アップロード中はメニューの「終了」も無効化する（`app_menus(false)`。macOS のメニューバー。完了時に戻す）`crates/app/src/workspace.rs:62-80,845-860`。
- 「アップロードして終了」は `drive.sync.folder_id` が無ければ「Drive 同期が未設定です」、Google 未ログインなら「Google にログインしてください」で失敗し、成功時は `db_path` 付きで同期してから `cx.quit()` `crates/app/src/workspace.rs:845-905`。

---

## 3. ログ / 計測

### 6.1 出力先と設定

| 項目 | 値 | アンカー |
|---|---|---|
| ロガー | `env_logger` + `log`（`RUST_LOG` で制御可） | `crates/app/src/main.rs:86-92`, `docs/features.md:74` |
| Windows の出力先 | `%TEMP%\thundoku-shelf\thundoku.log`（stderr は非表示のためファイルにも出す） | `crates/app/src/main.rs:48-60` |
| 既定フィルタ（Windows） | `default_filter_or("debug")` | `crates/app/src/main.rs:86-88` |
| ログのローテーション | 起動時に 4 MiB（`MAX_LOG_BYTES = 4 * 1024 * 1024`）を超えていたら `File::create` で捨てる | `crates/app/src/main.rs:75-80` |
| 書き込みモード | 追記（`append(true)`）。`File::create` だと 2 個目の起動が起動中インスタンスのログを消し、非 append ハンドルは自分のオフセットに書いて後続行を上書きするため | `crates/app/src/main.rs:81-85` |
| panic 時 | `std::panic::set_hook` で `PANIC: {info}` を同じログに追記 | `crates/app/src/main.rs:62-70` |
| 非 Windows | `env_logger::init()`（標準エラー） | `crates/app/src/main.rs:92` |
| 2 個目の起動 | `note_second_launch` がロックパス付きで追記（ウィンドウは開かない） | `crates/app/src/main.rs:180-190`, `:33` |

### 6.2 記録されるログの種類（用途別）

| 用途 | 例（リテラル） | アンカー |
|---|---|---|
| Drive 同期の進行 | `drive sync: list_files start` / `list_files -> {n} files` / `download direction start` / `download direction done, upload direction start` / `upload direction done, database backup start` | `crates/core/src/drive/sync.rs:206`, `:208`, `:241`, `:297`, `:361` |
| Drive の DB バックアップ判断 | `uploading database backup ({n} bytes)` / `database unchanged, touching modified time` | `crates/core/src/drive/sync.rs:376`, `:387` |
| 手動 Drive 同期の要約 | `sync_drive_now: start` / `folder_id={id:?}` / `creating folder` / `token ok, drive client ready` / `running sync engine` / `done dl={} ul={} skip={} conflicts={} db_backup={}` | `crates/app/src/views/settings.rs:783-830` |
| Drive 復元判定 | `startup backup check: drive backup differs from local (md5={:?})` | `crates/app/src/workspace.rs:564-567` |
| Google OAuth | `google authorize url: {url}` / `google callback received, exchanging code` / `token exchange: client_id=…, client_secret=configured\|MISSING` / `token exchange succeeded (status {s})` / `token exchange failed: status {s}, body: {body}` / `google callback error: {e}`(error) | `crates/core/src/google.rs:388`, `:436`, `:459-479`, `:191-192` |
| Google ログイン UI | `google login: ブラウザで認可を開始 {url（クエリ無し）}` / `google login: ブラウザを開けません: {e}`（warn） | `crates/app/src/views/google_login.rs` |
| Google プロフィール復元 | `google profile restored from stored tokens` | `crates/app/src/views/settings.rs:593` |
| セッション復元（BOOTH） | `booth session: 起動時復元 = ログイン済み（cookies={n}）` / `未ログイン` / `{store} session: 保存値を復号できないため破棄します（再ログインが必要）`（warn） | `crates/app/src/app_state.rs`, `crates/core/src/session_store.rs` |
| セッション保存（BOOTH） | `booth session: DB 保存成功（暗号化）` / `{store} session: DB 保存失敗: {e}`（error） / `{store} session: 暗号鍵が無いため保存しません（次回起動では再ログインが必要）`（warn） | `crates/app/src/app_state.rs` |
| セッション保存（技術書典） | `tbf session saved -> tbf_logged_in = true` | `crates/app/src/app_state.rs:383` |
| セッション復元（技術書典） | `tbf session: 起動時復元 = ログイン済み` / `未ログイン` / `{store} session: keyring の旧保存値を vault へ移行しました` / `{store} session: 旧 keyring 値を削除できません（次回起動で再試行）`（warn） / `{store} session: 印の記録にも失敗しました`（error） | `crates/app/src/app_state.rs`, `crates/core/src/session_store.rs` |
| ログアウト | `logout_tbf: server logout ok\|failed` / `logout_tbf: local cleared ({elapsed})` / `logout_tbf: WebView の保存データを消去できません: {e}`（error） / `webview: 保存データの消去を要求 ok\|failed` / `logout_google: client logout ({elapsed})` / `logout_booth: server logout …` / `logout_dlsite: cleared ({elapsed})` | `crates/app/src/views/settings.rs`, `crates/app/src/views/mod.rs`; `docs/logout.md` |
| ダウンロード/取り込み | `download_item: スレッド完了、UI 反映開始` / `download_item: 失敗しました: {msg}`(warn) / `download_item: UI 反映完了（reload 含む）（{elapsed}）` | `crates/app/src/views/bookshelf.rs:3218-3234` |
| 本棚リロード/同期開始 | `reload: 完了（{elapsed}）` / `sync_booth: 開始` / `sync_fanza: 開始` / `sync_dlsite: 開始` / `sync_tbf: 開始` | `crates/app/src/views/bookshelf.rs:1653`, `:2269`, `:2495`, `:2573`, `:2647` |
| チェックリストポーリング失敗 | `checklist poll failed for {slug}: {message}`（error） | `crates/app/src/workspace.rs:280` |
| トースト送出 | `set_toast: workspace host={bool}` | `crates/app/src/app_state.rs:355` |

### 6.3 計測（メトリクス）

- 専用のメトリクス／トレーシング機構は無い（`metrics` / `tracing` 系の依存なし、`log` + `env_logger` のみ `crates/app/Cargo.toml`, `Cargo.toml`）。
- 経過時間は各所で `std::time::Instant` を取って `log::info!` に `{:?}` で出しているだけ（例: `crates/app/src/views/settings.rs:653-658`, `:769-830`, `crates/app/src/views/bookshelf.rs:3218-3234`）。
- 永続化される計測値（設定画面表示用）: `drive.last_sync_at`（UTC `%Y-%m-%d %H:%M:%S`）、`drive.file_count`（Drive フォルダ内の全ファイル数）、`drive.total_bytes`（同 Σ size）`crates/app/src/views/settings.rs:845-858`、表示は `:1645-1663`。

---

## 不明点

- 本番 DB プールの最大接続数: `db::connect` は `SqlitePool::connect_with(options)` のみで pool options を指定しておらず、リポジトリ内に実効値の記述が無い `crates/core/src/db/mod.rs:50-58`。
- `docs/account-switch.md:78` は Drive フォルダを `drive.sync.folder_id.{sub}`（アカウントごとに別フォルダ）と記載するが、実装は単一キー `drive.sync.folder_id` のみ `crates/app/src/views/settings.rs:787`, `crates/app/src/workspace.rs:518`。doc と実装のどちらが正か（実装未追随か文書が先行案か）は不明。
- `drive.sync.enabled` を false にしたとき、どの自動同期（起動時の復元確認・チェックリスト連動・終了時アップロード）が止まるかの明記がコード・docs に無い `crates/app/src/views/settings.rs:754-766`。
- `credentials::USER_BOOTH`（`crates/core/src/secrets.rs:13`）は定義のみで参照が無い（BOOTH は DB 保存）。使用予定があったかは不明。
- Google ログインは WebView を使わない（システムブラウザ）ため、`webview.hide()` 相当の後始末は不要。残るのはブラウザ側の残存ログインのみ（アプリからは制御しない）。
- Drive 同期の多重実行排他: `sync_drive_now` は UI の `busy` フラグでしかガードしておらず `crates/app/src/views/settings.rs:770`、バックグラウンドのポーラーや終了時同期と衝突した場合の挙動は不明。
- `receive_callback` の 300 秒タイムアウト後の見え方は、`GoogleError::Auth` が `google_login_error` に入り `google_login_done` が立つ（Workspace が認証モーダルを閉じる）ところまで。ブラウザ側のタブはアプリから閉じられない。
- **[DLsite]** 購入履歴 1 ページあたりの実件数（コードに定数なし。`MAX_PAGES_PER_STORE` のコメントは「1 ページあたりの行数境界（実測は未確認）」だが実装は最大ページ数）。`crates/core/src/dlsite/client.rs:16-17`
- **[DLsite]** `age_category` の実測値の全パターン（コメントは「2 以上（R18 想定）」で、2 以上を実測確認した記述はない）。`crates/core/src/dlsite/mod.rs:78-80`
- **[DLsite]** `login.dlsite.com` 側に年齢確認・2 段階認証があるか（コードに分岐・文言が存在しないため判断不能）。`crates/app/src/views/dlsite_login.rs:112-171`
- **[DLsite]** セッション Cookie の有効期限、および `jwt` を永続化すべきか（実装は「その 1 リクエストのみ」）。`crates/core/src/dlsite/client.rs:336-361`
- **[DLsite]** 同期の途中キャンセル手段（実装なし）。
- **[DLsite]** `books` フロア（`STORES` に含まれる）に `RJ` 以外の ID 接頭辞の作品が並ぶ場合の挙動（コードは `RJ\d+` 以外の行を破棄するため、行があっても採用されない）。`crates/core/src/dlsite/client.rs:405`
- **[DLsite]** `product/info/ajax` を maniax 固定で叩いた場合に他フロア（`books` / `home` / `ai`）の ID が返る範囲（コメントは「store を跨いでも解決される」と主張するがテストは maniax/ai の ID のみ）。`crates/core/src/dlsite/client.rs:263-265`, `:196`
- **[DLsite]** 401/403 以外でセッション切れを表す実レスポンス（302 で `regist/user` へ飛ぶケースがコメントにあるが、実装は 302 を `Http(302)` として失敗させる）。`crates/app/src/views/dlsite_login.rs:124-129`, `crates/core/src/dlsite/client.rs:376-383`
- **[DLsite]** docs（`features.md` / `import-patterns.md` / `database.md` / `account-switch.md`）に **DLsite の同期手順・ログインフロー・Cookie 名の記述は存在しない**（`DLsite`/`dlsite`/`__DLsite_SID` で grep 済み。`docs/features.md:209` の release_date、`docs/features.md:206` の日付形式、`docs/features.md:570` の実装済み項目のみ）。
- **[DLsite]** 同期完了後に `synced_at` を用いた鮮度表示・差分スキップを将来入れる予定があるか（コード上は未使用の値を書くだけ）。
- **[FANZA]** **セッション Cookie の実名**（`session_id` 等）。コードは全 Cookie を無差別に保存し特定名に依存しないため、コードからは列挙不能。`login_id` はテスト用モック値（`crates/core/src/fanza/client.rs:479-484`、`crates/core/src/fanza/sync.rs:194`）。
- **[FANZA]** **一覧 API のレート制限・推奨間隔**。コードに存在せず、docs にも記述なし。
- **[FANZA]** **一覧 API の `total` と `hasNext` が矛盾した場合の実挙動**（コードは `hasNext` / `total` / 2000 件上限のいずれかで止まるのみ。`crates/core/src/fanza/client.rs:207-216`）。
- **[FANZA]** **`data.items` の日付グループ key のタイムゾーン**（例 `2026年09月03日` が JST かどうかはコード上判断不能。`crates/core/src/fanza/client.rs:200`, `:203`）。
- **[FANZA]** **セッション Cookie の有効期限**。`FanzaSession` は期限・発行時刻を保持しない（`crates/core/src/fanza/client.rs:48-51`）。
- **[FANZA]** **`FanzaPurchase.is_streaming` / `is_unavailable` の用途**。パース後、`crates/core/src/fanza/` 外に参照が無く、同期のフィルタにも未使用（`crates/core/src/fanza/client.rs:84-85`, `:391-414` のみ）。
- **[FANZA]** **`FanzaError::DrmProtected` の生成箇所**。宣言のみで構築されていない（`crates/core/src/fanza/client.rs:30`。リポジトリ全体で `DrmProtected` の出現はこの 1 行のみ）。
- **[FANZA]** **詳細 API が返す参照メタ（jar 等）の扱い**。コメントは「`details` / 商品ページで取得」とするが、実装が読むのは `contentId` / `title` / `genre` / `makerName` / `makerId` / `deliveryDate` / `downloadLinks.1` / `drm` / `isSdrm` / `fileSize` のみ（`crates/core/src/fanza/client.rs:250-263`）。
- **[FANZA]** **`FanzaDetail` の `delivery_date` / `file_size` / `meta()` の利用箇所**。`crates/core/src/fanza/client.rs` のテスト以外に参照が無く、UI では未使用（`grep` で 0 件）。
- **[FANZA]** **`clear_fanza_session` の呼び出し元**。定義（`crates/app/src/app_state.rs:424-430`）のみで呼び出しなし。実際のログアウトは `settings.rs:720-733` が同等処理を内包する。
- **[FANZA]** **FANZA の年齢確認ページの判定語**（`"age_check"` を含む URL のみを除外。それ以外の年齢確認 URL 形式は不明。`crates/app/src/views/fanza_login.rs:110`）。
- **[FANZA]** **`full_size_thumb` を通した後の URL が常に原寸を返すか**。コードコメントは実測 2 例のみ（`crates/core/src/fanza/sync.rs:26-30`）。
- **[FANZA]** **`sites.display_order = 2` の意味付け**（技術書典 / DLsite との相対順は DDL から読み取れるが、UI での並び制御は非ゴールのため未確認）。`crates/core/src/db/mod.rs:383-386`。
- **[BOOTH]** `library()` の「1 ページ 10 件」はコメント由来でコード定数が無い。実際のページ件数が 10 件である保証は HTML 依存（`crates/core/src/booth.rs:199`）。
- **[BOOTH]** `orders()` の終了閾値 12 の根拠（1 ページ 12 件という前提）はコードコメントにも書かれておらず**不明**。定数名も無い（`crates/core/src/booth.rs:287`）。
- **[BOOTH]** BOOTH 側の API レート制限値（429 の閾値、推奨間隔）はコード・ドキュメントに記述が無く**不明**。実装は無待機・無リトライ。
- **[BOOTH]** `with_incognito(true)` が Windows WebView2 で具体的にどのプロファイル（`InPrivate` 等）にマップされるかは `lb-wry` / `gpui-wry` の内部実装依存で、このリポジトリからは**不明**。
- **[BOOTH]** `cookies_for_url` が HttpOnly Cookie を返すかは wry 実装依存で**不明**（`crates/app/src/views/booth_login.rs:128-138` は戻り値をそのまま使う）。
- **[BOOTH]** `item_detail`（`Accept: application/json`）が Cloudflare 等で失敗する頻度、およびその場合の実挙動（表紙が `None` のまま残る）については運用ログ依存で、コードからは**不明**。
- **[BOOTH]** 「同期中にアプリを終了した場合」の状態（スレッド中断の有無）は `JoinHandle` を保持していないため挙動が読めず、**不明**（`crates/app/src/views/bookshelf.rs:2278`）。
- **[BOOTH]** 同期で書き込む `synced_at` の固定値 `"2026-08-25 00:00:00"` の由来（リリース日等）はコード中に説明が無く**不明**。
- **[BOOTH]** `bookshelf::upsert` の `author = excluded.author`（`:70`）と `author = CASE ...`（`:87-90`）の二重代入について、SQLite の重複 SET の解決順（後勝ち）を前提としているが、これを明記したコメント・テストは**無い**（`crates/core/src/db/bookshelf.rs:67-91`）。
- **[BOOTH]** `USER_BOOTH` 定数が未使用のまま残っている理由は**不明**（`crates/core/src/secrets.rs:12`）。
- **[技術書典]** セッション Cookie の**実名**（`session` か否か）はコード上特定名に依存しておらず不明。`crates/core/src/tbf/mod.rs:203-207` のコメントも「E2E で確認する」としており、`XSRF-TOKEN` 以外なら何でも可という実装（テストの `session` はモック値）。`docs/features.md:432-433` にも Cookie 名の記載なし。
- **[技術書典]** セッションの有効期限（サーバー側 TTL）は不明。クライアントに期限管理が無いことのみ確認済み（`crates/core/src/tbf/mod.rs:59-61`, `:224-230`）。
- **[技術書典]** 技術書典のレート制限仕様（429 の閾値、Retry-After の有無）は不明。コード側に対応実装が無いため、実際の挙動は未確認（`crates/core/src/tbf/transport.rs:127-219`）。
- **[技術書典]** `TbfClient::bootstrap()` / `login(email, password)` はアプリ本体から呼ばれていない（`crates/core/tests/tbf.rs` のみ）。将来の利用予定か、死線コードかは不明（`crates/core/src/tbf/mod.rs:184`, `:208`）。
- **[技術書典]** `checked_items.sample_fetch_attempted_at` を「試し読み取得を試みた」用途で参照している箇所は、TBF 同期経路では確認できない（同期で常に `None` に上書きする: `crates/core/src/tbf/sync.rs:194`）。試し読み UI（`fetch_sample`）は同カラムを更新していない（`crates/app/src/views/checklist.rs:516-570`）。
- **[技術書典]** canonical のイベント名（`技術書典20` 等）が将来 `tbf21` 以降でどのように命名されるかは不明（探索時はサーバー `data.event.name` を使用: `crates/core/src/tbf/mod.rs:394-397`）。
- **[技術書典]** `appVersion=20260417a-web` / `20260424a-web` の更新タイミング（アプリ側で固定文字列）は不明。サーバーが旧 appVersion を拒否するかは未確認（`crates/core/src/tbf/mod.rs:296`, `:417`）。
- **[技術書典]** **指定ファイル `crates/core/src/tbf/client.rs` は存在しない**（実体は `mod.rs`）。指定の 28.8KB というサイズのファイルはリポジトリ内に該当なし（`mod.rs` は 1223 行）。おそらく他ストア（fanza/dlsite）の `client.rs` との混同（`crates/core/src/fanza/client.rs`, `crates/core/src/dlsite/client.rs`）。

## 推測

- 本番プールの `max_connections` は sqlx の既定値（一般に 10）で動作している [根拠: `crates/core/src/db/mod.rs:57` が `connect_with` のみ、テスト側だけが `max_connections(1)` を明示 `:465-471`]（推測）。
- `set_toast` は種別を変更しないため、`crates/app/src/views/settings.rs` と `crates/app/src/workspace.rs` のトースト（ログアウト・同期完了・復元結果など）は直前に設定された種別（未設定なら初期値 Info）で表示される [根拠: `crates/app/src/app_state.rs:342-363`]（推測）。
- `drive.sync.enabled` は自動同期系の有効フラグであり、手動「今すぐ同期」は無効化しても実行できる [根拠: 手動ボタンの disabled 条件が `!google_logged_in || busy` のみ `crates/app/src/views/settings.rs:1862`]（推測）。
- 単一インスタンス化の主目的は DB・パック・ダウンロード先の二重利用防止と OAuth ループバックポート `127.0.0.1:38387` の取り合い回避 [根拠: `docs/features.md:78-88`, `crates/core/src/google.rs:17`]（推測）。
- Drive のダウンロード方向は `HashMap` 反復順に依存するため、同一実行内では同名 pack の先勝ち（`or_insert`）だが、実行ごとの処理順は不定 [根拠: `crates/core/src/drive/sync.rs:225-231`]（推測）。
- 復号不能な `owner_sub` を持つ行は非表示のまま残り続ける（クリーンアップ処理が存在しない） [根拠: `crates/core/src/db/books.rs:293-311` に該当行を削除する処理がなく、`crates/core/src/drive/sync.rs` にも削除処理がない]（推測）。
- **[DLsite]** `author` の DO UPDATE 二重代入（`crates/core/src/db/bookshelf.rs:70`, `:87-90`）は SQLite の重複 SET 許容により **後勝ち**（空文字のとき既存値を残す CASE 側が有効）になる（根拠: 同ファイルのコメントが「値が無いときは既存値を保持する」と明記。SQLite の評価順を実行確認はしていない）。
- **[DLsite]** `MAX_PAGES_PER_STORE` のコメント（「1 ページあたりの行数境界」）は古い設計の名残で、実装はページ上限（根拠: `crates/core/src/dlsite/client.rs:201` の比較が `page` に対する `>` 判定）。
- **[DLsite]** `books` フロアを `STORES` に入れているが `RJ\d+` 正規表現しか許容しないため、`BJ` 等の接頭辞の作品は取り込まれない（根拠: `crates/core/src/dlsite/client.rs:405` の必須キャプチャ。DLsite の書店フロアの ID 形式は外部知識で、リポジトリ内に裏付けはない）。
- **[DLsite]** `synced_at` は「最終同期時刻」用途で書かれており、同期間引き（差分判定）を将来入れる前提の布石（根拠: 毎回必ず上書きされ、他に DLsite の最終同期時刻を記録する箇所が無い）。
- **[DLsite]** ログイン完了判定に `uid_jp`/`uhashjp` を要求するのは、`__DLsite_SID` が未ログインでも発行されるゲスト ID であるため（根拠: `crates/app/src/views/dlsite_login.rs:149-154` のコメント）。
- **[DLsite]** `DlsiteError::SessionExpired` は未使用のため、実際のセッション切れ通知は `Unauthorized(401/403)`（文言「不正アクセス（401）」）と app 側の `"セッション"` 文字列（`"DLsite セッションがありません"`）に依存している（根拠: `crates/core/src/dlsite/client.rs:24-25` に構築箇所なし、`crates/app/src/views/bookshelf.rs:2618` の文字列一致）。
- **[DLsite]** `save_purchases` は `product_info` を後段でまとめて叩くため、一覧の `work_type` が空でもメタ取得に成功すれば救済される設計（根拠: `crates/core/src/dlsite/client.rs:431-439` の導出失敗＝空文字＋`crates/core/src/dlsite/sync.rs:36` の meta 優先 site_id）。
- **[FANZA]** 同期が「全件取得 → 全件 UPSERT」で削除を伴わないのは、**購入履歴が減らない前提**の設計と推測（根拠: `save_purchases` に削除処理が一切なく、`docs/database.md:77-79` が `bookshelf_items` を「同期データのスナップショット」と説明している）。
- **[FANZA]** `is_streaming` / `is_unavailable` は**ストリーミング作品や配信終了作品を後からフィルタするための先行パース**と推測（根拠: フィールドはパースされるが使用箇所がない。`crates/core/src/fanza/client.rs:84-85`）。
- **[FANZA]** `FanzaError::DrmProtected` は DRM 判定をコア層へ移す想定の名残で、現状は UI 側が `detail.is_drm` を見て `ImportFailure::Message("DRM 付き作品は取り込めません")` を返すと推測（根拠: `crates/app/src/views/bookshelf.rs:2804-2808` にメッセージがある）。
- **[FANZA]** ページ間 sleep が無いのは、1 ページ 20 件 × 最大 50 ページ＝実測 290 件（`docs/import-patterns.md:24`）程度で負荷が小さいためと推測（根拠: 終了条件 2000 件上限 `crates/core/src/fanza/client.rs:212`）。
- **[FANZA]** WebView が `www.dmm.co.jp` と `accounts.dmm.co.jp` の両オリジンの Cookie を保存するのは、**API が www のセッション Cookie を要求し、`accounts` 側にもログイン状態（SSO）が残る**ためと推測（根拠: コメント `crates/core/src/fanza/client.rs:47`、収集ループ `crates/app/src/views/fanza_login.rs:117`）。
- **[FANZA]** `synced_at` と `updated_at` に同一時刻を入れ、`created_at` を競合時に更新しないのは、**初回同期日時を保持しつつ最終同期時刻を記録する意図**と推測（根拠: `crates/core/src/fanza/sync.rs:83-85` と `crates/core/src/db/bookshelf.rs:88-89` の DO UPDATE 対象差）。
- **[BOOTH]** 表紙取得を 4 並列に固定しているのは「BOOTH API への負荷を抑えるため」というコメントが根拠（`crates/app/src/views/bookshelf.rs:2296`）。レート制限値の根拠はコード上にない（根拠: 同コメント）。
- **[BOOTH]** 同期の「全件 upsert + 集合差 DELETE」方式は、BOOTH 側に差分 API が無い（HTML スクレイピングのみ）ことの帰結と考えられる（根拠: `crates/core/src/booth.rs` に差分 API 呼び出しが存在しないこと）。
- **[BOOTH]** `synced_at` が固定値なのは、この列が Web 版スキーマ互換のために存在し、デスクトップ版では表示に使われていないためと考えられる（根拠: `synced_at` を読む箇所が本棚一覧のクエリ（`crates/core/src/db/bookshelf.rs:167-190`）に無いこと。ただし Web 版との互換目的である旨の明示記述は無い）。
- **[BOOTH]** ライブラリの判定に `html.len() < 20_000` を使っているのは、ログインページ（短い HTML）を弾くためのヒューリスティックと考えられる（根拠: 同条件の直前コメント `crates/core/src/booth.rs:224-226`）。
- **[BOOTH]** BOOTH 同期がアプリ層にあるのは、BOOTH の取得データ（購入履歴・表紙・作者名）が他ストアと異なり「本棚アイテムのメタ補完」に閉じており、core 側の `save_purchases` 相当の共通 IF に載せていないためと考えられる（根拠: `crates/core/src/dlsite/sync.rs:23` / `crates/core/src/fanza/sync.rs:51` に相当する `booth::sync` が存在しないこと）。
- **[技術書典]** `tbf_events.poll_sync_enabled` を UPSERT で保持する設計（`crates/core/src/tbf/sync.rs:148`）は、「同期してもイベント詳細の ON/OFF トグルを消さない」ための意図だと推測（根拠: `crates/core/src/db/checklist.rs` の `poll_sync_enabled` コメント「1 = ポーリング対象（サーバー同期で上書きしない）」）。
- **[技術書典]** `save_checklist` が `sample_fetch_attempted_at` を `None` で上書きするのは、試し読み未取得状態へ戻す意図（試し読みページ自体を毎回削除する `crates/core/src/tbf/sync.rs:170-172` と整合）。ただし参照箇所が見当たらないため未使用カラムの可能性（根拠: `crates/core/src/db/checklist.rs` の `CheckedItem` 定義と同期実装）。
- **[技術書典]** `events()` が毎回 `tbf21..tbf30` を探索するのは「将来イベントを best-effort で発見する」意図（根拠: `crates/core/src/tbf/mod.rs:329-330` の doc コメント "best-effort live discovery of future events"）。ただし 1 ポーリング周期あたり最大 10 リクエストを追加消費するため、レート制限が厳しい場合は負荷源になりうる（推測）。
- **[技術書典]** 探索が成功した場合 `display_order = 0` / `is_featured = true` が探索イベント（`tbf30` から降順で最初に成功したもの）に付く（根拠: `crates/core/src/tbf/mod.rs:351-365` の並びと採番。テストでは全探索 404 のため `tbf20` が featured: `crates/core/tests/tbf.rs:490-493`）。
- **[技術書典]** `AuthDialog` の技術書典ブランチにある「email/password フォーム」コメント（`crates/app/src/views/auth.rs:617`）は、WebView 方式へ移行する前の名残（推測。実際の UI は説明文 + WebView 起動ボタンのみ）。
- **[技術書典]** `docs/features.md:430-433` の「セッションは keyring（macOS キーチェーン）に保存」は macOS を代表例として挙げた記述で、Windows では Credential Manager（`keyring` の `windows-native` feature）に保存される（根拠: `crates/core/Cargo.toml:21`）。
