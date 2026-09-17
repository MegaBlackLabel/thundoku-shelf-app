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
}
