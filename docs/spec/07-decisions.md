# 07. 設計判断と意図の記録（Why）

> `docs/spec/README.md`（索引）から参照される設計仕様書の一部。
> **ここに書くのは「なぜそうなっているか」**。仕様の数値や手順は各章（02〜06）を参照。
> 根拠: コード内コメント / コミットメッセージ / `CHANGELOG.md` / 実測。
> 記録が無く推測に留まるものは「推測」と明記する。

## 1. プロダクトの前提

| 決定 | 理由 | 出典 |
|---|---|---|
| **対象は「購入済み・非DRM の同人誌のみ」** | 購入していないコンテンツや DRM 保護されたコンテンツは取り込めない。アプリの上部で強調している | `crates/app/src/views/about.rs` の `TARGET_NOTICE` |
| 対応ストアは 技術書典 / BOOTH / FANZA / DLsite | 各ストアに個別の同期・ログイン実装を持つ | `crates/core/src/{tbf,booth,fanza,dlsite}` |
| 対応形式は PDF / 画像（ZIP） / EPUB | 同人誌の配布形式に合わせる | `crates/core/src/import/mod.rs:1` |
| データは端末ローカルのみ（クラウドは任意のバックアップ） | 外部サーバーに購入履歴を残さない方針 | `crates/app/src/views/about.rs`（「このアプリについて」） |

## 2. ファイル形式とデータモデル

| 決定 | 理由 | 代替案/却下理由 |
|---|---|---|
| **1 冊 = 1 ファイルの自前形式 `.opfspack`** | 配布物（ZIP/PDF/画像）を 1 つにまとめ、コピー・バックアップ・別 PC 移行を単純化する | 元の ZIP をそのまま置く案は、複数コンテンツ・レンディションの表現が崩れる |
| **コンテンツ（作品）とレンディション（形式）を分離**（`content_id` / `format_id` / `document_id`） | 同じ作品の「PDF 版」「画像版」を別物として保持し、ビューアーで切り替えられるようにする | 1 作品 1 形式に潰す案は、同梱 ZIP の片方を失う |
| **DB は SQLite（WAL）** | 端末ローカル・単一プロセス・バックアップが単純 | |
| **`CREATE TABLE IF NOT EXISTS` + `ALTER TABLE` による冪等な後方互換** | 起動時に毎回実行しても安全で、既存ユーザーの DB を壊さない（マイグレーションファイルを増やさない方針） | 逐次マイグレーションは配布チャネルが無いデスクトップアプリでは運用が重い |
| **`schema.sql` は参照用の正本**（実行時は `migrations/0001_init.sql` + ランタイム DDL） | 新旧のスキーマを 1 枚で読めるようにする | 実行時に読ませると二重管理になる |

## 3. 表示・操作の一貫性

| 決定 | 理由 |
|---|---|
| **読書状態の判定を 1 箇所に集約**（`ReadingState::from_progress`） | 本棚 / 履歴 / 付箋 / 設定 / サイドバーで判定がずれない（実装初期に設定集計が off-by-one でずれていた） |
| **タグの並び順を 1 箇所に集約**（`TagOrder`: 選択中 → お気に入り → 集計数降順 → 名前順） | 画面ごとに順序が違うと操作感が壊れる |
| **タグの折りたたみ個数を幅から計算**（カード = `min(len, 6)`、リスト = 実測幅から `packed_tag_count`） | 見切れさせず、入るだけ見せる。固定個数だと窓幅で崩れる |
| **本棚 / 履歴は既存構成、付箋は 表紙 / 情報 / タグ / 付箋ページ / メモ / 本を見る の 6 列。画像枠・タグ部品は共有** | 付箋は付箋を付けたページの画像とメモ、「本を見る」の導線が必要なため 6 列とする。表紙の画像枠とタグ列の定数・描画関数（`LIST_COVER_W` / `cover_fit_inside_frame` / `LIST_TAGS_W_RATIO` / `TagOrder` / 折りたたみ個数）は本棚と共有する（別実装にすると必ずズレる） |
| **通知は右上のトースト（Info / Success / Error）** | 下部の 1 行表示を廃止して**表示領域を本に充てる**ため（下部 28px の余白削除と同時に実施） |
| **関連書籍は「ショートカット」**（同一サークル → 同一作者、最大 5 件） | 名前のとおり行からすぐ移動できる導線にする。未ダウンロードなら確認ダイアログを経て取り込み、完了後にその画面を開く |

## 4. 実装で踏んだ問題と対策（再発防止）

| 問題（実測） | 原因 | 対策 |
|---|---|---|
| メモリが **6,000MB** まで増える | (1) `ImageViewer` の**強参照の自己循環**と、`ReaderView` を `App::on_action` のアプリ寿命リスナーが**強参照**していた→閉じても解放されず本ごとに累積 (2) スクロールモードで**全ページ**を保持（1 ページ平均 16MiB、192 ページで 5,631MiB） | (1) すべて `WeakEntity` 化（回帰テスト `viewer_is_dropped_when_no_strong_handle_remains` / `reader_is_dropped_when_closed`） (2) **384MiB のバイト予算**で表示中ページの前後だけ保持し、`Window::drop_image` で GPU からも解放 |
| PDF の並列取り込みでプロセスが落ちる（`STATUS_ACCESS_VIOLATION`） | **PDFium はプロセスで 1 つのライブラリ状態を共有し、同時利用がスレッドセーフではない**（`pdfium-render` の `thread_safe` は `unsafe impl Send/Sync` を足すだけ） | PDFium を使う区間（初期化を含む）を `static Mutex` で**直列化**（`crates/core/src/import/pdf.rs`）。描画後の WebP エンコードは PDFium を触らないので 8 スレッドで並列のまま |
| 表紙のプレースホルダが**常に空枠** | SVG を `image` クレートで復号しようとして常に `None` | `usvg` + `resvg` + `tiny-skia` でラスタライズ（`rasterize_svg`）。BGRA 入れ替えが必要 |
| 履歴の表紙が日ごとに再デコード | 同じ本が複数の日に出る | `reload` 内で `book_id` ごとに**1 回だけデコードして共有** |
| 通知の自動消滅時間のコメントが「3 秒」 | 実装は gpui-kit の既定 **5 秒** | **修正済み**: コード内コメント（`app_state.rs` / `settings.rs` / `workspace.rs`）を実装に合わせて「既定 5 秒」に統一した |
| pack の `entry_count` で **377 GB の確保**を試みてプロセスが異常終了（`memory allocation of 377957121960 bytes failed` → `STATUS_STACK_BUFFER_OVERRUN`） | `entry_count` を index 長と突き合わせずに `Vec::with_capacity` に渡していた。ヘッダ CRC は攻撃者も計算できるため、68 バイトの細工データで再現する | **修正済み**: index 長から導ける件数上限（1 エントリ ≧ 48 バイト）を確保の前に検査（`crates/opfspack/src/reader.rs`）。回帰テスト `crates/opfspack/tests/hardening.rs` |
| 保存済み `download_url` / 302 の `Location` を外部ホストへ向けると**セッション Cookie が送られる** | 送信先の検証が無かった。`download_url` は Drive の JSON バックアップから復元でき、改変バックアップの復元で攻撃が成立する | **修正済み**: `download_url::check` を追加し、認証付き送信の直前と 302 の転送先で `https` + 許可ホスト（Cookie のスコープに合わせる）を検証（`crates/core/src/download_url.rs`）。回帰テスト `crates/core/tests/download_credentials.rs` |
| Windows で**作業ディレクトリの `pdfium.dll` を優先ロード** | `library_candidates()` の先頭が `./`（テスト用のつもりが製品コードにも残っていた）。CWD を用意して起動させられると任意 DLL を読む（DLL 配置攻撃） | **修正済み**: CWD の候補を削除し、開発時の探索はビルド時 `CARGO_MANIFEST_DIR` の絶対パスに限定（`crates/core/src/import/pdf.rs`） |
| 改変バックアップの `books.id` / `pack_id` で**保存領域の外**の pack を読み書き・削除できる | 復元した id を検証せず `packs_dir.join("{id}.opfspack")` へ渡していた | **修正済み**: `pack_path` モジュールに検証を集約し、復元時は不正 id を拒否、pack を触る全経路で同じ検証を通す（`crates/core/src/pack_path.rs`） |
| **ストアのログインを開くとアプリが落ちる**（`RefCell already borrowed` → `panic in a function that cannot unwind` で abort） | WebView2 の生成（`WebViewBuilder::build`）と Cookie 取得（`cookies_for_url`）が内部で `webview2_com::wait_with_pump` を呼び、**メッセージループを回す**。gpui はメッセージ処理のたびに保留中の foreground タスクを実行するため、App を借用したまま呼ぶと、その間に走った定期タスクの `handle.update` が `app_mut` の `borrow_mut` で panic する（実測: DLsite のログインを開いた瞬間に `Workspace::start_login_done_watcher` が衝突） | **修正済み**: `crates/app/src/app_state.rs` の `WebviewPumpGuard` / `webview_pumping()`（static。判定で借用を取ると本末転倒なので AppState ではない）で WebView2 を触る区間を示し、定期タスク（Workspace のログイン監視・4 ストアのログイン監視・本棚の同期/進捗ポーリング・ビューアーのフレームループ）は 0 でなければ 1 tick 待つ。**残るリスク**: WebView2 の呼び出し中に**単発**のタスクが待っている場合は依然として衝突し得る（恒久策は生成を借用の外＝タスク本体へ出すこと。未着手） |

## 5. ストア同期・認証の意図

| 決定 | 理由 |
|---|---|
| 認証情報は **技術書典 / Google / DB 鍵 / セッション鍵 = OS keyring**、**BOOTH / FANZA / DLsite のセッション = DB（keyring の鍵で AES-256-GCM）** | keyring の値長上限（2560 UTF-16 文字）にセッションが収まらないため DB に置くが、DB のコピーからセッションを復元されないよう鍵だけを keyring に置いて暗号化する（`crates/core/src/session_store.rs`） |
| 同期は**全件 UPSERT**（技術書典 / DLsite / FANZA）、**Drive のみ md5 + 更新時刻で差分** | ストア側 API に差分が無い。Drive はファイル転送コストが高い |
| ログインはストアごとに**専用 WebView** を開き、URL 遷移を監視してセッションを保存 | 各ストアのログイン方式（Cookie / メールログイン / OAuth）が異なる |
| **Cookie は収集元ホストごとに持ち、宛先ごとに絞って送る**（`FanzaSession` / `DlsiteSession` / `BoothSession` の `cookie_header_for`） | 収集元（`www` / `accounts`、`www` / `login`、`booth.pm` / `accounts.booth.pm`）を 1 つに潰すと、片方にしか送るべきでない Cookie がもう片方へ飛ぶ。とくにダウンロードの CDN は別システムなので、302 で受け取る署名 `jwt` / `CloudFront-*` だけを送る（www のセッション Cookie は送らない）。宛先は**完全一致**（サブドメインへは送らない） |
| WebView を監視するタスクは**弱参照**でビューを持つ | 監視タスクがアプリ寿命で動き続けるため、強参照だと閉じても WebView ごと残る |
| 複数 Google アカウントを切替えてもデータが消えない | データを `owner_sub` で**属性付け**して保持し、ログイン切替では削除しない（`crates/core/src/owner.rs`。設計は `docs/account-switch.md`） |

## 6. 既知の制約（移植時に判断が必要）

2026-09-14 の一斉修正で解消したものには「修正済み」を付ける。

| 項目 | 現状 |
|---|---|
| 並び替えの永続化 | **修正済み**: `bookshelf.sort_field` / `bookshelf.sort_ascending` に保存し、起動時に復元する |
| タグ絞り込みの対象 | **修正済み**: ローカル本のタグに加えて**本棚アイテムの `tags_json`** も見る（未ダウンロードの FANZA/DLsite 本もヒットする） |
| 見開きの滞在時間 | **修正済み**: 表示中ページ集合へ**均等配分**する（合計が実時間になる）。ページ送り時の計上も同様 |
| 単一 / 見開きモードの追い出し | **修正済み**: 追い出した画像を GPU 解放キューに積み、render で `window.drop_image` する（`images_released_to_gpu` で観測できる） |
| `view_history::touch` | **修正済み**: アプリから未使用だったため削除し、テスト専用ヘルパへ移動（`docs/database.md` も更新） |
| `reading_progress.scroll_position` | **修正済み**: Rust 側のフィールドを削除（列は互換のため残置。常に既定値 0） |
| Drive のフォルダ | **既知の制約**: 実装は単一キー `drive.sync.folder_id`（アカウント別ではない）。データ本体は `owner_sub` で属性付けされるため混ざらないが、フォルダは分かれない |
| 未読フィルタ | **未対応**: `ReadFilter::Unread` はローカル本のみ一致（未ダウンロード本はカード上「未読」表示だが未読フィルタに出ない） |
| 履歴一覧 | **未対応（見送り）**: 非仮想化（全行描画）。表紙は 1 冊 1 回に共有済みで、残る効果は約 15MiB |

### 6.1 依存の警告（脆弱性ではないもの）

`cargo audit` は**脆弱性（vulnerability）と警告（warning）を分けて**報告する。2026-09-22 時点で
**脆弱性 0 件 / 警告 13 件**（未保守 11・unsound 2）。警告はいずれも**修正版が無い**種類の勧告
（`patched_versions` が空）なので、版を上げても消えない。したがって「**実際に配布物へ入るか**」で
切って判断する（判定は `cargo tree -i <crate> --target <triple> -e all`。Windows は
`x86_64-pc-windows-msvc`、macOS は `aarch64-apple-darwin` / `x86_64-apple-darwin`）。

| crate | 勧告 | 種別 | 配布物（Win / macOS） | 由来 | 判断 |
|---|---|---|---|---|---|
| `ttf-parser` 0.24.1 / 0.25.1 | RUSTSEC-2026-0192 | 未保守 | **入る** | `usvg` / `resvg`（SVG のラスタライズ。gpui-component のアイコン描画） | 修正版なし。上流の更新待ち |
| `rustybuzz` 0.18.0 / 0.20.1 | RUSTSEC-2026-0206 | 未保守 | **入る** | 同上 | 同上 |
| `bincode` 2.0.1 | RUSTSEC-2025-0141 | 未保守 | **入る** | `lindera`（形態素解析の辞書読み込み。`crates/core/src/tags.rs`） | 修正版なし。`lindera` の更新待ち |
| `instant` 0.1.13 | RUSTSEC-2024-0384 | 未保守 | **入る** | `gpui-base` / `notify-types` | 上流待ち（後継は `web-time`） |
| `paste` 1.0.15 | RUSTSEC-2024-0436 | 未保守 | ビルド時のみ（proc-macro） | `gpui-component` / `pulp` | 上流待ち（後継は `pastey`） |
| `encoding` 0.2.33 | RUSTSEC-2021-0153 | 未保守 | ビルド時のみ | `lindera-dictionary` の `[build-dependencies]` | 上流待ち |
| `glib` 0.18.5 | RUSTSEC-2024-0429 | unsound | **入らない** | `gtk` ← `lb-wry`（Linux の WebView のみ） | 配布対象外なので許容 |
| `proc-macro-error` 1.0.4 | RUSTSEC-2024-0370 | 未保守 | **入らない** | `glib-macros` ← `glib`（同上） | 同上 |
| `rand` 0.7.3 | RUSTSEC-2026-0097 | unsound | **入らない** | `phf_codegen` ← `selectors` ← `kuchikiki` ← `lb-wry` | 同上 |
| `fxhash` 0.2.1 | RUSTSEC-2025-0057 | 未保守 | **入らない** | 同上 | 同上 |
| `rustls-pemfile` 2.2.0 | RUSTSEC-2025-0134 | 未保守 | **入らない** | `gpui-pre-reqwest` ← `gpui-kit-assets` | 同上 |

「入らない」は**配布ターゲットの依存グラフに現れない**という実測（`cargo tree -i … --target … -e all`
が空）。「入る」ものは上流（gpui / lindera / resvg 系）が置換・更新するまで解消できない。

見直し: CI の `cargo audit` は毎回これを警告として出す（消えれば上流が直った合図）。依存を更新する
たび、または四半期ごとにこの表を引き直す。**新しい勧告（とくに unsound）が出たら、まず配布物へ
入るかを確認する。**
