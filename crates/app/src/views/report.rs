//! レポート画面: GitHub の Issue を**ブラウザーで**投稿する。
//!
//! アプリは GitHub のトークンを取得・保存しない（Issue を 1 つ作るためだけに
//! `public_repo` を利用者へ求めるのは権限が過大）。「Issue を作成」で投稿先リポジトリの
//! Issue 作成画面を**既定のブラウザーで開く**だけにして、投稿そのものは利用者が GitHub の
//! 画面で行う（未ログインでも投稿できる）。投稿先はこのアプリのリポジトリ（[`TARGET_REPO`]）に
//! 固定する。宛先を利用者が差し替えられると、誘導された利用者に別のリポジトリへ文面を
//! 送らせる余地になるため。
//!
//! テンプレート（`.github/ISSUE_TEMPLATE/*.yml`）は**匿名で**取得し、[`compose_body`] で
//! 本文の下書きに展開する。取得に失敗しても本文へ直接書けるようにして、レポート画面を
//! 壊さない（理由だけを出して再取得できる）。
//! 本文が長すぎて URL に載らないときは、本文をクリップボードへコピーしてから素の作成画面を
//! 開く（判定は core の `issue_link`）。
//!
//! 画像（本文欄へのドロップ / クリップボードから追加）はサムネイルとして画面に出し、
//! **アプリからはアップロードしない**。「Issue を作成」で画像をクリップボードへ「ファイル」
//! として置き、利用者が GitHub の本文で `Ctrl+V` するとアップロードされる（`clipboard::copy_files`。
//! アプリは GitHub のトークンを持たないため、これが認証なしで画像を渡せる唯一の経路）。
//! **ファイルとして置けるのは Windows だけ**（他 OS では画面の案内どおり GitHub の画面で添付する）。
//!
//! テンプレート取得の HTTP は UI スレッドでは実行しない。`background_executor` に投げ、
//! 完了は `cx.spawn` で受ける。入力欄は `Window` が要るため render の冒頭で遅延生成する
//! （`new` は `cx.new(ReportView::new)` から呼ばれるので `Window` を持てない）。
//!
//! **ブラウザーを開く処理とクリップボード操作は update の外（背景スレッド）で行う**。
//! Windows の `ShellExecuteW` はシェルがメッセージループを回すため、App 借用中（update の中）
//! に呼ぶと gpui の window proc が再入して `RefCell already borrowed`（最悪 panic = アプリ終了。
//! 実測 2026-10-04: クリックごとに 3 件のエラー）になる。

use std::path::PathBuf;
use std::sync::Arc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Icon, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, ClipboardEntry, Context, Entity, ExternalPaths, FontWeight, ImageFormat,
    InteractiveElement as _, IntoElement, ParentElement, Render, RenderImage, SharedString,
    StatefulInteractiveElement as _, Styled as _, Window, div, img, px,
};
use thundoku_core::github::{
    GithubError, IssueTemplate, TemplateField, TemplateFieldKind, issue_link,
};
// テンプレート取得のクライアント。取得はテストでは実行しない（`ensure_templates` を参照）。
#[cfg(not(test))]
use thundoku_core::github::GithubClient;

use crate::app_state::{ToastKind, set_toast_kind};
use crate::components::image_thumb::decode_and_resize;

/// 投稿先リポジトリの owner（**固定**）。
const TARGET_REPO_OWNER: &str = "MegaBlackLabel";
/// 投稿先リポジトリの名前（**固定**）。
const TARGET_REPO_NAME: &str = "thundoku-shelf-app";
/// 画面に出す投稿先の表示。
pub(crate) const TARGET_REPO: &str = "MegaBlackLabel/thundoku-shelf-app";

/// 本文入力の高さ。これより長い本文は入力欄の中でスクロールする。
const BODY_HEIGHT: f32 = 280.0;

/// 画像の受け渡し方法（アプリからはアップロードしないことを画面に明記する）。
#[cfg(target_os = "windows")]
const IMAGE_NOTE: &str = "画像は本文欄へドロップ（または Ctrl+V）で添付できます。\
                          「Issue を作成」で画像をクリップボードへコピーするので、\
                          開いた GitHub の本文で Ctrl+V すると画像が上がります\
                          （アプリからはアップロードしません）";
/// 画像の受け渡し方法（クリップボードへファイルを置けない OS では GitHub の画面で添付してもらう）。
#[cfg(not(target_os = "windows"))]
const IMAGE_NOTE: &str =
    "画像は GitHub の画面で添付してください（このアプリからはアップロードしません）";

/// 添付できる画像 1 件の上限。大きすぎるファイルは読み込まない（サムネイルも作らない）。
const MAX_ATTACHMENT_BYTES: u64 = 25 * 1024 * 1024;
/// 添付の上限枚数。
const MAX_ATTACHMENTS: usize = 5;
/// 添付サムネイルの最大幅（px）。
const ATTACHMENT_THUMB_WIDTH: u32 = 128;

/// レポート画面のキーコンテキスト。`Ctrl+V` のインターセプタが「この画面か」を見るのに使う。
const REPORT_KEY_CONTEXT: &str = "Report";

/// 貼り付けのショートカット表示（macOS は `Cmd+V`）。
const PASTE_LABEL: &str = if cfg!(target_os = "macos") {
    "Cmd+V"
} else {
    "Ctrl+V"
};

/// 説明文を引用（blockquote）にして本文の下書きに載せる。
fn quote(text: &str) -> String {
    text.lines()
        .map(|line| {
            if line.trim().is_empty() {
                ">".to_string()
            } else {
                format!("> {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

/// 選択したテンプレートの各項目を Markdown の下書きに展開する。
///
/// - `markdown` の項目は `value`（説明文）をそのまま 1 節として出す
/// - それ以外は `## {label}` の見出し + `placeholder`（無ければ `value`）
/// - `description` は見出しの下に引用として出す。issue form では入力欄の補足として
///   表示される文で、「秘密情報を確認してから貼る」のような注意書きがここに書かれる。
///   アプリのフォームは field 単位の入力欄を作らないので、本文に載せて見えるようにする
/// - 節は空行 1 つで区切る（末尾に余分な空行は残さない）
/// - label も value も placeholder も無い項目はスキップする
fn compose_body(fields: &[TemplateField]) -> String {
    let mut sections: Vec<String> = Vec::new();
    for field in fields {
        let label = field
            .label
            .as_deref()
            .map(str::trim)
            .filter(|label| !label.is_empty());
        let text = field
            .placeholder
            .as_deref()
            .or(field.value.as_deref())
            .map(str::trim)
            .filter(|text| !text.is_empty());
        let description = field
            .description
            .as_deref()
            .map(str::trim)
            .filter(|description| !description.is_empty())
            .map(quote);
        let section = match field.kind {
            // 説明文は見出しを持たないので、本文だけをそのまま出す。
            TemplateFieldKind::Markdown => text.map(str::to_string),
            _ => match (label, text) {
                (Some(label), Some(text)) => Some(match description {
                    Some(description) => format!("## {label}\n{description}\n{text}"),
                    None => format!("## {label}\n{text}"),
                }),
                // 見出しが無い項目は本文だけを出す（`## ` だけの行を作らない）。
                (None, Some(text)) => Some(text.to_string()),
                // 本文が空でも見出しは出す（利用者がそこへ書く）。
                (Some(label), None) => Some(match description {
                    Some(description) => format!("## {label}\n{description}"),
                    None => format!("## {label}"),
                }),
                (None, None) => None,
            },
        };
        if let Some(section) = section {
            sections.push(section);
        }
    }
    sections.join("\n\n").trim_end().to_string()
}

/// 画像をクリップボードで渡すときの本文。先頭に「ここで Ctrl+V」の案内を足す。
///
/// GitHub のエディタはマークダウンの原文が見えているので案内が読め、**レンダリング後は
/// HTML コメントなので Issue には残らない**（貼り忘れて残っても本文を汚さない）。
fn attachment_body(body: &str, count: usize) -> String {
    let notice = format!(
        "<!-- 画像 {count} 件をクリップボードへコピーしました。\
         貼り付けたい位置で Ctrl+V してください（アプリからはアップロードしません） -->"
    );
    if body.trim().is_empty() {
        notice
    } else {
        format!("{notice}\n\n{body}")
    }
}

/// `GithubError` を画面に出す日本語にする。
///
/// 原因を取り違えると次の操作が変わってしまう（待って再試行する / アプリを更新する /
/// 通信を確認する）ため、種別ごとに別の文言を返す。
fn error_message(error: &GithubError) -> String {
    match error {
        GithubError::Network(_) => {
            "GitHub に接続できませんでした（オフラインか、GitHub 側の障害かもしれません）"
                .to_string()
        }
        // 匿名アクセスの拒否（403）。待っても直らないので「再取得」を促すだけにする。
        GithubError::Forbidden(detail) => {
            format!("GitHub がアクセスを拒否しました（時間をおいて再取得してください）: {detail}")
        }
        GithubError::RateLimited { retry_after } => match retry_after {
            Some(seconds) => format!(
                "GitHub の回数制限に当たりました。約 {seconds} 秒待ってからもう一度お試しください"
            ),
            None => "GitHub の回数制限に当たりました。しばらく待ってからもう一度お試しください"
                .to_string(),
        },
        GithubError::InvalidResponse(detail) => {
            format!("GitHub から予期しない応答が返りました: {detail}")
        }
    }
}

/// 添付した画像 1 件。
struct Attachment {
    /// クリップボードへ渡すファイル（クリップボードから取り込んだ画像はアプリが作った一時ファイル）。
    path: PathBuf,
    /// 画面に出すサムネイル。
    thumb: Arc<RenderImage>,
}

/// 既定のブラウザーで URL を開く処理。
type OpenBrowser = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;
/// 画像を「ファイル」としてクリップボードへ置く処理（`text` があれば本文も一緒に）。
type WriteClipboardFiles =
    Arc<dyn Fn(&[PathBuf], Option<&str>) -> Result<(), String> + Send + Sync>;
/// 文字列をクリップボードへ置く処理。
type WriteClipboardText = Arc<dyn Fn(&str) -> Result<(), String> + Send + Sync>;

/// 送信で外へ出る処理。テストは記録するだけの実装を差し込む。
///
/// いずれも Windows ではシェル / クリップボードを触る（`ShellExecuteW` はメッセージループを
/// 回す）。**update の中で呼ばない**（[`ReportView::submit`] を参照）。
#[derive(Clone)]
struct ReportIo {
    /// 既定のブラウザーで URL を開く。
    open_browser: OpenBrowser,
    /// 画像を「ファイル」としてクリップボードへ置く（`text` があれば本文も一緒に）。
    write_files: WriteClipboardFiles,
    /// 文字列をクリップボードへ置く（画像が無いときの本文）。
    write_text: WriteClipboardText,
}

impl Default for ReportIo {
    fn default() -> Self {
        Self {
            open_browser: Arc::new(|url| {
                thundoku_core::google::open_browser(url).map_err(|error| error.to_string())
            }),
            write_files: Arc::new(|paths, text| {
                thundoku_core::clipboard::copy_files(paths, text).map_err(|error| error.to_string())
            }),
            write_text: Arc::new(|text| {
                thundoku_core::clipboard::copy_text(text).map_err(|error| error.to_string())
            }),
        }
    }
}

/// 送信に失敗した原因（画面の文言が変わる）。
enum SubmitFailure {
    /// クリップボードへの書き込みに失敗（ブラウザーは開けた）。
    Clipboard(String),
    /// ブラウザーを開けなかった（クリップボードへの書き込みは成功したかもしれない）。
    OpenBrowser(String),
}

/// 画像ファイルを読み込んで添付にする（画像でない・大きすぎる・読めないときは理由を返す）。
fn load_attachment(path: &PathBuf) -> Result<Attachment, String> {
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
        .unwrap_or_else(|| path.display().to_string());
    let metadata =
        std::fs::metadata(path).map_err(|error| format!("{name} を読めません: {error}"))?;
    if !metadata.is_file() {
        return Err(format!("{name} はファイルではありません"));
    }
    if metadata.len() > MAX_ATTACHMENT_BYTES {
        return Err(format!(
            "{name} は大きすぎます（{}MB まで）",
            MAX_ATTACHMENT_BYTES / 1024 / 1024
        ));
    }
    let bytes = std::fs::read(path).map_err(|error| format!("{name} を読めません: {error}"))?;
    let thumb = decode_and_resize(&bytes, ATTACHMENT_THUMB_WIDTH)
        .ok_or_else(|| format!("{name} は対応していない画像形式です（PNG / JPEG / WebP）"))?;
    Ok(Attachment {
        path: path.clone(),
        thumb,
    })
}

/// クリップボードの画像（ファイルでない）を置く一時ファイルのフォルダ。
fn clipboard_attachment_dir() -> PathBuf {
    std::env::temp_dir()
        .join("thundoku-shelf")
        .join("attachments")
}

/// アプリが作った一時ファイルか（外すときに消す責任がある）。
fn is_temporary(path: &std::path::Path) -> bool {
    path.starts_with(clipboard_attachment_dir())
}

/// クリップボードの画像を置く一時フォルダを用意し、**使っていない**古いファイルを捨てる。
///
/// フォルダを丸ごと作り直すと、いま画面に出ている添付（まだ送っていない画像）の実体まで
/// 消えてしまう（`CF_HDROP` には実ファイルが要る）。そのため `keep` に載っているものは残す。
fn prepare_attachment_dir(keep: &[PathBuf]) -> Result<PathBuf, String> {
    let dir = clipboard_attachment_dir();
    std::fs::create_dir_all(&dir).map_err(|error| format!("一時フォルダを作れません: {error}"))?;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if !keep.contains(&path) {
                let _ = std::fs::remove_file(&path);
            }
        }
    }
    Ok(dir)
}

/// クリップボードの画像を一時ファイルへ書き出す（`dir` は [`prepare_attachment_dir`] が返す）。
///
/// `CF_HDROP` に載せるには実ファイルが要る（アプリは画像の中身を GitHub へ送らない）。
fn write_clipboard_image(
    dir: &std::path::Path,
    image: &gpui_kit::Image,
) -> Result<PathBuf, String> {
    let extension = match image.format {
        ImageFormat::Jpeg => "jpg",
        ImageFormat::Webp => "webp",
        // Windows のクリップボードは PNG にして返す（`gpui-pre-windows` の変換）。
        _ => "png",
    };
    let path = dir.join(format!("clipboard-{}.{extension}", image.id()));
    std::fs::write(&path, &image.bytes)
        .map_err(|error| format!("クリップボードの画像を保存できません: {error}"))?;
    Ok(path)
}

/// 送信に成功したときのトースト。
fn success_message(attachments: usize, copied_body: bool) -> String {
    match (attachments, copied_body) {
        (0, false) => "既定のブラウザーで Issue の作成画面を開きました".to_string(),
        (0, true) => "本文をコピーしました。GitHub の画面に貼り付けてください".to_string(),
        (count, false) => {
            format!("画像 {count} 件をコピーしました。GitHub の本文で Ctrl+V すると添付されます")
        }
        // 画像と本文を同時に置くと、貼り付け先（GitHub の本文）は画像を優先する。本文は
        // テキストとして貼る操作（Ctrl+Shift+V）で取り出せる（実測 2026-10-04）。
        (count, true) => format!(
            "画像 {count} 件と本文をコピーしました。GitHub の本文で Ctrl+V（画像）/ Ctrl+Shift+V（本文）を押してください"
        ),
    }
}

/// 送信に失敗したときの画面のエラー（原因ごとに次の操作が変わる）。
fn failure_message(attachments: usize, copied_body: bool, failure: SubmitFailure) -> String {
    match failure {
        SubmitFailure::Clipboard(error) => {
            let what = match (attachments, copied_body) {
                (count, true) if count > 0 => "画像と本文",
                (count, _) if count > 0 => "画像",
                _ => "本文",
            };
            format!(
                "{what}をクリップボードへコピーできませんでした（GitHub の画面で添付・貼り付けしてください）: {error}"
            )
        }
        SubmitFailure::OpenBrowser(error) => {
            // コピー済みなら本文 / 画像は失われていない（開く先だけ利用者が用意する）。
            let copied = match (attachments, copied_body) {
                (count, _) if count > 0 => "画像はコピー済みです。",
                (_, true) => "本文はコピー済みです。",
                _ => "",
            };
            format!(
                "ブラウザーを開けませんでした（{copied}既定のブラウザーの設定を確認してください）: {error}"
            )
        }
    }
}

pub struct ReportView {
    /// タイトル入力（render で遅延生成）。
    title_input: Option<Entity<InputState>>,
    /// 本文入力（複数行。render で遅延生成）。
    body_input: Option<Entity<TextareaState>>,
    /// 取得した Issue テンプレート（`config.yml` は core 側で除かれる）。
    templates: Vec<IssueTemplate>,
    /// 選択中のテンプレートの `file_name`（下書きの入れ直しに使う）。
    selected_template: Option<String>,
    /// テンプレート取得中（二重実行防止）。
    templates_loading: bool,
    /// 取得を試みたか（render ごとに取得し直さない）。
    templates_fetched: bool,
    /// テンプレートを取得できなかった理由（自由入力で投稿はできる）。
    templates_error: Option<String>,
    /// 添付した画像（`MAX_ATTACHMENTS` 件まで）。
    attachments: Vec<Attachment>,
    /// サムネイルを読み込んでいる件数（0 より大きいとき「読み込み中」を出す）。
    attaching: usize,
    /// 添付を追加できなかった理由（画像でない・大きすぎる・上限）。
    attach_error: Option<String>,
    /// 送信で外へ出る処理（テストは差し替える）。
    io: ReportIo,
    /// 本文欄の `Ctrl+V` を横取りする購読（view が生きている間だけ有効。持っている必要がある）。
    _paste_interceptor: gpui_kit::Subscription,
    /// 画面に赤字で出すエラー（タイトル未入力・ブラウザーを開けない）。
    error: Option<String>,
}

impl ReportView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self::with_io(cx, ReportIo::default())
    }

    /// 外へ出る処理を差し替えて作る（テストは記録装置を渡す）。
    fn with_io(cx: &mut Context<Self>, io: ReportIo) -> Self {
        // 本文欄の `Ctrl+V` を（画像があるときだけ）先に受け取る。キーバインドが解決される前に
        // 走る `intercept_keystrokes` を使う（`capture_key_down` はバインド処理で打ち切られて
        // 届かない）。アプリ全体に効くので、レポート画面のキーコンテキスト（`Report`）を
        // 見て他の画面では横取りしない。
        let weak = cx.entity().downgrade();
        let _paste_interceptor =
            cx.intercept_keystrokes(move |event: &gpui_kit::KeystrokeEvent, _window, cx| {
                // 貼り付けのキーは OS で違う（macOS は `cmd-v`、それ以外は `ctrl-v`。
                // 本文欄のバインドも同じ切り替えになっている）。
                let paste_modifier = event.keystroke.modifiers.control
                    || (cfg!(target_os = "macos") && event.keystroke.modifiers.platform);
                if event.keystroke.key != "v" || !paste_modifier {
                    return;
                }
                let on_report_screen = event.context_stack.iter().any(|context| {
                    context
                        .primary()
                        .is_some_and(|entry| entry.key == REPORT_KEY_CONTEXT)
                });
                if !on_report_screen {
                    return;
                }
                let Some(view) = weak.upgrade() else {
                    return;
                };
                if view.update(cx, |this, cx| this.paste_attachments(cx)) {
                    cx.stop_propagation();
                }
            });
        Self {
            title_input: None,
            body_input: None,
            templates: Vec::new(),
            selected_template: None,
            templates_loading: false,
            templates_fetched: false,
            templates_error: None,
            attachments: Vec::new(),
            attaching: 0,
            attach_error: None,
            io,
            _paste_interceptor,
            error: None,
        }
    }

    /// 入力欄を遅延生成する（`new` は `Window` を持てないため render で作る）。
    fn ensure_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.title_input.is_none() {
            let state = cx.new(|cx| {
                InputState::new(window, cx).placeholder("[Bug] 一覧のスクロールが引っかかる")
            });
            self.title_input = Some(state);
        }
        if self.body_input.is_none() {
            let state = cx.new(|cx| {
                TextareaState::new(window, cx)
                    .placeholder("テンプレートを選ぶと下書きが入ります。")
            });
            self.body_input = Some(state);
        }
    }

    /// 表示時に 1 度だけテンプレートを取得する（**匿名**。ログインは要らない）。
    fn ensure_templates(&mut self, cx: &mut Context<Self>) {
        if self.templates_loading || self.templates_fetched {
            return;
        }
        self.templates_loading = true;
        self.templates_fetched = true;
        self.start_template_fetch(cx);
    }

    /// テンプレート取得を背景で始める。
    ///
    /// テストは外部（HTTP）へ出ない。取得と解析の経路は core のテストが固定している
    /// （`GithubClient::list_issue_templates`）。
    #[cfg(not(test))]
    fn start_template_fetch(&mut self, cx: &mut Context<Self>) {
        let (owner, repo) = (TARGET_REPO_OWNER.to_string(), TARGET_REPO_NAME.to_string());
        // 弱参照: 画面が閉じたあとの完了でビューを復活させない。
        let handle = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { GithubClient::new().list_issue_templates(&owner, &repo) })
                .await;
            let _ = handle.update(cx, |this, cx| {
                this.templates_loading = false;
                match result {
                    Ok(templates) => {
                        this.templates = templates;
                        this.templates_error = None;
                    }
                    // テンプレートが無くても自由入力で投稿できる（理由だけ出す）。
                    Err(error) => {
                        log::warn!("report: テンプレートを取得できませんでした: {error}");
                        this.templates_error = Some(error_message(&error));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// テスト版: 取得せずに「テンプレートが無い」状態で止める。
    #[cfg(test)]
    fn start_template_fetch(&mut self, _cx: &mut Context<Self>) {
        self.templates_loading = false;
    }

    /// テンプレートを選ぶと、タイトルと本文の下書きを入力欄へ入れる。
    fn select_template(&mut self, file_name: &str, window: &mut Window, cx: &mut Context<Self>) {
        let Some(template) = self
            .templates
            .iter()
            .find(|template| template.file_name == file_name)
            .cloned()
        else {
            return;
        };
        self.selected_template = Some(template.file_name.clone());
        self.error = None;
        let title = template.title.clone().unwrap_or_default();
        let body = compose_body(&template.fields);
        if let Some(input) = self.title_input.clone() {
            input.update(cx, |state, cx| state.set_value(title, window, cx));
        }
        if let Some(input) = self.body_input.clone() {
            input.update(cx, |state, cx| state.set_value(body, window, cx));
        }
        cx.notify();
    }

    /// 「Issue を作成」: 投稿先の Issue 作成画面を既定のブラウザーで開く。
    ///
    /// 画像はアプリからアップロードせず、クリップボードへ「ファイル」として置く（利用者が
    /// GitHub の本文で `Ctrl+V` すると添付される）。本文が長すぎて URL に載らないときは、
    /// 本文もクリップボードへ渡す。
    ///
    /// ブラウザーを開く処理とクリップボード操作は **update の外**（背景スレッド）で行う。
    /// Windows の `ShellExecuteW` はシェルがメッセージループを回し、`OpenClipboard` も
    /// 他プロセスと交渉する。App 借用中に呼ぶと gpui の window proc が再入して
    /// `RefCell already borrowed`（最悪 panic = アプリ終了）になる（実測 2026-10-04）。
    /// 結果は後続の update で反映する。
    fn submit(&mut self, cx: &mut Context<Self>) {
        let Some(title_input) = self.title_input.clone() else {
            return;
        };
        let title = title_input.read(cx).value().trim().to_string();
        if title.is_empty() {
            self.error = Some("タイトルを入力してください".to_string());
            cx.notify();
            return;
        }
        let body = self
            .body_input
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default();
        let files: Vec<PathBuf> = self
            .attachments
            .iter()
            .map(|attachment| attachment.path.clone())
            .collect();
        let attachments = files.len();
        // 画像を渡すときは、本文の先頭に「ここで Ctrl+V」の案内を入れる。GitHub のエディタ
        // （マークダウン）では見えて、レンダリング後は HTML コメントなので見えない。本文は
        // URL に載るかクリップボードへ渡るかのどちらでも案内が届くように、リンクを作る前に足す。
        let body = if attachments == 0 {
            body
        } else {
            attachment_body(&body, attachments)
        };
        let link = issue_link(TARGET_REPO_OWNER, TARGET_REPO_NAME, &title, &body);
        // 画像があれば画像（+ 本文）を、無ければ本文だけをクリップボードへ置く。
        let copied_body = link.copy_body.is_some();
        let text = link.copy_body.clone().filter(|_| !files.is_empty());
        let text_only = link.copy_body.filter(|_| files.is_empty());
        let url = link.url.clone();
        let io = self.io.clone();
        if attachments > 0 {
            log::info!("report: 画像 {attachments} 件をクリップボードへ渡します");
        }
        cx.spawn(async move |this, cx| {
            // シェル / クリップボードは UI スレッドの外で扱う（update の借用と衝突させない）。
            // `cx.write_to_clipboard` も `OpenClipboard` を叩くので update の中では呼ばない。
            let failure = cx
                .background_executor()
                .spawn(async move {
                    let clipboard_failure = if !files.is_empty() {
                        (io.write_files)(&files, text.as_deref())
                            .err()
                            .map(SubmitFailure::Clipboard)
                    } else if let Some(text) = text_only {
                        (io.write_text)(&text).err().map(SubmitFailure::Clipboard)
                    } else {
                        None
                    };
                    // コピーに失敗してもブラウザーは開く（本文は URL に載っている）。
                    let open_failure = (io.open_browser)(&url)
                        .err()
                        .map(SubmitFailure::OpenBrowser);
                    // 開けない方が深刻（投稿そのものが始められない）ので優先して出す。
                    open_failure.or(clipboard_failure)
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.finish_submit(failure, attachments, copied_body, cx)
            });
        })
        .detach();
    }

    /// 送信の後処理（トースト / エラー）。update の外から呼ばれる。
    fn finish_submit(
        &mut self,
        failure: Option<SubmitFailure>,
        attachments: usize,
        copied_body: bool,
        cx: &mut Context<Self>,
    ) {
        match failure {
            None => {
                self.error = None;
                set_toast_kind(
                    cx,
                    ToastKind::Info,
                    success_message(attachments, copied_body),
                );
            }
            Some(failure) => {
                let message = failure_message(attachments, copied_body, failure);
                log::warn!("report: 送信できませんでした: {message}");
                self.error = Some(message);
            }
        }
        cx.notify();
    }

    /// `Ctrl+V`（本文欄 / タイトル）: クリップボードに**画像 / ファイルがあるときだけ**添付に
    /// 取り込む。
    ///
    /// 本文欄は `Ctrl+V` で文字を貼るので、画像が無いときは横取りしない（`false` を返して
    /// 本文欄に任せる）。キーを先に受け取るには `intercept_keystrokes` を使う（`capture_key_down`
    /// では、本文欄の `Paste` のキーバインドが先に解決されて**リスナーまで届かない**。
    /// `gpui-pre` の `dispatch_key_event` はバインドが処理された時点で戻る）。
    fn paste_attachments(&mut self, cx: &mut Context<Self>) -> bool {
        let item = cx.read_from_clipboard();
        // 内容そのものは出さない（種類だけ）。
        let kinds = item
            .as_ref()
            .map(|item| {
                item.entries
                    .iter()
                    .map(|entry| match entry {
                        ClipboardEntry::String(_) => "text",
                        ClipboardEntry::Image(_) => "image",
                        ClipboardEntry::ExternalPaths(_) => "files",
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            })
            .unwrap_or_else(|| "none".to_string());
        log::debug!("report: Ctrl+V のクリップボード = [{kinds}]");
        let has_media = item.is_some_and(|item| {
            item.entries.iter().any(|entry| {
                matches!(
                    entry,
                    ClipboardEntry::ExternalPaths(_) | ClipboardEntry::Image(_)
                )
            })
        });
        if !has_media {
            return false;
        }
        self.attach_from_clipboard(cx);
        true
    }

    /// ドロップ / クリップボードから来たパスを添付に足す。
    ///
    /// 読み込みと縮小は背景で行う（大きめの画像で画面を止めない）。重複と上限はここで弾き、
    /// 追加できなかった理由は `extra_errors`（呼び出し側で分かった失敗）と並べて出す。
    fn add_attachments(
        &mut self,
        paths: Vec<PathBuf>,
        extra_errors: Vec<String>,
        cx: &mut Context<Self>,
    ) {
        // 前の操作で出た理由は残さない（新しい操作の結果だけを見せる）。
        self.attach_error = None;
        let mut errors: Vec<String> = extra_errors;
        let mut targets: Vec<PathBuf> = Vec::new();
        for path in paths {
            if path.is_dir() {
                errors.push(format!("{} はフォルダです", path.display()));
                continue;
            }
            let known = self
                .attachments
                .iter()
                .any(|attachment| attachment.path == path)
                || targets.contains(&path);
            if known {
                continue;
            }
            if self.attachments.len() + targets.len() >= MAX_ATTACHMENTS {
                errors.push(format!("画像は {MAX_ATTACHMENTS} 件までです"));
                break;
            }
            targets.push(path);
        }
        if !errors.is_empty() {
            // 読み込み中の分の理由は後から足す（`attachments_loaded`）。ここでは今すぐ分かる分だけ。
            self.attach_error = Some(errors.join(" / "));
        }
        if targets.is_empty() {
            cx.notify();
            return;
        }
        self.attaching += targets.len();
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_executor()
                .spawn(async move {
                    targets
                        .into_iter()
                        .map(|path| load_attachment(&path))
                        .collect::<Vec<_>>()
                })
                .await;
            let _ = this.update(cx, |this, cx| this.attachments_loaded(loaded, cx));
        })
        .detach();
        cx.notify();
    }

    /// 背景での読み込みが終わった分を足す。
    fn attachments_loaded(
        &mut self,
        loaded: Vec<Result<Attachment, String>>,
        cx: &mut Context<Self>,
    ) {
        let mut errors: Vec<String> = Vec::new();
        for result in loaded {
            self.attaching = self.attaching.saturating_sub(1);
            match result {
                Ok(attachment) => {
                    let known = self
                        .attachments
                        .iter()
                        .any(|existing| existing.path == attachment.path);
                    if known || self.attachments.len() >= MAX_ATTACHMENTS {
                        continue;
                    }
                    self.attachments.push(attachment);
                }
                Err(reason) => errors.push(reason),
            }
        }
        if !errors.is_empty() {
            let existing = self.attach_error.take();
            self.attach_error = Some(match existing {
                Some(existing) => format!("{existing} / {}", errors.join(" / ")),
                None => errors.join(" / "),
            });
        }
        cx.notify();
    }

    /// 添付を外す（アプリが作った一時ファイルは消す）。
    fn remove_attachment(&mut self, path: &PathBuf, cx: &mut Context<Self>) {
        if let Some(index) = self
            .attachments
            .iter()
            .position(|attachment| attachment.path == *path)
        {
            let attachment = self.attachments.remove(index);
            if is_temporary(&attachment.path) {
                let _ = std::fs::remove_file(&attachment.path);
            }
        }
        self.attach_error = None;
        cx.notify();
    }

    /// 「クリップボードから追加」: クリップボードのファイル / 画像を添付に足す。
    ///
    /// 画像そのもの（スクリーンショットなど）は `CF_HDROP` に載せられないため、一時ファイルへ
    /// 書き出してから同じ道へ乗せる。`Ctrl+V` を本文欄で拾うのではなくボタンにしているのは、
    /// 本文欄の貼り付け（文字）と競合させないため。
    fn attach_from_clipboard(&mut self, cx: &mut Context<Self>) {
        let Some(item) = cx.read_from_clipboard() else {
            self.attach_error = Some("クリップボードが空です".to_string());
            cx.notify();
            return;
        };
        let mut paths: Vec<PathBuf> = Vec::new();
        let mut errors: Vec<String> = Vec::new();
        // 一時ファイルの置き場は 1 回だけ用意する（画像ごとに作り直すと、先に書いた実体が
        // 消える）。いま画面に出ている添付の実体は残す。
        let keep: Vec<PathBuf> = self
            .attachments
            .iter()
            .map(|attachment| attachment.path.clone())
            .collect();
        let dir = match prepare_attachment_dir(&keep) {
            Ok(dir) => Some(dir),
            Err(error) => {
                errors.push(error);
                None
            }
        };
        for entry in &item.entries {
            match entry {
                ClipboardEntry::ExternalPaths(external) => {
                    paths.extend(external.0.iter().cloned());
                }
                ClipboardEntry::Image(image) => {
                    let Some(dir) = dir.as_deref() else {
                        continue;
                    };
                    match write_clipboard_image(dir, image) {
                        Ok(path) => paths.push(path),
                        Err(error) => errors.push(error),
                    }
                }
                ClipboardEntry::String(_) => {}
            }
        }
        if paths.is_empty() {
            errors.push("クリップボードに画像がありません".to_string());
            self.add_attachments(Vec::new(), errors, cx);
            return;
        }
        self.add_attachments(paths, errors, cx);
    }

    /// 画像の添付（サムネイル + 追加 / 削除）。アプリからはアップロードしない。
    ///
    /// ここへドロップするだけでなく、レポート画面のどこへ落としても足せる（`render` の
    /// ルートに `on_drop` を付けてある）。
    fn attachments_section(
        &self,
        handle: &Entity<Self>,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        let border = cx.theme().border;
        let muted_fg = cx.theme().muted_foreground;
        let thumbnails: Vec<gpui_kit::AnyElement> = self
            .attachments
            .iter()
            .enumerate()
            .map(|(index, attachment)| {
                let path = attachment.path.clone();
                let selector = format!("report-attachment-{index}");
                div()
                    .debug_selector(move || selector.clone())
                    .relative()
                    .w(px(96.0))
                    .h(px(96.0))
                    .rounded_md()
                    .border_1()
                    .border_color(border)
                    .overflow_hidden()
                    .child(img(attachment.thumb.clone()).w_full().h_full())
                    .child(
                        div().absolute().top(px(2.0)).right(px(2.0)).child(
                            Button::new(("report-attachment-remove", index))
                                .outline()
                                .cursor_pointer()
                                .label("×")
                                .debug_selector({
                                    let selector = format!("report-attachment-remove-{index}");
                                    move || selector.clone()
                                })
                                .on_click({
                                    let handle = handle.clone();
                                    move |_, _window, cx| {
                                        let path = path.clone();
                                        handle.update(cx, |this, cx| {
                                            this.remove_attachment(&path, cx);
                                        });
                                    }
                                }),
                        ),
                    )
                    .into_any_element()
            })
            .collect();
        let (status, status_color) = if self.attaching > 0 {
            (
                Some(format!("画像を読み込んでいます…（{} 件）", self.attaching)),
                muted_fg,
            )
        } else {
            (self.attach_error.clone(), gpui_kit::red())
        };
        div()
            .debug_selector(|| "report-attach-zone".to_string())
            .flex()
            .flex_col()
            .gap_2()
            .p_3()
            .rounded_md()
            .border_1()
            .border_color(border)
            .bg(cx.theme().muted.opacity(0.2))
            .child(
                div()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(div().text_xs().text_color(muted_fg).child(format!(
                        "画像はここへドラッグ&ドロップ、または {PASTE_LABEL}（PNG / JPEG / WebP）"
                    )))
                    .child(
                        Button::new("report-attach-clipboard")
                            .outline()
                            .cursor_pointer()
                            .label("クリップボードから追加")
                            .debug_selector(|| "report-attach-clipboard".to_string())
                            .on_click({
                                let handle = handle.clone();
                                move |_, _window, cx| {
                                    handle.update(cx, |this, cx| {
                                        this.attach_from_clipboard(cx);
                                    });
                                }
                            }),
                    ),
            )
            .when(!thumbnails.is_empty(), |this| {
                this.child(
                    div()
                        .flex()
                        .flex_row()
                        .flex_wrap()
                        .gap_2()
                        .children(thumbnails),
                )
            })
            .child(
                div()
                    .debug_selector(|| "report-image-note".to_string())
                    .text_xs()
                    .text_color(muted_fg)
                    .child(IMAGE_NOTE),
            )
            .when_some(status, |this, message| {
                this.child(
                    div()
                        .debug_selector(|| "report-attach-status".to_string())
                        .text_xs()
                        .text_color(status_color)
                        .child(message),
                )
            })
            .into_any_element()
    }

    /// カード風の枠（アイコン + タイトル + 説明 + 中身）。設定画面と揃える。
    fn card(
        cx: &Context<Self>,
        title: &str,
        description: Option<&str>,
        icon: impl Into<gpui_kit::AnyElement>,
        content: impl IntoElement,
    ) -> gpui_kit::AnyElement {
        let border = cx.theme().border;
        let muted = cx.theme().muted;
        let muted_fg = cx.theme().muted_foreground;
        let card_bg = cx.theme().background;
        let title = title.to_string();
        let description = description.map(String::from);
        let icon: gpui_kit::AnyElement = icon.into();
        div()
            .rounded_xl()
            .border_1()
            .border_color(border)
            .bg(card_bg)
            .shadow_sm()
            .overflow_hidden()
            .child(
                div()
                    .px_5()
                    .py_4()
                    .border_b_1()
                    .border_color(border)
                    .bg(muted.opacity(0.3))
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .flex_row()
                            .items_center()
                            .gap_2()
                            .child(icon)
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child(title),
                            ),
                    )
                    .child(if let Some(description) = description {
                        div()
                            .text_xs()
                            .text_color(muted_fg)
                            .child(description)
                            .into_any_element()
                    } else {
                        div().into_any_element()
                    }),
            )
            .child(content)
            .into_any_element()
    }

    /// テンプレート 1 件分の選択行。
    fn template_row(
        handle: Entity<Self>,
        template: &IssueTemplate,
        selected: bool,
        cx: &Context<Self>,
    ) -> gpui_kit::AnyElement {
        let file_name = template.file_name.clone();
        let selector = format!("report-template-{file_name}");
        let name = template.name.clone();
        let description = template.description.clone();
        let muted_fg = cx.theme().muted_foreground;
        div()
            .id(SharedString::from(selector.clone()))
            .debug_selector({
                let selector = selector.clone();
                move || selector.clone()
            })
            .flex()
            .flex_col()
            .gap_0p5()
            .px_3()
            .py_2()
            .rounded_md()
            .cursor_pointer()
            .when(selected, |this| this.bg(cx.theme().primary.opacity(0.12)))
            .hover(|style| style.bg(cx.theme().secondary))
            .on_click({
                let handle = handle.clone();
                move |_, window, cx| {
                    handle.update(cx, |this, cx| {
                        this.select_template(&file_name, window, cx);
                    });
                }
            })
            .child(div().text_sm().font_weight(FontWeight::MEDIUM).child(name))
            .child(if let Some(description) = description {
                div()
                    .text_xs()
                    .text_color(muted_fg)
                    .child(description)
                    .into_any_element()
            } else {
                div().into_any_element()
            })
            .into_any_element()
    }
}

impl Render for ReportView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_inputs(window, cx);
        self.ensure_templates(cx);

        let error = self.error.clone();
        let templates_error = self.templates_error.clone();
        let templates_loading = self.templates_loading;
        let selected = self.selected_template.clone();
        let muted_fg = cx.theme().muted_foreground;
        let handle = cx.entity();
        let title_input = self
            .title_input
            .clone()
            .expect("タイトルの入力は ensure_inputs で作られる");
        let body_input = self
            .body_input
            .clone()
            .expect("本文の入力は ensure_inputs で作られる");
        let template_rows: Vec<gpui_kit::AnyElement> = self
            .templates
            .iter()
            .map(|template| {
                Self::template_row(
                    handle.clone(),
                    template,
                    selected.as_deref() == Some(template.file_name.as_str()),
                    cx,
                )
            })
            .collect();

        div()
            .id("report-screen")
            .key_context(REPORT_KEY_CONTEXT)
            .size_full()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
            // 画像は本文欄の上に限らずレポート画面のどこへ落としても足す
            // （`FileDropEvent` はホバー中の要素へ届く）。
            .on_drop({
                let handle = handle.clone();
                move |paths: &ExternalPaths, _window, cx| {
                    let files: Vec<PathBuf> = paths.0.iter().cloned().collect();
                    handle.update(cx, |this, cx| {
                        this.add_attachments(files, Vec::new(), cx);
                    });
                }
            })
            .child(
                div()
                    .mx_auto()
                    .w(px(720.0))
                    .py_8()
                    .flex()
                    .flex_col()
                    .gap_6()
                    // 見出し
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .text_xl()
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("レポート"),
                            )
                            .child(div().text_sm().text_color(muted_fg).child(
                                "不具合や要望を GitHub の Issue として報告します。\
                                 「Issue を作成」で作成画面を既定のブラウザーで開きます。",
                            )),
                    )
                    // 投稿先
                    .child(Self::card(
                        cx,
                        "投稿先",
                        Some("レポートはこのアプリの Issue に送られます"),
                        Icon::new(IconName::Github)
                            .size(px(16.0))
                            .text_color(muted_fg),
                        div()
                            .p_5()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(
                                div()
                                    .debug_selector(|| "report-target-repo".to_string())
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child(TARGET_REPO),
                            )
                            .child(div().text_xs().text_color(muted_fg).child(
                                "このリポジトリの Issue に投稿します（投稿先は変更できません）。\
                                 投稿は開いた GitHub の画面で行います\
                                 （アプリは GitHub のアカウント情報を扱いません）。",
                            )),
                    ))
                    // テンプレート
                    .child(Self::card(
                        cx,
                        "テンプレート",
                        Some("選ぶとタイトルと本文の下書きが入ります"),
                        Icon::new(IconName::File)
                            .size(px(16.0))
                            .text_color(muted_fg),
                        div()
                            .p_5()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .when(template_rows.is_empty(), |this| {
                                this.child(div().text_sm().text_color(muted_fg).child(
                                    if templates_loading {
                                        "テンプレートを取得しています…"
                                    } else {
                                        "テンプレートがありません（本文へ直接書けます）"
                                    },
                                ))
                            })
                            .children(template_rows)
                            .child(if let Some(message) = templates_error {
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_2()
                                    .child(div().text_xs().text_color(muted_fg).child(message))
                                    .child(
                                        Button::new("report-templates-retry")
                                            .outline()
                                            .cursor_pointer()
                                            .label("再取得")
                                            .on_click({
                                                let handle = handle.clone();
                                                move |_, _window, cx| {
                                                    handle.update(cx, |this, cx| {
                                                        // 1 度失敗すると取得済みフラグが
                                                        // 立ったままになるので、ここで戻す。
                                                        this.templates_fetched = false;
                                                        this.templates_error = None;
                                                        this.ensure_templates(cx);
                                                        cx.notify();
                                                    });
                                                }
                                            }),
                                    )
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            }),
                    ))
                    // 内容
                    .child(Self::card(
                        cx,
                        "内容",
                        Some("タイトルと本文を確認して、GitHub の作成画面を開きます"),
                        Icon::new(IconName::FileText)
                            .size(px(16.0))
                            .text_color(muted_fg),
                        div()
                            .p_5()
                            .flex()
                            .flex_col()
                            .gap_3()
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("タイトル"),
                            )
                            .child(
                                div()
                                    .debug_selector(|| "report-title".to_string())
                                    .w_full()
                                    .child(Input::new(&title_input).w_full()),
                            )
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(FontWeight::MEDIUM)
                                    .child("本文"),
                            )
                            .child(
                                div()
                                    .debug_selector(|| "report-body".to_string())
                                    .w_full()
                                    .child(Textarea::new(&body_input).h(px(BODY_HEIGHT)).w_full()),
                            )
                            // 画像の添付（アプリからはアップロードしない）
                            .child(self.attachments_section(&handle, cx))
                            // 送信の失敗（赤字）
                            .child(if let Some(message) = error {
                                div()
                                    .text_sm()
                                    .text_color(gpui_kit::red())
                                    .child(message)
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            })
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .justify_between()
                                    .gap_3()
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(muted_fg)
                                            .child(format!(
                                                "{TARGET_REPO} の Issue 作成画面を開きます"
                                            )),
                                    )
                                    .child(
                                        Button::new("report-submit")
                                            .primary()
                                            .cursor_pointer()
                                            .label("Issue を作成")
                                            .debug_selector(|| "report-submit".to_string())
                                            .on_click({
                                                let handle = handle.clone();
                                                move |_, _window, cx| {
                                                    handle.update(cx, |this, cx| {
                                                        this.submit(cx);
                                                    });
                                                }
                                            }),
                                    ),
                            ),
                    )),
            )
    }
}

#[cfg(test)]
mod tests {
    use gpui_kit::AppContext as _;
    use gpui_kit::Focusable as _;
    use gpui_kit::ReadGlobal as _;
    use gpui_kit::TestAppContext;

    use super::*;

    /// 一時フォルダ（`%TEMP%\thundoku-shelf\attachments`）はテスト間で共有されるので、
    /// 触るテストは直列化する（並列に走ると他テストのファイルを消してしまう）。
    /// **`attach_from_clipboard` を呼ぶテストは、ファイルを作らなくても必ず取る**
    /// （取り込み時に使っていないファイルを捨てるため）。
    static ATTACHMENT_DIR_LOCK: parking_lot::Mutex<()> = parking_lot::Mutex::new(());

    /// テスト用の PNG を一時フォルダへ書く（4x4 のベタ塗り）。
    ///
    /// `seed` ごとに色を変える: 同じバイト列の画像は同じハッシュ＝同じ一時ファイル名になる
    /// （実装の重複判定に当たるため、テストでは別の画像として作る）。
    fn write_test_png(name: &str, seed: u8) -> PathBuf {
        let dir = std::env::temp_dir().join("thundoku-report-test");
        std::fs::create_dir_all(&dir).expect("一時フォルダを作れる");
        let path = dir.join(name);
        let image = image::RgbaImage::from_pixel(4, 4, image::Rgba([seed, 57, 200, 255]));
        image.save(&path).expect("PNG を書ける");
        path
    }

    /// 送信で外へ出た処理を記録する（実物のブラウザー / クリップボードは触らない）。
    #[derive(Default)]
    struct Recorder {
        /// 開いた URL。
        opened: parking_lot::Mutex<Vec<String>>,
        /// クリップボードへ渡したファイルと本文。
        written: parking_lot::Mutex<Vec<(Vec<PathBuf>, Option<String>)>>,
        /// クリップボードへ渡した文字列（画像なしの本文）。
        texts: parking_lot::Mutex<Vec<String>>,
    }

    impl Recorder {
        fn io(self: &Arc<Self>) -> ReportIo {
            let opened = self.clone();
            let written = self.clone();
            let texts = self.clone();
            ReportIo {
                open_browser: Arc::new(move |url: &str| {
                    opened.opened.lock().push(url.to_string());
                    Ok(())
                }),
                write_files: Arc::new(move |paths: &[PathBuf], text: Option<&str>| {
                    written
                        .written
                        .lock()
                        .push((paths.to_vec(), text.map(str::to_string)));
                    Ok(())
                }),
                write_text: Arc::new(move |text: &str| {
                    texts.texts.lock().push(text.to_string());
                    Ok(())
                }),
            }
        }
    }

    /// レポート画面をウィンドウに載せる（入力欄は render で作られる）。
    fn open_report(
        cx: &mut TestAppContext,
        io: ReportIo,
    ) -> (Entity<ReportView>, &'static mut gpui_kit::VisualTestContext) {
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        let view = cx.new(|cx| ReportView::with_io(cx, io));
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1280.0),
                height: gpui_kit::px(1100.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        draw_frames(visual, 4);
        (view, visual)
    }

    /// 何フレームか描く（選択子と当たり判定を作る）。
    fn draw_frames(visual: &mut gpui_kit::VisualTestContext, frames: usize) {
        for _ in 0..frames {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }
    }

    /// タイトルを入力する（`InputState` は render で作られるのでウィンドウ越しに入れる）。
    fn set_title(visual: &mut gpui_kit::VisualTestContext, view: &Entity<ReportView>, title: &str) {
        let title = title.to_string();
        visual.update(|window, cx| {
            view.update(cx, |this, cx| {
                let input = this.title_input.clone().expect("タイトルの入力が無い");
                input.update(cx, |state, cx| state.set_value(title.clone(), window, cx));
            });
        });
    }

    /// 本文を入力する（`TextareaState` は render で作られるのでウィンドウ越しに入れる）。
    fn set_body(visual: &mut gpui_kit::VisualTestContext, view: &Entity<ReportView>, body: &str) {
        let body = body.to_string();
        visual.update(|window, cx| {
            view.update(cx, |this, cx| {
                let input = this.body_input.clone().expect("本文の入力が無い");
                input.update(cx, |state, cx| state.set_value(body.clone(), window, cx));
            });
        });
    }

    /// OS からのファイルドロップを再現する（`FileDropEvent` は位置の要素へ届く）。
    fn drop_paths(visual: &mut gpui_kit::VisualTestContext, paths: Vec<PathBuf>) {
        let position = gpui_kit::point(gpui_kit::px(200.0), gpui_kit::px(200.0));
        visual.update(|window, cx| {
            window.dispatch_event(
                gpui_kit::PlatformInput::FileDrop(gpui_kit::FileDropEvent::Entered {
                    position,
                    paths: gpui_kit::ExternalPaths(paths.into()),
                }),
                cx,
            );
            window.dispatch_event(
                gpui_kit::PlatformInput::FileDrop(gpui_kit::FileDropEvent::Submit { position }),
                cx,
            );
        });
    }

    /// トースト（送信の結果）を読む。
    fn toast(cx: &mut TestAppContext) -> (crate::app_state::ToastKind, Option<String>) {
        cx.update(|cx| {
            let state = crate::app_state::AppState::global(cx);
            (*state.toast_kind.lock(), state.toast_message.lock().clone())
        })
    }

    fn field(
        kind: TemplateFieldKind,
        label: Option<&str>,
        placeholder: Option<&str>,
        value: Option<&str>,
    ) -> TemplateField {
        TemplateField {
            kind,
            label: label.map(str::to_string),
            description: None,
            placeholder: placeholder.map(str::to_string),
            value: value.map(str::to_string),
            required: false,
        }
    }

    /// 見出し（`## label` + placeholder）と説明文（markdown）が節として並ぶこと。
    #[test]
    fn compose_body_expands_markdown_and_fields() {
        let fields = [
            field(
                TemplateFieldKind::Markdown,
                None,
                None,
                Some("報告ありがとうございます。\n"),
            ),
            field(
                TemplateFieldKind::Textarea,
                Some("概要"),
                Some("何が起きましたか？"),
                None,
            ),
            field(
                TemplateFieldKind::Input,
                Some("アプリのバージョン"),
                None,
                Some("0.0.2"),
            ),
        ];
        assert_eq!(
            compose_body(&fields),
            "報告ありがとうございます。\n\n\
             ## 概要\n何が起きましたか？\n\n\
             ## アプリのバージョン\n0.0.2"
        );
    }

    /// issue form の `description`（秘密情報の確認など）も下書きに含めること。
    /// アプリは field 単位の入力欄を作らないので、本文に載せないと注意書きが届かない。
    #[test]
    fn compose_body_includes_the_field_description() {
        let mut logs = field(
            TemplateFieldKind::Textarea,
            Some("ログ"),
            None,
            Some("<details>...</details>"),
        );
        logs.description =
            Some("秘密情報が含まれていないか確認してから貼ってください。".to_string());

        assert_eq!(
            compose_body(&[logs]),
            "## ログ\n\
             > 秘密情報が含まれていないか確認してから貼ってください。\n\
             <details>...</details>"
        );
    }

    /// 本文が空でも、見出しと説明は出すこと（利用者がそこへ書く）。
    #[test]
    fn compose_body_keeps_the_description_without_a_placeholder() {
        let mut version = field(TemplateFieldKind::Input, Some("バージョン"), None, None);
        version.description = Some("設定画面の下部で確認できます。".to_string());

        assert_eq!(
            compose_body(&[version]),
            "## バージョン\n> 設定画面の下部で確認できます。"
        );
    }

    /// placeholder が無い項目は value を使い、末尾に余分な空行を残さないこと
    /// （YAML のブロック値は末尾に改行を持ち込む）。
    #[test]
    fn compose_body_falls_back_to_the_value() {
        let fields = [
            field(
                TemplateFieldKind::Textarea,
                Some("再現手順"),
                None,
                Some("1. 本棚を開く\n2. スクロールする\n"),
            ),
            field(TemplateFieldKind::Textarea, Some("補足"), None, None),
        ];
        let body = compose_body(&fields);
        assert_eq!(
            body,
            "## 再現手順\n1. 本棚を開く\n2. スクロールする\n\n## 補足"
        );
        assert!(!body.ends_with('\n'), "末尾に空行が残っている: {body:?}");
    }

    /// label も value も placeholder も無い項目は節を作らないこと。
    #[test]
    fn compose_body_skips_empty_fields() {
        let fields = [
            field(TemplateFieldKind::Input, None, None, None),
            field(TemplateFieldKind::Textarea, Some("   "), None, Some("")),
            field(TemplateFieldKind::Markdown, None, None, Some("  \n")),
            field(
                TemplateFieldKind::Dropdown,
                Some("OS"),
                Some("Windows"),
                None,
            ),
        ];
        assert_eq!(compose_body(&fields), "## OS\nWindows");
    }

    /// 種別ごとに違う日本語になること（原因の取り違えを防ぐ）。
    #[test]
    fn error_messages_are_distinct_and_japanese() {
        let errors = [
            GithubError::Forbidden("Resource not accessible".to_string()),
            GithubError::Network("connection reset".to_string()),
            GithubError::RateLimited {
                retry_after: Some(60),
            },
            GithubError::RateLimited { retry_after: None },
            GithubError::InvalidResponse("status 500".to_string()),
        ];
        let messages: Vec<String> = errors.iter().map(error_message).collect();
        for message in &messages {
            assert!(!message.is_empty(), "空の文言がある");
            assert!(!message.is_ascii(), "日本語になっていない: {message}");
        }
        for (index, message) in messages.iter().enumerate() {
            assert!(
                !messages[..index].contains(message),
                "文言が重複している: {message}"
            );
        }
    }

    /// テンプレートは公開リポジトリから匿名で取るので**ログイン導線が無い**こと、
    /// 画像はアプリからアップロードせずクリップボード経由で渡すことを画面に明記すること。
    ///
    /// 旧バージョンが残した設定があっても、画面は固定の投稿先を出す。
    #[gpui_kit::test]
    async fn report_screen_shows_the_fixed_target_without_login(cx: &mut TestAppContext) {
        let (_, visual) = open_report(cx, ReportIo::default());
        cx.update(|cx| {
            let db = &crate::app_state::AppState::global(cx).db_pool;
            let _ = thundoku_core::db::settings::set(db, "report.target_repo", "someone/elsewhere");
        });
        draw_frames(visual, 2);

        assert!(
            visual.debug_bounds("report-target-repo").is_some(),
            "投稿先の表示が出ていない"
        );
        assert!(
            visual.debug_bounds("report-submit").is_some(),
            "「Issue を作成」が出ていない"
        );
        assert!(
            visual.debug_bounds("report-attach-zone").is_some(),
            "画像の添付（ドロップ）が出ていない"
        );
        assert!(
            visual.debug_bounds("report-attach-clipboard").is_some(),
            "「クリップボードから追加」が出ていない"
        );
        assert!(
            visual.debug_bounds("report-image-note").is_some(),
            "画像の受け渡しの説明が出ていない"
        );
        // ログインは要らない（トークンを持たない）。
        assert!(
            visual.debug_bounds("report-login-required").is_none(),
            "ログインの案内が残っている"
        );
    }

    /// タイトルが空なら作成画面を開かず、画面に理由を出すこと。
    #[gpui_kit::test]
    async fn submitting_without_a_title_reports_it(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        let view = cx.new(ReportView::new);
        let window = cx.open_window(
            gpui_kit::Size {
                width: gpui_kit::px(1280.0),
                height: gpui_kit::px(1100.0),
            },
            |window, cx| gpui_kit::component::Root::new(view.clone(), window, cx),
        );
        let visual = gpui_kit::VisualTestContext::from_window(*window, cx).into_mut();
        for _ in 0..4 {
            visual.update(|window, cx| {
                let arena_clear = window.draw(cx);
                arena_clear.clear(cx);
            });
        }

        // タイトルは空のまま（未入力）で送信 → ブラウザーを開かずに理由を出す。
        view.update(cx, |this, cx| this.submit(cx));
        let error = view.read_with(cx, |this, _| this.error.clone());
        assert_eq!(error.as_deref(), Some("タイトルを入力してください"));
    }

    /// 送信はブラウザーを update の中で呼ばず、タスク（実機では背景スレッド）で開くこと。
    ///
    /// Windows の `ShellExecuteW` はシェルがメッセージループを回す。App 借用中（update の中）に
    /// 呼ぶと gpui の window proc が再入して `RefCell already borrowed`（最悪 panic = アプリ終了）
    /// になる（実測 2026-10-04: クリックごとに 3 件のエラー）。実物のブラウザーは開かない。
    #[gpui_kit::test]
    async fn submit_opens_the_browser_outside_the_update(cx: &mut TestAppContext) {
        let recorder = Arc::new(Recorder::default());
        let (view, visual) = open_report(cx, recorder.io());
        set_title(visual, &view, "一覧のスクロールが引っかかる");

        view.update(cx, |this, cx| this.submit(cx));
        assert!(
            recorder.opened.lock().is_empty(),
            "update の中でブラウザーを開いている（ShellExecuteW が gpui を再入させる）"
        );

        visual.run_until_parked();
        let opened = recorder.opened.lock().clone();
        assert_eq!(opened.len(), 1, "ブラウザーを 1 回開いていない");
        assert!(
            opened[0].contains("/issues/new?title="),
            "URL が違う: {}",
            opened[0]
        );
        let (kind, message) = toast(cx);
        assert_eq!(kind, crate::app_state::ToastKind::Info);
        assert!(message.unwrap_or_default().contains("開きました"));
    }

    /// 画像ファイルを落とすとサムネイルが付くこと（落としても何も起きない、を直す）。
    #[gpui_kit::test]
    async fn dropping_an_image_adds_a_thumbnail(cx: &mut TestAppContext) {
        let path = write_test_png("dropped.png", 1);
        let (view, visual) = open_report(cx, ReportIo::default());
        drop_paths(visual, vec![path]);
        visual.run_until_parked();
        draw_frames(visual, 2);

        let (attachments, error) = view.read_with(cx, |this, _| {
            (this.attachments.len(), this.attach_error.clone())
        });
        assert_eq!(attachments, 1, "添付されていない");
        assert!(error.is_none(), "理由が出ている: {error:?}");
        assert!(
            visual.debug_bounds("report-attachment-0").is_some(),
            "サムネイルが出ていない"
        );
    }

    /// 画像でないファイルは理由を出して足さないこと（黙って無視しない）。
    #[gpui_kit::test]
    async fn dropping_a_non_image_file_reports_it(cx: &mut TestAppContext) {
        let dir = std::env::temp_dir().join("thundoku-report-test");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("notes.txt");
        std::fs::write(&path, b"not an image").unwrap();
        let (view, visual) = open_report(cx, ReportIo::default());
        drop_paths(visual, vec![path]);
        visual.run_until_parked();

        let (attachments, error) = view.read_with(cx, |this, _| {
            (this.attachments.len(), this.attach_error.clone())
        });
        assert_eq!(attachments, 0, "画像でないものを添付している");
        assert!(
            error.unwrap_or_default().contains("notes.txt"),
            "どのファイルか伝えていない"
        );

        // 次の操作（正しい画像）では前の理由が残らないこと。
        drop_paths(visual, vec![write_test_png("after-error.png", 21)]);
        visual.run_until_parked();
        let (attachments, error) = view.read_with(cx, |this, _| {
            (this.attachments.len(), this.attach_error.clone())
        });
        assert_eq!(attachments, 1, "正しい画像を取り込めていない");
        assert!(error.is_none(), "前の理由が残っている: {error:?}");
    }

    /// 添付は上限で止めること（画像を落とし続けても増え続けない）。
    #[gpui_kit::test]
    async fn attachments_stop_at_the_limit(cx: &mut TestAppContext) {
        let paths: Vec<PathBuf> = (0..MAX_ATTACHMENTS + 2)
            .map(|index| write_test_png(&format!("limit-{index}.png"), index as u8 + 2))
            .collect();
        let (view, visual) = open_report(cx, ReportIo::default());
        view.update(cx, |this, cx| this.add_attachments(paths, Vec::new(), cx));
        visual.run_until_parked();

        let (attachments, error) = view.read_with(cx, |this, _| {
            (this.attachments.len(), this.attach_error.clone())
        });
        assert_eq!(attachments, MAX_ATTACHMENTS);
        assert!(
            error.unwrap_or_default().contains("件まで"),
            "上限を伝えていない"
        );
    }

    /// 画像を渡すときに本文の先頭へ案内（HTML コメント）を足すこと。
    ///
    /// GitHub のエディタでは読めて、レンダリング後は見えない形にする（Issue を汚さない）。
    #[test]
    fn attachment_notice_is_prepended_as_an_html_comment() {
        assert_eq!(
            attachment_body("", 1),
            "<!-- 画像 1 件をクリップボードへコピーしました。貼り付けたい位置で Ctrl+V してください（アプリからはアップロードしません） -->"
        );
        let with_body = attachment_body("## 概要\n本文", 2);
        assert!(with_body.starts_with("<!-- 画像 2 件"), "{with_body}");
        assert!(with_body.ends_with("## 概要\n本文"), "{with_body}");
        assert!(
            with_body.contains("Ctrl+V"),
            "貼り方を書いていない: {with_body}"
        );
    }

    /// 添付があるときは、開く URL の本文に案内が入ること（短い本文＝URL に載る場合）。
    #[gpui_kit::test]
    async fn submitting_with_attachments_puts_the_notice_in_the_url(cx: &mut TestAppContext) {
        let recorder = Arc::new(Recorder::default());
        let (view, visual) = open_report(cx, recorder.io());
        view.update(cx, |this, cx| {
            this.add_attachments(vec![write_test_png("notice.png", 41)], Vec::new(), cx);
        });
        visual.run_until_parked();
        set_title(visual, &view, "案内が入るか");
        set_body(visual, &view, "## 概要\n本文");
        view.update(cx, |this, cx| this.submit(cx));
        visual.run_until_parked();

        let opened = recorder.opened.lock().clone();
        assert_eq!(opened.len(), 1);
        assert!(
            opened[0].contains("Ctrl%2BV"),
            "本文の案内が URL に無い: {}",
            opened[0]
        );
    }

    /// 添付があるときは、送信で画像をクリップボードへ「ファイル」として渡すこと
    /// （本文は URL に載っているので一緒には渡さない）。
    #[gpui_kit::test]
    async fn submit_hands_attachments_to_the_clipboard(cx: &mut TestAppContext) {
        let path = write_test_png("handoff.png", 9);
        let recorder = Arc::new(Recorder::default());
        let (view, visual) = open_report(cx, recorder.io());
        view.update(cx, |this, cx| {
            this.add_attachments(vec![path.clone()], Vec::new(), cx);
        });
        visual.run_until_parked();
        assert_eq!(view.read_with(cx, |this, _| this.attachments.len()), 1);
        set_title(visual, &view, "画像が添付できない");

        view.update(cx, |this, cx| this.submit(cx));
        visual.run_until_parked();

        let written = recorder.written.lock().clone();
        assert_eq!(written.len(), 1, "クリップボードへファイルを渡していない");
        assert_eq!(written[0].0, vec![path]);
        assert_eq!(
            written[0].1, None,
            "本文は URL に載っているので一緒に置かない"
        );
        let (_, message) = toast(cx);
        assert!(
            message.unwrap_or_default().contains("Ctrl+V"),
            "貼り方の案内が出ていない"
        );
    }

    /// 添付 + 長い本文（本文はクリップボード側）でも、案内が本文の先頭に載ること。
    #[gpui_kit::test]
    async fn long_body_with_attachment_gets_the_notice_too(cx: &mut TestAppContext) {
        let recorder = Arc::new(Recorder::default());
        let (view, visual) = open_report(cx, recorder.io());
        view.update(cx, |this, cx| {
            this.add_attachments(vec![write_test_png("notice-long.png", 42)], Vec::new(), cx);
        });
        visual.run_until_parked();
        set_title(visual, &view, "長い本文と画像");
        set_body(visual, &view, &format!("BODY-START {}", "A".repeat(9_000)));
        view.update(cx, |this, cx| this.submit(cx));
        visual.run_until_parked();

        let written = recorder.written.lock().clone();
        assert_eq!(written.len(), 1, "クリップボードへ渡していない");
        let text = written[0].1.clone().expect("本文も一緒に渡していない");
        assert!(
            text.starts_with("<!-- 画像 1 件"),
            "案内が先頭に無い: {text}"
        );
        assert!(text.contains("BODY-START"), "本文が消えている: {text}");
    }

    /// 「クリップボードから追加」でクリップボードの画像ファイルを取り込むこと。
    #[gpui_kit::test]
    async fn clipboard_files_are_added_as_attachments(cx: &mut TestAppContext) {
        let _guard = ATTACHMENT_DIR_LOCK.lock();
        let path = write_test_png("clipboard.png", 10);
        let (view, visual) = open_report(cx, ReportIo::default());
        cx.write_to_clipboard(gpui_kit::ClipboardItem {
            entries: vec![gpui_kit::ClipboardEntry::ExternalPaths(
                gpui_kit::ExternalPaths(vec![path].into()),
            )],
        });
        view.update(cx, |this, cx| this.attach_from_clipboard(cx));
        visual.run_until_parked();

        let (attachments, error) = view.read_with(cx, |this, _| {
            (this.attachments.len(), this.attach_error.clone())
        });
        assert_eq!(
            attachments, 1,
            "クリップボードのファイルを取り込めていない: {error:?}"
        );
    }

    /// クリップボードの画像（ファイルでない）は一時ファイルにして取り込むこと。
    ///
    /// `CF_HDROP` に載せるには実ファイルが要る（アプリは画像の中身を GitHub へ送らない）。
    /// 外したら一時ファイルも消えること。
    #[gpui_kit::test]
    async fn clipboard_images_become_temporary_files(cx: &mut TestAppContext) {
        let _guard = ATTACHMENT_DIR_LOCK.lock();
        let png = std::fs::read(write_test_png("clipboard-image.png", 11)).unwrap();
        let (view, visual) = open_report(cx, ReportIo::default());
        cx.write_to_clipboard(gpui_kit::ClipboardItem {
            entries: vec![gpui_kit::ClipboardEntry::Image(
                gpui_kit::Image::from_bytes(gpui_kit::ImageFormat::Png, png),
            )],
        });
        view.update(cx, |this, cx| this.attach_from_clipboard(cx));
        visual.run_until_parked();

        let path = view
            .read_with(cx, |this, _| {
                this.attachments.first().map(|a| a.path.clone())
            })
            .expect("取り込めていない");
        assert!(
            path.starts_with(clipboard_attachment_dir()),
            "一時ファイルになっていない: {}",
            path.display()
        );
        assert!(path.exists(), "一時ファイルが無い: {}", path.display());
        view.update(cx, |this, cx| this.remove_attachment(&path, cx));
        assert!(!path.exists(), "外しても一時ファイルが残っている");
    }

    /// 本文が URL に載らないときは、本文だけをクリップボードへ渡すこと。
    ///
    /// これも update の外で行う（`OpenClipboard` が他プロセスと交渉するため）。
    #[gpui_kit::test]
    async fn long_body_is_copied_outside_the_update(cx: &mut TestAppContext) {
        let recorder = Arc::new(Recorder::default());
        let (view, visual) = open_report(cx, recorder.io());
        set_title(visual, &view, "長い本文でも送れる");
        set_body(visual, &view, &"あ".repeat(9_000));

        view.update(cx, |this, cx| this.submit(cx));
        assert!(
            recorder.texts.lock().is_empty(),
            "update の中でクリップボードを触っている（OpenClipboard が gpui を再入させる）"
        );

        visual.run_until_parked();
        let texts = recorder.texts.lock().clone();
        assert_eq!(texts.len(), 1, "本文をクリップボードへ渡していない");
        assert_eq!(texts[0].chars().count(), 9_000);
        assert!(
            recorder.written.lock().is_empty(),
            "画像が無いのにファイルとして渡している"
        );
        let (_, message) = toast(cx);
        assert!(message.unwrap_or_default().contains("本文をコピー"));
    }

    /// クリップボードから 2 つ目の画像を取り込んでも、先に付けた添付の実体を消さないこと
    /// （一時フォルダを丸ごと作り直すと、まだ送っていない画像の実体が消える）。
    #[gpui_kit::test]
    async fn adding_another_clipboard_image_keeps_the_existing_files(cx: &mut TestAppContext) {
        let _guard = ATTACHMENT_DIR_LOCK.lock();
        let png_a = std::fs::read(write_test_png("keep-a.png", 12)).unwrap();
        let png_b = std::fs::read(write_test_png("keep-b.png", 13)).unwrap();
        let (view, visual) = open_report(cx, ReportIo::default());

        cx.write_to_clipboard(gpui_kit::ClipboardItem {
            entries: vec![gpui_kit::ClipboardEntry::Image(
                gpui_kit::Image::from_bytes(gpui_kit::ImageFormat::Png, png_a),
            )],
        });
        view.update(cx, |this, cx| this.attach_from_clipboard(cx));
        visual.run_until_parked();
        let first = view
            .read_with(cx, |this, _| {
                this.attachments.first().map(|a| a.path.clone())
            })
            .expect("1 件目が取り込めていない");

        cx.write_to_clipboard(gpui_kit::ClipboardItem {
            entries: vec![gpui_kit::ClipboardEntry::Image(
                gpui_kit::Image::from_bytes(gpui_kit::ImageFormat::Png, png_b),
            )],
        });
        view.update(cx, |this, cx| this.attach_from_clipboard(cx));
        visual.run_until_parked();

        let paths = view.read_with(cx, |this, _| {
            this.attachments
                .iter()
                .map(|a| a.path.clone())
                .collect::<Vec<_>>()
        });
        assert_eq!(paths.len(), 2, "2 件目が取り込めていない: {paths:?}");
        assert!(
            paths[0].exists(),
            "先に付けた実体が消えている: {}",
            paths[0].display()
        );
        assert_eq!(paths[0], first);
    }

    /// 貼り付けのキー（macOS は `cmd-v`、それ以外は `ctrl-v`。本文欄のバインドと同じ切り替え）。
    const PASTE_KEYSTROKE: &str = if cfg!(target_os = "macos") {
        "cmd-v"
    } else {
        "ctrl-v"
    };

    /// 本文欄にフォーカスする（`Ctrl+V` の貼り付け先）。
    fn focus_body(visual: &mut gpui_kit::VisualTestContext, view: &Entity<ReportView>) {
        visual.update(|window, cx| {
            view.update(cx, |this, cx| {
                let input = this.body_input.clone().expect("本文の入力が無い");
                window.focus(&input.read(cx).focus_handle(cx), cx);
            });
        });
    }

    /// 本文欄の `Ctrl+V` でクリップボードの画像を添付できること（issue #3 の再現手順）。
    #[gpui_kit::test]
    async fn ctrl_v_attaches_images_from_the_clipboard(cx: &mut TestAppContext) {
        let _guard = ATTACHMENT_DIR_LOCK.lock();
        let png = std::fs::read(write_test_png("paste-image.png", 31)).unwrap();
        let (view, visual) = open_report(cx, ReportIo::default());
        cx.write_to_clipboard(gpui_kit::ClipboardItem {
            entries: vec![gpui_kit::ClipboardEntry::Image(
                gpui_kit::Image::from_bytes(gpui_kit::ImageFormat::Png, png),
            )],
        });
        focus_body(visual, &view);
        visual.simulate_keystrokes(PASTE_KEYSTROKE);
        visual.run_until_parked();

        let (attachments, error) = view.read_with(cx, |this, _| {
            (this.attachments.len(), this.attach_error.clone())
        });
        assert_eq!(attachments, 1, "Ctrl+V で添付できていない: {error:?}");
        assert!(error.is_none(), "理由が出ている: {error:?}");
    }

    /// クリップボードが文字だけのときは横取りせず、本文欄に貼れること。
    #[gpui_kit::test]
    async fn ctrl_v_with_text_only_pastes_into_the_body(cx: &mut TestAppContext) {
        let (view, visual) = open_report(cx, ReportIo::default());
        cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string(
            "ただのテキスト".to_string(),
        ));
        focus_body(visual, &view);
        visual.simulate_keystrokes(PASTE_KEYSTROKE);
        visual.run_until_parked();

        let (attachments, body) = view.read_with(cx, |this, cx| {
            (
                this.attachments.len(),
                this.body_input
                    .as_ref()
                    .map(|input| input.read(cx).value().to_string())
                    .unwrap_or_default(),
            )
        });
        assert_eq!(attachments, 0, "文字の貼り付けなのに添付している");
        assert!(
            body.contains("ただのテキスト"),
            "本文欄に貼れていない: {body:?}"
        );
    }

    /// 送信の文言は「何がクリップボードへ渡ったか」と失敗の原因を言い分けること。
    #[test]
    fn submit_messages_say_what_was_copied() {
        assert!(success_message(0, false).contains("開きました"));
        assert!(success_message(0, true).contains("本文をコピー"));
        assert!(success_message(2, false).contains("画像 2 件"));
        // 画像と本文を同時に置くと GitHub は画像を優先するので、本文はテキスト貼り付けで出す。
        let both = success_message(2, true);
        assert!(
            both.contains("画像 2 件と本文"),
            "本文のコピーを伝えていない: {both}"
        );
        assert!(
            both.contains("Ctrl+Shift+V"),
            "本文の貼り方を伝えていない: {both}"
        );
        let copied = failure_message(
            1,
            false,
            SubmitFailure::Clipboard("clipboard is locked".to_string()),
        );
        assert!(
            copied.contains("clipboard is locked"),
            "原因が出ていない: {copied}"
        );
        assert!(
            copied.contains("画像をクリップボードへコピーできませんでした"),
            "何が渡らなかったか言っていない: {copied}"
        );
        let both_failed = failure_message(
            1,
            true,
            SubmitFailure::Clipboard("clipboard is locked".to_string()),
        );
        assert!(
            both_failed.contains("画像と本文"),
            "本文も渡らなかったことを言っていない: {both_failed}"
        );
        let open = failure_message(1, false, SubmitFailure::OpenBrowser("denied".to_string()));
        assert!(
            open.contains("画像はコピー済みです"),
            "コピー済みを伝えていない: {open}"
        );
    }
}
