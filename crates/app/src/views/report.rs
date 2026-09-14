//! レポート画面: GitHub の Issue テンプレートを下書きにして、投稿先リポジトリへ
//! Issue を送る。
//!
//! 投稿先は設定 `report.target_repo`（既定 [`DEFAULT_TARGET_REPO`]）で切り替えられる。
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
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Icon, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, Entity, FontWeight, InteractiveElement as _, IntoElement,
    ParentElement, ReadGlobal as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Window, div, px,
};
use parking_lot::Mutex;
use thundoku_core::db;
use thundoku_core::github::{
    GithubClient, GithubError, IssueTemplate, TemplateField, TemplateFieldKind,
};

use crate::app_state::{AppState, ToastKind, set_toast_kind};

/// 投稿先の既定リポジトリ（設定 `report.target_repo` が無いとき）。
pub(crate) const DEFAULT_TARGET_REPO: &str = "MegaBlackLabel/thundoku-shelf-app";

/// 投稿先リポジトリの設定キー。
const TARGET_REPO_KEY: &str = "report.target_repo";

/// 未ログインのときに出す案内（送信も添付もできない）。
const LOGIN_REQUIRED: &str = "GitHub にログインするとレポートを送れます";

/// 本文入力の高さ。これより長い本文は入力欄の中でスクロールする。
const BODY_HEIGHT: f32 = 280.0;

/// 添付できる画像の拡張子（`rfd` のフィルタと MIME 判定で同じ並びを使う）。
const IMAGE_EXTENSIONS: [&str; 5] = ["png", "jpg", "jpeg", "gif", "webp"];

/// 選択したテンプレートの各項目を Markdown の下書きに展開する。
///
/// - `markdown` の項目は `value`（説明文）をそのまま 1 節として出す
/// - それ以外は `## {label}` の見出し + `placeholder`（無ければ `value`）
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
        let section = match field.kind {
            // 説明文は見出しを持たないので、本文だけをそのまま出す。
            TemplateFieldKind::Markdown => text.map(str::to_string),
            _ => match (label, text) {
                (Some(label), Some(text)) => Some(format!("## {label}\n{text}")),
                // 見出しが無い項目は本文だけを出す（`## ` だけの行を作らない）。
                (None, Some(text)) => Some(text.to_string()),
                // 本文が空でも見出しは出す（利用者がそこへ書く）。
                (Some(label), None) => Some(format!("## {label}")),
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
        GithubError::NotFound(repo) => format!(
            "{repo} が見つかりません（投稿先の設定を確認してください。private リポジトリには投稿できません）"
        ),
        GithubError::InvalidResponse(detail) => {
            format!("GitHub から予期しない応答が返りました: {detail}")
        }
    }
}

/// `owner/repo` を分解する（`https://github.com/owner/repo` 形式や末尾の `.git` も許容）。
///
/// 形式が違えば `None`。そのまま API の URL に埋めると別のリポジトリを叩きかねない。
fn split_repo(value: &str) -> Option<(&str, &str)> {
    let value = value.trim();
    let value = value
        .strip_prefix("https://github.com/")
        .or_else(|| value.strip_prefix("http://github.com/"))
        .or_else(|| value.strip_prefix("github.com/"))
        .unwrap_or(value);
    let value = value
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .trim_end_matches('/');
    let (owner, repo) = value.split_once('/')?;
    let shaped = !owner.is_empty() && !repo.is_empty() && !repo.contains('/');
    if !shaped || value.contains(char::is_whitespace) {
        return None;
    }
    Some((owner, repo))
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
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            return AttachmentOutcome::Failed(GithubError::Network(format!(
                "{file_name}: {error}"
            )));
        }
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

pub struct ReportView {
    /// 投稿先リポジトリ（`owner/repo`。設定 `report.target_repo` と同期）。
    repo: String,
    /// 投稿先の編集入力（render で遅延生成）。
    repo_input: Option<Entity<InputState>>,
    repo_subscription: Option<Subscription>,
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
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            repo: Self::read_setting(cx, TARGET_REPO_KEY)
                .unwrap_or_else(|| DEFAULT_TARGET_REPO.to_string()),
            repo_input: None,
            repo_subscription: None,
            title_input: None,
            body_input: None,
            templates: Vec::new(),
            selected_template: None,
            templates_loading: false,
            templates_fetched: false,
            templates_error: None,
            busy: false,
            attaching: false,
            attachment: None,
            pending_body: None,
            error: None,
        }
    }

    fn read_setting(cx: &App, key: &str) -> Option<String> {
        let state = AppState::global(cx);
        let db = &state.db_pool;
        db::settings::get(db, key)
            .ok()
            .flatten()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty())
    }

    fn write_setting(cx: &mut Context<Self>, key: &str, value: &str) {
        let state = AppState::global(cx);
        let db = &state.db_pool;
        let _ = db::settings::set(db, key, value);
    }

    /// 入力欄を遅延生成する（`new` は `Window` を持てないため render で作る）。
    fn ensure_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.repo_input.is_none() {
            let state = cx.new(|cx| InputState::new(window, cx).placeholder(DEFAULT_TARGET_REPO));
            state.update(cx, |state, cx| {
                state.set_value(self.repo.clone(), window, cx);
            });
            let subscription = cx.subscribe(
                &state,
                |this: &mut Self, _: Entity<InputState>, event: &InputEvent, cx| {
                    if matches!(event, InputEvent::Blur | InputEvent::PressEnter { .. }) {
                        this.commit_repo(cx);
                    }
                },
            );
            self.repo_input = Some(state);
            self.repo_subscription = Some(subscription);
        }
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

    /// 投稿先の入力（Enter / フォーカス外れ）を確定して設定へ保存する。
    /// 変わったときはテンプレートも取り直す（次の render で取得する）。
    fn commit_repo(&mut self, cx: &mut Context<Self>) {
        let Some(input) = self.repo_input.as_ref() else {
            return;
        };
        let value = input.read(cx).value().trim().to_string();
        if value == self.repo {
            return;
        }
        self.repo = value;
        Self::write_setting(cx, TARGET_REPO_KEY, &self.repo);
        self.templates.clear();
        self.selected_template = None;
        self.templates_error = None;
        self.templates_fetched = false;
        self.error = None;
        cx.notify();
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
        let Some((owner, repo)) = split_repo(&self.repo) else {
            self.templates_fetched = true;
            self.templates_error = Some("投稿先は owner/repo の形式で入力してください".to_string());
            return;
        };
        let (owner, repo) = (owner.to_string(), repo.to_string());
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
        if self.attaching {
            return;
        }
        if !*AppState::global(cx).github_logged_in.lock() {
            self.error = Some(LOGIN_REQUIRED.to_string());
            cx.notify();
            return;
        }
        let Some((owner, repo)) = split_repo(&self.repo) else {
            self.error = Some("投稿先は owner/repo の形式で入力してください".to_string());
            cx.notify();
            return;
        };
        let (owner, repo) = (owner.to_string(), repo.to_string());
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
                        this.pending_body =
                            Some(PendingBody::Append(format!("![{file_name}]({url})")));
                        this.attachment = Some(AttachmentView {
                            status: AttachmentStatus::Complete,
                            title: format!("{file_name} を添付しました"),
                            description: String::new(),
                        });
                    }
                    AttachmentOutcome::Failed(error) => {
                        log::warn!("report: 画像を添付できませんでした: {error}");
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
        if self.busy {
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
        let Some((owner, repo)) = split_repo(&self.repo) else {
            self.error = Some("投稿先は owner/repo の形式で入力してください".to_string());
            cx.notify();
            return;
        };
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
                        this.attachment = None;
                        this.selected_template = None;
                        this.pending_body = Some(PendingBody::Clear);
                        set_toast_kind(
                            cx,
                            ToastKind::Success,
                            format!("Issue を作成しました: {}", issue.html_url),
                        );
                    }
                    Err(error) => {
                        log::error!("report: Issue を作成できませんでした: {error}");
                        this.error = Some(error_message(&error));
                    }
                }
                cx.notify();
            });
        })
        .detach();
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
        let target = self.repo.clone();
        let border = cx.theme().border;
        let muted_fg = cx.theme().muted_foreground;
        let handle = cx.entity();
        let repo_input = self
            .repo_input
            .clone()
            .expect("投稿先の入力は ensure_inputs で作られる");
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
                        Some("Issue を作るリポジトリ（owner/repo）"),
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
                                    .w_full()
                                    .child(Input::new(&repo_input).w_full()),
                            )
                            .child(div().text_xs().text_color(muted_fg).child(
                                "Enter かフォーカスを外すと保存します。\
                                     https://github.com/owner/repo の形式も使えます。",
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
                                    .text_xs()
                                    .text_color(muted_fg)
                                    .child(message)
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
                                            .disabled(attaching || !logged_in)
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
                                            .child(format!("{target} に Issue を作成します")),
                                    )
                                    .child(
                                        Button::new("report-submit")
                                            .primary()
                                            .cursor_pointer()
                                            .loading(busy)
                                            .disabled(busy || !logged_in)
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

    /// 投稿先は `owner/repo` と GitHub の URL を受け付け、壊れた値は弾くこと。
    #[test]
    fn split_repo_accepts_urls_and_rejects_invalid_values() {
        assert_eq!(
            split_repo(" MegaBlackLabel/thundoku-shelf-app "),
            Some(("MegaBlackLabel", "thundoku-shelf-app"))
        );
        assert_eq!(
            split_repo("https://github.com/MegaBlackLabel/thundoku-shelf-app.git"),
            Some(("MegaBlackLabel", "thundoku-shelf-app"))
        );
        assert_eq!(split_repo(""), None);
        assert_eq!(split_repo("MegaBlackLabel"), None);
        assert_eq!(split_repo("owner/repo/extra"), None);
        assert_eq!(split_repo("owner/re po"), None);
    }

    /// 投稿先は設定 `report.target_repo` から復元され、既定はアプリのリポジトリ。
    #[gpui_kit::test]
    async fn target_repo_comes_from_the_setting(cx: &mut TestAppContext) {
        cx.update(gpui_kit::component::init);
        cx.update(AppState::init_test);
        let view = cx.new(ReportView::new);
        assert_eq!(
            view.read_with(cx, |this, _| this.repo.clone()),
            DEFAULT_TARGET_REPO
        );

        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            db::settings::set(db, TARGET_REPO_KEY, "someone/elsewhere").unwrap();
        });
        let view = cx.new(ReportView::new);
        assert_eq!(
            view.read_with(cx, |this, _| this.repo.clone()),
            "someone/elsewhere"
        );

        // 他のテストと同じ data_dir を共有するため、既定へ戻しておく
        cx.update(|cx| {
            let db = &AppState::global(cx).db_pool;
            let _ = db::settings::set(db, TARGET_REPO_KEY, DEFAULT_TARGET_REPO);
        });
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
