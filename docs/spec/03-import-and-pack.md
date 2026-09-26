# 03. 取り込みパイプラインとファイル形式（.opfspack）

> `docs/spec/README.md`（索引）から参照される設計仕様書の一部。
> 本章は **入力判定 → コンテンツ/レンディション解決 → ページ画像生成 → pack 書き出し → DB 登録**。読み手は AI（別言語での再実装・Web 版への移植を想定）。
> 事実にはアンカー付き。断定できない事項は章末の「不明点 / 推測」に分離してある。
> 情報源: crates/core/src/import/**, crates/opfspack/**, crates/core/src/db/**
>
> **注（v3 移行に伴うアンカーのずれ）**: v3 対応（`crates/opfspack` の鍵スキームと `crates/core/src/import/mod.rs` の鍵の受け渡し）で行番号が動いた。§3.1・§3.5・§4・§5.1・§5.4・§5.5・§6 のアンカーは更新済み。**§3.2〜§3.4・§3.6〜§3.10 の `import/mod.rs` アンカーは移行前の値のまま**（目安として +85〜+100 行ずれている）なので、参照時は関数名・定数名で検索すること。

## 3. 取り込みパイプライン

### 3.1 段階の全体（関数名 + アンカー）

| # | 段 | 関数 / 定数 | アンカー | 入力 → 出力 |
|---|---|---|---|---|
| 0 | 起動（アプリ層・参考） | ダウンロード済み `bytes` + 拡張子で分岐（`epub` / `zip` / `jpg｜jpeg｜png｜webp｜gif` / `pdf`） | `crates/app/src/views/bookshelf.rs:3017-3057`, `:2976-2980` | ファイル → 取り込み API 呼び出し |
| 1 | 拡張子ディスパッチ | `import_file` | `import/mod.rs:1251-1279` | パス → `pdf`/`epub`/`zip` のいずれか、他は `ImportError::UnsupportedType` |
| 2 | ZIP 解析（書き込みなし） | `analyze_zip` | `import/mod.rs:1510-1554` | bytes → `ImportPlan` |
| 3 | エントリ名列挙 + 入れ子展開 | `collect_metas_with_nested` → `collect_entry_metas` + `expand_nested_archives` | `import/mod.rs:365-382`, `:289-311`, `:383-478` | ZIP → `Vec<EntryMeta>` |
| 4 | 種別判定（名前のみ） | `classify_entry` / `is_nested_archive` / `is_readable_kind` | `classify.rs:74-105`, `:63-71`, `:136-141` | パス → `EntryKind` |
| 5 | コンテンツ組み立て | `build_contents` / `group_push` / `plan_content` / `rendition_label` | `import/mod.rs:606-641`, `:644-660`, `:662-700`, `:702-723` | `Vec<EntryMeta>` → `Vec<PlannedContent>` |
| 6 | 既定コンテンツ決定 | `choose_primary` / `is_body_name` | `import/mod.rs:1457-1486`, `:1505-1508` | コンテンツ列 → `primary` 添字 |
| 7 | `_export.txt` 読み | `parse_export_text` / `decode_text_bytes` | `export_text.rs:11-31`, `zip_names.rs:21-23` | txt bytes → `Vec<(i64, String)>` |
| 8 | 実体化 + pack 生成 + DB 登録 | `commit_zip` | `import/mod.rs:1556-1786` | `ImportPlan` + bytes → `ImportedBook` |
| 8a | （ZIP 経由の PDF） | `render_pdf_with` → `pdf::render_pdf_pages` | `import/mod.rs:1494-1503`, `pdf.rs:71-120`(Win) / `:122-193`(非 Win) | PDF bytes → `Vec<PageImage>` |
| 8b | ページ画像（画像レンディション） | `render_page_images` → `render_page_image` → `upscale_if_small` + `encode_webp` | `import/mod.rs:180-215`, `:153-159`, `:112-124`, `:126-132` | 元画像 bytes → `(WebP, w, h)` |
| 8c | サムネイル | `thumbnail_of` | `import/mod.rs:135-150` | ページ bytes → `(WebP, 200, h)` |
| 9 | 単体 PDF 経路 | `import_pdf_bytes` → `import_rendered_pdf_pages` | `import/mod.rs:1282-1304`, `:1306-1377` | PDF bytes → `finish_import` |
| 10 | 単体 EPUB 経路 | `import_epub_bytes` | `import/mod.rs:1379-1411` | EPUB bytes → `finish_import`（生 1 エントリ） |
| 11 | 単体画像経路 | `import_image_bytes` | `import/mod.rs:2155-2210` | 画像 bytes → `finish_import`（1 ページ） |
| 12 | pack 書き出し + DB 一括登録 | `finish_import` | `import/mod.rs:945-1249` | `PackSpec` → `ImportedBook` |
| 13 | Drive 復元（pack → DB） | `rebuild_from_pack` / `content_from_metadata` / `infer_contents` | `import/mod.rs:1818-1997`, `:1999-2050`, `:2052-2091` | pack bytes → DB 行（再構築したら `true`） |
| 14 | pack 内メタ書き換え | `rename_content_in_pack` | `import/mod.rs:819-895` | pack bytes + 新表示名 → 新しい pack bytes（変更なしは `None`） |

### 3.2 判定（拡張子 / 名前 / ZIP 解析）

| 判定対象 | 規則 | アンカー |
|---|---|---|
| 取り込み対象の拡張子（アプリ層） | `epub` / `zip` / `jpg`,`jpeg`,`png`,`webp`,`gif` / `pdf`（`pdf` は `analyze_zip` を通さない） | `crates/app/src/views/bookshelf.rs:3017-3056` |
| 画像拡張子（ZIP 内） | `png, jpg, jpeg, webp, gif, bmp, tif, tiff`（小文字比較） | `classify.rs:32` |
| 音声拡張子 | `mp3, wav, m4a, ogg, flac` | `classify.rs:35` |
| 動画拡張子 | `mp4, mov, mkv, webm, avi` | `classify.rs:38` |
| junk ファイル名 | `thumbs.db`, `.ds_store`, `desktop.ini`, `readme.txt`, `ご挨拶.txt`, `メモ.txt`, `あとがき.txt`、パス成分に `__MACOSX` を含むもの | `classify.rs:41-49`, `:80-83` |
| 表紙判定 | パス成分のいずれかが `表紙` を**部分一致**で含む、または `cover` / `omote` / `ura` / `back` を**単語として**含む（`discover` を誤判定しない）。ファイル名は拡張子を落として判定 | `classify.rs:52-56`, `:108-133` |
| `_export.txt` | 小文字のファイル名が `_export.txt` で終わる `.txt` | `classify.rs:85-87` |
| 入れ子 ZIP | 拡張子が `.zip`（大文字小文字無視）かつ `__MACOSX` を含まない | `classify.rs:63-71` |
| 読める種別 | 画像 / PDF / EPUB / 音声 / 動画（表紙・`_export.txt`・junk を除く） | `classify.rs:136-141` |
| ビューアで読める種別 | 画像 / PDF / EPUB のみ（音声・動画は取り込み対象外） | `import/mod.rs:1396-1398` |
| エントリ名の復号 | まず UTF-8、不正なら CP932（Shift-JIS）として復号（`zip` crate の CP437 復号を使わないため生バイト `name_raw()` を使う） | `zip_names.rs:26-31`, `:16-18` |
| ZIP が空 | エントリ 0 件 → `ImportError::EmptyArchive` | `import/mod.rs:1422-1424` |
| 読めるものが無い | `contents` が空 → `ImportPlan.skip_reason = Some(SkipReason::NotAReadableWork)` | `import/mod.rs:1456-1462` |

### 3.3 入れ子 ZIP の展開（決定 D5 / §11.2 R1）

| 項目 | 値 / 規則 | アンカー |
|---|---|---|
| 深さ上限 | `MAX_NESTED_DEPTH = 1`（入れ子の中の `.zip` は展開せず警告） | `import/mod.rs:185`, `:340-346`, `:296-299` |
| 合流サイズ上限 | `MAX_NESTED_BYTES = 512 * 1024 * 1024`（= 512 MiB、非圧縮合計。宣言サイズで先に弾き、実際に読めたバイト数でも判定） | `import/mod.rs:191`, `:313-330` |
| 合流エントリ数上限 | `MAX_NESTED_ENTRIES = 2000` | `import/mod.rs:197`, `:357-363` |
| 合流規則 | 入れ子 ZIP のパス（拡張子除去）+ `/` + 内側エントリ名。外側の分類・グルーピングがそのまま効く | `import/mod.rs:336`, `:351-355` |
| 上限超過時 | 半分だけ取り込まず**その入れ子全体をスキップ**し、`warnings` に 1 行積む（取り込み全体は失敗させない） | `import/mod.rs:357-364` |
| 通常エントリの上限 | `MAX_ZIP_ENTRY_BYTES = 2 GiB`（**展開後**。宣言サイズではなく実際に読めたバイト数で判定）。超過は `ImportError::Zip`（部分的なデータで先へ進めない）。取り込み元ファイルの上限と同値にしてある: 512 MiB にしていたときは「142 MB の ZIP に含まれる PDF が展開後 512 MiB を超える」正当な本を弾いていた（2026-09-26） | `import/mod.rs`（`read_zip_entry_capped`） |
| 外側 ZIP 全体の上限 | **展開の前**にエントリ数（`opfspack::MAX_ENTRY_COUNT` = 10000）と、中央ディレクトリが宣言する展開後サイズの合計（`opfspack::MAX_TOTAL_SIZE` = 10 GiB）を検査する。超過は `ImportError::ZipTooLarge { detail }`。宣言サイズは信用せず（実際の長さは個別上限で別途見る）、事前に弾ける分だけをここで弾く | `import/mod.rs`（`analyze_zip` / `EntryMeta::declared_size`） |
| 取り込み元ファイルの上限 | `MAX_IMPORT_SOURCE_BYTES = 10 GiB`（pack の上限と同値）。**読む前に**メタデータで検査する。超過は `ImportError::SourceTooLarge { size, limit }`。アプリ側は①ストアが申告するサイズが分かるときは**転送の前**に断り（無駄なダウンロードをしない）②ダウンロード直後にも同じ値で検査する | `import/mod.rs`（`import_file`）、`crates/app/src/views/bookshelf.rs` |
| 取り込み元の読み方 | **PDF と ZIP はファイルから読む**（`import_pdf_path` / `import_zip_path`。PDF は PDFium の `load_pdf_from_file`、ZIP は `ZipArchive<File>`）ので、取り込み元の大きさがそのまま RAM 使用量にはならない。EPUB（ビューアー非対応で 1 エントリとして入れるだけ）と単体画像は全体を読む（実データでは小さい）。**アプリのダウンロードはまだメモリ経由**（`download_with_progress`）なので、4 GiB 級の本は一時的に大きめの RAM を使う（一時ファイル経由への切り替えが残件） | `crates/core/src/import/{mod,pdf}.rs` |
| PDF の上限 | **描画の前**に総ページ数（`MAX_PDF_PAGES` = 9000。`MAX_ENTRY_COUNT` から metadata / サムネイル分を引いた値）を検査。描画中は 1 ページ 16MPix の検査に加えて**エンコード済みの累積出力**（`opfspack::MAX_TOTAL_SIZE` = 10 GiB）で打ち切る。ピークは 8 ページ窓分 | `import/pdf.rs`（`render_pdf_pages`） |
| 壊れた入れ子 | `warnings` に `"{name}: {error}"` を積んでスキップ | `import/mod.rs:326-333` |
| 警告文言（実装値） | `"{name}: nested zip is larger than the size limit (536870912 bytes)"` / `"{name}: nested zip inside a nested zip is not expanded (depth limit 1)"` / `"{name}: nested zip exceeds the entry/size limit and was skipped"` | `import/mod.rs:314-317`, `:347-349`, `:358-361` |

> 本表は**取り込み（ZIP を読む）側**の上限。pack を**読み出す**ときの上限（`MAX_ENTRY_SIZE` / `MAX_ENTRY_COUNT` 等）は §4.6.1 を参照。値の一部は揃っているが、判定対象も失敗の扱いも別物。

### 3.4 コンテンツ・レンディションの組み立て

| 規則 | 内容 | アンカー |
|---|---|---|
| パス区切り | `/`, `\`, 全角 `／` の 3 種で分割 | `import/mod.rs:469` |
| 形式フォルダを飛ばす | 末尾成分が `jpg, jpeg, png, webp, gif, bmp, tif, tiff, pdf, epub, カラー, モノクロ, 文字あり, 文字なし, seあり, seなし`、`*版` で終わる、`画像*` で始まる場合は 1 つ上を内容フォルダ名にする | `import/mod.rs:473-497` |
| 直下の画像 | まとめて 1 コンテンツ（表示名 `本文`、キー `""`） | `import/mod.rs:545-547` |
| 直下の PDF/EPUB/音声/動画 | ファイル（拡張子を除いた名前）ごとに 1 コンテンツ。同名フォルダが既にあればそのレンディションとして畳む | `import/mod.rs:548-555` |
| コンテンツの整列 | 表示名の自然順（`natural_cmp`: 数字連続は数値比較、`1.jpg` < `2.jpg` < `10.jpg`） | `import/mod.rs:557`, `:1321-1360` |
| レンディションの順序 | `Image, Pdf, Epub, Audio, Video` の固定順（画像が先頭＝主）。同種内は `natural_cmp` | `import/mod.rs:581-607` |
| レンディション表示名 | 画像: 元拡張子 → `JPEG`/`PNG`/`WEBP`/`GIF`/`BMP`/`TIFF`（未知は種別名）。PDF/EPUB/音声/動画: 種別名（`PDF`/`EPUB`/`音声`/`動画`） | `import/mod.rs:620-638`, `db/contents.rs:215-225` |
| `media_kind` の DB 値 | `image` / `pdf` / `epub` / `audio` / `video` | `import/mod.rs:653-663` |
| 既定コンテンツの決定 | ① ビューア対象種別のみ候補 ② 名前に `本文`/`本編` を含むもの ③ ページ数の目安が最大 ④ 同数なら PDF/EPUB 優先 ⑤ 最後は索引順 | `import/mod.rs:1365-1393`, `:1413-1415` |
| ページ数の目安 | 主レンディションのエントリ数（PDF/EPUB は展開前なので 1 ファイル = 1） | `import/mod.rs:428-434` |
| 主コンテンツが読めない場合 | `commit_zip` は即 `ImportError::NotAReadableWork`（音声・動画のみ等） | `import/mod.rs:1470-1477` |

### 3.5 pack 内のエントリ配置（生成規則）

| 条件 | エントリパス | compress フラグ | アンカー |
|---|---|---|---|
| 常に最初 | `metadata.json`（`application/json`） | `false` | `import/mod.rs:1002`, `:765-808` |
| 既定表示コンテンツの第 1 レンディション（画像/PDF） | `pages/page_{n:04}.webp`（n は 1 始まり） | `false` | `import/mod.rs:1605-1610`, `:1639-1645`, `:1671-1677`, `:1326` |
| それ以外のコンテンツ/レンディション（画像/PDF） | `contents/{content_index}/r{rendition_index}/page_{n:04}.webp` | `false` | `import/mod.rs:1605-1610`, `:1639`, `:1671` |
| EPUB（既定コンテンツの第 1 レンディション） | 元のファイル名そのもの（`dir/book.epub` → `book.epub`） | `false` | `import/mod.rs:1699-1711` |
| EPUB（それ以外） | `contents/{c}/r{r}/{元ファイル名}` | `false` | `import/mod.rs:1699-1706` |
| 表紙 | `cover.webp`（**既定コンテンツが PDF のときだけ**。ZIP 経路では `primary_cover` が立つ場合のみ） | `false` | `import/mod.rs:1744-1751`, `:1679-1684` |
| サムネイル | `thumbnail.webp`（既定コンテンツ第 1 ページの 200px 版） | `false` | `import/mod.rs:1753-1758` |
| 音声・動画 | エントリを作らない（構造だけ DB に記録） | — | `import/mod.rs:1712-1713` |
| pack レベル | `PackBuilder::build_to_file_streaming(..., true)` = pack フラグ `COMPRESSED` を常に立てる（**エントリ個別の compress はすべて false**。鍵を渡したときは `ENCRYPTED` も立つ）。エントリは `PackEntryStore` から**1 件ずつ**供給する（全ページを同時にメモリへ載せない）。**一時ファイルへ直接組み立ててから rename** し、pack 全体の SHA-256 はファイルを順に読んで計算する（`PackFileReader::source_sha256`） | — | `crates/opfspack/src/builder.rs`（`write_pack`）, `reader.rs:350`（`PackRead`） |

### 3.5.1 取り込み中のメモリ（`PackEntryStore`）

ページデータは `PackEntryStore` が保持し、**受け取った合計が 64 MiB を超えたら以降は一時ファイル**
（`std::env::temp_dir()/thundoku-import-spill-<pid>-<時刻>/`）へ書く。pack の組み立ては
`build_to_file_streaming` で 1 件ずつ供給し、必要なときだけ読み出す（メモリに残るのは
64 MiB + 1 ページ分 + 組み立て中の 1 エントリ）。一時ファイルは `Drop` で必ず消す。

PDF も `pdf::render_pdf_pages_into` で**1 ページずつ** store へ入れる（`import_pdf_bytes`。
以前は全ページを `Vec<PageImage>` に集めてから clone していた）。画像 1 枚・EPUB・ZIP も
store へ直接入る。

**残る制約**: 取り込み元ファイルは `import_*_bytes` が全体をメモリへ読む（2 GiB 超は
`MAX_IMPORT_SOURCE_BYTES` で先に断る）。metadata の書き戻し（`rename_content_in_pack`）も pack 全体を引数で受け取る
（呼び出し側が既に全体を持っている経路）。

### 3.6 ページ画像生成（数値・アルゴリズム）

| 段 | 内容 | アンカー |
|---|---|---|
| 元画像デコード | `image::load_from_memory`（`image` 0.25、features: webp/png/jpeg/bmp/tiff） | `import/mod.rs:109` |
| 小さい画像の拡大 | 幅が 1000 px 未満なら `Lanczos3` で**幅 1000 px** まで拡大（`scale = 1000/width`、四捨五入、最小 1 px） | `import/mod.rs:67-78`, `:110` |
| ページ WebP | `encode_webp(&decoded, 88)`（品質 **88**、lossy、`webp` crate = libwebp）。**libwebp の effort は 2**（既定 4。実測 1433×2024 q88 で 220ms → 103ms、+2% のサイズ。変換時間の 91% がここだった） | `import/mod.rs`（`encode_webp` / `WEBP_METHOD`） |
| 変換の並列度 | ページ変換は `page_render_workers`（`min(コア数, 12)`）で並列。1 ワーカー 30〜40MB なので 12 で頭打ち。実測 1 ページ 129ms（release、effort 2） | `import/mod.rs`（`render_page_images` / `page_render_workers`） |
| 品質の丸め | `quality.clamp(0, 100)` | `import/mod.rs:84` |
| サムネイル | 最初のページを**幅 200 px**（高さはアスペクト比から四捨五入、最小 1）に `Triangle` で縮小し、**品質 80** の WebP | `import/mod.rs:90-105` |
| 並列度 | `page_render_workers(n) = min(available_parallelism, 8, max(n,1))`（未取得時は 4 コア扱い。ワーカー数を 8 で頭打ちにするのはデコード済み画像のメモリ保護） | `import/mod.rs:123-129` |
| チャンク | `PAGE_RENDER_CHUNK = 64` ページ単位で ZIP から読み出して並列変換（順序は保持） | `import/mod.rs:117`, `:1530-1541` |
| 失敗時 | そのページを `warnings` に積んで**スキップ**（1 枚の破損で全体を失敗させない。決定 D6） | `import/mod.rs:1547-1554`, `:135-163` |
| ページ番号 | レンディションごとに 1 始まりで振り直す（`page_rows.len() - start + 1`） | `import/mod.rs:1555-1556` |

### 3.7 PDF レンダリング（PDFium）

| 環境 | 実装 | 解像度 / 品質 | 並列 | アンカー |
|---|---|---|---|---|
| 全プラットフォーム | `pdfium-render` 0.9（`pdfium_7881` / `image_latest` / `thread_safe`）を**実行時ロード**。探索順は ① ビルド時の `CARGO_MANIFEST_DIR`（= `crates/core`）② 実行ファイルと同じディレクトリ ③ macOS は `../Frameworks`（`.app` の `Contents/Frameworks`）。**CWD は探索しない**（DLL 配置攻撃対策） | `set_target_width(1000)`、WebP 品質 **80**。描画前に「幅 1000px 換算の画素数」を見積もり、`MAX_PDF_PAGE_PIXELS = 16 MPix` を超えるページは描画せずエラー（極端なアスペクト比のページでビットマップが巨大化するのを防ぐ） | PDFium の呼び出し（初期化を含む）は **`PDFIUM_LOCK: Mutex<()>` で直列**（`thread_safe` feature はロックしないため自前で排他）。描画後の WebP エンコードは PDFium を触らないので **8 スレッドで並列**（`chunk_size = total.div_ceil(8)`） | `pdf.rs` |
| テキスト抽出 | `page.text()` の文字列。取れない場合は空文字 | — | — | `pdf.rs:118` |
| 進捗 | `progress(finished / total)`（0.0〜1.0）。**エンコードが終わったページ数**を `AtomicUsize` で数えてページごとに通知 | — | — | `pdf.rs:157-160` |
| 失敗時 | 1 ページでも失敗したら `ImportError::Pdf`（失敗したページ番号はログに残す） | — | — | `pdf.rs:166-183` |
| ページ 0 件 | `import_rendered_pdf_pages` が `ImportError::Pdf("no pages rendered")` | — | — | `import/mod.rs:1223` |

以前は macOS / Linux が mupdf だったが、mupdf は **AGPL-3.0** で MIT 配布の本アプリと両立しないため依存から外し、全プラットフォーム PDFium に統一した（`crates/core/Cargo.toml:38-44`）。

### 3.8 DB 登録（`finish_import` の順序）

| # | 処理 | アンカー |
|---|---|---|
| 1 | `book_id` 決定（`book_id_for`） / `title` = ファイル名から拡張子を除いたもの | `import/mod.rs:870-873`, `:676-681` |
| 2 | 再取り込み時: 既存 `book_contents` と件数が一致するときだけ `content_id` と `display_name` を引き継ぎ、`page_rows[].content_id` も張り替える | `import/mod.rs:880-910` |
| 3 | pack 構築（`metadata.json` → 追加エントリ）と `packs_dir/{book_id}.opfspack` への書き出し | `import/mod.rs:916-924` |
| 4 | `books` 行を組み立て（`file_name` = 取り込みファイル名、`file_size` = **元ファイルのバイト数**、`opfs_path` = `{book_id}.opfspack`、`cover_thumbnail` = `None`、`tbf_product_id`/`site_id` = `None`、`tags_fetched` = 1） | `import/mod.rs:927-956` |
| 5 | 再取り込み時は `is_favorite` / `is_hidden` / `created_at` を既存行から引き継ぎ、`documents::delete_for_book` + `contents::delete_for_book` の後に `books::upsert`。新規は `books::insert` | `import/mod.rs:958-981` |
| 6 | `imported_documents` 行（`source_type` = `pdf`/`epub`/`image-set`/`image`、`file_hash` = pack の SHA-256、`status` = `"completed"`、`metadata` = `None`） | `import/mod.rs:979-988`, `:1687-1691` |
| 7 | `book_contents` + `content_formats` を `contents::insert_batch`（1 トランザクション） | `import/mod.rs:990-1017`, `db/contents.rs:41-83` |
| 8 | `document_images`: ページ行（`image_type = "page"`、`opfs_path = {book_id}.opfspack`、`mime_type = "image/webp"`） | `import/mod.rs:1019-1056` |
| 9 | `document_images`: サムネイル行（`image_type = "thumbnail"`、`page_number = 1`、寸法は WebP をデコードして取得） | `import/mod.rs:1058-1085` |
| 10 | `documents::insert_images_batch`（画像 200 件ずつ / 1 トランザクション） | `import/mod.rs:1086`, `db/documents.rs:260-301` |
| 11 | `document_text`（PDF のページテキスト + `_export.txt` のパース結果）。200 件ずつ | `import/mod.rs:1094-1107`, `db/documents.rs:303-335` |
| 12 | `token_analysis`（ページテキストごとに `crate::tags::extract_nouns(text, &[title])` の名詞）。500 件ずつ | `import/mod.rs:1109-1128`, `db/documents.rs:338-371` |
| 13 | `book_tags`: `fetch_zenn_tags()` → `generate_tags(texts, &[title], zenn_tags)` → `tags::set_for_book(pool, book_id, &[(tag, "generated")])`（既存タグは全削除して入れ直す） | `import/mod.rs:1129-1137`, `db/tags.rs:18-44` |
| 14 | `ImportedBook { book, document, tags, warnings }` を返す | `import/mod.rs:1139-1145`, `:22-30` |

### 3.9 再取り込み・復元・改名

| 機能 | 内容 | アンカー |
|---|---|---|
| 再取り込み（同一 source） | `reuse_book_id` を渡すと `books.id` を再利用。既存 `documents` / `contents` を削除してから入れ直し、`is_favorite` / `is_hidden` / `created_at` と `content_id` / `display_name` を引き継ぐ | `import/mod.rs:891-981`, `db/books.rs:336-362` |
| 再利用 id の探索 | `(site_id, tbf_product_id)` 一致行のうち owner（暗号化 `owner_sub` を鍵で復号した sub）が一致するもの。未ログインは `owner_sub IS NULL` の行 | `db/books.rs:315-362` |
| Drive からの再構築 | `rebuild_from_pack`: 既にドキュメント行があれば `false`（何もしない）。`metadata.json` の `contents` を使い、無ければ `infer_contents` で推定（ページエントリがあれば `image`/`pages`、`.epub` なら `epub`、ルート直下の `.pdf` なら `pdf`）。`cover.webp` は行を作らない。主レンディションのページ数が 0 なら `false` | `import/mod.rs:1728-1800`, `:1960-1998` |
| 復元時の寸法 | 画素デコードせず `image::ImageReader::into_dimensions()` でヘッダのみ読む | `import/mod.rs:2020-2028` |
| ページ番号の復元 | パス末尾の `page_NNNN` から数値化（`page_` + 数字） | `import/mod.rs:2013-2017` |
| コンテンツ名の改名 | DB（`book_contents.display_name`、最大 60 文字、空白畳み込み）と pack の `metadata.json` の両方に書く。pack は全エントリを読み直して再構築（`created_at` と pack フラグ・エントリの compress フラグを維持） | `db/contents.rs:167-213`, `import/mod.rs:737-786` |

### 3.10 進捗コールバック

| 事実 | 内容 | アンカー |
|---|---|---|
| 型 | `&mut (dyn FnMut(f32) + Send)`（WebP エンコードを 8 スレッドに流すため `+ Send` が必要） | `import/mod.rs:1163`, `pdf.rs:86` |
| 値域 | 0.0〜1.0（アプリは `(fraction * 100.0).round()` で % 表示） | `pdf.rs:157-159`, `crates/app/src/views/bookshelf.rs:3080`, `:4377`, `:5613` |
| 呼ばれる条件 | ZIP 経路では**既定表示コンテンツの PDF レンディションのみ**（他は副作用回避のため `None` を渡す）。画像・EPUB では呼ばれない | `import/mod.rs:1572-1577`, `:1402-1410` |
| 単位 | PDF のページ単位（**エンコードが終わったページ数 / 全ページ**） | `pdf.rs:157-159` |

---

## 4. `.opfspack` ファイル形式

### 4.1 概要

| 項目 | 値 | アンカー |
|---|---|---|
| 実装 | Rust 移植（TS 参照実装 `packages/opfspack`（thundoku-shelf Web 側）とバイト互換が要件） | `crates/opfspack/src/lib.rs:1-24` |
| マジック | `OPFS` = `[0x4F, 0x50, 0x46, 0x53]`（offset 0） | `crates/opfspack/src/lib.rs:39` |
| ヘッダサイズ | `HEADER_SIZE = 64` バイト固定 | `crates/opfspack/src/lib.rs:41` |
| バージョン | `FORMAT_VERSION = 3`（**これ以外は `PackError::Version`**。v2 の pack は読めない＝再取り込み） | `crates/opfspack/src/lib.rs:43`, `reader.rs:22-24` |
| エンディアン | すべて little-endian | `crates/opfspack/src/lib.rs:7-8`, `format.rs:34-53` |
| レイアウト | `[header 64B][entry payload × N（8 バイト境界に整列）][index][index CRC 4B]` | `crates/opfspack/src/builder.rs:96-158` |
| 圧縮 | RAW DEFLATE（RFC 1951、zlib ヘッダ無し。fflate `deflateSync` 互換）、`flate2::Compression::new(6)` | `crates/opfspack/src/builder.rs:176-181`, `:70` |
| 暗号化 | 任意（`PackKey` を渡したときのみ＝**全エントリ**）。AES-256-GCM（AAD なし）、エントリ単位 | `crates/opfspack/src/builder.rs:74-80`, `crypto.rs:40-45` |
| 鍵（v3） | アカウントごとの乱数ルート鍵（PRK）→ `HKDF(PRK, pack_id)` で冊ごとの pack 鍵。PRK はラップして keyring と Drive に置く | `crates/opfspack/src/keys.rs`, `docs/spec/10-pack-keys.md` §2〜§3 |
| チェックサム | CRC-32（IEEE、`crc32fast`）。ヘッダ（0..60）と index の 2 か所 | `crates/opfspack/src/crypto.rs:19-21`, `format.rs:71-73`, `builder.rs:148-158` |

### 4.2 ヘッダ（64 バイト）フィールド表

| オフセット (byte) | サイズ | 型 | 内容 | アンカー |
|---|---|---|---|---|
| 0 | 4 | bytes | マジック `OPFS` | `format.rs:58` |
| 4 | 4 | u32 LE | `version`（= 3） | `format.rs:59` |
| 8 | 4 | u32 LE | `flags`（pack レベル） | `format.rs:60` |
| 12 | 4 | — | 予約（0） | `format.rs:61` |
| 16 | 8 | u64 LE | `index_offset`（index 先頭の絶対オフセット） | `format.rs:62` |
| 24 | 8 | u64 LE | `index_size`（**末尾 4 バイトの CRC を含む**） | `format.rs:63` |
| 32 | 4 | u32 LE | `entry_count` | `format.rs:64` |
| 36 | 4 | — | 予約（0） | `format.rs:65` |
| 40 | 8 | u64 LE | `created_at`（UNIX ミリ秒） | `format.rs:66` |
| 48 | 12 | — | 予約（0） | `format.rs:67` |
| 60 | 4 | u32 LE | CRC-32（bytes 0..60 に対して） | `format.rs:68-73` |

### 4.3 インデックスエントリ（可変長・8 バイト整列）

| フィールド順 | サイズ | 型 | 内容 | アンカー |
|---|---|---|---|---|
| `pathLen` | 2 | u16 LE | パスのバイト長 | `format.rs:109-112` |
| `path` | 可変 | UTF-8 | エントリパス | `format.rs:113-115` |
| `mimeLen` | 2 | u16 LE | MIME のバイト長 | `format.rs:116-118` |
| `mime` | 可変 | UTF-8 | MIME タイプ | `format.rs:119-121` |
| `offset` | 8 | u64 LE | 本体先頭の絶対オフセット | `format.rs:122-124` |
| `size` | 8 | u64 LE | **元の（展開・復号後の）サイズ** | `format.rs:125-127` |
| `compressedSize` | 8 | u64 LE | 保存サイズ（圧縮・暗号化後） | `format.rs:128-130` |
| `flags` | 4 | u32 LE | エントリフラグ | `format.rs:131-133` |
| `iv` | 12 | bytes | AES-GCM nonce（非暗号エントリは全 0） | `format.rs:134` |
| パディング | 0-7 | zero | エントリ長を 8 バイト境界へ（`align8`） | `format.rs:30-32`, `:114-117` |

計算式（そのまま再実装可能）: 生サイズ = `2 + path.len() + 2 + mime.len() + 8 + 8 + 8 + 4 + 12` = `44 + path.len() + mime.len()`、保存サイズ = `align8(生サイズ)`。
検算（フィクスチャ）: `metadata.json`(13) + `application/json`(16) → 44+29 = 73 → align8 = **80 バイト**（`crates/opfspack/tests/interop.rs:77` の `index_size = 80+80+80+4` と一致）。

インデックス全体 = 全エントリの整列済みエントリ連結 + 末尾 CRC-32 4 バイト（CRC は エントリ連結部のみを対象、`index_size` に含む）: `builder.rs:143-158`, `reader.rs:38-46`。

### 4.4 フラグ定義

| スコープ | 定数 | 値 | 意味 | アンカー |
|---|---|---|---|---|
| pack | `pack_flags::NONE` | `0` | フラグ無し | `lib.rs:77` |
| pack | `pack_flags::COMPRESSED` | `1 << 0` | 構築時に全体圧縮が有効だった | `lib.rs:79` |
| pack | `pack_flags::ENCRYPTED` | `1 << 1` | `PackBuilder::build` が `PackKey` を渡されたとき立つ（＝全エントリが暗号化されている） | `lib.rs:82`, `builder.rs:110-115` |
| pack | `pack_flags::KNOWN_MASK` | `COMPRESSED \| ENCRYPTED` | 定義済みビット。これ以外が立つヘッダは `Corrupted("unknown pack flags: …")`（Header CRC の直後、version 検査より前） | `lib.rs:85`, `format.rs:96-101` |
| entry | `entry_flags::NONE` | `0` | フラグ無し | `lib.rs:91` |
| entry | `entry_flags::COMPRESSED` | `1 << 0` | ペイロードが RAW DEFLATE 圧縮 | `lib.rs:93` |
| entry | `entry_flags::ENCRYPTED` | `1 << 1` | ペイロードが pack 鍵（`PackKey`）で AES-256-GCM 暗号化 | `lib.rs:95` |
| entry | `entry_flags::IDENTITY_BOUND` | `1 << 2` | **v2 のみ**（復号に `Identity` が必要、の意味）。v3 の鍵スケジュールは header version で選ぶので**定義済みビットではなく**、v3 で立っていれば `Corrupted("unknown entry flags …")` として拒否する | `lib.rs:96-99`, `reader.rs:222-233` |
| entry | `entry_flags::LZ4` | `1 << 3` | LZ4 圧縮。**TS 実装は生成しない**。読み出すと `PackError::UnsupportedLz4` | `lib.rs:102`, `reader.rs:87-89` |
| entry | `entry_flags::KNOWN_MASK` | `COMPRESSED \| ENCRYPTED \| LZ4`（**`IDENTITY_BOUND` を含まない**） | 定義済みビット。これ以外が立つエントリは index 解析時に `Corrupted("unknown entry flags …")` | `lib.rs:105`, `reader.rs:227-233` |

v3 の builder が立てるのは `COMPRESSED`（`add_entry` の `compress`）と `ENCRYPTED`（鍵があるとき。全エントリ共通）だけで、`IDENTITY_BOUND` は決して立てない（`builder.rs:66-80`）。ヘッダの `ENCRYPTED` と実際のエントリが食い違う pack（混在・不宣言）は index 解析時に拒否する（§4.6 の順 15）。**エントリ単位の「片方だけ」検査（v2 の `ENCRYPTED` と `IDENTITY_BOUND` の同時必須）は v3 には無い**（`IDENTITY_BOUND` は未知ビットとして弾かれる）。

### 4.5 鍵導出（v3 = 乱数ルート鍵 + ラップ）

**v3 の鍵材料はアカウントごとの乱数ルート鍵（PRK）**で、`sub` からは導出できない。PRK は
32 バイトの乱数（`OsRng`）で、**ラップ**（AES-256-GCM で包んだもの）として端末の keyring と
Drive に置く。ラップの形式・保管・解決順序は `docs/spec/10-pack-keys.md` §2〜§5 が正。

| 段 | 定義 | アンカー |
|---|---|---|
| PRK（ルート鍵） | 32 バイトの乱数（アカウント = `owner_id` ごとに 1 個）。`PackRootKey::generate()` が `OsRng` で作る | `keys.rs:66-72`, `:84-89` |
| pack 鍵 | `HKDF-SHA256(ikm = PRK, salt = UTF8(pack_id), info = HKDF_INFO, 32 bytes)` → `PackKey` | `keys.rs:103-110`, `crypto.rs:31-37` |
| `pack_id` | 本の id（`book_id`）。**先に `book_id` を決めてから** `PRK.derive_pack_key(&book_id)` を呼ぶ | `import/mod.rs:214-227`, `:1008-1009`, `drive/sync.rs:107-109` |
| HKDF info | `HKDF_INFO = b"opfspack-entry-key"`（v2 と同一） | `crypto.rs:16` |
| `sub` ラップの KEK | `PBKDF2-SHA256(password = UTF8(sub) ‖ APP_SALT, salt = APP_SALT, iterations = 100_000, 32 bytes)` ＝ **v2 の master key と同式**（Web 版の既存 `deriveMasterKey` をそのまま使える） | `keys.rs:45-49`, `APP_SALT` = `lib.rs:46`, `SUB_WRAP_ITERATIONS` = `lib.rs:48` |
| パスフレーズラップの KEK | `PBKDF2-SHA256(password = NFKC(passphrase) の UTF-8, salt = ラップごとの乱数 16B, iterations = ラップに記録した値（既定 600_000）, 32 bytes)` | `keys.rs:54-57`, `PASSPHRASE_WRAP_ITERATIONS` = `lib.rs:51` |
| ラップ | `AES-256-GCM(KEK)(PRK)`。nonce は 12 バイト乱数、暗号文 = 32 + 16（tag）= **48 バイト固定**。AAD = `UTF8("thundoku-pack-root:1:" + owner_id)`（別アカウントのラップへの差し替えを検出） | `keys.rs:158-278`, `keys.rs:28-29`, `:40` |
| ラップの保存 | 端末 = OS keyring の **`thundoku-shelf.pack-root-key:<owner_id>`**（値は PRK の base64）／Drive = **`thundoku-keys.json`**（`owner_id` ごとの bundle。`thundoku-backup.json` と同じフォルダ） | `secrets.rs`（`save_pack_root_key` / `load_pack_root_key`）, `crates/core/src/pack_keys.rs:23`, `docs/spec/10-pack-keys.md` §3 |
| エントリ暗号 | `AES-256-GCM(key = pack 鍵, AAD なし)`、IV は `OsRng` で毎回 12 バイト乱数、暗号文 = `ciphertext ‖ tag`（tag 16 バイト。フィクスチャ検算: `compressed_size = size + 16`） | `builder.rs:74-80`, `crypto.rs:40-45`, `tests/interop.rs:325-331` |
| 鍵のキャッシュ用 API | `PackRootKey::derive_pack_key(&self, pack_id) -> PackKey`（本ごとに 1 回導出して使い回す。ページごとの再計算を避ける） | `keys.rs:103-110`（呼び出し: `import/mod.rs:1008`, `drive/sync.rs:108`） |
| 所有者 ID | `SHA-256("opfspack:v1:{sub}")` の小文字 hex（TS `deriveOwnerId` と一致。keyring のスロット名とラップの AAD に使う）。テストベクタ: `derive_owner_id("test-sub") == "6366bfc3b6ab37feaf2adb385aeaa515c4aa52cf09e70cac890d888e4409f3b0"` | `lib.rs:158-162`, `tests/interop.rs:381-386` |
| テストベクタ | `pack_key v3 = HKDF(PRK, "test-pack")` = `9c65c870…058f7cd6`、`sub` ラップの暗号文 = `9c7b7700…e0e094fa`（`docs/spec/10-pack-keys.md` §8 の表）。`tests/keys_v3.rs` がこの値を固定している | `tests/keys_v3.rs`, `docs/spec/10-pack-keys.md` §8 |

- **v2 の導出（`sub` → master key → `HKDF(master, pack_id)`）は廃止**。v2 の pack は読めない
  （`PackError::Version(2)`。§10 §6 も同じ）。旧 pack の再暗号化はせず、**再取り込み**を案内する
  （取り込み直せば v3 で保存される）。したがって **`Identity` / `derived_pack_key` は公開 API から
  削除されている**（呼び出し側は `PackRootKey` / `PackKey` を渡す形へ移行中）。
- v3 の pack は**鍵材料も識別子も持たない**（`pack_id` も `sub` も埋め込まない）。別アカウントの
  pack を開こうとしたときの挙動は v2 と同じ（AES-GCM のタグ不一致 →
  `Corrupted("decryption failed: {path}")`）。
- `sub` ラップは「Google ログインだけで読める」ための利便性の経路で、**bundle が漏れれば
  `sub` を知る相手に解ける**。強度を決めるのはパスフレーズラップの有無（正直な強度の表は
  §10 §1.2）。`thundoku-keys.json` に `sub` そのものは入らない（`owner_id` は一方向のハッシュ）。


### 4.6 検証順序（`PackReader::open`）

| 順 | 検証 | 失敗時 | アンカー |
|---|---|---|---|
| 1 | バッファ長 ≧ 64 | `Corrupted("buffer too small for header")` | `format.rs:80-82` |
| 2 | マジック一致 | `Corrupted("invalid magic number: expected OPFS")` | `format.rs:83-87` |
| 3 | ヘッダ CRC-32 | `Corrupted("header checksum mismatch: stored=…, computed=…")` | `format.rs:88-94` |
| 4 | pack フラグに未知ビットが無い（`pack_flags::KNOWN_MASK`） | `Corrupted("unknown pack flags: …")` | `format.rs:95-102` |
| 5 | `version == 3`（**v2 は `Version(2)` で拒否**＝再取り込みを案内） | `Version(stored)` | `reader.rs:22-24` |
| 6 | `index_offset + index_size` がバッファ内（オーバーフロー検査付き） | `Corrupted("index offset overflow")` / `Corrupted("index extends beyond buffer")` | `reader.rs:25-32` |
| 7 | `index_size >= 4` | `Corrupted("index too small for CRC")` | `reader.rs:33-35` |
| 8 | index CRC-32 | `Corrupted("index CRC mismatch: …")` | `reader.rs:36-42` |
| 9 | `entry_count` が index 長から導ける上限以内（1 エントリ ≧ 48 バイト） | `Corrupted("entry_count N exceeds index capacity M")` | `reader.rs:177-182`（`MIN_INDEX_ENTRY_SIZE` = `reader.rs:162`） |
| 10 | `entry_count <= MAX_ENTRY_COUNT`（= 10,000） | `Corrupted("entry_count N exceeds limit 10000")` | `reader.rs:184-189` |
| 11 | 各エントリが `entry_count` 件読める | `Corrupted("index truncated: expected N entries")` | `reader.rs:193-199` |
| 12 | 各エントリのフラグ整合（未知ビットが無い = **`IDENTITY_BOUND` を含む v3 に存在しないビットは拒否**） | `Corrupted("unknown entry flags …")` | `reader.rs:222-233` |
| 13 | 各エントリのサイズ上限（`size <= MAX_ENTRY_SIZE` / `compressed_size <= MAX_STORED_ENTRY_SIZE` / 合計 `<= MAX_TOTAL_SIZE`。合計は `checked_add`） | `Corrupted("entry size … exceeds limit …")` / `Corrupted("stored size … exceeds limit …")` / `Corrupted("total size … exceeds limit …")` / `Corrupted("total size overflow: {path}")` | `reader.rs:234-262` |
| 14 | 各エントリの `offset >= 64` かつ `offset + compressed_size <= index_offset` | `Corrupted("entry extends beyond body: {path}")` / `Corrupted("entry offset overflow: {path}")` | `reader.rs:204-215` |
| 15 | header の `ENCRYPTED` と各エントリの暗号化が一致（全エントリ読了後） | `Corrupted("encrypted pack contains a plaintext entry")` / `Corrupted("pack contains encrypted entries without the ENCRYPTED flag")` | `reader.rs:216`, `:264-278` |

順 9〜15 は `parse_index_entries`（`reader.rs:167-220`）の中で、**`Vec` の確保（順 9・10 は `Vec::with_capacity` より前）と本体の読み出しより前**に効く。`PackFileReader::open`（`reader.rs:331-378`）も順 1〜15 を共有する（順 1 だけ「バッファ長」ではなく「ファイル長 ≧ 64」で判定し、以降は同じ経路・同じ上限）。

読み出し時: `NotFound(path)` → LZ4 判定（`UnsupportedLz4`）→ **`ENCRYPTED` なら鍵必須**（無ければ `KeyRequired(path)`、タグ不一致は `Corrupted("decryption failed: {path}")`）→ `COMPRESSED` なら RAW DEFLATE 展開（`raw_inflate(data, entry.size)` が**宣言サイズ + 1 バイトで打ち切る**。超過・失敗は `Corrupted("decompression failed: …")`）→ **復号・展開後の実長が `entry.size` と一致しなければ `Corrupted("entry size mismatch: …")`**（圧縮・非圧縮・暗号化の全経路。`reader.rs:281-303`）。`read_entry_range` は `start >= end || end > entry.size` で `InvalidRange`、さらに復号結果の実長でも再検査して範囲外なら `Corrupted`（宣言 `size` を信じてスライスしない＝細工した index でも panic しない。`reader.rs:107-138`）。

### 4.6.1 index 解析時の上限（資源上限）

index の `size` / `compressed_size` / `entry_count` は攻撃者が自由に書ける（ヘッダ CRC も計算できるので改竄検知にはならない）。そのため「`Corrupted` を返す」だけでなく、**確保・読み出しの前**に上限と突き合わせる（§4.6 の順 9・10・13）。定数は `lib.rs:58-72`。

| 定数 | 値 | 対象 | 正常な本を通す根拠 |
|---|---|---|---|
| 書き出し側の検査 | `PackBuilder` も同じ上限（`MAX_ENTRY_COUNT` / `MAX_ENTRY_SIZE` / `MAX_STORED_ENTRY_SIZE` / `MAX_TOTAL_SIZE`）を検査する。これが無いと「書けたのに開けない pack」ができる | - | `crates/opfspack/src/builder.rs`（`write_pack` / `check_entry_limits`） |
| `MAX_ENTRY_SIZE` | `512 * 1024 * 1024`（512 MiB） | 1 エントリの展開後 `size` | 1 エントリ = 1 ページで、既知の実データ（外側 ZIP の展開後 1.33 GB。`import/mod.rs:265`, `:1585`）でも 1 ページは数 MB 級 |
| `MAX_TOTAL_SIZE` | `2 * 1024 * 1024 * 1024`（2 GiB） | 1 pack の展開後合計（`checked_add` で積算） | 上記 1.33 GB を上回り、かつ上限として意味のある範囲 |
| `MAX_STORED_ENTRY_SIZE` | `2 * 1024 * 1024 * 1024`（2 GiB） | 1 エントリの保存サイズ `compressed_size` | 展開上限 512 MiB に AES-GCM タグ 16 バイトと DEFLATE の膨張余地を足しても収まる |
| `MAX_ENTRY_COUNT` | `10_000` | index のエントリ数 | 既知の最大 3,321 ページ本（`crates/app/src/components/image_viewer/mod.rs:948`）は 1 ページ = 1 エントリ（§4.8）なので 3,000 件超。その約 3 倍を許す |

`raw_inflate` が宣言サイズ + 1 バイトで打ち切る（§4.6 の読み出し時）ことと併せて、小さな DEFLATE 入力から数 GB を展開させる細工への防波堤になる。回帰テストは `crates/opfspack/tests/limits.rs`（展開爆弾 / 宣言サイズの不一致（大・小）/ 切断 DEFLATE / 未知フラグ / 暗号化の不整合 / 件数・サイズ上限の境界と上限ちょうど / 正常な多ページ pack）。

> **取り込み側の上限とは別物**: §3.3 の `MAX_ZIP_ENTRY_BYTES = 2 GiB` / `MAX_NESTED_BYTES = 512 MiB` / `MAX_NESTED_ENTRIES = 2000`（`crates/core/src/import/mod.rs`）は **ZIP 取り込み時**の上限で、こちらは **pack 読み出し時**の検証。判定対象（ZIP エントリ / 入れ子の合流 vs pack の index）も失敗の扱い（警告を積んでスキップ vs `Corrupted` を返す）も異なる。値を揃えたのは 1 エントリあたり 512 MiB だけで、件数上限は **`MAX_NESTED_ENTRIES = 2000` に合わせられない**（`MAX_NESTED_ENTRIES` は入れ子 ZIP の合流件数であって pack のエントリ数ではなく、3,321 ページの pack は 3,000 件超のエントリを持つため、2000 で切ると正常本が読めなくなる）。

### 4.7 公開 API

| API | シグネチャ要点 | アンカー |
|---|---|---|
| `PackBuilder::new(created_at: u64)` | pack 構築開始（`created_at` は UNIX ミリ秒） | `builder.rs:37-43` |
| `PackBuilder::add_entry(path, data, mime_type, compress)` | エントリ追加（重複パスの検査はしない）。追加順は保持され、build 時にパスでソート | `builder.rs:44-53` |
| `PackBuilder::build_to_file(path, key, compress) -> Result<u64, PackError>` | [`PackBuilder::build`] と同じレイアウトを**ファイルへ直接**書く（組み立て中のバイト列を RAM に持たない）。一時ファイルへ書いてから rename するため、途中で失敗しても壊れた pack を残さない。書き出したバイト数を返す | `builder.rs:68-99`, `tests/build_to_file.rs` |
| `PackBuilder::build(key: Option<&PackKey>, compress: bool) -> Result<Vec<u8>, PackError>` | ① パスを **UTF-16 コード単位順**にソート ② エントリ単位で deflate（`compress`）→ 暗号化（`key` があるとき**全エントリ**） ③ オフセット確定（64 から 8 バイト整列で連結） ④ ヘッダ → 本体 → index → index CRC。pack フラグは `compress` → `COMPRESSED`、`key.is_some()` → `ENCRYPTED`。空 pack も生成可能（`entry_count = 0`） | `builder.rs:55-159`, `:110-115`, `tests/interop.rs:352-359` |
| `PackReader::open(bytes) -> Result<Self, PackError>` | 全検証を実行（§4.6。件数・サイズの上限は §4.6.1）。`version != 3` は `Version` | `reader.rs:18-54` |
| `PackReader::header() / entries() / entry(path)` | ヘッダ参照・エントリ一覧・パス検索 | `reader.rs:57-67` |
| `PackReader::read_entry(path, key: Option<&PackKey>) -> Result<Vec<u8>, PackError>` | 復号 + 展開した全バイト。暗号化エントリで鍵が無ければ `KeyRequired`。実長が `size` と一致しなければ `Corrupted` | `reader.rs:69-74` |
| `PackReader::read_entry_with_key(path, key: Option<&PackKey>)` | 事前導出鍵（＝本ごとの pack 鍵）を使う版。`read_entry` はこれへ委譲する（`HKDF` を毎ページ回さない） | `reader.rs:77-102` |
| `PackReader::read_entry_range(path, start, end, key: Option<&PackKey>)` | 範囲読み出し（宣言 `size` と**復号後の実長**の両方で検証。実装は全読みしてスライス） | `reader.rs:105-134` |
| `PackFileReader::open(path)` / `read_entry_with_key(path, key)` | ファイル裏打ちの読み出し（pack 全体をメモリに載せない）。検証は `PackReader` と同じ §4.6 の順 1〜15 | `reader.rs:331-378`, `:388-409` |
| `PackRootKey::derive_pack_key(&self, pack_id: &str) -> PackKey` | 冊ごとの pack 鍵 = `HKDF(PRK, pack_id)`。**鍵キャッシュ用**（本ごとに 1 回導出して使い回す）。旧 `derived_pack_key(&Identity)` の置き換え | `keys.rs:103-110`（呼び出し: `crates/core/src/import/mod.rs:1008`, `:828`, `crates/core/src/drive/sync.rs:108`） |
| `PackRootKey::generate() / from_bytes / as_bytes / to_base64 / from_base64` | PRK の生成（`OsRng`）と keyring 保存用の base64 変換。`Debug` は中身を出さない | `keys.rs:66-101` |
| `PackKey::as_bytes(&self) -> &[u8; 32]` | AES-256-GCM の鍵。`Debug` は中身を出さない | `keys.rs:115-127` |
| `sub_wrap_kek(sub) -> [u8; 32]` | `sub` ラップの KEK（v2 の master key と同式。§4.5） | `keys.rs:45-49` |
| `PackRootKeyWrap`（`wrap_with_kek` / `wrap_with_sub` / `wrap_with_passphrase` / `unwrap` / `unwrap_with_passphrase` / `kind` / `iterations` / `nonce` / `ciphertext`） | PRK を KEK で包んだ 1 個のラップ（仕様 §3.2）。`ciphertext` は常に 48 バイト | `keys.rs:131-278` |
| `PackKeyBundle`（`new` / `format_version` / `owner_id` / `created_at` / `updated_at` / `wraps` / `wrap` / `has_wrap` / `upsert_wrap` / `remove_wrap` / `touch` / `unwrap_with_sub` / `unwrap_with_passphrase` / `to_json` / `from_json`） | Drive の `thundoku-keys.json`（仕様 §3.2）。`from_json` は `format_version != 1` で `UnsupportedKeyBundle`、構造不正で `Corrupted` | `keys.rs:280-400`, `KEY_BUNDLE_FORMAT_VERSION` = `keys.rs:24` |
| `derive_owner_id(sub)` | 所有者 ID（hex）。keyring のスロット名とラップの AAD に使う | `lib.rs:158-162` |
| `PackHeader { version, flags, index_offset, index_size, entry_count, created_at }` | — | `lib.rs:110-117` |
| `PackEntry { path, mime_type, offset, size, compressed_size, flags, iv }` | — | `lib.rs:121-133` |
| 上限定数（`MAX_ENTRY_SIZE` / `MAX_TOTAL_SIZE` / `MAX_STORED_ENTRY_SIZE` / `MAX_ENTRY_COUNT`） | pack 読み出し時の資源上限（§4.6.1） | `lib.rs:58-72` |

アプリ（core）側の入口:

| API | シグネチャ要点 | アンカー |
|---|---|---|
| `import::pack_root_key_for_import(profile, resolve_root)` | 取り込みで使う PRK を決める（**fail-closed**）。未ログイン（`profile` = `None`）→ `ImportError::LoginRequired`（**平文 pack を作らない** — セキュリティ評価 F03）、ログイン済み + 鍵なし → `ImportError::IdentityKeyUnavailable`、`sub` 空 → `ImportError::IdentitySubMissing` | `crates/core/src/import/mod.rs:248-261` |
| `PackKeyStore`（`resolve` / `ensure` / `set_passphrase` / `remove_passphrase` / `has_passphrase` / `pending_owner` / `retry_pending_upload`） | PRK の解決（keyring → パスフレーズラップ → `sub` ラップ）と bundle の Drive 反映（仕様 §4〜§5）。**パスフレーズはコールバックで受け取る**（core は UI を知らない） | `crates/core/src/pack_keys.rs:61-345` |
| `pack_keys::{KEY_BUNDLE_NAME, PENDING_UPLOAD_KEY}` | `"thundoku-keys.json"` / `"drive.pack_keys.pending"` | `crates/core/src/pack_keys.rs:23`, `:29` |
| `PackKeysError` | `Unavailable` / `PassphraseFailed` / `OwnerMismatch` / `Secret` / `Drive` / `Pack` / `Db`。`ImportError` への変換は `IdentityKeyUnavailable` / `PassphraseFailed` / `KeyStore` | `crates/core/src/pack_keys.rs:37-58`, `crates/core/src/import/mod.rs:98-109` |

### 4.8 アプリが書き出すエントリ構成（実データ）

| pack 種別 | エントリ（ソート後 = UTF-16 順で並ぶ） | 出典 |
|---|---|---|
| 画像 ZIP（既定コンテンツ第 1 レンディション） | `metadata.json`, `pages/page_0001.webp` … `pages/page_NNNN.webp`, `thumbnail.webp` | `import/mod.rs:1002`, `:1639-1645`, `:1753-1758` |
| 複数コンテンツ/レンディション | `metadata.json`, `pages/…`（既定の第 1 のみ）, `contents/{c}/r{r}/page_0001.webp` …, `thumbnail.webp` | `import/mod.rs:1605-1610`, `:1639` |
| PDF 単体 | `metadata.json`, `pages/page_0001.webp` …, `cover.webp`, `thumbnail.webp` | `import/mod.rs:1324-1356` |
| ZIP 内の PDF（既定レンディション） | 同上（`pages/…`）+ `cover.webp`（`primary_cover` が立つときだけ） | `import/mod.rs:1671-1684`, `:1744-1751` |
| EPUB 単体 | `metadata.json`, `<元ファイル名>.epub`（圧縮なしの生エントリ） | `import/mod.rs:1394-1400` |
| ZIP 内の EPUB | `metadata.json`, `<元ファイル名>.epub`（既定レンディション）または `contents/{c}/r{r}/<元ファイル名>.epub` | `import/mod.rs:1699-1711` |
| 画像 1 枚 | `metadata.json`, `pages/page_0001.webp`, `thumbnail.webp` | `import/mod.rs:2174-2196` |
| 音声/動画のみを含む ZIP | 当該コンテンツはエントリを作らない（`page_count = 0`）。主コンテンツなら `NotAReadableWork` | `import/mod.rs:1712-1713`, `:1570-1573` |
| pack レベルフラグ | 常に `pack_flags::COMPRESSED`（`build(pack_key.as_ref(), true)`）、鍵があるとき（＝ログイン中の取り込み）のみ `ENCRYPTED`（**全エントリ**） | `import/mod.rs:1009`, `builder.rs:111-117` |
| エントリの compress | **すべて `false`**（metadata.json / ページ / 表紙 / サムネ / EPUB 生データ） | `import/mod.rs:1002`, `:1640-1645`, `:1672-1677`, `:1703-1710`, `:1746-1758`, `:1330-1335` |
| 暗号化の伝播 | `rename_content_in_pack` は元 pack の `ENCRYPTED` フラグを見て `pack_key` の有無を決め、エントリごとの `COMPRESSED` フラグも維持して再構築する | `import/mod.rs:855-870` |
| 検算例（フィクスチャ） | `plain.opfspack`: 3 件・pack フラグ `COMPRESSED`・index 244 B・`created_at = 1_728_000_000_000`（`metadata.json` 62 B 非圧縮 / `pages/page_0001.webp` 12,019 B → 圧縮 56 B / `pages/page_0002.webp` 6,019 B 非圧縮）。`encrypted.opfspack`: 同じ 3 件・pack フラグ `COMPRESSED \| ENCRYPTED`・各エントリの保存サイズは +16 B（暗号タグ）で `flags = ENCRYPTED`（`IDENTITY_BOUND` は立たない） | `crates/opfspack/tests/interop.rs:61-108`, `:151-186`（実測値は §4.10 の表と同じ） |

### 4.9 `metadata.json`（pack 内メタデータ）

`finish_import` が必ず `metadata.json` を 1 件目として書く（`import/mod.rs:1002`）。内容は `metadata_entry()` が生成（`import/mod.rs:765-808`）:

| キー | 型 | 値 / 由来 | アンカー |
|---|---|---|---|
| `schemaVersion` | number | `1`（固定） | `import/mod.rs:797` |
| `title` | string | ファイル名から拡張子を除いたもの（ZIP に PDF/EPUB が 1 つだけの場合は内側エントリ名から採る） | `import/mod.rs:758-763`, `:1680-1684` |
| `author` | string | `""`（取り込み時は常に空。後から `books::set_metadata` で補完） | `import/mod.rs:799`, `db/books.rs:166-189` |
| `circleName` | string | `""` | `import/mod.rs:800` |
| `purchaseDate` | null | `null` | `import/mod.rs:801` |
| `readingProgress.currentPage` | number | `0` | `import/mod.rs:802` |
| `readingProgress.totalPages` | number \| null | 主コンテンツのページ数 | `import/mod.rs:802` |
| `contents[]` | array | 1 コンテンツ 1 要素（下記） | `import/mod.rs:770-795` |
| `contents[].contentId` | string | `book_contents.content_id`（UUID v4） | `import/mod.rs:774` |
| `contents[].displayName` | string | 表示名（`本文` / フォルダ名 / ファイル名 / ユーザー設定名） | `import/mod.rs:775`, `:843-844` |
| `contents[].mediaKind` | string | `image` / `pdf` / `epub` / `audio` / `video` | `import/mod.rs:776`, `:747-756` |
| `contents[].isPrimary` | bool | 既定表示コンテンツか | `import/mod.rs:777` |
| `contents[].sortOrder` | number | コンテンツの並び順（0 始まり） | `import/mod.rs:778` |
| `contents[].formats[]` | array | レンディション。`formatId` / `label` / `formatKind` / `pageCount` / `packEntryPrefix` / `sortOrder` | `import/mod.rs:782-793` |
| 復元側の対応 | — | `content_from_metadata` が同じキーを読み戻す。`mediaKind`/`formatKind` の未知値は `None`（そのコンテンツ/レンディションを捨てる） | `import/mod.rs:1999-2050`, `:2093-2103` |
| 旧 pack 互換 | — | `contents` が無い pack は `infer_contents` が 1 コンテンツを推定（`pages/page_*` があれば `image`/接頭辞 `pages`、`.epub` なら `epub`、ルート直下 `.pdf` なら `pdf`。それ以外は空 → 復元しない） | `import/mod.rs:2052-2091` |

### 4.10 相互運用の担保

| 事実 | アンカー |
|---|---|
| 参照実装は TS（thundoku-shelf の `packages/opfspack`）で、**バイトレイアウト互換が要件**。v3 の TS 側対応はこれから（`docs/spec/10-pack-keys.md` §7 のチェックリスト） | `crates/opfspack/src/lib.rs:1-24` |
| **フィクスチャの生成元**: **v3 のフィクスチャは Rust の builder が生成**している（TS 版 v3 が入るまでは Rust 側が正）。バイトレイアウト自体は v2 時代の TS 実装（`gen-fixtures.ts`、bun + fflate 0.8.2）で検証済みで、差分は **header version と `IDENTITY_BOUND` の不在だけ** | `crates/opfspack/tests/interop.rs:1-22` |
| **フィクスチャ**: `plain.opfspack`（平文・pack フラグ `COMPRESSED`）、`encrypted.opfspack`（pack フラグ `COMPRESSED \| ENCRYPTED`・全エントリ暗号化）。どちらも `created_at = 1_728_000_000_000`、PRK は仕様 §8 のベクタ（`00 01 … 1f`）で `pack_id = "test-pack"`。**`identity-bound.opfspack`（v2）は削除済み** | `crates/opfspack/tests/interop.rs:35-58`, `:61-108`, `:151-186`, `tests/keys_v3.rs:9-20` |
| **v2 の拒否**: CRCs を直した v2 の pack（header version だけ 2 にしたもの）が `Version(2)` で拒否されることをテストで固定 | `crates/opfspack/tests/interop.rs:236-247` |
| **Rust → TS の書き出し確認用 CLI**: `examples/roundtrip_emit.rs`（`cargo run --example roundtrip_emit <out.opfspack>`。PRK = 仕様 §8 のベクタ → `derive_pack_key("test-pack")` を `build(Some(&key), true)` に渡す。`created_at = 1_700_000_000_000`。TS 側は bundle 無しでこのファイルを読める） | `crates/opfspack/examples/roundtrip_emit.rs:1-43` |
| **エントリの並び順一致**: Rust 側は UTF-16 コード単位比較でソート（JS の文字列比較と同一） | `builder.rs:161-176`, `tests/interop.rs:360-369` |

---

## 5. 数値定数一覧（画像 / 並列 / 上限 / バッチ / 形式）

### 5.1 取り込み（画像生成・並列・上限）

| 名前 | 値 | 単位 | 用途 / 備考 | アンカー |
|---|---|---|---|---|
| `PAGE_RENDER_CHUNK` | `64` | ページ | ZIP から一度に読み出して並列変換する単位（1 ページ約 0.5 MiB 想定で 1 チャンク約 30 MiB） | `import/mod.rs:162` |
| ページ変換ワーカー数 | `min(available_parallelism, 8, max(page_count,1))`、取得失敗時は 4 扱い | スレッド | 上限 8 はデコード済み画像（1 枚 数十 MB）のメモリ保護 | `import/mod.rs:168-178` |
| 小画像の拡大しきい値 | 幅 `1000` 未満を拡大（`scale = 1000/width`、`Lanczos3`、最小 1 px） | px | ページ画像・単体画像の両方 | `import/mod.rs:112-124`, `:155`, `:2164-2165` |
| ページ WebP 品質 | `88`（`clamp(0,100)`）と effort `2` | 品質 0-100 / effort 0-6 | lossy（`webp` crate = libwebp。effort は 2026-09-26 に 4 → 2。2.1 倍速・+2%） | `import/mod.rs`（`encode_webp` / `WEBP_METHOD`） |
| サムネイル幅 | `200` | px | 高さはアスペクト比で四捨五入・最小 1 | `import/mod.rs:135-142` |
| サムネイル WebP 品質 | `80` | 0-100 | 縮小フィルタは `Triangle` | `import/mod.rs:143-147` |
| `MAX_NESTED_DEPTH` | `1` | 階層 | 入れ子 ZIP の展開深さ | `import/mod.rs:261` |
| `MAX_NESTED_BYTES` | `512 * 1024 * 1024` = `536,870,912` | バイト (512 MiB) | 入れ子合流の非圧縮合計上限。宣言サイズと実読バイトの両方で判定 | `import/mod.rs:267`, `:407-420` |
| `MAX_NESTED_ENTRIES` | `2000` | 件 | 入れ子合流エントリ数上限 | `import/mod.rs:273` |
| パス区切り文字 | `['/', '\\', '／']` | — | ZIP エントリ名の分割（全角スラッシュ対応） | `import/mod.rs:551` |
| ページ番号の書式 | `page_{n:04}.webp` | — | n は 1 始まり・4 桁ゼロ埋め | `import/mod.rs:1639`, `:1326` |

### 5.2 PDF

| 名前 | 値 | 単位 | アンカー |
|---|---|---|---|
| PDF レンダリング目標幅 | `set_target_width(1000)`（高さ制限なし） | px | `pdf.rs:104-107` |
| PDF ページ WebP 品質 | `80` | 0-100 | `pdf.rs:147` |
| PDFium の直列化 | プロセス全体で `Mutex` 1 本（初期化を含む。`thread_safe` feature はロックしないため自前で排他） | — | `pdf.rs:80`, `:91-95` |
| PDFium の WebP エンコード並列度 | `8`（`chunk_size = total.div_ceil(8)`。PDFium を触らないので並列可） | スレッド | `pdf.rs:122-131` |
| PDFium ライブラリ探索順 | ① ビルド時の `CARGO_MANIFEST_DIR`（= `crates/core`） ② 実行ファイルと同じディレクトリ ③ macOS は実行ファイルの `../Frameworks`（`.app` の `Contents/Frameworks`）。**CWD は探索しない**（DLL 配置攻撃対策） | — | `pdf.rs` |

### 5.3 DB（接続・バッチ）

| 名前 | 値 | 単位 | アンカー |
|---|---|---|---|
| `busy_timeout` | `5` | 秒 | `db/mod.rs:55` |
| journal mode | `WAL` | — | `db/mod.rs:54` |
| foreign keys | `true` | — | `db/mod.rs:53` |
| tokio ランタイム worker | `2` | スレッド | `db/mod.rs:36` |
| テストプール最大接続 | `1` | 接続 | `db/mod.rs:467` |
| `document_images` バッチ | `200` 行 / INSERT 文 | 行 | `db/documents.rs:268` |
| `document_text` バッチ | `200` 行 / INSERT 文 | 行 | `db/documents.rs:311` |
| `token_analysis` バッチ | `500` 行 / INSERT 文 | 行 | `db/documents.rs:346` |
| `MAX_DISPLAY_NAME_CHARS` | `60` | 文字（バイトではない） | `db/contents.rs:168` |
| 改名時の空白処理 | 連続空白（改行・タブ含む）を半角スペース 1 個に畳む → 60 文字で切る → 末尾空白を除去 | — | `db/contents.rs:175-186` |
| `view_history.id` | `hex(randomblob(16))` = 32 文字 hex | — | `db/view_history.rs:26-28` |
| 非表示一覧の補正 | `is_hidden = 1 AND hidden_at IS NULL` の行に `CURRENT_TIMESTAMP` を書き込む | — | `db/bookshelf.rs:293-298` |

### 5.4 `.opfspack`

| 名前 | 値 | 単位 | アンカー |
|---|---|---|---|
| `MAGIC` | `OPFS` | 4 バイト | `crates/opfspack/src/lib.rs:39` |
| `HEADER_SIZE` | `64` | バイト | `lib.rs:41` |
| `FORMAT_VERSION` | `3`（v2 は読めない＝再取り込み） | — | `lib.rs:43` |
| 整列 | `8` バイト境界（`align8(n) = ceil(n/8)*8`） | バイト | `format.rs:30-32` |
| deflate レベル | `6`（RAW DEFLATE、zlib ラッパー無し） | — | `builder.rs:70`, `:176-181` |
| `APP_SALT` | `b"opfspack-v1-identity-salt-2024"`（v2 と同一。`sub` ラップの KEK に使う） | — | `lib.rs:46` |
| `SUB_WRAP_ITERATIONS` | `100_000` | 回 | `lib.rs:48` |
| `PASSPHRASE_WRAP_ITERATIONS` | `600_000`（暫定。ラップごとに記録するので後から上げられる） | 回 | `lib.rs:51` |
| `KEY_BUNDLE_FORMAT_VERSION` | `1`（`thundoku-keys.json` の `format_version`。pack の `FORMAT_VERSION` とは別物） | — | `keys.rs:24` |
| ラップの AAD | `UTF8("thundoku-pack-root:1:" + owner_id)` | — | `keys.rs:28`, `:58-60` |
| ラップの salt 長 | `16`（`kind=passphrase` のみ。`kind=sub` は `APP_SALT` 固定） | バイト | `keys.rs:34` |
| `nonce` 長 | `12` | バイト（ラップ・エントリ共通） | `keys.rs:37`, `crypto.rs:41-42` |
| ラップの暗号文長 | `48` = 32（PRK）+ 16（GCM タグ） | バイト | `keys.rs:40` |
| 鍵長 | `32` | バイト（AES-256） | `keys.rs:103-110`, `crypto.rs:24-29` |
| GCM タグ長 | `16` | バイト（暗号文末尾） | `tests/interop.rs:325-331` |
| index CRC | `4` | バイト（`index_size` に含む） | `reader.rs:38-45`, `builder.rs:148-149` |
| エントリ生サイズ式 | `44 + path.len() + mime.len()` | バイト | `format.rs:114-117` |

### 5.5 アプリ層（データ配置に関わるもの）

| 名前 | 値 | 単位 | アンカー |
|---|---|---|---|
| pack 配置 | `<data_dir>/packs/{book_id}.opfspack` | — | `crates/app/src/app_state.rs:139-142`, `import/mod.rs:1013-1019` |
| DB 配置 | `<data_dir>/thundoku-shelf.db` | — | `crates/app/src/app_state.rs:141` |
| 既定 data_dir | `dirs::data_dir()/thundoku-shelf` | — | `crates/app/src/app_state.rs:129-133` |
| サイト表紙キャッシュ | `<data_dir>/thumbnails/{site_id}_{database_id}_448.png`（448 px に縮小） | px | `crates/app/src/views/bookshelf.rs:7035-7045`, `:7123-7129` |
| 表紙ダウンロード並列度 | `4` | 並列 | `crates/app/src/views/bookshelf.rs:1715-1717` |
| 進捗表示の丸め | `(fraction * 100.0).round()` % | % | `crates/app/src/views/bookshelf.rs:2926-2928` |

---

## 6. エラー処理

### 6.1 `ImportError`（全 variant・定義と発生条件）

定義: `crates/core/src/import/mod.rs:34-92`。

| variant | `Display` 文字列（実装値） | 発生条件 | アンカー |
|---|---|---|---|
| `UnsupportedType(String)` | `unsupported file type: {0}` | `import_file` の拡張子が `pdf`/`epub`/`zip` 以外（文字列は小文字化した拡張子。拡張子なしは空文字） | `import/mod.rs:36-37`, `:1277` |
| `Io(std::io::Error)` | `io error: {0}` | ソース読み込み・pack 書き出し・ディレクトリ作成の失敗（`?` 伝播、保存領域外を指す pack パスの拒否を含む） | `import/mod.rs:38-39`, `:1014-1018` |
| `Pack(opfspack::PackError)` | `pack error: {0}` | pack 構築・読み出しの失敗（`builder.build` / `PackReader::open` / `read_entry`） | `import/mod.rs:40-41`, `:1009`, `:829`, `:861`, `:1835` |
| `Db(sqlx::Error)` | `database error: {0}` | すべての DB 書き込み・読み出し失敗（`books::*` / `documents::*` / `contents::*` / `tags::*` の `?` 伝播） | `import/mod.rs:42-43` |
| `Pdf(String)` | `pdf error: {0}` | `pdf.rs` の描画・読み込みエラーを文字列化（PDFium の `load` / `render` / RGB バッファ不正 / ライブラリのロード失敗 ほか）、および `import_rendered_pdf_pages` のページ 0 件 `"no pages rendered"` | `import/mod.rs:44-45`, `:1316`, `pdf.rs:57-79`, `:96-98`, `:110-116`, `:144-149`, `:166-183` |
| `Image(String)` | `image error: {0}` | ① `image::load_from_memory` の失敗（壊れた画像。ページ単位では警告に落ちるが、単体画像取り込みやサムネ生成では致命） ② 主コンテンツのページ数 0 `"primary content has no pages: {display_name}"` ③ 改名時の `metadata.json` 再シリアライズ失敗を `Image(e.to_string())` として流用 | `import/mod.rs:46-47`, `:136`, `:154`, `:859`, `:1740`, `:2163` |
| `Zip(String)` | `zip error: {0}` | `zip` crate のオープン/エントリ取得/伸長失敗、および内部不整合 `"entry index out of range"`（`plan` の ordinal が `metas` の範囲外） | `import/mod.rs:48-49`, `:296`, `:346`, `:1512`, `:1576` |
| `EmptyArchive` | `empty archive` | ZIP のエントリが 0 件（`collect_metas_with_nested` の結果が空） | `import/mod.rs:50-51`, `:1518` |
| `NotAReadableWork` | `not a readable work` | ① `commit_zip` で既定コンテンツが画像/PDF/EPUB 以外（音声・動画のみ） ② `plan.contents` が空（`get(plan.primary)` が `None`） | `import/mod.rs:54-55`, `:1569-1572`（UI 文言は `docs/import-patterns.md:687-691` §11.2 R3） |
| `LoginRequired` | `本を取り込むには Google にログインしてください（本はアカウントごとの鍵で暗号化されます）` | 未ログイン（Google のプロフィールが無い）で取り込みを要求された。**平文 pack を作らない**（fail-closed。セキュリティ評価 F03）。UI はダウンロードを始める前にこれを出し、ログイン導線（Google の認証モーダル）を開く | `import/mod.rs:55-65`, `:252-255`, `crates/app/src/views/bookshelf.rs:3822`（`require_import_login`） |
| `IdentityKeyUnavailable` | `Google にログイン済みですが、本を復号する鍵を取得できません。…`（長文。§10 §4.1 の fail-closed） | ログイン中なのに v3 のルート鍵（PRK）を用意できない（bundle が無い / どのラップも解けない / keyring 障害）。**平文 pack へは落とさない** | `import/mod.rs:66-76`, `:102` |
| `PassphraseFailed` | `パスフレーズが違います。もう一度入力してください` | 入力されたパスフレーズでラップが解けない（**`sub` ラップへ黙って落ちない**） | `import/mod.rs:77-79`, `:103` |
| `KeyStore(String)` | `本の鍵を取得できませんでした: {0}` | 鍵 bundle / keyring の入出力失敗（Drive の通信・壊れた bundle など）。`PackKeysError` のうち上 2 つ以外をここへ畳む | `import/mod.rs:80-82`, `:104-106` |
| `IdentitySubMissing` | `Google アカウントの識別子（sub）を取得できません。設定画面からログインし直してください` | `sub` が空（userinfo の欠落・保存値の破損）。v3 では `owner_id` と `sub` ラップの鍵材料なので空文字は通さない | `import/mod.rs:83-91`, `:256-258` |

非致命の扱い（エラーにしない）: `warnings: Vec<String>` に積む。形式は主に `"{エントリ名}: {理由}"`（例: 壊れた画像・入れ子上限・入れ子破損）。`ImportedBook.warnings` として呼び出し側へ返す（`import/mod.rs:26-29`, `:409-468`, `:1534`, `:1631`）。

### 6.2 `PackError`（全 variant）

定義: `crates/opfspack/src/lib.rs:136-154`。

| variant | `Display` | 発生条件 | アンカー |
|---|---|---|---|
| `Version(u32)` | `unsupported pack version: {0}` | ヘッダの `version != 3`。**v2 の pack はここで拒否**される（同期は `SyncError::UnsupportedPackVersion` に写して再取り込みを案内。`crates/core/src/drive/sync.rs:255-260`） | `lib.rs:138-139`, `reader.rs:22-24`, `:341-343` |
| `Corrupted(String)` | `corrupted pack: {0}` | サイズ不足 / マジック不一致 / ヘッダ CRC 不一致 / ヘッダの未知フラグ / index 範囲外 / index CRC 不一致 / 件数・サイズ上限超過 / エントリフラグ不整合（**`IDENTITY_BOUND` を含む未知ビットもここ**） / エントリ境界違反 / `size` と実長の不一致 / 暗号化の不整合 / 復号失敗（他アカウントの鍵・別 pack の鍵） / 展開失敗（宣言サイズ超過を含む） / 圧縮失敗。鍵 bundle 側では JSON 不正・シリアライズ失敗にも使う | `lib.rs:140-141`, `format.rs:80-101`, `reader.rs:25-54`, `:177-262`, `:281-303`, `keys.rs:372-373`, `:383-390` |
| `UnsupportedLz4` | `lz4 compression not supported` | エントリの `LZ4` フラグが立っている | `lib.rs:142-143`, `reader.rs:87-89`, `:402-404` |
| `KeyRequired(String)` | `pack key required for encrypted entry: {0}` | `ENCRYPTED` 付きエントリを鍵なし（`Option<&PackKey>` = `None`）で読んだ。**v2 の `IdentityRequired` の置き換え** | `lib.rs:144-145`, `reader.rs:286-287` |
| `UnsupportedKeyBundle(u32)` | `unsupported key bundle format version: {0}` | `thundoku-keys.json` の `format_version != 1`（構造を読む前に版だけ先に判定） | `lib.rs:146-147`, `keys.rs:391-393` |
| `NotFound(String)` | `entry not found: {0}` | 指定パスのエントリが無い | `lib.rs:148-149`, `reader.rs:84-86`, `:114-116`, `:398-400` |
| `InvalidRange(String)` | `invalid range: {0}` | `read_entry_range` で `start >= end` または `end > entry.size`（復号後の実長での再検査は `Corrupted`） | `lib.rs:150-151`, `reader.rs:117-122` |
| `Io(String)` | `pack I/O error: {0}` | `PackFileReader` のファイル読み出し・seek の失敗（`io_error` で包む） | `lib.rs:152-153`, `reader.rs:308-310`, `:312-315` |

### 6.3 DB エラーの扱い

| 事実 | 内容 | アンカー |
|---|---|---|
| エラー型 | リポジトリ層に専用エラー型は無く、すべて `Result<_, sqlx::Error>` | `crates/core/src/db/*.rs` 全関数 |
| マイグレーション失敗 | `sqlx::migrate::MigrateError` を `sqlx::Error::Protocol(e.to_string())` に変換して返す | `crates/core/src/db/mod.rs:195-198` |
| バックアップ取り込みの JSON 不正 | `serde_json` のエラーを `sqlx::Error::Protocol` に変換 | `crates/core/src/db/backup.rs:61-63` |
| 初回クリアの失敗 | `DELETE`（16 テーブル）とファイル操作は `let _ =` で**握り潰し**、`settings::set` のフラグ書き込みだけ `?` で伝播 | `crates/core/src/db/mod.rs:419-457` |
| トランザクション | `books::delete`（孫→子→親の 8 文）、`contents::insert_batch`、`documents::insert_*_batch`、`tags::set_for_book`（DELETE→INSERT）、`contents::set_primary`（2 文）は `pool.begin()` + `commit()`。途中失敗時はロールバック（`tx` の drop） | `db/books.rs:365-389`, `db/contents.rs:41-83`, `:148-165`, `db/documents.rs:260-371`, `db/tags.rs:18-44` |
| 空スライス短絡 | `insert_images_batch` / `insert_texts_batch` / `insert_tokens_batch` は空入力で `Ok(())` | `db/documents.rs:261-263`, `:304-306`, `:339-341` |
| 存在しない行 | `get` 系は `fetch_optional` → `Ok(None)`。`fetch_one` は `sqlx::Error::RowNotFound` になり得る（`pragma_table_info` の COUNT は常に 1 行を返す） | `db/books.rs:145-151`, `db/mod.rs:65-84` |
| FK 制約違反 | `ON DELETE CASCADE` が無い子テーブルは明示削除が必要。`books::delete` のコメントが「呼び出し側が `let _ =` で握り潰すと無言で失敗する」と警告 | `db/books.rs:356-363` |
| アプリ側の握り潰し例 | サイト表紙キャッシュやオプション処理は `let _ =` / `if let Err(error) = ... { log::warn!(...) }` | `crates/app/src/views/bookshelf.rs:3359`, `crates/core/src/drive/sync.rs:172-176` |
| ログ | 取り込みの主要段は `log::info!` で経過時間つきに記録（`finish_import: 完了（{:?}）` 等） | `import/mod.rs:957`, `:1020`, `:1068-1071`, `:1086`, `:1193`, `:1206`, `:1241` |

---

## 7. 不明点（コードから判断できなかったこと）

| # | 不明点 | 理由 |
|---|---|---|
| 1 | `crates/core/src/db/schema.sql` がどうやって更新・検証されるか（Web 版スキーマとの同期手順、差分チェックの有無） | リポジトリ内に参照が一切無い（`db/mod.rs:200` のコメントと `docs/database.md:4` のみ）。生成スクリプト・テストも見つからない |
| 2 | `document_text` / `token_analysis` の `content_id` を INSERT で埋めない理由（列は存在し既定値 `''` だが、常に空になる） | `db/documents.rs:240-258`, `:338-371` の INSERT 文に列が無く、意図を述べたコメントも無い |
| 3 | `book_first_events` / `zenn_tag_metadata` の書き込み経路、`product_sample_pages.pack_id` / `pack_entry_path` を埋める経路 | `crates/core/src/db/**` に該当の書き込み関数が無い（`samples.rs` は `pack_id`/`pack_entry_path` を書かない）。範囲外モジュールにあるか未実装かは判断できない |
| 4 | `books.page_count` / `bookshelf_items.page_count` に入る値の定義（何ページを指すか） | 本担当範囲（FANZA/DLsite 同期）にその代入コードが無い |
| 5 | import が `entry_flags::COMPRESSED` を一切立てない理由 | 呼び出し側は全エントリ `compress=false` 固定（`import/mod.rs:1002`, `:1640-1645`, `:1672-1677`, `:1703-1710`, `:1746-1758`）。判断根拠のコメントは無い |
| 6 | DEFLATE レベル `6` の根拠（TS の fflate 既定と一致するか） | `builder.rs:70` の値のみ。TS 側ソースは本リポジトリに無い |
| 7 | ~~pack・エントリ単位のサイズ上限（1 pack 最大バイト数等）~~ → **実装済み（R04）**: `MAX_ENTRY_SIZE` = 512 MiB / `MAX_TOTAL_SIZE` = 2 GiB / `MAX_STORED_ENTRY_SIZE` = 2 GiB / `MAX_ENTRY_COUNT` = 10,000 を index 解析時に強制する（§4.6.1）。値は既知の正常本（展開後 1.33 GB・3,321 ページ）を通すため、1 エントリ 512 MiB は取り込み側の `MAX_ZIP_ENTRY_BYTES` と同値、件数は取り込み側の `MAX_NESTED_ENTRIES = 2000` とは**別物**として 10,000 とした | `lib.rs:58-72`, `reader.rs:177-262`, `tests/limits.rs`。取り込み側の上限（通常エントリ 512 MiB / 入れ子 ZIP 512 MiB / 2000 件、HTTP 応答 2 GiB）は §3.3 のままで、pack 読み出しとは独立 |
| 8 | `books.cover_thumbnail` を埋める経路 | 取り込みは常に `None`（`import/mod.rs:933`）。Drive 取り込みも `None`（`drive/sync.rs:149`） |
| 9 | `view_history.started_at` / `ended_at` をローカル時刻へ直す責務の所在（コメントは「表示側でローカルに直す」） | `db/view_history.rs:142-160` は `chrono::Local` で日付集計するが、どの層が正かは本担当範囲外（詳細は `local://spec-core.md` を参照） |
| 10 | PDFium ライブラリ（`pdfium.dll` / `libpdfium.dylib`）の配布手順・バージョン整合（feature `pdfium_7881`） | 依存宣言のみで、取得の手順は本担当範囲のファイルに無い（配布物は `.github/workflows/release.yml`、開発/テスト用は `scripts/fetch-pdfium.sh` = `mise run pdfium` が `crates/core/` へ取得する） |

## 8. 推測（根拠つき）

| # | 推測 | 根拠 |
|---|---|---|
| 1 | `schema.sql` は Web 版から持ち込んだ「参照用の正本」で、実 DB の最終形は `0001_init.sql` + `migrate()` の runtime DDL が作る | コードからの参照がコメントのみ（`db/mod.rs:200`）、`docs/database.md:3-7` が「後発テーブルは migrate() 内の冪等 DDL で適用」と明記。`schema.sql` にしか無いテーブル・列と、runtime DDL にしか無いテーブル（`view_history` / `page_notes`）が混在している |
| 2 | エントリ単位の圧縮を常に無効にしているのは、中身が WebP（既圧縮）で deflate の利得が小さいため | ページ・表紙・サムネイルはすべて `image/webp`（`import/mod.rs:1640-1645`, `:1746-1758`）。`build(pack_key.as_ref(), true)` で pack フラグだけ立てている（`:1009`） |
| 3 | pack レベル `COMPRESSED` を常に立てるのは TS 実装のグローバル既定に合わせるため | `builder.rs:55-58` の doc コメント「`compress` sets the pack-level flag (and is the TS global default)」 |
| 4 | `document_text` / `token_analysis` に `content_id` を入れないのは、テキスト利用（タグ生成）が本全体単位で、コンテンツ別検索が未実装だから | `finish_import` は全コンテンツのテキストを連結せずそのまま投入し、`tags::generate_tags` には全テキスト配列を渡す（`import/mod.rs:1192-1236`）。`content_id` 列は移行で `DEFAULT ''` として足されただけ（`db/mod.rs:164-177`） |
| 5 | 付箋 id を決定論的（`note-{book_id}-{content_id}-{page}`）にしたのは、リーダーと付箋画面の 2 経路から同じ行を UPSERT するため | 両画面が同一書式で id を作る（`crates/app/src/views/reader.rs:598-599`, `crates/app/src/views/notes.rs:305`）。UUID だと二重登録になる |
| 6 | サムネイル行の `page_number` が常に `1` なのは「1 ページ目＝表紙」規約に合わせるため | `thumbnail_of` の入力は常に先頭ページ（`import/mod.rs:1063-1082` の `primary_thumbnail` は legacy 第 1 ページ、PDF 経路は `pages[0]`） |
| 7 | `MAX_NESTED_BYTES = 512 MiB` は「外側 ZIP の実データ展開後 1.33 GB より小さく、実在する補助的な入れ子（1 階層）を弾かない」値として選ばれた | `import/mod.rs:187-191` のコメント（実データ最大 1.33GB / 入れ子は補助的 / 解凍爆弾対策）。`docs/import-patterns.md:672-681`（R1）と一致 |
| 8 | `page_notes.page` が 1-indexed なのは `document_images.page_number`（1 始まり）に揃えたため | `import/mod.rs:1555-1556` がページを 1 始まりで採番し、`db/notes.rs:157-178` が `page > 0` を要求して `-1` して 0-indexed に変換している |

---

## 付録: 担当範囲外だが参照したもの（重複回避）

- `crates/core/src/db/{progress,tags,notes,view_history,page_views}.rs` の**詳細な挙動**（読書状態判定、集計クエリ、付箋の ON/OFF 等）は `local://spec-core.md`（担当: SpecCore）を参照。本ドキュメントではスキーマ・ID 規約・エラー扱いの範囲に限定して列挙した。
- 技術書典 / BOOTH / FANZA / DLsite の同期（`bookshelf_items` / `checked_items` / `tbf_events` の行生成）は本担当範囲外（ID の出所のみ §2.1 に記載）。
