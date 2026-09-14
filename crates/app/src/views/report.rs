//! レポート画面: GitHub の Issue テンプレートを下書きにして、投稿先リポジトリへ
//! Issue を送る。
//!
//! 投稿先はこのアプリのリポジトリ（[`TARGET_REPO`]）に固定する。Issue の宛先を
//! 利用者が差し替えられるのは、誘導された利用者に別のリポジトリへ文面（と
//! スクリーンショット）を送らせる余地になるため。
//! テンプレートは Contents API から取得し、[`compose_body`] で本文の下書きに展開する。
//! 画像は user-attachments へ上げて本文の末尾に `![file](url)` を足す（失敗しても
//! 本文だけで投稿できる）。
//!
//! ネットワークも `rfd` のファイル選択も UI スレッドでは実行しない。HTTP は
//! `background_executor` に投げ、完了は `cx.spawn` で受ける。入力欄は `Window` が
//! 要るため render の冒頭で遅延生成する（`new` は `cx.new(ReportView::new)` から
//! 呼ばれるので `Window` を持てない）。

use std::sync::Arc;

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::attachment::{
    Attachment, AttachmentContent, AttachmentDescription, AttachmentStatus, AttachmentTitle,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Icon, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement, ReadGlobal as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};
use parking_lot::Mutex;
use thundoku_core::github::{
    GithubClient, GithubError, IssueTemplate, TemplateField, TemplateFieldKind,
};

use crate::app_state::{
    AppState, ToastKind, clear_github_session, delete_github_token_secret, set_toast_kind,
};

/// 投稿先リポジトリの owner（**固定**）。
const TARGET_REPO_OWNER: &str = "MegaBlackLabel";
/// 投稿先リポジトリの名前（**固定**）。
const TARGET_REPO_NAME: &str = "thundoku-shelf-app";
/// 画面に出す投稿先の表示。
///
/// 投稿先を設定で切り替えられるようにすると、誘導された利用者が別のリポジトリへ
/// 文面（スクリーンショット込み）を送ってしまうため、ここは固定にする。
pub(crate) const TARGET_REPO: &str = "MegaBlackLabel/thundoku-shelf-app";

/// 未ログインのときに出す案内（送信も添付もできない）。
const LOGIN_REQUIRED: &str = "GitHub にログインするとレポートを送れます";

/// 本文入力の高さ。これより長い本文は入力欄の中でスクロールする。
const BODY_HEIGHT: f32 = 280.0;

/// 添付できる画像の拡張子（`rfd` のフィルタと MIME 判定で同じ並びを使う）。
const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

/// 添付できる画像の上限サイズ。GitHub 側の上限（10MB）に合わせて手前で弾く。
/// 大きいファイルを選んだまま無言でアップロードを始めないための確認でもある。
const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;

/// 本文に貼る alt テキストを作る。
///
/// ファイル名はローカル FS 由来の任意文字列で、`]` `)` などが入ると Markdown の
/// リンク構造を壊し、外部 URL を本文に紛れ込ませられる（テンプレート文面を
/// そのまま流し込むのと同じ理由で、そのままは使わない）。
fn markdown_alt(file_name: &str) -> String {
    let cleaned: String = file_name
        .chars()
        .filter(|c| !matches!(c, '[' | ']' | '(' | ')' | '\\' | '\n' | '\r'))
        .take(80)
        .collect();
    let cleaned = cleaned.trim();
    if cleaned.is_empty() {
        "画像".to_string()
    } else {
        cleaned.to_string()
    }
}

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
pub(crate) fn compose_body(fields: &[TemplateField]) -> String {
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

/// `GithubError` を画面に出す日本語にする。
///
/// 原因を取り違えると次の操作が変わってしまう（ログインし直す / 投稿先を直す /
/// 待って再試行する）ため、種別ごとに別の文言を返す。
pub(crate) fn error_message(error: &GithubError) -> String {
    match error {
        GithubError::Network(_) => {
            "GitHub に接続できませんでした（オフラインか、GitHub 側の障害かもしれません）"
                .to_string()
        }
        GithubError::Auth(_) => "GitHub の認証に失敗しました。ログインし直してください".to_string(),
        GithubError::DeviceCodeExpired => {
            "GitHub の認証コードの有効期限が切れました。もう一度ログインしてください".to_string()
        }
        GithubError::AccessDenied => {
            "GitHub の認証が拒否されました。もう一度ログインしてください".to_string()
        }
        GithubError::NotAuthorized => LOGIN_REQUIRED.to_string(),
        GithubError::Forbidden(detail) => {
            format!("この操作は許可されていません（権限か回数制限を確認してください）: {detail}")
        }
        GithubError::IssuesDisabled(repo) => {
            format!("{repo} では Issue が無効になっています（投稿できません）")
        }
        GithubError::AssetUploadDenied => {
            "画像を添付できませんでした（本文のみで投稿できます）".to_string()
        }
        // 投稿先は固定なので、404 は「アプリが古い（リポジトリが移動/改名された）」か
        // 「リポジトリが削除された」ことを意味する。設定を直しても解決しない。
        GithubError::NotFound(repo) => format!(
            "{repo} が見つかりません。アプリを最新版に更新してください（リポジトリが移動・改名・削除された可能性があります）"
        ),
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

/// 拡張子から画像の MIME を決める（`IMAGE_EXTENSIONS` 以外は PNG として送る）。
fn image_content_type(path: &std::path::Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) if ext.eq_ignore_ascii_case("jpg") || ext.eq_ignore_ascii_case("jpeg") => {
            "image/jpeg"
        }
        Some(ext) if ext.eq_ignore_ascii_case("gif") => "image/gif",
        Some(ext) if ext.eq_ignore_ascii_case("webp") => "image/webp",
        _ => "image/png",
    }
}

/// 画像添付の結果（背景スレッド → UI）。
enum AttachmentOutcome {
    /// ファイル選択がキャンセルされた（画面には何も出さない）。
    Cancelled,
    /// アップロードできた（ファイル名と本文に貼る URL）。
    Uploaded { file_name: String, url: String },
    /// アップロード前に分かった問題（サイズ超過など）。GitHub 側の失敗と区別する。
    Rejected(String),
    /// 失敗（理由は種別ごとに文言化する）。
    Failed(GithubError),
}

/// 背景でファイルを選び、`user-attachments` へ上げて URL を返す。
///
/// ファイル選択ダイアログは UI スレッドをブロックしうるので、この関数ごと
/// 背景で実行する（`Window` を触らないので UI 側と競合しない）。
fn pick_and_upload(
    github: &Arc<Mutex<Option<GithubClient>>>,
    owner: &str,
    repo: &str,
) -> AttachmentOutcome {
    let Some(path) = rfd::FileDialog::new()
        .set_title("本文に添付する画像を選択")
        .add_filter("画像", &IMAGE_EXTENSIONS[..])
        .pick_file()
    else {
        return AttachmentOutcome::Cancelled;
    };
    let Some(file_name) = path
        .file_name()
        .map(|name| name.to_string_lossy().to_string())
    else {
        return AttachmentOutcome::Failed(GithubError::InvalidResponse(
            "ファイル名が取得できない".to_string(),
        ));
    };
    let bytes = match std::fs::metadata(&path) {
        Ok(meta) if meta.len() > MAX_IMAGE_BYTES => {
            return AttachmentOutcome::Rejected(format!(
                "{file_name} は大きすぎます（上限 {}MB）",
                MAX_IMAGE_BYTES / (1024 * 1024)
            ));
        }
        _ => match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                return AttachmentOutcome::Failed(GithubError::Network(format!(
                    "{file_name}: {error}"
                )));
            }
        },
    };
    let mut guard = github.lock();
    let Some(client) = guard.as_mut() else {
        return AttachmentOutcome::Failed(GithubError::NotAuthorized);
    };
    let repo_id = match client.repository_id(owner, repo) {
        Ok(repo_id) => repo_id,
        Err(error) => return AttachmentOutcome::Failed(error),
    };
    let content_type = image_content_type(&path);
    match client.upload_asset(repo_id, &file_name, content_type, &bytes) {
        Ok(url) => AttachmentOutcome::Uploaded { file_name, url },
        Err(error) => AttachmentOutcome::Failed(error),
    }
}

/// 添付 1 件の表示（[`Attachment`] チップに載せる）。
#[derive(Clone)]
struct AttachmentView {
    status: AttachmentStatus,
    title: String,
    /// 追加の説明（無いときは空文字。チップ側で出し分ける）。
    description: String,
}

/// 背景タスクの完了時に溜める本文の更新。
///
/// [gpui_kit::component::input::InputState] の `set_value` は `Window` を要るが、
/// 背景タスクの完了時には無いので、次の render で反映する。
enum PendingBody {
    /// 本文の末尾に足す（画像の添付）。
    Append(String),
    /// タイトルと本文を空にする（送信の成功）。
    Clear,
}

/// 送信した時点の下書き（成功時に「まだ同じ内容か」を見るために持つ）。
///
/// 送信の待ち時間に利用者が次を書き始めていることがあるので、無条件にクリアすると
/// 書いた内容が消える。同じ内容のときだけ消す。
struct SubmitSnapshot {
    title: String,
    body: String,
}

/// 送信時と同じ下書きか（同じときだけクリアしてよい）。
fn draft_unchanged(snapshot: &SubmitSnapshot, title: &str, body: &str) -> bool {
    snapshot.title == title && snapshot.body == body
}

pub struct ReportView {
    /// タイトル入力（render で遅延生成）。
    title_input: Option<Entity<InputState>>,
    /// 本文入力（複数行。render で遅延生成）。
    body_input: Option<Entity<TextareaState>>,
    /// 取得した Issue テンプレート（`config.yml` は core 側で除かれる）。
    templates: Vec<IssueTemplate>,
    /// 選択中のテンプレートの `file_name`（送信時の `labels` に使う）。
    selected_template: Option<String>,
    /// テンプレート取得中（二重実行防止）。
    templates_loading: bool,
    /// 取得を試みたか（render ごとに取得し直さない）。
    templates_fetched: bool,
    /// テンプレートを取得できなかった理由（自由入力で投稿はできる）。
    templates_error: Option<String>,
    /// 送信中（二重実行防止）。
    busy: bool,
    /// 送信した時点の下書き（成功時にクリアしてよいかの判定に使う）。
    snapshot: Option<SubmitSnapshot>,
    /// 画像の添付中（二重実行防止）。
    attaching: bool,
    /// 直近の画像添付の状態（チップ表示）。
    attachment: Option<AttachmentView>,
    /// 背景タスクから受け取った本文の更新（次の render で反映する）。
    pending_body: Option<PendingBody>,
    /// 画面に赤字で出す送信エラー。
    error: Option<String>,
}

impl ReportView {
    pub fn new(_cx: &mut Context<Self>) -> Self {
        Self {
            title_input: None,
            body_input: None,
            templates: Vec::new(),
            selected_template: None,
            templates_loading: false,
            templates_fetched: false,
            templates_error: None,
            busy: false,
            snapshot: None,
            attaching: false,
            attachment: None,
            pending_body: None,
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
                TextareaState::new(window, cx).placeholder(
                    "テンプレートを選ぶと下書きが入ります。画像は本文の末尾に足されます。",
                )
            });
            self.body_input = Some(state);
        }
    }

    /// 画面を開き直したときにテンプレートを取り直す（ログイン直後でも拾えるように）。
    pub fn reload(&mut self, cx: &mut Context<Self>) {
        self.templates.clear();
        self.selected_template = None;
        self.templates_error = None;
        self.templates_fetched = false;
        self.ensure_templates(cx);
        cx.notify();
    }

    /// 表示時に 1 度だけテンプレートを取得する（未ログインでは取得しない）。
    fn ensure_templates(&mut self, cx: &mut Context<Self>) {
        if self.templates_loading || self.templates_fetched {
            return;
        }
        if !*AppState::global(cx).github_logged_in.lock() {
            return;
        }
        let (owner, repo) = (TARGET_REPO_OWNER.to_string(), TARGET_REPO_NAME.to_string());
        self.templates_loading = true;
        self.templates_fetched = true;
        let github = AppState::global(cx).github.clone();
        // 弱参照: 画面が閉じたあとの完了でビューを復活させない。
        let handle = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut guard = github.lock();
                    match guard.as_mut() {
                        Some(client) => client.list_issue_templates(&owner, &repo),
                        None => Err(GithubError::NotAuthorized),
                    }
                })
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
                        // 失効したトークンならログイン状態を解除して再ログインへ誘導する。
                        if matches!(error, GithubError::Auth(_)) {
                            // 失効したトークンは keyring からも消す。削除は OS の応答待ちで
                            // 止まり得るので背景で行い、結果は待たない（メモリ上の状態は
                            // すぐ落として再ログインできるようにする）。
                            clear_github_session(cx);
                            cx.background_executor()
                                .spawn(async move {
                                    let _ = delete_github_token_secret();
                                })
                                .detach();
                        }
                        this.templates_error = Some(error_message(&error));
                    }
                }
                cx.notify();
            });
        })
        .detach();
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
        // 本文を下書きで置き換えるので、前に足した画像の Markdown は消える。
        // チップだけ残すと「添付したのに本文に無い」状態になるため一緒に消す。
        self.attachment = None;
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

    /// 「画像を添付」: 選んだ画像を上げて、本文の末尾に `![file](url)` を足す。
    /// 失敗しても本文だけで投稿できる。
    fn attach_image(&mut self, cx: &mut Context<Self>) {
        // 送信中に添付を始めると、本文へ足す処理と送信後のクリアが競合する。
        if self.attaching || self.busy {
            return;
        }
        if !*AppState::global(cx).github_logged_in.lock() {
            self.error = Some(LOGIN_REQUIRED.to_string());
            cx.notify();
            return;
        }
        let (owner, repo) = (TARGET_REPO_OWNER.to_string(), TARGET_REPO_NAME.to_string());
        self.attaching = true;
        self.error = None;
        self.attachment = Some(AttachmentView {
            status: AttachmentStatus::Uploading,
            title: "画像を選択しています…".to_string(),
            description: String::new(),
        });
        cx.notify();
        let github = AppState::global(cx).github.clone();
        let handle = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let outcome = cx
                .background_executor()
                .spawn(async move { pick_and_upload(&github, &owner, &repo) })
                .await;
            let _ = handle.update(cx, |this, cx| {
                this.attaching = false;
                match outcome {
                    // キャンセルは失敗ではない（チップも残さない）。
                    AttachmentOutcome::Cancelled => this.attachment = None,
                    AttachmentOutcome::Uploaded { file_name, url } => {
                        // ファイル名はそのまま alt に使わない（Markdown を壊せる）。
                        this.pending_body = Some(PendingBody::Append(format!(
                            "![{}]({url})",
                            markdown_alt(&file_name)
                        )));
                        this.attachment = Some(AttachmentView {
                            status: AttachmentStatus::Complete,
                            title: format!("{file_name} を添付しました"),
                            description: String::new(),
                        });
                    }
                    AttachmentOutcome::Rejected(reason) => {
                        this.attachment = Some(AttachmentView {
                            status: AttachmentStatus::Failed,
                            title: reason,
                            description: "本文のみで投稿できます".to_string(),
                        });
                    }
                    AttachmentOutcome::Failed(error) => {
                        log::warn!("report: 画像を添付できませんでした: {error}");
                        // 失効したトークンならログイン状態を解除して再ログインへ誘導する。
                        if matches!(error, GithubError::Auth(_)) {
                            // 失効したトークンは keyring からも消す。削除は OS の応答待ちで
                            // 止まり得るので背景で行い、結果は待たない（メモリ上の状態は
                            // すぐ落として再ログインできるようにする）。
                            clear_github_session(cx);
                            cx.background_executor()
                                .spawn(async move {
                                    let _ = delete_github_token_secret();
                                })
                                .detach();
                        }
                        // write 権限が無いだけの失敗は、それ以上の説明が無い。
                        let detail = match error {
                            GithubError::AssetUploadDenied => String::new(),
                            other => error_message(&other),
                        };
                        this.attachment = Some(AttachmentView {
                            status: AttachmentStatus::Failed,
                            title: "画像を添付できませんでした（本文のみで投稿できます）"
                                .to_string(),
                            description: detail,
                        });
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// 「Issue を作成」: 背景で `create_issue` を呼び、成功したら通知してフォームを空にする。
    fn submit(&mut self, cx: &mut Context<Self>) {
        // 添付中に送ると、本文へ足す処理と送信後のクリアが競合して内容が入れ替わる。
        if self.busy || self.attaching {
            return;
        }
        if !*AppState::global(cx).github_logged_in.lock() {
            self.error = Some(LOGIN_REQUIRED.to_string());
            cx.notify();
            return;
        }
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
        let (owner, repo) = (TARGET_REPO_OWNER.to_string(), TARGET_REPO_NAME.to_string());
        // 成功時に「まだ同じ下書きか」を判定できるように控えておく。
        self.snapshot = Some(SubmitSnapshot {
            title: title.clone(),
            body: body.clone(),
        });
        // 選択中のテンプレートの labels を引き継ぐ（未選択ならラベル無し）。
        let labels = self
            .selected_template
            .as_ref()
            .and_then(|file_name| {
                self.templates
                    .iter()
                    .find(|template| &template.file_name == file_name)
            })
            .map(|template| template.labels.clone())
            .unwrap_or_default();
        let (owner, repo) = (owner.to_string(), repo.to_string());
        self.busy = true;
        self.error = None;
        cx.notify();
        let github = AppState::global(cx).github.clone();
        let handle = cx.entity().downgrade();
        cx.spawn(async move |_, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let mut guard = github.lock();
                    match guard.as_mut() {
                        Some(client) => client.create_issue(&owner, &repo, &title, &body, &labels),
                        None => Err(GithubError::NotAuthorized),
                    }
                })
                .await;
            let _ = handle.update(cx, |this, cx| {
                this.busy = false;
                match result {
                    Ok(issue) => {
                        this.error = None;
                        // 待っている間に利用者が次の下書きを書き始めていたら消さない。
                        let (title, body) = this.current_draft(cx);
                        let unchanged = this
                            .snapshot
                            .take()
                            .is_some_and(|snapshot| draft_unchanged(&snapshot, &title, &body));
                        if unchanged {
                            this.attachment = None;
                            this.selected_template = None;
                            this.pending_body = Some(PendingBody::Clear);
                        }
                        set_toast_kind(
                            cx,
                            ToastKind::Success,
                            format!("Issue を作成しました: {}", issue.html_url),
                        );
                    }
                    Err(error) => {
                        log::error!("report: Issue を作成できませんでした: {error}");
                        this.snapshot = None;
                        // トークンが失効/取り消しされている場合は、ログイン状態を解除して
                        // 再ログインできる状態に戻す（keyring からも消す）。そうしないと
                        // 「予期しない応答」だけが出て復帰できない。
                        if matches!(error, GithubError::Auth(_)) {
                            // 失効したトークンは keyring からも消す。削除は OS の応答待ちで
                            // 止まり得るので背景で行い、結果は待たない（メモリ上の状態は
                            // すぐ落として再ログインできるようにする）。
                            clear_github_session(cx);
                            cx.background_executor()
                                .spawn(async move {
                                    let _ = delete_github_token_secret();
                                })
                                .detach();
                        }
                        this.error = Some(error_message(&error));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// いま入力欄に入っているタイトルと本文（成功時に「同じ下書きか」を見るため）。
    fn current_draft(&self, cx: &gpui_kit::App) -> (String, String) {
        let title = self
            .title_input
            .as_ref()
            .map(|input| input.read(cx).value().trim().to_string())
            .unwrap_or_default();
        let body = self
            .body_input
            .as_ref()
            .map(|input| input.read(cx).value().to_string())
            .unwrap_or_default();
        (title, body)
    }

    /// 背景タスクから受け取った本文の更新を反映する（次の render で呼ぶ）。
    fn apply_pending_body(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(pending) = self.pending_body.take() else {
            return;
        };
        match pending {
            PendingBody::Append(markdown) => {
                let Some(input) = self.body_input.clone() else {
                    return;
                };
                input.update(cx, |state, cx| {
                    let current = state.value();
                    let current = current.trim_end();
                    let value = if current.is_empty() {
                        markdown
                    } else {
                        format!("{current}\n\n{markdown}")
                    };
                    state.set_value(value, window, cx);
                });
            }
            PendingBody::Clear => {
                if let Some(input) = self.title_input.clone() {
                    input.update(cx, |state, cx| state.set_value("", window, cx));
                }
                if let Some(input) = self.body_input.clone() {
                    input.update(cx, |state, cx| state.set_value("", window, cx));
                }
            }
        }
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
        self.apply_pending_body(window, cx);
        self.ensure_templates(cx);

        let logged_in = *AppState::global(cx).github_logged_in.lock();
        let busy = self.busy;
        let attaching = self.attaching;
        let error = self.error.clone();
        let templates_error = self.templates_error.clone();
        let templates_loading = self.templates_loading;
        let attachment = self.attachment.clone();
        let selected = self.selected_template.clone();
        let border = cx.theme().border;
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
            .size_full()
            .flex_1()
            .min_h_0()
            .overflow_y_scrollbar()
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
                                "不具合や要望を GitHub の Issue として送ります。\
                                     テンプレートを選ぶと下書きが入ります。",
                            )),
                    )
                    // 未ログインの案内
                    .child(if logged_in {
                        div().into_any_element()
                    } else {
                        div()
                            .debug_selector(|| "report-login-required".to_string())
                            .rounded_xl()
                            .border_1()
                            .border_color(border)
                            .bg(cx.theme().muted)
                            .p_4()
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(div().text_sm().child(LOGIN_REQUIRED))
                            .child(div().text_xs().text_color(muted_fg).child(
                                "サイドバー下部のアカウント、または設定画面からログインできます。",
                            ))
                            .into_any_element()
                    })
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
                                     送信には GitHub へのログインが必要です。",
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
                        Some("タイトルと本文を確認して送ります"),
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
                            // 画像の添付
                            .child(
                                div()
                                    .flex()
                                    .flex_row()
                                    .items_center()
                                    .gap_3()
                                    .child(
                                        Button::new("report-attach")
                                            .outline()
                                            .cursor_pointer()
                                            .disabled(attaching || busy || !logged_in)
                                            .label("画像を添付")
                                            .debug_selector(|| "report-attach".to_string())
                                            .on_click({
                                                let handle = handle.clone();
                                                move |_, _window, cx| {
                                                    handle.update(cx, |this, cx| {
                                                        this.attach_image(cx);
                                                    });
                                                }
                                            }),
                                    )
                                    .child(div().text_xs().text_color(muted_fg).child(
                                        "画像は GitHub へアップロードし、本文の末尾へ貼ります",
                                    )),
                            )
                            // 添付の状態
                            .child(if let Some(view) = attachment {
                                let content = if view.description.is_empty() {
                                    AttachmentContent::new().title(AttachmentTitle::new(view.title))
                                } else {
                                    AttachmentContent::new()
                                        .title(AttachmentTitle::new(view.title))
                                        .description(AttachmentDescription::new(view.description))
                                };
                                Attachment::new()
                                    .status(view.status)
                                    .content(content)
                                    .into_any_element()
                            } else {
                                div().into_any_element()
                            })
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
                                            .child(format!("{TARGET_REPO} に Issue を作成します")),
                                    )
                                    .child(
                                        Button::new("report-submit")
                                            .primary()
                                            .cursor_pointer()
                                            .loading(busy)
                                            .disabled(busy || attaching || !logged_in)
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
    use gpui_kit::TestAppContext;

    use super::*;

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

    /// 送信の待ち時間に書き換えられていたら、成功時でも下書きを消さないこと。
    #[test]
    fn draft_unchanged_detects_newer_edits() {
        let snapshot = SubmitSnapshot {
            title: "[Bug] スクロール".to_string(),
            body: "## 概要\n引っかかる".to_string(),
        };

        assert!(draft_unchanged(
            &snapshot,
            "[Bug] スクロール",
            "## 概要\n引っかかる"
        ));
        // タイトルだけ書き換えた
        assert!(!draft_unchanged(
            &snapshot,
            "[Bug] 別の件",
            "## 概要\n引っかかる"
        ));
        // 本文だけ書き換えた（テンプレ選択で置き換わった場合など）
        assert!(!draft_unchanged(
            &snapshot,
            "[Bug] スクロール",
            "## 概要\n別の内容"
        ));
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
            GithubError::IssuesDisabled("owner/repo".to_string()),
            GithubError::NotFound("owner/repo".to_string()),
            GithubError::AssetUploadDenied,
            GithubError::NotAuthorized,
            GithubError::Network("connection reset".to_string()),
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

    /// 投稿先はアプリのリポジトリに固定されている（設定で差し替える入力が無い）。
    #[gpui_kit::test]
    async fn report_screen_shows_the_fixed_target(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        // 旧バージョンが残した設定があっても、画面は固定の投稿先を出す
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            let _ = thundoku_core::db::settings::set(db, "report.target_repo", "someone/elsewhere");
        });
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

        assert!(
            visual.debug_bounds("report-target-repo").is_some(),
            "投稿先の表示が出ていない"
        );
    }

    /// 未ログインでは案内を出して送信しない（Issue を作りに行かない）。
    #[gpui_kit::test]
    async fn logged_out_shows_the_notice_and_does_not_submit(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        cx.update(|cx| *AppState::global(cx).github_logged_in.lock() = false);
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

        assert!(
            visual.debug_bounds("report-login-required").is_some(),
            "未ログインの案内が出ていない"
        );
        assert!(
            visual.debug_bounds("report-title").is_some(),
            "タイトル入力が出ていない"
        );
        assert!(
            visual.debug_bounds("report-body").is_some(),
            "本文入力が出ていない"
        );

        // 送信しようとしても問い合わせず、画面に案内を出すだけ
        view.update(cx, |this, cx| this.submit(cx));
        let error = view.read_with(cx, |this, _| this.error.clone());
        assert_eq!(error.as_deref(), Some(LOGIN_REQUIRED));
        assert!(
            !view.read_with(cx, |this, _| this.busy),
            "未ログインなのに送信を始めている"
        );
    }
}
