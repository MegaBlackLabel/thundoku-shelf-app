//! オープンソースライセンス表示のデータ層（案内画面の「ライセンスについて」から開く）。
//!
//! 表示する 1 件は 3 種類ある:
//!
//! - アプリ本体（リポジトリ直下の `LICENSE`）
//! - 同梱アセット（Lucide アイコン・PDFium など、Rust クレートではないもの）
//! - 依存クレート（`Cargo.lock`。クレートに同梱されているライセンス全文つき）
//!
//! 依存クレートの一覧と全文は `examples/regen_licenses.rs` が `Cargo.lock` とローカルの cargo
//! キャッシュから生成し、`assets/third-party/licenses.json` としてコミットしてある
//! （依存を変えたら `mise run licenses` で作り直す。作り忘れはテスト
//! `licenses_json_matches_cargo_lock` が検出する）。

use std::sync::LazyLock;

/// アプリの表示名（案内画面の見出しと同じ）。
const APP_NAME: &str = "Thundoku Shelf";

/// アプリ本体のライセンス全文（リポジトリ直下の `LICENSE`。配布物と同一）。
const APP_LICENSE: &str = include_str!("../../../../LICENSE");

/// 依存クレートの一覧。生成物なので手で直さない
/// （`mise run licenses` = `cargo run -p thundoku-shelf --example regen_licenses` で更新する）。
const GENERATED: &str = include_str!("../../assets/third-party/licenses.json");

/// Rust クレートではない同梱物（クレートの `license` 表記では表せないもの）。
struct Bundled {
    name: &'static str,
    license: &'static str,
    repository: &'static str,
    note: &'static str,
    text: &'static str,
}

const BUNDLED: [Bundled; 2] = [
    Bundled {
        name: "Lucide",
        license: "ISC",
        repository: "https://lucide.dev",
        note: "アイコン（サイドバー・各画面の表示に使用）",
        text: include_str!("../../assets/third-party/lucide-LICENSE.txt"),
    },
    Bundled {
        name: "PDFium",
        license: "BSD-3-Clause AND Apache-2.0",
        repository: "https://pdfium.googlesource.com/pdfium/",
        note: "PDF 表示（Windows 版に同梱している pdfium.dll）",
        text: include_str!("../../assets/third-party/pdfium-LICENSE.txt"),
    },
];

/// 表示用の 1 件。
#[derive(Debug, Clone)]
pub struct Entry {
    pub name: String,
    pub version: String,
    /// SPDX のライセンス表記（例: `MIT OR Apache-2.0`）。
    pub license: String,
    /// 配布元（クレートは `repository`、同梱アセットは配布サイト）。
    pub repository: String,
    /// 何に使っているかの補足（同梱アセットのみ）。
    pub note: String,
    pub kind: Kind,
    /// (ライセンス名, 全文の添字)。全文が同梱されていないクレートは空。
    pub texts: Vec<(String, usize)>,
}

/// 1 件の種別（表示順とチップに使う）。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// アプリ本体。
    App,
    /// Rust クレートではない同梱物（アイコン・PDF ライブラリなど）。
    Bundled,
    /// Rust の依存クレート。
    Crate,
}

impl Kind {
    /// 行に出すチップ。クレートはライセンス表記を出すので `None`。
    pub fn chip(self) -> Option<&'static str> {
        match self {
            Self::App => Some("アプリ本体"),
            Self::Bundled => Some("同梱アセット"),
            Self::Crate => None,
        }
    }
}

/// 表示する全件（表示順に整列済み）とライセンス全文。
pub struct Catalog {
    pub entries: Vec<Entry>,
    texts: Vec<String>,
}

impl Catalog {
    /// ライセンス全文（[`Entry::texts`] の添字で引く）。
    pub fn text(&self, ix: usize) -> &str {
        self.texts.get(ix).map(String::as_str).unwrap_or_default()
    }
}

/// 表示する全件（初回参照時に生成して使い回す）。
pub static CATALOG: LazyLock<Catalog> = LazyLock::new(load);

/// `licenses.json`（`examples/regen_licenses.rs` が書き出す）。
#[derive(serde::Deserialize)]
struct Generated {
    #[serde(default)]
    texts: Vec<String>,
    #[serde(default)]
    packages: Vec<GeneratedPackage>,
}

#[derive(serde::Deserialize)]
struct GeneratedPackage {
    name: String,
    version: String,
    #[serde(default)]
    license: String,
    #[serde(default)]
    repository: String,
    #[serde(default)]
    texts: Vec<(String, usize)>,
}

fn load() -> Catalog {
    let generated: Generated = serde_json::from_str(GENERATED)
        .expect("licenses.json は examples/regen_licenses.rs が生成する");
    let mut texts = generated.texts;
    // クレートの全文の添字は生成側が付けたもの。アプリ本体と同梱アセットの全文は
    // この後ろに足すので、区切りを覚えておく。
    let generated_texts = texts.len();

    let crates: Vec<Entry> = generated
        .packages
        .into_iter()
        .map(|package| Entry {
            name: package.name,
            version: package.version,
            license: package.license,
            repository: package.repository,
            note: String::new(),
            kind: Kind::Crate,
            // 添字が壊れていても画面が落ちないように弾く（全文は `Catalog::text` が空を返す）
            texts: package
                .texts
                .into_iter()
                .filter(|(_, ix)| *ix < generated_texts)
                .collect(),
        })
        .collect();

    let mut entries = Vec::with_capacity(crates.len() + BUNDLED.len() + 1);
    // アプリ本体（1 件目に出す）
    entries.push(Entry {
        name: APP_NAME.to_string(),
        version: env!("CARGO_PKG_VERSION").to_string(),
        license: "MIT".to_string(),
        repository: "https://github.com/MegaBlackLabel/thundoku-shelf-app".to_string(),
        note: String::new(),
        kind: Kind::App,
        texts: vec![("MIT".to_string(), texts.len())],
    });
    texts.push(APP_LICENSE.to_string());

    for bundled in BUNDLED {
        entries.push(Entry {
            name: bundled.name.to_string(),
            version: String::new(),
            license: bundled.license.to_string(),
            repository: bundled.repository.to_string(),
            note: bundled.note.to_string(),
            kind: Kind::Bundled,
            texts: vec![(bundled.license.to_string(), texts.len())],
        });
        texts.push(bundled.text.to_string());
    }

    entries.extend(crates);
    Catalog { entries, texts }
}

/// 依存クレートに許すライセンス（SPDX 識別子。判定では大文字小文字を無視する）。
///
/// **許可リスト方式**であることが要点。GPL/AGPL を列挙する deny リスト方式だと、一覧に
/// 無いライセンスが黙って通ってしまう（依存を 1 本足すだけで AGPL が混入しうる）。
/// ここに無いライセンスは**失敗**させて、人間が「MIT 配布のこのアプリと両立するか」を
/// 判断してから足す。追加するときは根拠を docs に書くこと。
///
/// - 許諾的なもの（コピーレフト無し）: `MIT` / `Apache-2.0` / `BSD-2-Clause` /
///   `BSD-3-Clause` / `ISC` / `Zlib` / `Unlicense` / `CC0-1.0` / `0BSD` / `MIT-0` /
///   `Unicode-3.0` / `CDLA-Permissive-2.0` / `BSL-1.0`（Boost Software License）
/// - `MPL-2.0`: gpui 経由の cssparser / cssparser-macros / selectors / dtoa-short /
///   option-ext / resvg / usvg / dwrote などが MPL-2.0。**ファイル単位のコピーレフト**で、
///   未改変の依存として使う限り（このアプリはそうしている）MIT 配布のアプリと両立する。
///   クレートを改変して配るときだけソース開示義務が生じるので、そのときはここを見直す。
/// - `NCSA`: `libfuzzer-sys`（`rav1e` 経由）の `(MIT OR Apache-2.0) AND NCSA` に含まれる
///   イリノイ大学のライセンス（許諾的。BSD 系）。
/// - `bzip2-1.0.6`: `libbz2-rs-sys`（`bzip2` → `compression-codecs` 経由で入る bzip2 の
///   Rust 実装）の単独ライセンス（許諾的。BSD 系）。
#[cfg(test)]
const ALLOWED_LICENSES: [&str; 16] = [
    "0BSD",
    "Apache-2.0",
    "BSD-2-Clause",
    "BSD-3-Clause",
    "BSL-1.0",
    "bzip2-1.0.6",
    "CC0-1.0",
    "CDLA-Permissive-2.0",
    "ISC",
    "MIT",
    "MIT-0",
    "MPL-2.0",
    "NCSA",
    "Unicode-3.0",
    "Unlicense",
    "Zlib",
];

/// `WITH` で付く SPDX の例外。例外は**制約を緩めるだけ**（付け足しても禁止事項は増えない）ので、
/// 基本ライセンスが許可リストにあることを条件に、既知のものだけ認める。
/// 実データにあるのは `Apache-2.0 WITH LLVM-exception`（rustix / wasi / target-lexicon など）。
#[cfg(test)]
const ALLOWED_EXCEPTIONS: [&str; 1] = ["LLVM-exception"];

/// `expr`（SPDX のライセンス式。古い `/` 区切りも含む）が許可リストで満たせるか。
///
/// - `OR` と `/`: **どれか 1 つ**が許可されていれば可（`MIT/Apache-2.0` は OR の意味）
/// - `AND`: **全部**が許可されている必要がある（`AND` の方が `OR` より強く結合する）
/// - `WITH <exception>`: 基本ライセンスで判定する（例外は [`ALLOWED_EXCEPTIONS`] にあるものだけ）
/// - 括弧: 入れ子を解釈する（実データに `(MIT OR Apache-2.0) AND Unicode-3.0` がある）
/// - 大文字小文字と空白の揺れは無視する
///
/// `Ok(false)` は「許可リストに無い識別子がある」、`Err` は「解釈できない表記（空文字など）」。
/// **どちらも拒否**で、人間が判断して許可リストを足すまで CI が落ちる。
#[cfg(test)]
fn license_allowed(expr: &str) -> Result<bool, String> {
    let tokens = tokenize(expr);
    let mut parser = Parser {
        tokens: &tokens,
        pos: 0,
    };
    let allowed = parser.expression()?;
    match parser.peek() {
        None => Ok(allowed),
        Some(token) => Err(format!("余分な `{token}` がある")),
    }
}

/// `expr` をトークンに分ける（空白区切り。括弧と `/` は 1 文字ずつ）。
#[cfg(test)]
fn tokenize(expr: &str) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    for ch in expr.chars() {
        match ch {
            '(' | ')' | '/' => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
                tokens.push(ch.to_string());
            }
            c if c.is_whitespace() => {
                if !current.is_empty() {
                    tokens.push(std::mem::take(&mut current));
                }
            }
            c => current.push(c),
        }
    }
    if !current.is_empty() {
        tokens.push(current);
    }
    tokens
}

/// SPDX 式の読み手（`AND` を `OR` より強く結合する再帰下降）。
#[cfg(test)]
struct Parser<'a> {
    tokens: &'a [String],
    pos: usize,
}

#[cfg(test)]
impl<'a> Parser<'a> {
    /// 次に読むトークン（読まない）。
    fn peek(&self) -> Option<&'a str> {
        let tokens: &'a [String] = self.tokens;
        tokens.get(self.pos).map(String::as_str)
    }

    /// 1 つ読む。
    fn next(&mut self) -> Option<&'a str> {
        let tokens: &'a [String] = self.tokens;
        let token = tokens.get(self.pos)?;
        self.pos += 1;
        Some(token.as_str())
    }

    /// `OR`（と古い `/`）で繋がった項。1 つでも満たせれば可。
    fn expression(&mut self) -> Result<bool, String> {
        let mut allowed = self.term()?;
        loop {
            let is_or = matches!(
                self.peek(),
                Some(token) if token.eq_ignore_ascii_case("OR") || token == "/"
            );
            if !is_or {
                return Ok(allowed);
            }
            self.pos += 1;
            let right = self.term()?;
            allowed = allowed || right;
        }
    }

    /// `AND` で繋がった因子。全部満たせなければ不可。
    fn term(&mut self) -> Result<bool, String> {
        let mut allowed = self.factor()?;
        while matches!(self.peek(), Some(token) if token.eq_ignore_ascii_case("AND")) {
            self.pos += 1;
            let right = self.factor()?;
            allowed = allowed && right;
        }
        Ok(allowed)
    }

    /// `(...)` か、`WITH <exception>` つきのライセンス識別子。
    fn factor(&mut self) -> Result<bool, String> {
        let token = self
            .next()
            .ok_or_else(|| "ライセンス表記が空。宣言が無いクレートは判断できない".to_string())?;
        if token == "(" {
            let allowed = self.expression()?;
            return match self.next() {
                Some(")") => Ok(allowed),
                _ => Err("`(` を閉じる `)` が無い".to_string()),
            };
        }
        if token == ")" {
            return Err("対応する `(` の無い `)` がある".to_string());
        }
        // 演算子の位置がおかしい（`MIT AND` の後ろ・`AND MIT` の前など）
        if token == "/"
            || ["AND", "OR", "WITH"]
                .iter()
                .any(|keyword| token.eq_ignore_ascii_case(keyword))
        {
            return Err(format!("`{token}` の前後にライセンスが無い"));
        }

        let mut allowed = listed_in(&ALLOWED_LICENSES, token);
        if matches!(self.peek(), Some(t) if t.eq_ignore_ascii_case("WITH")) {
            self.pos += 1;
            let exception = self
                .next()
                .ok_or_else(|| format!("`{token} WITH` の後ろに例外名が無い"))?;
            allowed = allowed && listed_in(&ALLOWED_EXCEPTIONS, exception);
        }
        Ok(allowed)
    }
}

/// `list` のどれかに一致するか（大文字小文字は無視する）。
#[cfg(test)]
fn listed_in(list: &[&str], id: &str) -> bool {
    list.iter().any(|listed| listed.eq_ignore_ascii_case(id))
}

/// 許可リスト（[`ALLOWED_LICENSES`]）で満たせないライセンスの依存。
///
/// 返す 1 件は `(クレート名, バージョン, licenses.json のままのライセンス表記)`。
/// ライセンス表記が空のクレートや解釈できない表記も返す（判断材料が無いため）。
#[cfg(test)]
fn disallowed_licenses(packages: &[GeneratedPackage]) -> Vec<(String, String, String)> {
    packages
        .iter()
        .filter(|package| !matches!(license_allowed(&package.license), Ok(true)))
        .map(|package| {
            (
                package.name.clone(),
                package.version.clone(),
                package.license.clone(),
            )
        })
        .collect()
}

/// 拒否された依存を、失敗メッセージ（クレート名・バージョン・そのままのライセンス表記つき）にする。
#[cfg(test)]
fn disallowed_message(disallowed: &[(String, String, String)]) -> String {
    let mut message = String::from(
        "許可リストに無いライセンスの依存がある。許可リストは crates/app/src/views/licenses.rs の \
         `ALLOWED_LICENSES`。追加するなら、MIT 配布のこのアプリと両立する根拠を docs に書いてから足すこと:",
    );
    for (name, version, license) in disallowed {
        // 判断材料（そのままの表記）と、解釈できないときの理由を出す
        let shown = match license_allowed(license) {
            Err(reason) => format!("（{reason}）"),
            Ok(_) => license.clone(),
        };
        message.push_str(&format!("\n  - {name} {version}: {shown}"));
    }
    message
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 1 件目はアプリ本体で、リポジトリの `LICENSE` の全文が付いていること。
    #[test]
    fn lists_the_app_first_with_its_own_license_text() {
        let catalog = &*CATALOG;
        let entry = catalog.entries.first().expect("ライセンスの 1 件目が無い");
        assert_eq!(entry.name, "Thundoku Shelf");
        assert_eq!(entry.kind, Kind::App);
        assert_eq!(entry.license, "MIT");

        let (label, ix) = entry
            .texts
            .first()
            .expect("アプリ本体のライセンス全文が無い");
        assert_eq!(label, "MIT");
        let text = catalog.text(*ix);
        assert!(
            text.contains("MIT License") && text.contains("MegaBlackLabel"),
            "アプリ本体の全文がリポジトリの LICENSE と違う: {text:.80}"
        );
    }

    /// `Cargo.lock` の依存クレートが一覧に入っていること。
    #[test]
    fn lists_the_locked_crates() {
        let catalog = &*CATALOG;
        for expected in ["serde", "tokio", "sqlx", "gpui-kit", "pdfium-render"] {
            assert!(
                catalog
                    .entries
                    .iter()
                    .any(|e| e.name == expected && e.kind == Kind::Crate),
                "依存クレート {expected} が一覧に無い"
            );
        }
        assert!(
            catalog.entries.len() > 500,
            "一覧が少なすぎる（{} 件）: Cargo.lock の生成が失敗している",
            catalog.entries.len()
        );
    }

    /// どの行も「ライセンス表記」か「全文」のどちらかを持ち、全文が空でないこと。
    #[test]
    fn every_entry_has_license_information() {
        let catalog = &*CATALOG;
        for entry in &catalog.entries {
            assert!(!entry.name.is_empty(), "名前の無い行がある");
            assert!(
                !entry.license.is_empty() || !entry.texts.is_empty(),
                "{} {} にライセンス情報が無い",
                entry.name,
                entry.version
            );
            for (label, ix) in &entry.texts {
                assert!(!label.is_empty(), "{} に名前の無い全文がある", entry.name);
                assert!(
                    !catalog.text(*ix).trim().is_empty(),
                    "{} の {label} の全文が空",
                    entry.name
                );
            }
        }
    }

    /// 同梱アセット（アイコン・PDF 表示）の帰属表示があること。
    #[test]
    fn attributes_the_bundled_assets() {
        let catalog = &*CATALOG;
        let lucide = catalog
            .entries
            .iter()
            .find(|e| e.name == "Lucide")
            .expect("Lucide（アイコン）の表示が無い");
        assert_eq!(lucide.kind, Kind::Bundled);
        assert_eq!(lucide.license, "ISC");
        let text = catalog.text(lucide.texts[0].1);
        assert!(
            text.contains("Permission to use, copy, modify"),
            "Lucide の ISC 全文が無い: {text:.80}"
        );

        let pdfium = catalog
            .entries
            .iter()
            .find(|e| e.name == "PDFium")
            .expect("PDFium（PDF 表示）の表示が無い");
        assert_eq!(pdfium.kind, Kind::Bundled);
        assert!(
            pdfium.license.contains("BSD-3-Clause"),
            "PDFium のライセンス表記が違う: {}",
            pdfium.license
        );
        let text = catalog.text(pdfium.texts[0].1);
        assert!(
            text.contains("Copyright 2014 The PDFium Authors"),
            "PDFium の BSD 全文が無い: {text:.80}"
        );
    }

    /// 同梱した一覧が `Cargo.lock` と食い違っていないこと。
    ///
    /// 一覧は生成物なので、依存を足したのに `mise run licenses` を忘れると**古い一覧のまま
    /// 出荷されてしまう**（ライセンス表示が実態とずれる）。ここで名前とバージョンの集合を
    /// 突き合わせて、作り忘れと消し忘れの両方を見る。
    #[test]
    fn licenses_json_matches_cargo_lock() {
        const LOCK: &str = include_str!("../../../../Cargo.lock");

        #[derive(serde::Deserialize)]
        struct Lockfile {
            #[serde(default)]
            package: Vec<Locked>,
        }
        #[derive(serde::Deserialize)]
        struct Locked {
            name: String,
            version: String,
            #[serde(default)]
            source: Option<String>,
        }

        let lock: Lockfile = toml::from_str(LOCK).expect("Cargo.lock を解釈できない");
        // ソースの無いパッケージ = このワークスペース自身のクレート（アプリ本体として別に出す）
        let expected: std::collections::BTreeSet<(String, String)> = lock
            .package
            .into_iter()
            .filter(|package| package.source.is_some())
            .map(|package| (package.name, package.version))
            .collect();
        let listed: std::collections::BTreeSet<(String, String)> = CATALOG
            .entries
            .iter()
            .filter(|entry| entry.kind == Kind::Crate)
            .map(|entry| (entry.name.clone(), entry.version.clone()))
            .collect();

        let missing: Vec<_> = expected.difference(&listed).collect();
        assert!(
            missing.is_empty(),
            "Cargo.lock にあるのに一覧に無い: {missing:?}（`mise run licenses` で作り直すこと）"
        );
        let extra: Vec<_> = listed.difference(&expected).collect();
        assert!(
            extra.is_empty(),
            "一覧にあるのに Cargo.lock に無い: {extra:?}（`mise run licenses` で作り直すこと）"
        );
    }

    /// テスト用の依存 1 件（`licenses.json` の 1 件と同じ形）。
    fn package(name: &str, version: &str, license: &str) -> GeneratedPackage {
        GeneratedPackage {
            name: name.to_string(),
            version: version.to_string(),
            license: license.to_string(),
            repository: String::new(),
            texts: Vec::new(),
        }
    }

    /// `licenses.json` の依存（実データ）を、判定に使う形で読み直す。
    fn generated_packages() -> Vec<GeneratedPackage> {
        let generated: Generated =
            serde_json::from_str(GENERATED).expect("licenses.json を解釈できない");
        // 生成側の綴りが変わっても `packages` は `serde(default)` で空になるので、
        // 空のまま判定が空回りしないよう件数を見る。
        assert!(
            generated.packages.len() > 500,
            "licenses.json の packages が {} 件しか無い（判定が空回りしている）",
            generated.packages.len()
        );
        generated.packages
    }

    /// 許可リストの 1 件は識別子そのものであること（空白・括弧・`/` を含むとトークンと一致しない）。
    #[test]
    fn allowed_licenses_are_plain_identifiers() {
        for license in ALLOWED_LICENSES {
            assert!(!license.is_empty(), "許可リストに空の項目がある");
            assert!(
                !license.contains(|c: char| c.is_whitespace() || c == '(' || c == ')' || c == '/'),
                "許可リストの {license} は識別子ではない（式を書いても一致しない）"
            );
        }
    }

    /// 式の解釈: `OR` はどれか 1 つ、`AND` は全部が許可リストにあることを要求すること。
    #[test]
    fn license_expression_needs_one_allowed_for_or_and_all_for_and() {
        // 許可リストにあるものは通り、無いもの（GPL・未知のライセンス）は通さない
        assert_eq!(license_allowed("MIT"), Ok(true));
        assert_eq!(license_allowed("GPL-3.0-only"), Ok(false));
        assert_eq!(
            license_allowed("WTFPL"),
            Ok(false),
            "未知のライセンスは通さず人間に判断させる"
        );

        // OR はどれか 1 つ、AND は全部
        assert_eq!(license_allowed("MIT OR GPL-3.0-only"), Ok(true));
        assert_eq!(license_allowed("MIT AND GPL-3.0-only"), Ok(false));
        assert_eq!(
            license_allowed("MIT OR Apache-2.0 OR LGPL-2.1-or-later"),
            Ok(true)
        );
    }

    /// 古い `/` 区切りと、大文字小文字・空白の揺れを吸収すること。
    #[test]
    fn license_expression_accepts_legacy_slashes_and_spacing() {
        // `/` は OR と同じ意味（実データに `MIT/Apache-2.0` や `Unlicense/MIT` がある）
        assert_eq!(license_allowed("MIT/Apache-2.0"), Ok(true));
        assert_eq!(license_allowed("MIT / Apache-2.0"), Ok(true));
        assert_eq!(license_allowed("Unlicense/MIT"), Ok(true));
        assert_eq!(
            license_allowed("MIT/GPL-3.0-only"),
            Ok(true),
            "どちらかを選べるなら可"
        );
        assert_eq!(
            license_allowed("  mit  Or  apache-2.0  "),
            Ok(true),
            "大文字小文字と空白の揺れを吸収する"
        );
    }

    /// `WITH <exception>` は基本ライセンスで判定すること。
    #[test]
    fn license_expression_judges_with_exceptions_by_the_base_license() {
        assert_eq!(license_allowed("Apache-2.0 WITH LLVM-exception"), Ok(true));
        assert_eq!(
            license_allowed("Apache-2.0 WITH LLVM-exception OR Apache-2.0 OR MIT"),
            Ok(true)
        );
        assert_eq!(
            license_allowed("GPL-3.0-only WITH LLVM-exception"),
            Ok(false),
            "基本ライセンスが許可リストに無いものは例外が付いても通さない"
        );
        assert_eq!(
            license_allowed("Apache-2.0 WITH Fake-exception"),
            Ok(false),
            "知らない例外は人間に判断させる"
        );
    }

    /// 括弧を入れ子で解釈すること（`AND` の方が `OR` より強く結合する）。
    #[test]
    fn license_expression_parses_parentheses() {
        assert_eq!(
            license_allowed("(MIT OR Apache-2.0) AND Unicode-3.0"),
            Ok(true)
        );
        assert_eq!(
            license_allowed("(MIT OR Apache-2.0) AND GPL-3.0-only"),
            Ok(false),
            "括弧の中に選べるものがあっても、AND の相方は全部許可が要る"
        );
        assert_eq!(
            license_allowed("MIT OR (Apache-2.0 AND GPL-3.0-only)"),
            Ok(true)
        );
        assert_eq!(
            license_allowed("MIT AND (Apache-2.0 OR GPL-3.0-only)"),
            Ok(true)
        );

        // 実データにも括弧つきの式がある（例: `(MIT OR Apache-2.0) AND Unicode-3.0`）。
        // 括弧を識別子の一部として扱う実装だと、ここが解釈できずに落ちる。
        let mut parenthesized: Vec<String> = generated_packages()
            .into_iter()
            .map(|package| package.license)
            .filter(|license| license.contains('('))
            .collect();
        parenthesized.sort();
        parenthesized.dedup();
        assert!(
            !parenthesized.is_empty(),
            "実データから括弧つきの式が消えた。括弧の解釈が要るか確認すること"
        );
        for license in &parenthesized {
            assert!(
                license_allowed(license).is_ok(),
                "括弧つきの式を解釈できない: {license}"
            );
        }
    }

    /// 解釈できない表記（空・演算子の欠落・対応しない括弧）は拒否すること。
    #[test]
    fn unparseable_license_expressions_are_rejected() {
        for broken in [
            "",
            "   ",
            "MIT AND",
            "AND MIT",
            "(MIT",
            "MIT)",
            "MIT OR OR Apache-2.0",
        ] {
            assert!(
                license_allowed(broken).is_err(),
                "{broken:?} を解釈してしまった（判断できないものは拒否する）"
            );
        }
    }

    /// 実データ（`licenses.json` = `Cargo.lock` の依存）に、許可リスト外のライセンスが 1 件も無いこと。
    ///
    /// これが CI（`cargo test --workspace`）で回ることで、依存を 1 本足して GPL/AGPL が
    /// 混入したときに気づける（一覧の作り忘れは `licenses_json_matches_cargo_lock` が見張っている
    /// ので、この判定は常に今の `Cargo.lock` に対して行われる）。
    #[test]
    fn licenses_json_has_no_disallowed_license() {
        let disallowed = disallowed_licenses(&generated_packages());
        assert!(disallowed.is_empty(), "{}", disallowed_message(&disallowed));
    }

    /// 拒否するときは、クレート名・バージョン・そのままのライセンス表記と、
    /// 許可リスト（`ALLOWED_LICENSES`）の場所と追加手順を出すこと。
    #[test]
    fn disallowed_licenses_are_reported_with_the_allow_list_location() {
        let disallowed = disallowed_licenses(&[
            package("wtfpl-crate", "9.9.9", "WTFPL"),
            package("no-license", "1.2.3", ""),
            package("gpl-crate", "0.1.0", "MIT AND GPL-3.0-only"),
        ]);
        assert_eq!(
            disallowed,
            vec![
                (
                    "wtfpl-crate".to_string(),
                    "9.9.9".to_string(),
                    "WTFPL".to_string()
                ),
                ("no-license".to_string(), "1.2.3".to_string(), String::new()),
                (
                    "gpl-crate".to_string(),
                    "0.1.0".to_string(),
                    "MIT AND GPL-3.0-only".to_string()
                ),
            ]
        );

        let message = disallowed_message(&disallowed);
        for expected in [
            "wtfpl-crate 9.9.9: WTFPL",
            "no-license 1.2.3",
            "ライセンス表記が空",
            "ALLOWED_LICENSES",
            "docs",
        ] {
            assert!(
                message.contains(expected),
                "失敗メッセージに {expected:?} が無い:\n{message}"
            );
        }
    }
}
