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
//! 画像はこのアプリからアップロードしない（選択 UI も持たない）。GitHub の画面で添付してもらう。
//!
//! テンプレート取得の HTTP は UI スレッドでは実行しない。`background_executor` に投げ、
//! 完了は `cx.spawn` で受ける。入力欄は `Window` が要るため render の冒頭で遅延生成する
//! （`new` は `cx.new(ReportView::new)` から呼ばれるので `Window` を持てない）。

use gpui_kit::component::ActiveTheme as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputState, Textarea, TextareaState};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{Icon, IconName};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AppContext as _, ClipboardItem, Context, Entity, FontWeight, InteractiveElement as _,
    IntoElement, ParentElement, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Window, div, px,
};
use thundoku_core::github::{
    GithubError, IssueTemplate, TemplateField, TemplateFieldKind, issue_link,
};
// テンプレート取得のクライアント。取得はテストでは実行しない（`ensure_templates` を参照）。
#[cfg(not(test))]
use thundoku_core::github::GithubClient;

use crate::app_state::{ToastKind, set_toast_kind};

/// 投稿先リポジトリの owner（**固定**）。
const TARGET_REPO_OWNER: &str = "MegaBlackLabel";
/// 投稿先リポジトリの名前（**固定**）。
const TARGET_REPO_NAME: &str = "thundoku-shelf-app";
/// 画面に出す投稿先の表示。
pub(crate) const TARGET_REPO: &str = "MegaBlackLabel/thundoku-shelf-app";

/// 本文入力の高さ。これより長い本文は入力欄の中でスクロールする。
const BODY_HEIGHT: f32 = 280.0;

/// 画像の添付方法（アプリからは上げないことを画面に明記する）。
const IMAGE_NOTE: &str = "画像は GitHub の画面で添付してください（このアプリからはアップロードしません）";

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
    /// 画面に赤字で出すエラー（タイトル未入力・ブラウザーを開けない）。
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
    /// 本文が長すぎて URL に載らないときは、**先にクリップボードへコピーしてから**素の
    /// 作成画面を開く（利用者が GitHub の画面へ貼り付ける）。
    ///
    /// ブラウザーを開く処理は Google ログインと同じ実装（`google::open_browser`）を使う。
    /// Windows では `ShellExecuteW` で URL をそのままシェルへ渡す（コマンドライン経由だと
    /// `&` で URL が切れる）。
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
        let link = issue_link(TARGET_REPO_OWNER, TARGET_REPO_NAME, &title, &body);
        if let Some(body) = &link.copy_body {
            cx.write_to_clipboard(ClipboardItem::new_string(body.clone()));
        }
        match thundoku_core::google::open_browser(&link.url) {
            Ok(()) => {
                self.error = None;
                set_toast_kind(
                    cx,
                    ToastKind::Info,
                    if link.copy_body.is_some() {
                        "本文をコピーしました。GitHub の画面に貼り付けてください"
                    } else {
                        "既定のブラウザーで Issue の作成画面を開きました"
                    },
                );
            }
            Err(error) => {
                log::warn!("report: ブラウザーを開けませんでした: {error}");
                // コピー済みなら本文は失われていない（貼り付ける先だけ利用者が開く）。
                self.error = Some(if link.copy_body.is_some() {
                    "ブラウザーを開けませんでした（本文はコピー済みです。\
                     ブラウザーで GitHub を開いて貼り付けてください）"
                        .to_string()
                } else {
                    "ブラウザーを開けませんでした（既定のブラウザーの設定を確認してください）"
                        .to_string()
                });
            }
        }
        cx.notify();
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
                            // 画像の添付（アプリからは上げない）
                            .child(
                                div()
                                    .debug_selector(|| "report-image-note".to_string())
                                    .text_xs()
                                    .text_color(muted_fg)
                                    .child(IMAGE_NOTE),
                            )
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
    use gpui_kit::ReadGlobal as _;
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

    /// テンプレートは公開リポジトリから匿名で取るので、**ログイン導線も画像の選択 UI も
    /// 無い**こと（アプリから画像をアップロードしないことを画面に明記する）。
    ///
    /// 旧バージョンが残した設定があっても、画面は固定の投稿先を出す。
    #[gpui_kit::test]
    async fn report_screen_shows_the_fixed_target_without_login_or_upload_ui(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_kit::component::init);
        cx.update(crate::app_state::AppState::init_test);
        cx.update(|cx| {
            let db = &crate::app_state::AppState::global(cx).db_pool;
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
        assert!(
            visual.debug_bounds("report-submit").is_some(),
            "「Issue を作成」が出ていない"
        );
        assert!(
            visual.debug_bounds("report-image-note").is_some(),
            "画像は GitHub の画面で添付する旨が出ていない"
        );
        // ログインは要らない（トークンを持たない）ので、案内も画像の選択 UI も無い。
        assert!(
            visual.debug_bounds("report-login-required").is_none(),
            "ログインの案内が残っている"
        );
        assert!(
            visual.debug_bounds("report-attach").is_none(),
            "画像の選択 UI が残っている"
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
}
