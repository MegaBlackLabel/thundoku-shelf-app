//! Import pipeline: PDF / EPUB / image-ZIP -> .opfspack pack + DB rows
//! (books, imported_documents, document_images, document_text,
//! token_analysis, book_tags). Mirrors the Web `file-import.ts` rules.

pub mod pdf;

pub mod classify;
pub mod export_text;
mod zip_names;

use classify::{EntryKind, classify_entry, is_readable_kind};

use std::io::Read;
use std::path::Path;
use std::sync::Arc;

use opfspack::{PackBuilder, PackRead, PackRootKey};

use crate::db::{SqlitePool, books, contents, documents, tags as tags_repo};
use crate::google::GoogleProfile;

#[derive(Debug)]
pub struct ImportedBook {
    pub book: books::Book,
    pub document: documents::ImportedDocument,
    /// Tags written to `book_tags` (source=generated).
    pub tags: Vec<String>,
    /// 取り込み時に読み飛ばしたエントリ（壊れた画像など）。0 件なら空。
    pub warnings: Vec<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("unsupported file type: {0}")]
    UnsupportedType(String),
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("pack error: {0}")]
    Pack(#[from] opfspack::PackError),
    #[error("database error: {0}")]
    Db(#[from] sqlx::Error),
    #[error("pdf error: {0}")]
    Pdf(String),
    #[error("image error: {0}")]
    Image(String),
    #[error("zip error: {0}")]
    Zip(String),
    #[error("empty archive")]
    EmptyArchive,
    /// 読めるコンテンツ（画像 / PDF / EPUB）が 1 件も無い ZIP。
    /// アプリ側が出し分けるための識別子（文言は UI が決める。§11.2 R3）。
    #[error("not a readable work")]
    NotAReadableWork,
    /// 未ログイン（Google のプロフィールが無い）で取り込みを要求された。
    ///
    /// v3 の pack 鍵（PRK）は Google アカウント（`sub`）ごとに作るため、未ログインでは
    /// 鍵を用意できない。ここで平文 pack に落とすと、所有者（`owner_sub`）を持たない
    /// 本が増えて Drive 同期の対象にもならず（別端末から復元できない）、ログイン後に
    /// 同じ本を取り込んでも別の本として二重になる。**平文に落とさず取り込みを失敗
    /// させる**（fail-closed。セキュリティ評価 F03）。文言はそのまま画面に出る。
    #[error(
        "本を取り込むには Google にログインしてください（本はアカウントごとの鍵で暗号化されます）"
    )]
    LoginRequired,
    /// ZIP のエントリ数・展開後の合計が上限を超える（**展開の前**に弾いた）。
    /// 個別上限（`MAX_ZIP_ENTRY_BYTES`）だけでは、上限内のエントリが大量にある
    /// アーカイブで変換を走らせてしまう（セキュリティ評価 F06）。
    #[error("この ZIP は大きすぎて取り込めません（{detail}）")]
    ZipTooLarge { detail: String },
    /// 取り込み元のファイルが大きすぎる（**読む前に**弾いた。セキュリティ評価 F06）。
    /// 文言はそのまま画面に出る。
    #[error("この本は大きすぎて取り込めません（{size} バイト。上限は {limit} バイト）")]
    SourceTooLarge { size: u64, limit: u64 },
    /// Google にログイン済みなのに pack の鍵（v3 のルート鍵 = PRK）を用意できない。
    ///
    /// v3 の鍵材料は乱数のルート鍵で、端末の keyring と Drive の
    /// `thundoku-keys.json`（`sub` / パスフレーズでラップして保管）にしか無い。
    /// この状態で取り込みを続けると平文 pack ができ、`owner_sub` 付きの本が
    /// 復号できない pack（＝ログイン中の閲覧で開けない本）を指す不整合になる。
    /// データを壊すより取り込みを失敗させる（fail-closed）。文言はそのまま画面に出る。
    #[error(
        "Google にログイン済みですが、本を復号する鍵を取得できません。鍵を持つ端末でパスフレーズを設定し、この端末で入力して復元してください（鍵がどこにも無い場合は、ストアから取り込み直す必要があります）"
    )]
    IdentityKeyUnavailable,
    /// 入力されたパスフレーズが違う（`sub` ラップへ**黙って落ちない** — 仕様 §4.1）。
    #[error("パスフレーズが違います。もう一度入力してください")]
    PassphraseFailed,
    /// 鍵 bundle / keyring の入出力に失敗した（Drive の通信・壊れた bundle など）。
    #[error("本の鍵を取得できませんでした: {0}")]
    KeyStore(String),
    /// ログイン済みプロフィールの `sub` が空（userinfo の欠落・保存値の破損）。
    ///
    /// v3 では `sub` から `owner_id`（keyring のスロット名・ラップの AAD）と
    /// `sub` ラップの KEK を作るため、空のままでは自分の鍵を引けない。
    /// 空文字は通さない。
    #[error(
        "Google アカウントの識別子（sub）を取得できません。設定画面からログインし直してください"
    )]
    IdentitySubMissing,
}

/// [`crate::pack_keys::PackKeysError`] を利用者に伝わる取り込みエラーにする。
///
/// 呼び出し側（アプリ）は「鍵が無い」「パスフレーズが違う」を区別して
/// 案内を出し分けられる（前者は復元の案内、後者は再入力）。
impl From<crate::pack_keys::PackKeysError> for ImportError {
    fn from(value: crate::pack_keys::PackKeysError) -> Self {
        use crate::pack_keys::PackKeysError;
        match value {
            PackKeysError::Unavailable => ImportError::IdentityKeyUnavailable,
            PackKeysError::PassphraseFailed => ImportError::PassphraseFailed,
            other => {
                log::warn!("import: 本の鍵の取得に失敗: {other}");
                ImportError::KeyStore(other.to_string())
            }
        }
    }
}

fn now() -> String {
    chrono::Utc::now().format("%Y-%m-%d %H:%M:%S").to_string()
}

/// 幅が `min_width` 未満の画像を Lanczos3 で拡大する（元が小さい画像の表示
/// ぼやけを軽減するため。真の解像度は増えないがピクセルの滑らかさは向上する）。
fn upscale_if_small(img: &image::DynamicImage, min_width: u32) -> image::DynamicImage {
    let width = img.width();
    if width >= min_width || width == 0 {
        return img.clone();
    }
    let scale = min_width as f32 / width as f32;
    let new_width = (width as f32 * scale).round().max(1.0) as u32;
    let new_height = (img.height() as f32 * scale).round().max(1.0) as u32;
    img.resize_exact(new_width, new_height, image::imageops::FilterType::Lanczos3)
}

/// Encode any dynamic image as lossy webp with the given quality (0-100).
/// `image` 0.25's own webp encoder is lossless-only, so lossy encoding goes
/// through the `webp` crate (bundled libwebp).
pub fn encode_webp(image: &image::DynamicImage, quality: u8) -> Result<Vec<u8>, ImportError> {
    let rgba = image.to_rgba8();
    let (width, height) = rgba.dimensions();
    let memory =
        webp::Encoder::from_rgba(&rgba, width, height).encode(quality.clamp(0, 100) as f32);
    Ok(memory.to_vec())
}

/// Thumbnail: first page scaled to 200px width.
fn thumbnail_of(page: &[u8]) -> Result<(Vec<u8>, u32, u32), ImportError> {
    let decoded = image::load_from_memory(page).map_err(|e| ImportError::Image(e.to_string()))?;
    let (width, height) = (decoded.width(), decoded.height());
    let thumb_width = 200u32;
    let thumb_height = ((height as f64 * thumb_width as f64) / width.max(1) as f64)
        .round()
        .max(1.0) as u32;
    let resized = decoded.resize_exact(
        thumb_width,
        thumb_height,
        image::imageops::FilterType::Triangle,
    );
    let data = encode_webp(&resized, 80)?;
    Ok((data, thumb_width, thumb_height))
}

/// 画像エントリ 1 件をページ用 WebP へ変換する
/// （元が小さい画像は Lanczos3 で 1000px 幅まで拡大。表示時のぼやけ軽減）。
fn render_page_image(data: &[u8]) -> Result<(Vec<u8>, u32, u32), ImportError> {
    let decoded = image::load_from_memory(data).map_err(|e| ImportError::Image(e.to_string()))?;
    let decoded = upscale_if_small(&decoded, 1000);
    let (width, height) = (decoded.width(), decoded.height());
    Ok((encode_webp(&decoded, 88)?, width, height))
}

/// 取り込み時に並列で扱うページ数（1 チャンク分の圧縮バイトを保持する）。
/// 1 ページ約 0.5MB なので 64 ページで 30MB 程度。
const PAGE_RENDER_CHUNK: usize = 64;

/// ページ変換（デコード + webp 再圧縮）に使うワーカー数。
/// 1 ページあたり実測 0.8 秒（release）で、重いのはほぼ webp 再圧縮。
/// 逐次だと 3,000 ページ級で 40 分を超えるため並列化する。デコード済み画像は
/// 1 枚 数十 MB あるため、ワーカー数は 8 で頭打ちにする（メモリ保護）。
fn page_render_workers(page_count: usize) -> usize {
    let cores = std::thread::available_parallelism()
        .map(|count| count.get())
        .unwrap_or(4);
    cores.min(8).min(page_count.max(1))
}

/// 1 ページ分の変換結果（WebP バイト列・幅・高さ）。失敗時は理由を持つ。
type RenderedPage = Result<(Vec<u8>, u32, u32), String>;

/// ページ画像を**入力順**で変換する（デコード + webp 再圧縮を並列実行）。
/// 失敗したページは `Err(理由)` を返す（呼び出し側が警告に積んでスキップする）。
fn render_page_images(pages: &[(String, Vec<u8>)]) -> Vec<RenderedPage> {
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<RenderedPage>>> =
        Mutex::new((0..pages.len()).map(|_| None).collect());
    std::thread::scope(|scope| {
        for _ in 0..page_render_workers(pages.len()) {
            let next = &next;
            let results = &results;
            scope.spawn(move || {
                loop {
                    let index = next.fetch_add(1, Ordering::Relaxed);
                    let Some((_, data)) = pages.get(index) else {
                        break;
                    };
                    let rendered = render_page_image(data).map_err(|error| error.to_string());
                    results.lock().expect("page results")[index] = Some(rendered);
                }
            });
        }
    });
    results
        .into_inner()
        .expect("page results")
        .into_iter()
        .map(|result| result.expect("全 index を処理済み"))
        .collect()
}

/// 取り込み先の book id（= pack id）を決める。
///
/// 再ダウンロード時は既存本の id を再利用して重複を防ぐ。無ければ UUIDv4 を振る。
/// v3 の pack 鍵は **この id から導出する**（`PRK.derive_pack_key(book_id)`）ので、
/// id を決めてから鍵を作る順序になる。
fn book_id_for(reuse_book_id: Option<&str>) -> String {
    reuse_book_id
        .map(str::to_string)
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string())
}

/// 取り込みで pack を作る鍵（v3 の PRK）を決める（**fail-closed**）。
///
/// - 未ログイン（`profile` = `None`）: **エラー**（[`ImportError::LoginRequired`]）。
///   平文 pack の取り込みは行わない（セキュリティ評価 F03）。
/// - ログイン済み + 鍵あり: `Ok(prk)`。実際の pack 鍵は冊ごとに
///   `prk.derive_pack_key(&book_id)` で導出する（`finish_import` が行う）。
/// - ログイン済み + 鍵なし: **平文に落とさずエラー**。ログイン中の閲覧は暗号化
///   pack 前提（`reader.rs`）なので、平文で作ると `owner_sub` 付きの行が
///   「開けない本」を指す。Drive 同期で平文がクラウドへ上がる危険もある。
/// - ログイン済みでも `sub` が空: エラー（`sub` は `owner_id` とラップの材料なので、
///   空のままでは自分の鍵を引けない）。
///
/// `resolve_root` は PRK の解決（keyring → Drive の `thundoku-keys.json` → 必要なら
/// パスフレーズ入力・新規作成）を行う。core の [`crate::pack_keys::PackKeyStore`] を
/// 呼ぶのが想定経路で、UI（パスフレーズの入力）は呼び出し側が用意する。
pub fn pack_root_key_for_import(
    profile: Option<&GoogleProfile>,
    resolve_root: impl FnOnce(&str) -> Result<PackRootKey, ImportError>,
) -> Result<PackRootKey, ImportError> {
    let Some(profile) = profile else {
        // 未ログインでは鍵を作れない（平文 pack は作らない）
        return Err(ImportError::LoginRequired);
    };
    let sub = profile.sub.trim();
    if sub.is_empty() {
        return Err(ImportError::IdentitySubMissing);
    }
    resolve_root(sub)
}

/// ZIP エントリパスからファイル名部分を取り出す（`dir/book.pdf` → `book.pdf`）。
fn entry_file_name(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

/// 1 冊の PDF のページ数上限（9000）。
///
/// `opfspack::MAX_ENTRY_COUNT`（10000）から metadata / サムネイル分を引いた値。
/// これを超える PDF は 1 エントリ 16MPix の検査を通っても、レンダリングと
/// WebP エンコードに長時間かけてから pack の上限で落ちる（セキュリティ評価 F06）。
pub const MAX_PDF_PAGES: usize = 9000;

/// 取り込み元ファイルの上限（2 GiB）。**読む前に**検査する。
///
/// `import_file` は変換のために全体をメモリへ読む（`import_*_bytes` 系の API）。
/// 読んでから大きさに気付くとその時点で RAM を食い潰すため、メタデータだけで先に弾く
/// （セキュリティ評価 F06）。これを超える本は、変換をストリーム化してから対応する。
pub const MAX_IMPORT_SOURCE_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// 入れ子 ZIP の再帰展開の上限（決定 D5 / `docs/import-patterns.md` §11.2 R1）。
///
/// 実データで確認できた入れ子は 1 階層だけ（`d_305009` の ZIP 内 `.zip`）なので、
/// 深さは 1 に固定する。2 階層目以降は展開せず警告に積む。
pub const MAX_NESTED_DEPTH: usize = 1;

/// 入れ子 ZIP から合流させるエントリの**非圧縮合計**サイズの上限（512 MiB）。
///
/// 外側 ZIP の実データ最大は展開後 1.33GB だが、入れ子は補助的な同梱物であり、
/// これだけの量を 1 冊に含む例は無い。解凍爆弾・事故を弾くための天井として置く。
pub const MAX_NESTED_BYTES: u64 = 512 * 1024 * 1024;

/// 入れ子 ZIP から合流させるエントリ数の上限（2000）。
///
/// 1 冊（1 コンテンツ）のページ数としては十分大きく、数万エントリの異常な
/// アーカイブを弾ける値。
pub const MAX_NESTED_ENTRIES: usize = 2000;

/// ZIP エントリの索引と名前（本体データは保持しない）。
struct EntryMeta {
    /// 読み出し先のアーカイブ内の索引（`nested` があればその入れ子内の索引）。
    index: usize,
    name: String,
    /// 中央ディレクトリが宣言する展開後サイズ（**信用はしない**。上限の事前検査にだけ使う）。
    declared_size: u64,
    /// 入れ子 ZIP 由来のとき、その入れ子アーカイブの生バイト。
    /// 同じ入れ子のエントリ間で `Arc` を共有し、外側から読み直さない。
    nested: Option<Arc<[u8]>>,
}

/// エントリの名前だけを集める（本体は伸長しない）。
///
/// `zip` crate の `name()` は UTF-8 フラグの無い名前を CP437 として復号するため、
/// 日本語（Shift-JIS / CP932）の名前が文字化けする。生バイトから自前でデコードする。
fn collect_entry_metas<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
) -> Result<Vec<EntryMeta>, ImportError> {
    let mut metas = Vec::new();
    for index in 0..archive.len() {
        let entry = archive
            .by_index_raw(index)
            .map_err(|e| ImportError::Zip(e.to_string()))?;
        if entry.is_dir() {
            continue;
        }
        metas.push(EntryMeta {
            index,
            name: zip_names::decode_entry_name(entry.name_raw()),
            declared_size: entry.size(),
            nested: None,
        });
    }
    Ok(metas)
}

/// 上限付きで ZIP エントリを読み出す（解凍爆弾対策）。
/// 宣言サイズを信用せず、実際に読めたバイト数で上限を判定する。
/// 上限超過は `Ok(None)`（呼び出し側が警告にしてスキップできるようにする）。
fn read_zip_entry_capped<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    index: usize,
    limit: u64,
) -> Result<Option<Vec<u8>>, ImportError> {
    Ok(read_zip_entry_capped_detail(archive, index, limit)?.0)
}

/// 上限付き読み出しの本体。`(読めたデータ, 実際に読めた長さ)` を返す
/// （上限超過は `None` と読めた長さ。エラーの内訳表示に使う）。
fn read_zip_entry_capped_detail<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    index: usize,
    limit: u64,
) -> Result<(Option<Vec<u8>>, u64), ImportError> {
    let mut entry = archive
        .by_index(index)
        .map_err(|e| ImportError::Zip(e.to_string()))?;
    let mut data = Vec::new();
    entry
        .by_ref()
        .take(limit + 1)
        .read_to_end(&mut data)
        .map_err(|e| ImportError::Zip(e.to_string()))?;
    let read = data.len() as u64;
    if read > limit {
        return Ok((None, read));
    }
    Ok((Some(data), read))
}

/// 通常 ZIP エントリ 1 件の非圧縮サイズ上限（2 GiB。解凍爆弾対策）。
///
/// **展開後**のサイズで判定する（宣言サイズは信用しない）。取り込み元ファイルの上限
/// （[`MAX_IMPORT_SOURCE_BYTES`]）と同じ値にしてある: ここで許すのは
/// 「そのサイズの 1 エントリをメモリに読んでよい」という意味で、それ以上は
/// 取り込み側（`import_*_bytes`）が扱えない。
///
/// 512 MiB にしていたときは、**142 MB の ZIP に含まれる PDF が展開後 512 MiB を
/// 超える**（スキャン画像を多く含む PDF は deflate がよく効く）という正当な本を
/// 弾いていた。ページ画像として pack に入るのは**レンダリング後**の小さな webp なので、
/// 元 PDF の大きさが pack の上限に効くことはない。
pub const MAX_ZIP_ENTRY_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// 上限超過エントリのエラー文言。
///
/// 内訳（宣言サイズ・実際に読めた長さ）を出すのは、**上限に当たったのか
/// データが壊れているのか**を切り分けるため（宣言より実際がはるかに大きい＝
/// 展開爆弾か壊れた ZIP。どちらも同じ上限で弾くが、原因を追える）。
fn zip_entry_too_large_error(name: &str, declared: u64, read: u64) -> ImportError {
    ImportError::Zip(format!(
        "エントリがサイズ上限（{MAX_ZIP_ENTRY_BYTES} バイト）を超えています: {name}（宣言 {declared} バイト / 実際に読めた {read} バイト）"
    ))
}

/// `EntryMeta` が指すエントリを 1 件読み出す（入れ子 ZIP の中身にも対応）。
fn read_entry<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    meta: &EntryMeta,
) -> Result<Vec<u8>, ImportError> {
    match &meta.nested {
        None => {
            let (data, read) =
                read_zip_entry_capped_detail(archive, meta.index, MAX_ZIP_ENTRY_BYTES)?;
            data.ok_or_else(|| zip_entry_too_large_error(&meta.name, meta.declared_size, read))
        }
        Some(bytes) => {
            let mut nested = zip::ZipArchive::new(std::io::Cursor::new(Arc::clone(bytes)))
                .map_err(|e| ImportError::Zip(e.to_string()))?;
            let (data, read) =
                read_zip_entry_capped_detail(&mut nested, meta.index, MAX_ZIP_ENTRY_BYTES)?;
            data.ok_or_else(|| zip_entry_too_large_error(&meta.name, meta.declared_size, read))
        }
    }
}

/// エントリ名を集め、`.zip` エントリを 1 階層だけ展開して合流させる（決定 D5）。
fn collect_metas_with_nested<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    warnings: &mut Vec<String>,
) -> Result<Vec<EntryMeta>, ImportError> {
    let mut metas = collect_entry_metas(archive)?;
    expand_nested_archives(archive, &mut metas, warnings)?;
    Ok(metas)
}

/// 外側アーカイブの `.zip` エントリを [`MAX_NESTED_DEPTH`] 階層だけ展開し、
/// 中のエントリを `metas` に合流させる。
///
/// - 合流したエントリの名前は入れ子 ZIP のパス（拡張子を除く）を前置する。
///   外側の分類規則（`classify_entry` / `content_folder`）がそのまま効き、
///   入れ子内の画像・PDF・EPUB が 1 コンテンツとして認識される。
/// - 壊れた入れ子 ZIP・上限超過は `warnings` に積んでスキップする
///   （取り込み全体は失敗させない）。
/// - 入れ子の中の `.zip` は展開しない（深さ上限）。警告だけ残す。
fn expand_nested_archives<R: std::io::Read + std::io::Seek>(
    archive: &mut zip::ZipArchive<R>,
    metas: &mut Vec<EntryMeta>,
    warnings: &mut Vec<String>,
) -> Result<(), ImportError> {
    if MAX_NESTED_DEPTH == 0 {
        return Ok(());
    }
    // 展開中は `metas` を伸ばすため、対象（外側の `.zip`）を先に確定させる。
    let targets: Vec<(usize, String)> = metas
        .iter()
        .filter(|meta| meta.nested.is_none() && classify::is_nested_archive(&meta.name))
        .map(|meta| (meta.index, meta.name.clone()))
        .collect();

    let mut merged_entries = 0usize;
    let mut merged_bytes = 0u64;
    for (index, name) in targets {
        // 宣言サイズが上限を超える入れ子は開かない（読む前に弾く）。
        let declared = {
            let entry = archive
                .by_index_raw(index)
                .map_err(|e| ImportError::Zip(e.to_string()))?;
            entry.size()
        };
        if declared > MAX_NESTED_BYTES {
            warnings.push(format!(
                "{name}: nested zip is larger than the size limit ({MAX_NESTED_BYTES} bytes)（宣言 {declared} バイト）"
            ));
            continue;
        }
        let bytes: Arc<[u8]> = match read_zip_entry_capped(archive, index, MAX_NESTED_BYTES)? {
            Some(bytes) => Arc::from(bytes.into_boxed_slice()),
            None => {
                warnings.push(format!(
                    "{name}: nested zip is larger than the size limit ({MAX_NESTED_BYTES} bytes)"
                ));
                continue;
            }
        };
        let mut nested = match zip::ZipArchive::new(std::io::Cursor::new(Arc::clone(&bytes))) {
            Ok(nested) => nested,
            Err(error) => {
                warnings.push(format!("{name}: {error}"));
                continue;
            }
        };
        // 入れ子の中身を先に組み立て、上限内のときだけ合流させる
        // （超過時に半分だけ取り込まない）。
        let prefix = file_stem(&name).to_string();
        let mut pending = Vec::new();
        let mut count = 0usize;
        let mut total = 0u64;
        let mut deeper = false;
        for inner_index in 0..nested.len() {
            let entry = match nested.by_index_raw(inner_index) {
                Ok(entry) => entry,
                Err(error) => {
                    warnings.push(format!("{name}: {error}"));
                    continue;
                }
            };
            if entry.is_dir() {
                continue;
            }
            let inner_name = zip_names::decode_entry_name(entry.name_raw());
            if classify::is_nested_archive(&inner_name) {
                // 深さ上限: 入れ子の中の ZIP は展開しない。
                deeper = true;
                continue;
            }
            count += 1;
            total = total.saturating_add(entry.size());
            pending.push(EntryMeta {
                declared_size: entry.size(),
                index: inner_index,
                name: format!("{prefix}/{inner_name}"),
                nested: Some(Arc::clone(&bytes)),
            });
        }
        if deeper {
            warnings.push(format!(
                "{name}: nested zip inside a nested zip is not expanded (depth limit {MAX_NESTED_DEPTH})"
            ));
        }
        if merged_entries + count > MAX_NESTED_ENTRIES || merged_bytes + total > MAX_NESTED_BYTES {
            warnings.push(format!(
                "{name}: nested zip exceeds the entry/size limit and was skipped"
            ));
            continue;
        }
        merged_entries += count;
        merged_bytes += total;
        metas.extend(pending);
    }
    Ok(())
}

/// コンテンツのメディア種別（`book_contents.media_kind` の下地）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaKind {
    Image,
    Pdf,
    Epub,
    Audio,
    Video,
}

/// レンディション（切替可能な表示形態）。同じ内容の別形式・別バリアント
/// （`PDF版` / `画像版`、`文字あり` / `文字なし` など）をここに畳む。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedRendition {
    /// 切替 UI に出す名前（`画像` / `PDF` など）。
    pub label: String,
    /// メディア種別（`content_formats.format_kind` の元）。
    pub kind: MediaKind,
    /// エントリの並び順（`collect_entry_metas` が返す一覧の添字。ページ順）。
    pub entries: Vec<usize>,
}

/// 読む単位（`book_contents` の下地）。表紙・junk は含まない。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedContent {
    pub display_name: String,
    pub media_kind: MediaKind,
    pub renditions: Vec<PlannedRendition>,
}

impl PlannedContent {
    /// 主レンディションのページ数の目安。
    /// PDF / EPUB はページ数が展開するまで不明なので 1 ファイル = 1 として数える。
    fn page_hint(&self) -> usize {
        self.renditions.first().map_or(0, |r| r.entries.len())
    }
}

/// 取り込む対象が無いときの理由（通知文言は UI 側で決める。§11.2 R3）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    /// 画像 / PDF / EPUB / 音声 / 動画が 1 件も無い（txt のみ・ゲーム等）。
    NotAReadableWork,
}

/// ZIP の解析結果。DB にもディスクにも書かない。
#[derive(Debug, Clone)]
pub struct ImportPlan {
    /// 読む単位。`import_zip_bytes` が見るのは `primary` の 1 件だけ。
    pub contents: Vec<PlannedContent>,
    /// 既定で選ばれるコンテンツの添字（`contents` が空なら無意味）。
    pub primary: usize,
    /// `_export.txt` から読んだ `(page_number, text)`。
    pub export_text: Vec<(i64, String)>,
    /// 解析時に読み飛ばしたエントリ。
    pub warnings: Vec<String>,
    /// 取り込む対象が無いときの理由。
    pub skip_reason: Option<SkipReason>,
}

/// コンテンツごとのエントリを集めるための作業用バケット。
struct Group {
    /// 同一性のキー（フォルダのパス。直下のファイルは `""` かファイル名）。
    key: String,
    /// 表示名（フォルダ名 / ファイル名 / `本文`）。
    display_name: String,
    /// `(種別, メタ情報の並び順)`。
    entries: Vec<(EntryKind, usize)>,
}

/// パスの区切り（ZIP は `/`。全角 `／` で書き出す作品もあるため両方見る）。
const PATH_SEPARATORS: [char; 3] = ['/', '\\', '／'];

/// 形式だけを表すフォルダ名か（このフォルダ自体は読む単位にしない）。
/// `1.尻穴便女/jpg/…` のように形式フォルダが挟まる構造で、内容のフォルダ名を採るため。
fn is_format_folder(name: &str) -> bool {
    let lower = name.trim().to_lowercase();
    matches!(
        lower.as_str(),
        "jpg"
            | "jpeg"
            | "png"
            | "webp"
            | "gif"
            | "bmp"
            | "tif"
            | "tiff"
            | "pdf"
            | "epub"
            | "カラー"
            | "モノクロ"
            | "文字あり"
            | "文字なし"
            | "seあり"
            | "seなし"
    ) || lower.ends_with("版")
        || lower.starts_with("画像")
}

/// エントリが属する読む単位（コンテンツ）のフォルダ。
///
/// 末尾が形式フォルダなら 1 つ上を使う（`1.尻穴便女/jpg/001.jpg` → `1.尻穴便女`）。
/// 戻り値は `(キー, 表示名)`。直下のファイルは `None`。
fn content_folder(name: &str) -> Option<(String, String)> {
    let mut components: Vec<&str> = name.split(PATH_SEPARATORS).collect();
    components.pop(); // ファイル名を落とす
    while components.last().is_some_and(|last| is_format_folder(last)) {
        components.pop();
    }
    let display_name = components.last()?.to_string();
    Some((components.join("/"), display_name))
}

/// ファイル名から拡張子を除いた部分。
fn file_stem(name: &str) -> &str {
    let base = name.rsplit(PATH_SEPARATORS).next().unwrap_or(name);
    base.rsplit_once('.').map_or(base, |(stem, _)| stem)
}

/// エントリ一覧からコンテンツ（読む単位）を組み立てる。
///
/// - **ページを直接含むフォルダ**を 1 コンテンツにする（形式フォルダは飛ばす）。
///   入れ子の上位フォルダ（`総集編/1.話A/…` の `総集編`）は単位にしない。
/// - 直下の画像はまとめて 1 コンテンツ（`本文`）
/// - 直下の PDF / EPUB / 音声 / 動画はファイルごとに 1 コンテンツ。
///   同じ名前のフォルダがあればそのレンディションとして畳む（`PDF版` / `画像版`。§3.2）
fn build_contents(metas: &[EntryMeta]) -> Vec<PlannedContent> {
    let mut groups: Vec<Group> = Vec::new();
    let mut root_entries: Vec<(EntryKind, usize, String)> = Vec::new();

    // 索引はアーカイブ索引ではなく `metas` 内の**並び順**を使う
    // （ディレクトリエントリを除いた分だけ両者はずれる）。
    for (ordinal, meta) in metas.iter().enumerate() {
        let kind = classify_entry(&meta.name);
        if !is_readable_kind(kind) {
            continue;
        }
        match content_folder(&meta.name) {
            Some((key, display_name)) => {
                group_push(&mut groups, &key, &display_name, kind, ordinal)
            }
            None => root_entries.push((kind, ordinal, file_stem(&meta.name).to_string())),
        }
    }
    // 直下の画像は 1 つにまとめ、それ以外はファイル単位。PDF は同名コンテンツへ畳む。
    for (kind, ordinal, stem) in root_entries {
        match kind {
            EntryKind::Image => group_push(&mut groups, "", "本文", kind, ordinal),
            _ => match groups.iter_mut().find(|group| group.display_name == stem) {
                Some(group) => group.entries.push((kind, ordinal)),
                None => group_push(&mut groups, &stem, &stem, kind, ordinal),
            },
        }
    }

    // フォルダ名の数字接頭辞（`1.` / `2.`）は作者の意図的な順序（§5.1）なので自然順で並べる
    groups.sort_by(|a, b| natural_cmp(&a.display_name, &b.display_name));

    groups
        .into_iter()
        .filter_map(|group| plan_content(group, metas))
        .collect()
}

fn group_push(
    groups: &mut Vec<Group>,
    key: &str,
    display_name: &str,
    kind: EntryKind,
    ordinal: usize,
) {
    match groups.iter_mut().find(|group| group.key == key) {
        Some(group) => group.entries.push((kind, ordinal)),
        None => groups.push(Group {
            key: key.to_string(),
            display_name: display_name.to_string(),
            entries: vec![(kind, ordinal)],
        }),
    }
}

/// バケットを `PlannedContent` へ変換する。読める種別が無ければ `None`。
fn plan_content(group: Group, metas: &[EntryMeta]) -> Option<PlannedContent> {
    // レンディションの並び順（先頭が主）。画像を先頭にする。
    const ORDER: &[EntryKind] = &[
        EntryKind::Image,
        EntryKind::Pdf,
        EntryKind::Epub,
        EntryKind::Audio,
        EntryKind::Video,
    ];
    let mut renditions = Vec::new();
    let mut media_kind = None;
    for kind in ORDER {
        let mut ordinals: Vec<usize> = group
            .entries
            .iter()
            .filter(|(entry_kind, _)| entry_kind == kind)
            .map(|(_, ordinal)| *ordinal)
            .collect();
        if ordinals.is_empty() {
            continue;
        }
        ordinals.sort_by(|a, b| natural_cmp(&metas[*a].name, &metas[*b].name));
        if media_kind.is_none() {
            media_kind = media_kind_of(*kind);
        }
        renditions.push(PlannedRendition {
            label: rendition_label(*kind, &ordinals, metas),
            kind: media_kind_of(*kind)?,
            entries: ordinals,
        });
    }
    Some(PlannedContent {
        display_name: group.display_name,
        media_kind: media_kind?,
        renditions,
    })
}

/// レンディションの表示名。画像は実際の拡張子（`JPEG` / `PNG` …）、
/// PDF / EPUB は種別名（拡張子を出さない: メニューでは内容名と並べるため）。
fn rendition_label(kind: EntryKind, ordinals: &[usize], metas: &[EntryMeta]) -> String {
    let first = ordinals.first().and_then(|ordinal| metas.get(*ordinal));
    let extension = first
        .and_then(|meta| meta.name.rsplit_once('.'))
        .map(|(_, ext)| ext.to_lowercase());
    let fallback = || {
        media_kind_of(kind)
            .map(|media| media.label().to_string())
            .unwrap_or_default()
    };
    match kind {
        EntryKind::Image => extension
            .as_deref()
            .and_then(crate::db::contents::image_label_for_extension)
            .map(str::to_string)
            .unwrap_or_else(fallback),
        EntryKind::Pdf | EntryKind::Epub => fallback(),
        _ => fallback(),
    }
}

impl MediaKind {
    /// 切替 UI・取り込み確認に出す名前。
    pub fn label(self) -> &'static str {
        match self {
            MediaKind::Image => "画像",
            MediaKind::Pdf => "PDF",
            MediaKind::Epub => "EPUB",
            MediaKind::Audio => "音声",
            MediaKind::Video => "動画",
        }
    }

    /// DB（`book_contents.media_kind` / `content_formats.format_kind`）に入れる値。
    fn as_str(self) -> &'static str {
        match self {
            MediaKind::Image => "image",
            MediaKind::Pdf => "pdf",
            MediaKind::Epub => "epub",
            MediaKind::Audio => "audio",
            MediaKind::Video => "video",
        }
    }
}

fn media_kind_of(kind: EntryKind) -> Option<MediaKind> {
    match kind {
        EntryKind::Image => Some(MediaKind::Image),
        EntryKind::Pdf => Some(MediaKind::Pdf),
        EntryKind::Epub => Some(MediaKind::Epub),
        EntryKind::Audio => Some(MediaKind::Audio),
        EntryKind::Video => Some(MediaKind::Video),
        EntryKind::Cover | EntryKind::ExportText | EntryKind::Junk => None,
    }
}

fn base_title(file_name: &str) -> String {
    file_name
        .rsplit_once('.')
        .map(|(stem, _)| stem.to_string())
        .unwrap_or_else(|| file_name.to_string())
}

fn metadata_entry(
    title: &str,
    total_pages: Option<i64>,
    contents: &[ContentSpec],
) -> (Vec<u8>, String) {
    let contents_json: Vec<serde_json::Value> = contents
        .iter()
        .map(|content| {
            serde_json::json!({
                "contentId": content.content_id,
                "displayName": content.display_name,
                "mediaKind": content.media_kind.as_str(),
                "isPrimary": content.is_primary,
                "sortOrder": content.sort_order,
                "formats": content
                    .formats
                    .iter()
                    .map(|format| {
                        serde_json::json!({
                            "formatId": format.format_id,
                            "label": format.label,
                            "formatKind": format.kind.as_str(),
                            "pageCount": format.page_count,
                            "packEntryPrefix": format.pack_entry_prefix,
                            "sortOrder": format.sort_order,
                        })
                    })
                    .collect::<Vec<_>>(),
            })
        })
        .collect();
    let metadata = serde_json::json!({
        "schemaVersion": 1,
        "title": title,
        "author": "",
        "circleName": "",
        "purchaseDate": null,
        "readingProgress": { "currentPage": 0, "totalPages": total_pages },
        "contents": contents_json,
    });
    (
        serde_json::to_vec(&metadata).expect("metadata json"),
        "metadata.json".to_string(),
    )
}

/// pack の `metadata.json` に記録された 1 コンテンツの表示名を書き換えた
/// **新しい pack のバイト列**を返す（フェーズ7: 名前カスタム）。
///
/// 名前は DB（`book_contents.display_name`）と pack の両方に書く。pack を直さないと
/// Drive から復元したとき（[`rebuild_from_pack`]）に取り込み時の名前に戻ってしまう。
///
/// 変更が無いときは `None` を返す（`content_id` が無い / `metadata.json` が無い /
/// JSON が壊れている）。元の pack は変更しない。
pub fn rename_content_in_pack(
    pack_bytes: &[u8],
    pack_id: &str,
    content_id: &str,
    display_name: &str,
    root_key: Option<&PackRootKey>,
) -> Result<Option<Vec<u8>>, ImportError> {
    let reader = opfspack::PackReader::open(pack_bytes)?;
    // v3 の pack 鍵は pack id（= book id）から導出する。
    let pack_key = root_key.map(|root| root.derive_pack_key(pack_id));
    let Ok(raw) = reader.read_entry("metadata.json", pack_key.as_ref()) else {
        return Ok(None);
    };
    let Ok(mut metadata) = serde_json::from_slice::<serde_json::Value>(&raw) else {
        return Ok(None);
    };
    let Some(contents) = metadata
        .get_mut("contents")
        .and_then(|value| value.as_array_mut())
    else {
        return Ok(None);
    };
    let mut changed = false;
    for content in contents.iter_mut() {
        if content.get("contentId").and_then(|value| value.as_str()) == Some(content_id) {
            content["displayName"] = serde_json::Value::String(display_name.to_string());
            changed = true;
        }
    }
    if !changed {
        return Ok(None);
    }

    // 全エントリを読み直して組み直す（index は path 順・offset は再計算されるため、
    // metadata.json のサイズが変わっても整合する）。暗号化・圧縮の設定は元の pack に合わせる。
    let header = reader.header();
    let encrypted = header.flags & opfspack::pack_flags::ENCRYPTED != 0;
    let mut builder = opfspack::PackBuilder::new(header.created_at);
    for entry in reader.entries() {
        let data = if entry.path == "metadata.json" {
            serde_json::to_vec(&metadata).map_err(|e| ImportError::Image(e.to_string()))?
        } else {
            reader.read_entry(&entry.path, pack_key.as_ref())?
        };
        let compress = entry.flags & opfspack::entry_flags::COMPRESSED != 0;
        builder.add_entry(&entry.path, data, &entry.mime_type, compress);
    }
    let pack_key = if encrypted { pack_key } else { None };
    Ok(Some(builder.build(
        pack_key.as_ref(),
        header.flags & opfspack::pack_flags::COMPRESSED != 0,
    )?))
}

/// `book_contents` に書く 1 コンテンツ分の行。
#[derive(Clone)]
struct ContentSpec {
    content_id: String,
    display_name: String,
    media_kind: MediaKind,
    is_primary: bool,
    sort_order: i64,
    formats: Vec<FormatSpec>,
}

/// `content_formats` に書く 1 レンディション分の行。
#[derive(Clone)]
struct FormatSpec {
    format_id: String,
    label: String,
    kind: MediaKind,
    page_count: i64,
    /// pack 内でこの形式のページが置かれる接頭辞（`pages` / `contents/1/r0`）。
    pack_entry_prefix: Option<String>,
    sort_order: i64,
}

/// 単体ファイル取り込み用の「1 コンテンツ + 1 レンディション」を作る。
fn single_content(
    media_kind: MediaKind,
    format_label: &str,
    page_count: i64,
    pack_entry_prefix: Option<&str>,
) -> ContentSpec {
    ContentSpec {
        content_id: uuid::Uuid::new_v4().to_string(),
        display_name: "本文".to_string(),
        media_kind,
        is_primary: true,
        sort_order: 0,
        formats: vec![FormatSpec {
            format_id: uuid::Uuid::new_v4().to_string(),
            label: format_label.to_string(),
            kind: media_kind,
            page_count,
            pack_entry_prefix: pack_entry_prefix.map(str::to_string),
            sort_order: 0,
        }],
    }
}

struct PackSpec {
    /// 取り込み中のページデータ（合計が閾値を超えたら一時ファイルへ逃がす）。
    entries: PackEntryStore,
    page_rows: Vec<PageRow>,
    /// (page_number, text) — PDF の抽出テキストや `_export.txt` の中身。
    texts: Vec<(i64, String)>,
    /// 読み飛ばしたエントリの説明（壊れた画像など）。
    warnings: Vec<String>,
    /// 永続化するコンテンツ構造（フェーズ2）。
    contents: Vec<ContentSpec>,
    source_type: String,
    /// 既定表示（primary）コンテンツのページ数。
    total_pages: i64,
}

/// 取り込み中にページデータを保持する場所。
///
/// 小さい本はメモリに持ち（速い）、**合計が [`SPILL_THRESHOLD_BYTES`] を超えたらそれ以降は
/// 一時ファイルへ**書く。これで pack を組み立てるときのピークメモリが「閾値 + 1 ページ」に
/// 収まり、大きい本（数 GiB）でも RAM を食い潰さない（セキュリティ評価 F06 の
/// 「変換は一時ファイルへ逐次出力」）。一時ファイルは `Drop` で必ず消す。
struct PackEntryStore {
    /// (entry path, stored data, mime, compress)
    entries: Vec<(String, StoredData, String, bool)>,
    /// 受け取ったデータの合計（閾値判定に使う）。
    total_bytes: u64,
    /// 一時ファイルを置くディレクトリ（最初に必要になったときに作る）。
    spill_dir: Option<std::path::PathBuf>,
    /// 一時ファイルの連番（同じディレクトリで衝突させない）。
    spill_index: usize,
    /// これを超えたら一時ファイルへ逃がす（テストで小さくできるようにフィールドにしてある）。
    threshold: u64,
}

/// 1 エントリ分のデータの置き場所。
enum StoredData {
    /// まだ取り出していないメモリ上のデータ。
    Memory(Option<Vec<u8>>),
    /// 一時ファイル（取り出したら消す）。
    File(std::path::PathBuf),
    /// すでに builder へ渡した。
    Taken,
}

/// これを超えたら一時ファイルへ逃がす（小さい本はメモリのままにして I/O を増やさない）。
const SPILL_THRESHOLD_BYTES: u64 = 64 * 1024 * 1024;

impl PackEntryStore {
    fn new() -> Self {
        Self::with_threshold(SPILL_THRESHOLD_BYTES)
    }

    fn with_threshold(threshold: u64) -> Self {
        Self {
            entries: Vec::new(),
            total_bytes: 0,
            spill_dir: None,
            spill_index: 0,
            threshold,
        }
    }

    /// すでにメモリ上にあるエントリ群から作る（小さい本・画像 1 枚などの経路）。
    fn from_entries(entries: Vec<(String, Vec<u8>, String, bool)>) -> Self {
        let total_bytes: u64 = entries.iter().map(|(_, data, _, _)| data.len() as u64).sum();
        Self {
            entries: entries
                .into_iter()
                .map(|(path, data, mime_type, compress)| {
                    (path, StoredData::Memory(Some(data)), mime_type, compress)
                })
                .collect(),
            total_bytes,
            spill_dir: None,
            spill_index: 0,
            threshold: SPILL_THRESHOLD_BYTES,
        }
    }

    /// path のデータを取り出す（**コピー**。サムネイルの寸法確認など、小さくて
    /// 1 回しか使わない用途向け）。無ければ `None`。
    fn get(&self, path: &str) -> Result<Option<Vec<u8>>, ImportError> {
        for (entry_path, stored, _, _) in &self.entries {
            if entry_path != path {
                continue;
            }
            return match stored {
                StoredData::Memory(Some(data)) => Ok(Some(data.clone())),
                StoredData::File(file) => Ok(Some(std::fs::read(file)?)),
                StoredData::Memory(None) | StoredData::Taken => Ok(None),
            };
        }
        Ok(None)
    }

    /// 1 エントリ分を受け取る（閾値を超えていれば一時ファイルへ書く）。
    fn push(
        &mut self,
        path: String,
        data: Vec<u8>,
        mime_type: String,
        compress: bool,
    ) -> Result<(), ImportError> {
        self.total_bytes += data.len() as u64;
        let stored = if self.total_bytes > self.threshold {
            self.spill(data)?
        } else {
            StoredData::Memory(Some(data))
        };
        self.entries.push((path, stored, mime_type, compress));
        Ok(())
    }

    /// データを一時ファイルへ書く（ディレクトリは必要になったときに作る）。
    fn spill(&mut self, data: Vec<u8>) -> Result<StoredData, ImportError> {
        let dir = match &self.spill_dir {
            Some(dir) => dir.clone(),
            None => {
                let dir = std::env::temp_dir().join(format!(
                    "thundoku-import-spill-{}-{}",
                    std::process::id(),
                    chrono::Utc::now().timestamp_millis()
                ));
                std::fs::create_dir_all(&dir)?;
                self.spill_dir = Some(dir.clone());
                dir
            }
        };
        let file = dir.join(format!("{:06}.bin", self.spill_index));
        self.spill_index += 1;
        std::fs::write(&file, &data)?;
        Ok(StoredData::File(file))
    }

    /// pack へ書き出すエントリ（path と MIME と圧縮指定）。
    fn specs(&self) -> Vec<opfspack::EntrySpec> {
        self.entries
            .iter()
            .map(|(path, _, mime_type, compress)| opfspack::EntrySpec {
                path: path.clone(),
                mime_type: mime_type.clone(),
                compress: *compress,
            })
            .collect()
    }

    /// path ごとのエントリ位置（同じ path が複数ある場合は受け取った順）。
    fn index_by_path(
        &self,
    ) -> std::collections::HashMap<String, std::collections::VecDeque<usize>> {
        let mut map: std::collections::HashMap<String, std::collections::VecDeque<usize>> =
            std::collections::HashMap::new();
        for (index, (path, _, _, _)) in self.entries.iter().enumerate() {
            map.entry(path.clone()).or_default().push_back(index);
        }
        map
    }

    /// 1 エントリ分のデータを取り出す（メモリなら move、一時ファイルなら読んで消す）。
    fn take(&mut self, index: usize) -> Result<Vec<u8>, ImportError> {
        let (_, stored, _, _) = self
            .entries
            .get_mut(index)
            .ok_or_else(|| ImportError::Zip("entry index out of range".into()))?;
        match std::mem::replace(stored, StoredData::Taken) {
            StoredData::Memory(Some(data)) => Ok(data),
            StoredData::File(path) => {
                let data = std::fs::read(&path)?;
                let _ = std::fs::remove_file(&path);
                Ok(data)
            }
            StoredData::Memory(None) | StoredData::Taken => Err(ImportError::Zip(
                "pack entry already taken".into(),
            )),
        }
    }
}

impl Drop for PackEntryStore {
    fn drop(&mut self) {
        if let Some(dir) = &self.spill_dir {
            let _ = std::fs::remove_dir_all(dir);
        }
    }
}

struct PageRow {
    content_id: Option<String>,
    format_id: Option<String>,
    page_number: i64,
    width: i64,
    height: i64,
    entry_path: String,
    file_size: i64,
}

fn finish_import(
    pool: &SqlitePool,
    packs_dir: &Path,
    root_key: Option<&PackRootKey>,
    file_name: &str,
    source_bytes_len: i64,
    spec: PackSpec,
    reuse_book_id: Option<&str>,
) -> Result<ImportedBook, ImportError> {
    let book_id = book_id_for(reuse_book_id);
    let title = base_title(file_name);
    let timestamp = now();
    log::info!("finish_import: 開始（{} ページ）", spec.total_pages);
    let save_start = std::time::Instant::now();

    // 再取り込み（同じ book_id の再利用 = 再取得相当）では、**前回の content_id と表示名を
    // 引き継ぐ**。ビューアーで付けたカスタム名（`book_contents.display_name`）と、
    // content_id に紐づく進捗（`reading_progress`）・ページ毎記録（`page_views`）を維持するため。
    // 件数とメディア種別が一致するときだけ引き継ぐ（ソースが変わったときの誤対応を防ぐ）。
    let mut spec = spec;
    if reuse_book_id.is_some() {
        let previous_contents = contents::list_for_book(pool, &book_id).unwrap_or_default();
        if previous_contents.len() == spec.contents.len() {
            // 旧 content_id → 引き継ぎ先 content_id（ページ行の張り替えに使う）
            let mut remap: Vec<(String, String)> = Vec::new();
            for content in spec.contents.iter_mut() {
                if let Some(previous_content) = previous_contents
                    .iter()
                    .find(|previous| previous.sort_order == content.sort_order)
                    .filter(|previous| previous.media_kind == content.media_kind.as_str())
                {
                    if previous_content.content_id != content.content_id {
                        remap.push((
                            content.content_id.clone(),
                            previous_content.content_id.clone(),
                        ));
                    }
                    content.content_id = previous_content.content_id.clone();
                    content.display_name = previous_content.display_name.clone();
                }
            }
            // ページ行は content_id を別に持つので、同じく引き継ぎ先へ張り替える。
            // 張り替えないと削除済みの旧 content_id を参照して FK 違反になる。
            for row in spec.page_rows.iter_mut() {
                if let Some(content_id) = row.content_id.as_deref()
                    && let Some((_, new_id)) = remap.iter().find(|(old_id, _)| old_id == content_id)
                {
                    row.content_id = Some(new_id.clone());
                }
            }
        }
    }

    // Build the pack first (metadata + pages), so document.file_hash can
    // reference the real pack bytes.
    let (metadata, metadata_path) = metadata_entry(&title, Some(spec.total_pages), &spec.contents);
    // **エントリを 1 件ずつ供給して組み立てる**: ページデータは `PackEntryStore` から
    // 必要なときに取り出す（メモリか一時ファイル）。全ページを同時に持たない。
    let mut entries = vec![opfspack::EntrySpec {
        path: metadata_path.clone(),
        mime_type: "application/json".to_string(),
        compress: false,
    }];
    entries.extend(spec.entries.specs());
    // サムネイルの寸法は DB 行に要るので、`store` を動かす前に読んでおく（小さい）。
    let thumbnail_entry = spec.entries.get("thumbnail.webp")?;
    let mut store = spec.entries;
    let by_path = store.index_by_path();
    let metadata_for_build = metadata.clone();
    // v3 の pack 鍵は book id から導出する（冊ごとに別鍵）。
    // `root_key` が無い（未ログイン）ときは平文 pack。
    let pack_key = root_key.map(|root| root.derive_pack_key(&book_id));
    std::fs::create_dir_all(packs_dir)?;
    // 書き出し先は保存領域内に収まることを検証する（`book_id` は再利用 id や
    // 復元 id 由来でも同じ検査を通す）。
    let pack_file = crate::pack_path::pack_path(packs_dir, &book_id).map_err(|error| {
        ImportError::Io(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            error.to_string(),
        ))
    })?;
    // **ファイルへ直接組み立てる**（pack 全体を RAM に持たない）。書き込み途中の
    // クラッシュで壊れた pack が残らないよう、一時ファイル → rename（原子的）で置換する。
    let temp_file = pack_file.with_file_name(format!("{book_id}.{}.tmp", crate::pack_path::PACK_EXTENSION));
    let mut remainder = by_path;
    let built = PackBuilder::new(chrono::Utc::now().timestamp_millis() as u64)
        .build_to_file_streaming(
            &temp_file,
            entries,
            pack_key.as_ref(),
            true,
            |path| {
                if path == metadata_path {
                    return Ok(metadata_for_build.clone());
                }
                let index = remainder
                    .get_mut(path)
                    .and_then(std::collections::VecDeque::pop_front)
                    .ok_or_else(|| {
                        opfspack::PackError::Io(format!("import entry missing: {path}"))
                    })?;
                store
                    .take(index)
                    .map_err(|error| opfspack::PackError::Io(error.to_string()))
            },
        );
    drop(store);
    built?;
    if let Err(error) = std::fs::rename(&temp_file, &pack_file) {
        let _ = std::fs::remove_file(&temp_file);
        return Err(ImportError::Io(error));
    }
    log::info!("finish_import: パック作成（{:?}）", save_start.elapsed());
    // pack 全体のハッシュは**ファイルを順に読んで**計算する（大きい pack を RAM に載せない）。
    let pack_hash = {
        let reader = opfspack::PackFileReader::open(&pack_file)?;
        reader.source_sha256()?
    };

    let book = books::Book {
        id: book_id.clone(),
        title: title.clone(),
        author: String::new(),
        circle_name: String::new(),
        purchase_date: None,
        file_name: file_name.to_string(),
        file_size: source_bytes_len,
        opfs_path: format!("{book_id}.opfspack"),
        cover_thumbnail: None,
        tbf_product_id: None,
        site_id: None,
        tags_fetched: 1,
        pack_id: Some(book_id.clone()),
        is_favorite: 0,
        is_hidden: 0,
        created_at: timestamp.clone(),
        updated_at: timestamp.clone(),
        media_category: None,
        ai_type: None,
        is_drm: 0,
        release_date: None,
        description: None,
        theme: None,
        maker_id: None,
        page_count: None,
        age_rating: None,
        series_name: None,
    };
    // 再ダウンロード時は同じ book_id を再利用する（重複本を作らない）。
    // 既存行がある場合はユーザー状態を引き継いで置き換え、古いページ行を消してから入れる。
    let previous = reuse_book_id.and_then(|id| books::get(pool, id).ok().flatten());
    let book = match &previous {
        Some(prev) => {
            let mut book = book;
            book.is_favorite = prev.is_favorite;
            book.is_hidden = prev.is_hidden;
            book.created_at = prev.created_at.clone();
            book
        }
        None => book,
    };
    if previous.is_some() {
        documents::delete_for_book(pool, &book_id)?;
        contents::delete_for_book(pool, &book_id)?;
        books::upsert(pool, &book)?;
        log::info!("finish_import: books 更新（再取り込み）");
    } else {
        books::insert(pool, &book)?;
        log::info!("finish_import: books 挿入完了");
    }

    let document = documents::ImportedDocument {
        id: uuid::Uuid::new_v4().to_string(),
        book_id: book_id.clone(),
        source_type: spec.source_type.clone(),
        file_hash: pack_hash,
        total_pages: spec.total_pages,
        metadata: None,
        status: "completed".to_string(),
        created_at: timestamp.clone(),
        updated_at: timestamp.clone(),
    };
    documents::insert_document(pool, &document)?;
    log::info!("finish_import: document 挿入完了");

    // コンテンツ構造（book_contents / content_formats）を保存する（フェーズ2）。
    // 再取り込み時は `contents::delete_for_book` で消してから入れ直す。
    let content_rows: Vec<contents::BookContent> = spec
        .contents
        .iter()
        .map(|content| contents::BookContent {
            content_id: content.content_id.clone(),
            book_id: book_id.clone(),
            display_name: content.display_name.clone(),
            media_kind: content.media_kind.as_str().to_string(),
            is_primary: i64::from(content.is_primary),
            sort_order: content.sort_order,
            created_at: timestamp.clone(),
        })
        .collect();
    let format_rows: Vec<contents::ContentFormat> = spec
        .contents
        .iter()
        .flat_map(|content| {
            content
                .formats
                .iter()
                .map(|format| contents::ContentFormat {
                    format_id: format.format_id.clone(),
                    content_id: content.content_id.clone(),
                    label: format.label.clone(),
                    format_kind: format.kind.as_str().to_string(),
                    page_count: format.page_count,
                    pack_entry_prefix: format.pack_entry_prefix.clone(),
                    sort_order: format.sort_order,
                    created_at: timestamp.clone(),
                })
        })
        .collect();
    contents::insert_batch(pool, &content_rows, &format_rows)?;
    log::info!(
        "finish_import: contents 挿入完了（{} コンテンツ / {} レンディション）",
        content_rows.len(),
        format_rows.len()
    );

    // 画像・テキスト・トークンをバッチで一括 INSERT する
    // （1 件 1 クエリだと数百ページ × トークン数万件の block_on が重く、
    //   取り込みが遅くなるため。トランザクション + バッチに集約する）。
    let mut image_rows = Vec::with_capacity(spec.page_rows.len() + 1);
    for row in &spec.page_rows {
        image_rows.push(documents::DocumentImage {
            id: uuid::Uuid::new_v4().to_string(),
            document_id: document.id.clone(),
            content_id: row.content_id.clone(),
            format_id: row.format_id.clone(),
            page_number: row.page_number,
            image_type: "page".to_string(),
            opfs_path: format!("{book_id}.opfspack"),
            width: row.width,
            height: row.height,
            mime_type: "image/webp".to_string(),
            file_size: row.file_size,
            pack_entry_path: Some(row.entry_path.clone()),
            created_at: timestamp.clone(),
        });
    }
    // サムネイル行は既定表示コンテンツのものとして紐づける
    let primary_ids = spec
        .contents
        .iter()
        .find(|content| content.is_primary)
        .map(|content| {
            (
                content.content_id.clone(),
                content
                    .formats
                    .first()
                    .map(|format| format.format_id.clone()),
            )
        });
    if let Some(data) = thumbnail_entry {
        let (width, height) = image::load_from_memory(&data)
            .map(|d| (d.width() as i64, d.height() as i64))
            .unwrap_or((0, 0));
        image_rows.push(documents::DocumentImage {
            id: uuid::Uuid::new_v4().to_string(),
            document_id: document.id.clone(),
            content_id: primary_ids
                .as_ref()
                .map(|(content_id, _)| content_id.clone()),
            format_id: primary_ids
                .as_ref()
                .and_then(|(_, format_id)| format_id.clone()),
            page_number: 1,
            image_type: "thumbnail".to_string(),
            opfs_path: format!("{book_id}.opfspack"),
            width,
            height,
            mime_type: "image/webp".to_string(),
            file_size: data.len() as i64,
            pack_entry_path: Some("thumbnail.webp".to_string()),
            created_at: timestamp.clone(),
        });
    }
    documents::insert_images_batch(pool, &image_rows)?;
    log::info!("finish_import: images バッチ挿入完了");

    let mut text_rows = Vec::with_capacity(spec.texts.len());
    for (page_number, text) in &spec.texts {
        text_rows.push(documents::DocumentText {
            id: uuid::Uuid::new_v4().to_string(),
            document_id: document.id.clone(),
            page_number: *page_number,
            text_content: text.clone(),
            created_at: timestamp.clone(),
        });
    }
    documents::insert_texts_batch(pool, &text_rows)?;
    log::info!("finish_import: texts バッチ挿入完了");

    // Token analysis rows (nouns per page).
    let mut token_rows = Vec::new();
    for (page_number, text) in &spec.texts {
        for (word, count) in crate::tags::extract_nouns(text, &[&title]) {
            token_rows.push(documents::TokenRow {
                id: uuid::Uuid::new_v4().to_string(),
                document_id: document.id.clone(),
                page_number: *page_number,
                token: word.clone(),
                pos: "名詞".to_string(),
                base_form: Some(word.clone()),
                reading: None,
                frequency: count as i64,
                created_at: timestamp.clone(),
            });
        }
    }
    documents::insert_tokens_batch(pool, &token_rows)?;

    log::info!(
        "finish_import: DB 書き込み完了（images/texts/tokens）（{:?}）",
        save_start.elapsed()
    );
    // Generated tags (Zenn matching with noun-only fallback).
    let zenn_tags = crate::tags::fetch_zenn_tags().unwrap_or_default();
    let text_refs: Vec<&str> = spec.texts.iter().map(|(_, text)| text.as_str()).collect();
    let generated = crate::tags::generate_tags(&text_refs, &[&title], &zenn_tags);
    let tag_pairs: Vec<(&str, &str)> = generated
        .iter()
        .map(|t| (t.as_str(), "generated"))
        .collect();
    tags_repo::set_for_book(pool, &book_id, &tag_pairs)?;

    log::info!("finish_import: 完了（{:?}）", save_start.elapsed());
    Ok(ImportedBook {
        book,
        document,
        tags: generated,
        warnings: spec.warnings,
    })
}

/// Import a file by extension: pdf / epub / zip.
pub fn import_file(
    pool: &SqlitePool,
    source_path: &Path,
    packs_dir: &Path,
    root_key: Option<&PackRootKey>,
    progress: &mut (dyn FnMut(f32) + Send),
) -> Result<ImportedBook, ImportError> {
    let file_name = source_path
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("import")
        .to_string();
    let extension = source_path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_lowercase();
    // **読む前に**大きさを見る（読んでから気付くと、その時点で RAM を食っている）。
    let size = std::fs::metadata(source_path)?.len();
    if size > MAX_IMPORT_SOURCE_BYTES {
        return Err(ImportError::SourceTooLarge {
            size,
            limit: MAX_IMPORT_SOURCE_BYTES,
        });
    }
    let bytes = std::fs::read(source_path)?;
    match extension.as_str() {
        "pdf" => import_pdf_bytes(
            pool, &file_name, &bytes, packs_dir, root_key, progress, None,
        ),
        "epub" => import_epub_bytes(pool, &file_name, &bytes, packs_dir, root_key, None),
        "zip" => import_zip_bytes(
            pool, &file_name, &bytes, packs_dir, root_key, progress, None,
        ),
        _ => Err(ImportError::UnsupportedType(extension)),
    }
}

/// Import a PDF from memory.
pub fn import_pdf_bytes(
    pool: &SqlitePool,
    file_name: &str,
    bytes: &[u8],
    packs_dir: &Path,
    root_key: Option<&PackRootKey>,
    progress: &mut (dyn FnMut(f32) + Send),
    reuse_book_id: Option<&str>,
) -> Result<ImportedBook, ImportError> {
    let pages = pdf::render_pdf_pages(bytes, progress)?;
    import_rendered_pdf_pages(
        pool,
        file_name,
        bytes.len() as i64,
        pages,
        packs_dir,
        root_key,
        reuse_book_id,
    )
}

/// Import already-rendered PDF pages. Rendering is expensive and must happen
/// **outside** the DB lock, so callers run `pdf::render_pdf_pages` first and
/// only take the connection for this final write step.
pub fn import_rendered_pdf_pages(
    pool: &SqlitePool,
    file_name: &str,
    file_size: i64,
    pages: Vec<pdf::PageImage>,
    packs_dir: &Path,
    root_key: Option<&PackRootKey>,
    reuse_book_id: Option<&str>,
) -> Result<ImportedBook, ImportError> {
    if pages.is_empty() {
        return Err(ImportError::Pdf("no pages rendered".into()));
    }
    let total_pages = pages.len() as i64;
    let content = single_content(MediaKind::Pdf, "PDF", total_pages, Some("pages"));
    let content_id = content.content_id.clone();
    let format_id = content.formats[0].format_id.clone();
    let mut entries = Vec::new();
    let mut page_rows = Vec::new();
    let mut texts = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        let entry_path = format!("pages/page_{:04}.webp", index + 1);
        entries.push((
            entry_path.clone(),
            page.data.clone(),
            "image/webp".to_string(),
            false,
        ));
        page_rows.push(PageRow {
            content_id: Some(content_id.clone()),
            format_id: Some(format_id.clone()),
            page_number: index as i64 + 1,
            width: page.width as i64,
            height: page.height as i64,
            entry_path,
            file_size: page.data.len() as i64,
        });
        texts.push((index as i64 + 1, page.text.clone()));
    }
    // cover = page 1; thumbnail = page 1 scaled to 200px width.
    entries.push((
        "cover.webp".to_string(),
        pages[0].data.clone(),
        "image/webp".to_string(),
        false,
    ));
    let (thumb, _, _) = thumbnail_of(&pages[0].data)?;
    entries.push((
        "thumbnail.webp".to_string(),
        thumb,
        "image/webp".to_string(),
        false,
    ));

    finish_import(
        pool,
        packs_dir,
        root_key,
        file_name,
        file_size,
        PackSpec {
            entries: PackEntryStore::from_entries(entries),
            page_rows,
            texts,
            warnings: Vec::new(),
            contents: vec![content],
            source_type: "pdf".to_string(),
            total_pages,
        },
        reuse_book_id,
    )
}

/// Import an EPUB as a single raw entry (viewing not supported — same as Web).
pub fn import_epub_bytes(
    pool: &SqlitePool,
    file_name: &str,
    bytes: &[u8],
    packs_dir: &Path,
    root_key: Option<&PackRootKey>,
    reuse_book_id: Option<&str>,
) -> Result<ImportedBook, ImportError> {
    let entry_path = file_name.to_string();
    finish_import(
        pool,
        packs_dir,
        root_key,
        file_name,
        bytes.len() as i64,
        PackSpec {
            entries: PackEntryStore::from_entries(vec![(
                entry_path,
                bytes.to_vec(),
                "application/epub+zip".to_string(),
                false,
            )]),
            page_rows: Vec::new(),
            texts: Vec::new(),
            warnings: Vec::new(),
            contents: vec![single_content(MediaKind::Epub, "EPUB", 0, None)],
            source_type: "epub".to_string(),
            total_pages: 0,
        },
        reuse_book_id,
    )
}

/// ファイル名を (is_numeric, chunk) の列に分解する（自然順ソート用）。
fn natural_key(s: &str) -> Vec<(bool, String)> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_num = false;
    for c in s.chars() {
        let is_num = c.is_ascii_digit();
        if cur.is_empty() || is_num == cur_num {
            cur.push(c);
        } else {
            out.push((cur_num, std::mem::take(&mut cur)));
            cur.push(c);
        }
        cur_num = is_num;
    }
    if !cur.is_empty() {
        out.push((cur_num, cur));
    }
    out
}

/// ファイル名の自然順比較（`1.jpg` < `2.jpg` < `10.jpg`）。数字の連続は数値として比較する。
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let ka = natural_key(a);
    let kb = natural_key(b);
    for (ia, ib) in ka.iter().zip(kb.iter()) {
        let ord = if ia.0 && ib.0 {
            let at = ia.1.trim_start_matches('0');
            let bt = ib.1.trim_start_matches('0');
            at.len().cmp(&bt.len()).then_with(|| at.cmp(bt))
        } else {
            ia.1.cmp(&ib.1)
        };
        if ord != std::cmp::Ordering::Equal {
            return ord;
        }
    }
    ka.len().cmp(&kb.len())
}

/// 既定の優先コンテンツを選ぶ（表紙・junk は `build_contents` で除外済み）。
///
/// 1. 名前に「本文」系の語を含むもの
/// 2. ページ数の目安が最大のもの
/// 3. 同数なら PDF / EPUB を優先、最後は索引順
fn choose_primary(contents: &[PlannedContent]) -> usize {
    // ビューアで読める種別だけを候補にする（音声・動画は取り込めない）
    let candidates: Vec<usize> = contents
        .iter()
        .enumerate()
        .filter(|(_, content)| is_viewable_media(content.media_kind))
        .map(|(index, _)| index)
        .collect();
    let Some(&fallback) = candidates.first() else {
        return 0;
    };
    if let Some(&body) = candidates
        .iter()
        .find(|&&index| is_body_name(&contents[index].display_name))
    {
        return body;
    }
    let mut best = fallback;
    for &index in &candidates {
        let pages = contents[index].page_hint();
        let best_pages = contents[best].page_hint();
        let prefer_media = matches!(contents[index].media_kind, MediaKind::Pdf | MediaKind::Epub)
            && !matches!(contents[best].media_kind, MediaKind::Pdf | MediaKind::Epub);
        if pages > best_pages || (pages == best_pages && prefer_media) {
            best = index;
        }
    }
    best
}

/// 現行ビューアで読めるメディア種別か（コンテンツの候補・取り込み対象）。
fn is_viewable_media(kind: MediaKind) -> bool {
    matches!(kind, MediaKind::Image | MediaKind::Pdf | MediaKind::Epub)
}

/// PDF を描画する。`progress` が `Some` のときだけ進捗を流す
/// （`progress` の型がプラットフォームで違うため、ここで吸収する）。
fn render_pdf_with(
    bytes: &[u8],
    progress: Option<&mut (dyn FnMut(f32) + Send)>,
) -> Result<Vec<pdf::PageImage>, ImportError> {
    match progress {
        Some(progress) => pdf::render_pdf_pages(bytes, progress),
        None => pdf::render_pdf_pages(bytes, &mut |_| {}),
    }
}

/// 名前に「本文」系の語を含むか（docs/import-patterns.md §5.1 の語彙）。
fn is_body_name(name: &str) -> bool {
    name.contains("本文") || name.contains("本編")
}

/// ZIP を解析して取り込み計画を立てる。DB にもディスクにも書かない。
pub fn analyze_zip(bytes: &[u8]) -> Result<ImportPlan, ImportError> {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| ImportError::Zip(e.to_string()))?;
    // 1 周目: 名前だけを集める（ページ画像は伸長しない）。
    // 入れ子 ZIP はここで 1 階層だけ展開して名前一覧に合流させる（決定 D5）。
    let mut warnings = Vec::new();
    let metas = collect_metas_with_nested(&mut archive, &mut warnings)?;
    if metas.is_empty() {
        return Err(ImportError::EmptyArchive);
    }
    // **展開の前**に外側全体の件数と宣言サイズの合計を見る（セキュリティ評価 F06）。
    // 個別の上限（`MAX_ZIP_ENTRY_BYTES`）だけでは、上限内のエントリが大量にある
    // アーカイブで変換を走らせてしまう。宣言サイズは信用しない（実際の長さは
    // 読み出し時に別途上限で見る）が、事前に弾ける分はここで弾く。
    let declared_total: u64 = metas
        .iter()
        .fold(0u64, |acc, meta| acc.saturating_add(meta.declared_size));
    if metas.len() > opfspack::MAX_ENTRY_COUNT as usize {
        return Err(ImportError::ZipTooLarge {
            detail: format!(
                "エントリ数 {} が上限 {} を超えています",
                metas.len(),
                opfspack::MAX_ENTRY_COUNT
            ),
        });
    }
    if declared_total > opfspack::MAX_TOTAL_SIZE {
        return Err(ImportError::ZipTooLarge {
            detail: format!(
                "展開後の合計 {declared_total} バイトが上限 {} バイトを超えています",
                opfspack::MAX_TOTAL_SIZE
            ),
        });
    }
    let contents = build_contents(&metas);
    let primary = choose_primary(&contents);

    // `_export.txt` は本文テキストなので解析時に読む（小さく、画像の伸長は伴わない）。
    let mut export_texts: Vec<(i64, String)> = Vec::new();
    for meta in metas
        .iter()
        .filter(|meta| classify_entry(&meta.name) == EntryKind::ExportText)
    {
        match read_entry(&mut archive, meta) {
            Ok(data) => {
                let decoded = zip_names::decode_text_bytes(&data);
                export_texts.extend(export_text::parse_export_text(&decoded));
            }
            Err(error) => warnings.push(format!("{}: {error}", meta.name)),
        }
    }

    let skip_reason = contents.is_empty().then_some(SkipReason::NotAReadableWork);
    Ok(ImportPlan {
        contents,
        primary,
        export_text: export_texts,
        warnings,
        skip_reason,
    })
}

/// `ImportPlan` の全コンテンツ／全レンディションを実際に取り込み、pack と DB を作る。
///
/// pack 内のパスは、既定表示コンテンツの第 1 レンディションだけ従来どおり
/// `pages/page_NNNN.webp` に置く（既存 pack・カバー規約との互換）。それ以外は
/// `contents/{content}/{rendition}/...` に入れる。
// 引数は取り込みの文脈そのもの（plan を足すと 8 個になる）。分割すると呼び出し側で
// 束ね直すだけなので、そのまま受け取る。
#[allow(clippy::too_many_arguments)]
pub fn commit_zip(
    pool: &SqlitePool,
    file_name: &str,
    bytes: &[u8],
    packs_dir: &Path,
    root_key: Option<&PackRootKey>,
    progress: &mut (dyn FnMut(f32) + Send),
    reuse_book_id: Option<&str>,
    plan: &ImportPlan,
) -> Result<ImportedBook, ImportError> {
    let primary = plan
        .contents
        .get(plan.primary)
        .ok_or(ImportError::NotAReadableWork)?;
    if !is_viewable_media(primary.media_kind) {
        // 音声・動画は現行ビューアの対象外（docs/import-patterns.md §3.3）
        return Err(ImportError::NotAReadableWork);
    }
    // 内部不整合（ordinal が範囲外）用。呼び出し側の入力に由来する
    // 「読めるものが無い」は `NotAReadableWork` で返す。
    let missing_entry = || ImportError::Zip("entry index out of range".into());
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| ImportError::Zip(e.to_string()))?;
    // 入れ子 ZIP の展開警告は `plan.warnings` に既に入っている
    // （`analyze_zip` が同じ規則で解析する）ため、ここでは捨てる。
    let metas = collect_metas_with_nested(&mut archive, &mut Vec::new())?;

    // 2 周目: 選んだエントリだけを 1 件ずつ読む。
    // 旧実装は全エントリを `Vec<(String, Vec<u8>)>` に読み込んでいたため、
    // 巨大 ZIP（実データ最大 1.19GB / 展開後 1.33GB）で展開後のデータを
    // 同時に保持していた。ここでは 1 件ずつ伸長して使い終わったら捨てる。
    let mut pack_entries = PackEntryStore::new();
    let mut page_rows: Vec<PageRow> = Vec::new();
    let mut warnings = plan.warnings.clone();
    let mut contents_spec: Vec<ContentSpec> = Vec::new();
    let mut primary_pages = 0i64;
    let mut primary_thumbnail: Option<Vec<u8>> = None;
    let mut primary_cover: Option<Vec<u8>> = None;
    // ZIP に PDF / EPUB が 1 つだけ入っている場合は、その中身を本の実体として
    // 扱う（題名・ファイル名を内側のエントリ名から採る。フェーズ1の仕様）。
    let mut book_file_name = file_name.to_string();

    for (content_index, content) in plan.contents.iter().enumerate() {
        let is_primary = content_index == plan.primary;
        let content_id = uuid::Uuid::new_v4().to_string();
        let mut formats = Vec::new();
        for (rendition_index, rendition) in content.renditions.iter().enumerate() {
            let format_id = uuid::Uuid::new_v4().to_string();
            // 既定表示コンテンツの第 1 レンディションだけ従来のパスに置く
            let legacy = is_primary && rendition_index == 0;
            let prefix = if legacy {
                "pages".to_string()
            } else {
                format!("contents/{content_index}/r{rendition_index}")
            };
            let page_count = match rendition.kind {
                MediaKind::Image => {
                    let start = page_rows.len();
                    // ページ変換（デコード + webp 再圧縮）が重いので、チャンク単位で
                    // 並列に処理する（順序は保つ）。チャンクに切るのは、全ページ分の
                    // 圧縮バイトを一度に抱えないため。
                    for chunk in rendition.entries.chunks(PAGE_RENDER_CHUNK) {
                        // 1) チャンク分のページバイトを順に読む（ZIP は直列アクセス）
                        let mut pages = Vec::with_capacity(chunk.len());
                        for ordinal in chunk {
                            let meta = metas.get(*ordinal).ok_or_else(missing_entry)?;
                            pages.push((meta.name.clone(), read_entry(&mut archive, meta)?));
                        }
                        // 2) デコード + webp 再圧縮を並列に
                        let rendered = render_page_images(&pages);
                        // 3) 入力順に積む（壊れた画像 1 枚で全体を失敗させない: 決定 D6）
                        for ((name, _), result) in pages.iter().zip(rendered) {
                            let (webp, width, height) = match result {
                                Ok(rendered) => rendered,
                                Err(error) => {
                                    warnings.push(format!("{name}: {error}"));
                                    continue;
                                }
                            };
                            if legacy && primary_thumbnail.is_none() {
                                primary_thumbnail = Some(webp.clone());
                            }
                            let page_number = (page_rows.len() - start) as i64 + 1;
                            let entry_path = format!("{prefix}/page_{page_number:04}.webp");
                            pack_entries.push(
                                entry_path.clone(),
                                webp.clone(),
                                "image/webp".to_string(),
                                false,
                            )?;
                            page_rows.push(PageRow {
                                content_id: Some(content_id.clone()),
                                format_id: Some(format_id.clone()),
                                page_number,
                                width: width as i64,
                                height: height as i64,
                                entry_path,
                                file_size: webp.len() as i64,
                            });
                        }
                    }
                    (page_rows.len() - start) as i64
                }
                MediaKind::Pdf => {
                    let ordinal = rendition.entries.first().ok_or_else(missing_entry)?;
                    let meta = metas.get(*ordinal).ok_or_else(missing_entry)?;
                    let data = read_entry(&mut archive, meta)?;
                    // 進捗は既定表示コンテンツの PDF だけに流す（他は描画の副作用を避ける）
                    let pages = if legacy {
                        render_pdf_with(&data, Some(&mut *progress))?
                    } else {
                        render_pdf_with(&data, None)?
                    };
                    for (index, page) in pages.iter().enumerate() {
                        let page_number = index as i64 + 1;
                        let entry_path = format!("{prefix}/page_{page_number:04}.webp");
                        pack_entries.push(
                            entry_path.clone(),
                            page.data.clone(),
                            "image/webp".to_string(),
                            false,
                        )?;
                        page_rows.push(PageRow {
                            content_id: Some(content_id.clone()),
                            format_id: Some(format_id.clone()),
                            page_number,
                            width: page.width as i64,
                            height: page.height as i64,
                            entry_path,
                            file_size: page.data.len() as i64,
                        });
                    }
                    if legacy {
                        book_file_name = entry_file_name(&meta.name).to_string();
                        primary_thumbnail = pages.first().map(|page| page.data.clone());
                        primary_cover = pages.first().map(|page| page.data.clone());
                    }
                    pages.len() as i64
                }
                MediaKind::Epub => {
                    let ordinal = rendition.entries.first().ok_or_else(missing_entry)?;
                    let meta = metas.get(*ordinal).ok_or_else(missing_entry)?;
                    let data = read_entry(&mut archive, meta)?;
                    let entry_path = if legacy {
                        book_file_name = entry_file_name(&meta.name).to_string();
                        entry_file_name(&meta.name).to_string()
                    } else {
                        format!("{prefix}/{}", entry_file_name(&meta.name))
                    };
                    pack_entries.push(
                        entry_path,
                        data,
                        "application/epub+zip".to_string(),
                        false,
                    )?;
                    0
                }
                // 音声・動画はページを持たない（構造だけ記録する）
                MediaKind::Audio | MediaKind::Video => 0,
            };
            if legacy {
                primary_pages = page_count;
            }
            formats.push(FormatSpec {
                format_id,
                label: rendition.label.clone(),
                kind: rendition.kind,
                page_count,
                pack_entry_prefix: Some(prefix),
                sort_order: rendition_index as i64,
            });
        }
        contents_spec.push(ContentSpec {
            content_id,
            display_name: content.display_name.clone(),
            media_kind: content.media_kind,
            is_primary,
            sort_order: content_index as i64,
            formats,
        });
    }

    // 既定表示コンテンツが読めるページを持っていること（EPUB はページ列を持たない）
    if primary_pages == 0 && primary.media_kind != MediaKind::Epub {
        return Err(ImportError::Image(format!(
            "primary content has no pages: {}",
            primary.display_name
        )));
    }
    if let Some(thumbnail_source) = &primary_thumbnail {
        if let Some(cover_source) = &primary_cover {
            pack_entries.push(
                "cover.webp".to_string(),
                cover_source.clone(),
                "image/webp".to_string(),
                false,
            )?;
        }
        let (thumb, _, _) = thumbnail_of(thumbnail_source)?;
        pack_entries.push(
            "thumbnail.webp".to_string(),
            thumb,
            "image/webp".to_string(),
            false,
        )?;
    }

    let source_type = match primary.media_kind {
        MediaKind::Pdf => "pdf",
        MediaKind::Epub => "epub",
        _ => "image-set",
    };
    finish_import(
        pool,
        packs_dir,
        root_key,
        &book_file_name,
        bytes.len() as i64,
        PackSpec {
            entries: pack_entries,
            page_rows,
            texts: plan.export_text.clone(),
            warnings,
            contents: contents_spec,
            source_type: source_type.to_string(),
            total_pages: primary_pages,
        },
        reuse_book_id,
    )
}

/// Import a ZIP: `analyze_zip` で計画を立て、既定の優先コンテンツを取り込む。
pub fn import_zip_bytes(
    pool: &SqlitePool,
    file_name: &str,
    bytes: &[u8],
    packs_dir: &Path,
    root_key: Option<&PackRootKey>,
    progress: &mut (dyn FnMut(f32) + Send),
    reuse_book_id: Option<&str>,
) -> Result<ImportedBook, ImportError> {
    let plan = analyze_zip(bytes)?;
    commit_zip(
        pool,
        file_name,
        bytes,
        packs_dir,
        root_key,
        progress,
        reuse_book_id,
        &plan,
    )
}

/// pack（`.opfspack`）から DB の取り込み状態（`imported_documents` / `book_contents` /
/// `content_formats` / `document_images`）を再構築する（Drive 復元用）。
///
/// - すでにその本のドキュメント行があるときは何もしない（ローカルの取り込みを壊さない）
/// - 構造は pack の `metadata.json` の `contents`（フェーズ2で書き出し）を使い、
///   無い場合はエントリから 1 コンテンツとして推定する
/// - ページ画像の寸法はエントリのヘッダから読む（画素デコードはしない）
/// - 戻り値は再構築したかどうか
pub fn rebuild_from_pack(
    pool: &SqlitePool,
    pack_id: &str,
    reader: &dyn PackRead,
    root_key: Option<&PackRootKey>,
) -> Result<bool, ImportError> {
    if documents::get_document_by_book_id(pool, pack_id)?.is_some() {
        return Ok(false);
    }
    // v3 の pack 鍵は pack id（= book id）から導出する。
    let pack_key = root_key.map(|root| root.derive_pack_key(pack_id));
    let timestamp = now();
    let entry_paths: Vec<String> = reader.entries().iter().map(|e| e.path.clone()).collect();

    // metadata.json から題名と構造を読む
    let metadata: Option<serde_json::Value> = reader
        .read_entry("metadata.json", pack_key.as_ref())
        .ok()
        .and_then(|data| serde_json::from_slice(&data).ok());
    let title = metadata
        .as_ref()
        .and_then(|value| value.get("title").and_then(|v| v.as_str()))
        .unwrap_or(pack_id)
        .to_string();
    let mut contents: Vec<ContentSpec> = metadata
        .as_ref()
        .and_then(|value| value.get("contents"))
        .and_then(|value| value.as_array())
        .map(|list| list.iter().filter_map(content_from_metadata).collect())
        .unwrap_or_default();
    if contents.is_empty() {
        contents = infer_contents(&title, &entry_paths);
    }
    if contents.is_empty() {
        return Ok(false); // ページもコンテンツも無い pack は復元しない
    }
    let primary = contents
        .iter()
        .find(|content| content.is_primary)
        .cloned()
        .or_else(|| contents.first().cloned())
        .expect("contents is not empty");

    // 画像行（ページ + サムネイル）。document 行は最後に作るが、行の document_id は先に採番する
    let document_id = uuid::Uuid::new_v4().to_string();
    let mut image_rows: Vec<documents::DocumentImage> = Vec::new();
    let mut primary_pages = 0i64;
    for entry in reader.entries() {
        let path = &entry.path;
        if path == "metadata.json" {
            continue;
        }
        let Ok(data) = reader.read_entry(path, pack_key.as_ref()) else {
            continue;
        };
        let (width, height) = image_dimensions(&data);
        if path == "thumbnail.webp" {
            image_rows.push(document_image_row(
                &document_id,
                pack_id,
                &primary.content_id,
                primary
                    .formats
                    .first()
                    .map(|format| format.format_id.clone()),
                "thumbnail",
                1,
                path,
                width,
                height,
                data.len() as i64,
                timestamp.clone(),
            ));
            continue;
        }
        if path == "cover.webp" {
            continue; // カバーは pack 側のエントリで完結（行は作らない）
        }
        let Some((content, format)) = contents.iter().find_map(|content| {
            content
                .formats
                .iter()
                .find(|format| {
                    format
                        .pack_entry_prefix
                        .as_deref()
                        .is_some_and(|prefix| path.starts_with(prefix))
                })
                .map(|format| (content, format))
        }) else {
            continue; // EPUB の生エントリなど
        };
        let Some(page_number) = page_number_of(path) else {
            continue;
        };
        let is_primary_format = content.content_id == primary.content_id
            && primary
                .formats
                .first()
                .is_some_and(|first| first.format_id == format.format_id);
        if is_primary_format {
            primary_pages = primary_pages.max(page_number);
        }
        image_rows.push(document_image_row(
            &document_id,
            pack_id,
            &content.content_id,
            Some(format.format_id.clone()),
            "page",
            page_number,
            path,
            width,
            height,
            data.len() as i64,
            timestamp.clone(),
        ));
    }
    if primary_pages == 0 {
        return Ok(false);
    }

    let source_type = match primary.media_kind {
        MediaKind::Pdf => "pdf",
        MediaKind::Epub => "epub",
        _ => "image-set",
    };
    let document = documents::ImportedDocument {
        id: document_id.clone(),
        book_id: pack_id.to_string(),
        source_type: source_type.to_string(),
        file_hash: reader.source_sha256()?,
        total_pages: primary_pages,
        metadata: None,
        status: "completed".to_string(),
        created_at: timestamp.clone(),
        updated_at: timestamp.clone(),
    };
    documents::insert_document(pool, &document)?;

    let content_rows: Vec<contents::BookContent> = contents
        .iter()
        .map(|content| contents::BookContent {
            content_id: content.content_id.clone(),
            book_id: pack_id.to_string(),
            display_name: content.display_name.clone(),
            media_kind: content.media_kind.as_str().to_string(),
            is_primary: i64::from(content.is_primary),
            sort_order: content.sort_order,
            created_at: timestamp.clone(),
        })
        .collect();
    let format_rows: Vec<contents::ContentFormat> = contents
        .iter()
        .flat_map(|content| {
            content
                .formats
                .iter()
                .map(|format| contents::ContentFormat {
                    format_id: format.format_id.clone(),
                    content_id: content.content_id.clone(),
                    label: format.label.clone(),
                    format_kind: format.kind.as_str().to_string(),
                    page_count: format.page_count,
                    pack_entry_prefix: format.pack_entry_prefix.clone(),
                    sort_order: format.sort_order,
                    created_at: timestamp.clone(),
                })
        })
        .collect();
    contents::insert_batch(pool, &content_rows, &format_rows)?;
    documents::insert_images_batch(pool, &image_rows)?;
    log::info!(
        "drive restore: pack から再構築（{pack_id}: {} コンテンツ / {} ページ）",
        content_rows.len(),
        primary_pages
    );
    Ok(true)
}

/// `metadata.json` の 1 コンテンツ分を復元する。
fn content_from_metadata(value: &serde_json::Value) -> Option<ContentSpec> {
    let content_id = value.get("contentId")?.as_str()?.to_string();
    let media_kind = media_kind_from_str(value.get("mediaKind")?.as_str()?)?;
    let formats = value
        .get("formats")
        .and_then(|value| value.as_array())
        .map(|formats| {
            formats
                .iter()
                .filter_map(|format| {
                    Some(FormatSpec {
                        format_id: format.get("formatId")?.as_str()?.to_string(),
                        label: format
                            .get("label")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                        kind: media_kind_from_str(format.get("formatKind")?.as_str()?)?,
                        page_count: format
                            .get("pageCount")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0),
                        pack_entry_prefix: format
                            .get("packEntryPrefix")
                            .and_then(|v| v.as_str())
                            .map(str::to_string),
                        sort_order: format
                            .get("sortOrder")
                            .and_then(|v| v.as_i64())
                            .unwrap_or(0),
                    })
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    Some(ContentSpec {
        content_id,
        display_name: value
            .get("displayName")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        media_kind,
        is_primary: value
            .get("isPrimary")
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
        sort_order: value.get("sortOrder").and_then(|v| v.as_i64()).unwrap_or(0),
        formats,
    })
}

/// `contents` を持たない古い pack から 1 コンテンツを推定する。
fn infer_contents(title: &str, entry_paths: &[String]) -> Vec<ContentSpec> {
    let pages: Vec<&String> = entry_paths
        .iter()
        .filter(|path| page_number_of(path).is_some())
        .collect();
    let raw = entry_paths
        .iter()
        .find(|path| path.ends_with(".epub") || (path.ends_with(".pdf") && !path.contains('/')));
    let (media_kind, prefix) = if !pages.is_empty() {
        (MediaKind::Image, "pages")
    } else if let Some(path) = raw {
        if path.ends_with(".epub") {
            (MediaKind::Epub, "")
        } else {
            (MediaKind::Pdf, "")
        }
    } else {
        return Vec::new();
    };
    let page_count = pages.len() as i64;
    vec![ContentSpec {
        content_id: uuid::Uuid::new_v4().to_string(),
        display_name: title.to_string(),
        media_kind,
        is_primary: true,
        sort_order: 0,
        formats: vec![FormatSpec {
            format_id: uuid::Uuid::new_v4().to_string(),
            label: media_kind.label().to_string(),
            kind: media_kind,
            page_count,
            pack_entry_prefix: if prefix.is_empty() {
                None
            } else {
                Some(prefix.to_string())
            },
            sort_order: 0,
        }],
    }]
}

fn media_kind_from_str(value: &str) -> Option<MediaKind> {
    match value {
        "image" => Some(MediaKind::Image),
        "pdf" => Some(MediaKind::Pdf),
        "epub" => Some(MediaKind::Epub),
        "audio" => Some(MediaKind::Audio),
        "video" => Some(MediaKind::Video),
        _ => None,
    }
}

/// `pages/page_0001.webp` / `contents/0/r1/page_0012.webp` からページ番号を取る。
fn page_number_of(path: &str) -> Option<i64> {
    let name = path.rsplit('/').next()?;
    let stem = name.rsplit_once('.').map(|(stem, _)| stem).unwrap_or(name);
    stem.strip_prefix("page_")?.parse().ok()
}

/// 画像の寸法をヘッダから読む（画素はデコードしない）。
fn image_dimensions(data: &[u8]) -> (i64, i64) {
    image::ImageReader::new(std::io::Cursor::new(data))
        .with_guessed_format()
        .ok()
        .and_then(|reader| reader.into_dimensions().ok())
        .map(|(width, height)| (width as i64, height as i64))
        .unwrap_or((0, 0))
}

/// 再構築した 1 行分（ページ / サムネイル）。
#[allow(clippy::too_many_arguments)]
fn document_image_row(
    document_id: &str,
    pack_id: &str,
    content_id: &str,
    format_id: Option<String>,
    image_type: &str,
    page_number: i64,
    entry_path: &str,
    width: i64,
    height: i64,
    file_size: i64,
    timestamp: String,
) -> documents::DocumentImage {
    documents::DocumentImage {
        id: uuid::Uuid::new_v4().to_string(),
        document_id: document_id.to_string(),
        content_id: Some(content_id.to_string()),
        format_id,
        page_number,
        image_type: image_type.to_string(),
        opfs_path: format!("{pack_id}.opfspack"),
        width,
        height,
        mime_type: "image/webp".to_string(),
        file_size,
        pack_entry_path: Some(entry_path.to_string()),
        created_at: timestamp,
    }
}

/// 単体画像（jpg / png / webp / gif 等）を 1 ページの本として取り込む。
/// BOOTH のダウンロードが PDF ではなく画像ファイルの場合に使う。
pub fn import_image_bytes(
    pool: &SqlitePool,
    file_name: &str,
    bytes: &[u8],
    packs_dir: &Path,
    root_key: Option<&PackRootKey>,
    reuse_book_id: Option<&str>,
) -> Result<ImportedBook, ImportError> {
    let decoded = image::load_from_memory(bytes).map_err(|e| ImportError::Image(e.to_string()))?;
    // 元画像が小さい場合は Lanczos3 で 1000px 幅まで拡大してから保存する
    let decoded = upscale_if_small(&decoded, 1000);
    let (width, height) = (decoded.width(), decoded.height());
    let webp = encode_webp(&decoded, 88)?;
    let (thumb, _, _) = thumbnail_of(&webp)?;
    let content = single_content(MediaKind::Image, "画像", 1, Some("pages"));
    let content_id = content.content_id.clone();
    let format_id = content.formats[0].format_id.clone();
    finish_import(
        pool,
        packs_dir,
        root_key,
        file_name,
        bytes.len() as i64,
        PackSpec {
            entries: PackEntryStore::from_entries(vec![
                (
                    "pages/page_0001.webp".to_string(),
                    webp.clone(),
                    "image/webp".to_string(),
                    false,
                ),
                (
                    "thumbnail.webp".to_string(),
                    thumb,
                    "image/webp".to_string(),
                    false,
                ),
            ]),
            page_rows: vec![PageRow {
                content_id: Some(content_id),
                format_id: Some(format_id),
                page_number: 1,
                width: width as i64,
                height: height as i64,
                entry_path: "pages/page_0001.webp".to_string(),
                file_size: webp.len() as i64,
            }],
            texts: Vec::new(),
            warnings: Vec::new(),
            contents: vec![content],
            source_type: "image".to_string(),
            total_pages: 1,
        },
        reuse_book_id,
    )
}

#[cfg(test)]
mod natural_sort_tests {
    use super::{natural_cmp, natural_key};
    use std::cmp::Ordering;

    #[test]
    fn natural_cmp_orders_numeric_sequences() {
        let mut v = vec!["2.jpg", "10.jpg", "1.jpg", "3.jpg"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, vec!["1.jpg", "2.jpg", "3.jpg", "10.jpg"]);
    }

    #[test]
    fn natural_cmp_matches_lexicographic_for_nonnumeric() {
        assert_eq!(natural_cmp("a.jpg", "b.jpg"), Ordering::Less);
        assert_eq!(natural_cmp("b.jpg", "a.jpg"), Ordering::Greater);
    }

    #[test]
    fn natural_key_splits_numeric_runs() {
        assert_eq!(
            natural_key("page10.jpg"),
            vec![
                (false, "page".into()),
                (true, "10".into()),
                (false, ".jpg".into())
            ]
        );
    }
}

#[cfg(test)]
mod pack_root_key_tests {
    use super::{ImportError, pack_root_key_for_import};
    use crate::google::GoogleProfile;
    use opfspack::PackRootKey;

    fn profile(sub: &str) -> GoogleProfile {
        GoogleProfile {
            sub: sub.to_string(),
            email: "u@example.com".to_string(),
            name: "ユーザー".to_string(),
            picture: None,
        }
    }

    /// ログイン中の取り込みは PRK を解決して暗号化する（冊ごとの鍵は book id から導出）。
    #[test]
    fn logged_in_resolves_the_root_key() {
        let owner = profile("sub-1");
        let root = PackRootKey::generate();
        let expected = root.clone();
        let resolved = pack_root_key_for_import(Some(&owner), move |sub| {
            assert_eq!(sub, "sub-1", "解決には本人の sub を渡す");
            Ok(root)
        })
        .expect("鍵があるので暗号化できる");

        assert_eq!(resolved.as_bytes(), expected.as_bytes());
    }

    /// ログイン済みなのに鍵が取れない場合は**平文に落とさず**取り込みを失敗させる。
    #[test]
    fn logged_in_without_key_fails_instead_of_plaintext() {
        let owner = profile("sub-1");
        let error = pack_root_key_for_import(Some(&owner), |_| {
            Err(ImportError::IdentityKeyUnavailable)
        })
        .expect_err("鍵が無いなら平文にしない");

        // 利用者に伝わる文言（内部型名や空文字を出さない）。
        // 「旧形式のため再取り込みが必要」（v2 pack の案内）とは別の、復元を促す文言。
        let message = error.to_string();
        assert!(
            matches!(error, ImportError::IdentityKeyUnavailable),
            "{error:?}"
        );
        assert!(message.contains("鍵"), "{message}");
        assert!(!message.contains("旧形式"), "{message}");
        assert!(!message.contains("ImportError"), "{message}");
    }

    /// 未ログインの取り込みは**失敗させる**（平文 pack を作らない — F03 fail-closed）。
    ///
    /// v3 の pack 鍵は Google アカウント（`sub`）ごとに作るため、未ログインでは
    /// 用意できない。平文で作り続けると所有者（`owner_sub`）を持たない本が増え、
    /// Drive 同期の対象にもならない（読めるのはその端末だけになる）。
    #[test]
    fn logged_out_requires_login() {
        let error = pack_root_key_for_import(None, |_| panic!("未ログインでは解決しない"))
            .expect_err("未ログインでは平文 pack を作らない");

        // 利用者に伝わる文言（内部型名や空文字を出さない）。
        let message = error.to_string();
        assert!(matches!(error, ImportError::LoginRequired), "{error:?}");
        assert!(message.contains("Google"), "{message}");
        assert!(message.contains("ログイン"), "{message}");
        assert!(!message.contains("ImportError"), "{message}");
    }

    /// 空の `sub` では暗号化しない（`owner_id` と `sub` ラップの材料が空になる）。
    #[test]
    fn empty_sub_is_rejected() {
        for sub in ["", "   "] {
            let broken = profile(sub);
            let error = pack_root_key_for_import(Some(&broken), |_| {
                panic!("空の sub では鍵を解決しない")
            })
            .expect_err("空の sub で暗号化しない");
            assert!(matches!(error, ImportError::IdentitySubMissing), "{error:?}");
        }
    }
}


#[cfg(test)]
mod pack_entry_store_tests {
    use super::{PackEntryStore, SPILL_THRESHOLD_BYTES};

    fn entries(store: &PackEntryStore) -> Vec<(String, String, bool)> {
        store
            .specs()
            .into_iter()
            .map(|spec| (spec.path, spec.mime_type, spec.compress))
            .collect()
    }

    /// 閾値まではメモリに持ち、一時ファイルを作らない（小さい本で I/O を増やさない）。
    #[test]
    fn keeps_small_entries_in_memory() {
        let mut store = PackEntryStore::new();
        store
            .push("a".into(), vec![1u8; 10], "image/webp".into(), false)
            .unwrap();
        assert!(store.spill_dir.is_none(), "一時ファイルを作らない");
        assert_eq!(
            entries(&store),
            vec![("a".to_string(), "image/webp".to_string(), false)]
        );
        assert_eq!(store.get("a").unwrap(), Some(vec![1u8; 10]));
        // index は受け取った順に同じ path を複数扱える
        store
            .push("a".into(), vec![2u8; 10], "image/webp".into(), true)
            .unwrap();
        let index = store.index_by_path();
        assert_eq!(index["a"].clone().into_iter().collect::<Vec<_>>(), vec![0, 1]);
        assert_eq!(store.specs().len(), 2);
    }

    /// 閾値を超えたら一時ファイルへ書き、取り出したら消す（合計メモリを抑える）。
    #[test]
    fn spills_entries_over_the_threshold_to_a_temp_file() {
        let mut store = PackEntryStore::with_threshold(100);
        store
            .push("small".into(), vec![1u8; 10], "image/webp".into(), false)
            .unwrap();
        assert!(store.spill_dir.is_none(), "閾値まではメモリ");
        // 合計 10 + 200 > 100 → こちらはファイルへ。
        store
            .push("big".into(), vec![9u8; 200], "image/webp".into(), false)
            .unwrap();
        let dir = store.spill_dir.clone().expect("一時ディレクトリを作る");
        assert!(dir.exists());
        let files: Vec<_> = std::fs::read_dir(&dir).unwrap().flatten().collect();
        assert_eq!(files.len(), 1, "大きいエントリだけファイルへ");

        // メモリ側もファイル側も同じ内容が取り出せる。
        assert_eq!(store.get("small").unwrap(), Some(vec![1u8; 10]));
        assert_eq!(store.get("big").unwrap(), Some(vec![9u8; 200]));

        let index = store.index_by_path();
        let small_index = index["small"].clone().pop_front().unwrap();
        let big_index = index["big"].clone().pop_front().unwrap();
        assert_eq!(store.take(small_index).unwrap(), vec![1u8; 10]);
        assert_eq!(store.take(big_index).unwrap(), vec![9u8; 200]);
        // 2 回目は取れない（builder が同じエントリを二度要求していないことの検査）
        assert!(store.take(big_index).is_err());

        drop(store);
        assert!(!dir.exists(), "drop で一時ディレクトリを消す");
    }

    /// 一時ファイルを消しても（= drop しても）網羅的に残らない。
    #[test]
    fn default_threshold_is_the_documented_value() {
        assert_eq!(SPILL_THRESHOLD_BYTES, 64 * 1024 * 1024);
        let store = PackEntryStore::new();
        assert_eq!(store.threshold, SPILL_THRESHOLD_BYTES);
    }
}

#[cfg(test)]
mod zip_entry_limit_tests {
    use super::{
        ImportError, MAX_ZIP_ENTRY_BYTES, read_zip_entry_capped_detail,
        zip_entry_too_large_error,
    };
    use std::io::Write as _;

    /// 上限ちょうどは読めて、1 バイト超は `None` + 実際に読めた長さを返す。
    ///
    /// 実際の上限（2 GiB）を試すのは非現実的なので、小さな上限で同じ判定を通す
    /// （`limit` を受け取る形にしてある）。
    #[test]
    fn capped_read_reports_the_actual_length_when_over_the_limit() {
        let mut buffer = std::io::Cursor::new(Vec::new());
        {
            let mut writer = zip::ZipWriter::new(&mut buffer);
            let options: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            writer.start_file("pages/page_0001.webp", options).unwrap();
            writer.write_all(&vec![7u8; 2000]).unwrap();
            writer.finish().unwrap();
        }
        let bytes = buffer.into_inner();

        // 上限内（2000 バイトちょうど）は読める
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes.clone())).unwrap();
        let (data, read) = read_zip_entry_capped_detail(&mut archive, 0, 2000).unwrap();
        assert_eq!(read, 2000);
        assert_eq!(data.map(|d| d.len()), Some(2000));

        // 上限 1999 では読めない（読み切った長さ 2000 を報告する）
        let mut archive = zip::ZipArchive::new(std::io::Cursor::new(bytes)).unwrap();
        let (data, read) = read_zip_entry_capped_detail(&mut archive, 0, 1999).unwrap();
        assert!(data.is_none(), "上限を超えたらデータを返さない");
        assert_eq!(read, 2000, "実際に読めた長さを報告する");
    }

    /// エラー文言に内訳（宣言サイズ・実際に読めた長さ）と上限値が入る。
    #[test]
    fn too_large_error_includes_the_numbers() {
        let error = zip_entry_too_large_error("book.pdf", 1_000, 700_000_000);
        let ImportError::Zip(message) = &error else {
            panic!("Zip エラーになる: {error}");
        };
        assert!(message.contains("book.pdf"), "{message}");
        assert!(
            message.contains(&MAX_ZIP_ENTRY_BYTES.to_string()),
            "上限値が入る: {message}"
        );
        assert!(message.contains("宣言 1000 バイト"), "{message}");
        assert!(message.contains("実際に読めた 700000000 バイト"), "{message}");
    }
}
