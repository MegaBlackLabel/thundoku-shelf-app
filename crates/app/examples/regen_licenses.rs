//! ライセンス一覧（`assets/third-party/licenses.json`）を生成する。
//!
//! アプリは生成済みのファイルを `include_str!` で埋め込む（`src/views/licenses.rs`）。
//! 依存クレートを増減・更新したら、これを実行して差分をコミットする:
//!
//! ```sh
//! mise run licenses
//! # = cargo run -p thundoku-shelf --example regen_licenses
//! ```
//!
//! 生成の材料は `Cargo.lock` と、ローカルの cargo キャッシュに展開されたクレートの
//! `Cargo.toml` / 同梱ライセンスファイル。全てのクレートが見つからない場合は
//! **書き換えずに失敗する**（不完全な一覧をコミットしないため）。
//! `cargo metadata --format-version 1 > /dev/null` を一度実行すると
//! 全プラットフォームぶんのソースが展開される。
//!
//! 一覧が `Cargo.lock` と食い違っていないかは `src/views/licenses.rs` のテスト
//! `licenses_json_matches_cargo_lock` が CI で見張る。

use std::collections::HashMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// ライセンス全文として拾うファイル名の接頭辞（小文字。`LICENSE-MIT` なども拾う）。
const LICENSE_FILE_PREFIXES: [&str; 6] = [
    "license",
    "licence",
    "copying",
    "copyright",
    "notice",
    "unlicense",
];

/// これを超えるファイルはライセンス全文とみなさない（同梱物の誤検出よけ）。
const MAX_LICENSE_BYTES: u64 = 256 * 1024;

/// `Cargo.lock`。
#[derive(Deserialize, Default)]
struct Lockfile {
    #[serde(default)]
    package: Vec<Locked>,
}

/// `Cargo.lock` の 1 パッケージ。
#[derive(Deserialize)]
struct Locked {
    name: String,
    version: String,
    /// 無い場合はこのワークスペース自身のクレート（アプリ本体として別に出す）。
    #[serde(default)]
    source: Option<String>,
}

/// クレートの `Cargo.toml`（使う項目だけ）。
#[derive(Deserialize, Default)]
struct Manifest {
    #[serde(default)]
    package: Package,
    #[serde(default)]
    workspace: Option<WorkspaceSection>,
}

#[derive(Deserialize, Default)]
struct Package {
    name: Option<String>,
    version: Option<String>,
    /// `"MIT"` のような表記か、`{ workspace = true }`（ワークスペースの値を使う）。
    #[serde(default)]
    license: Option<LicenseField>,
    #[serde(default, rename = "license-file")]
    license_file: Option<String>,
    #[serde(default)]
    repository: Option<String>,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum LicenseField {
    Spdx(String),
    /// `license = { workspace = true }`（値はワークスペースから継承する）。
    Inherited {
        #[serde(default, rename = "workspace")]
        _workspace: bool,
    },
}

#[derive(Deserialize, Default)]
struct WorkspaceSection {
    #[serde(default)]
    package: Package,
    #[serde(default)]
    members: Vec<String>,
}

/// `licenses.json`（`src/views/licenses.rs` がデシリアライズする）。
#[derive(Serialize, Default)]
struct OutCatalog {
    /// 全文。同じ内容は 1 つだけ持つ（クレートごとの重複で数 MB になるのを避ける）。
    texts: Vec<String>,
    packages: Vec<OutPackage>,
}

#[derive(Serialize)]
struct OutPackage {
    name: String,
    version: String,
    /// SPDX のライセンス表記（クレートの `license`）。
    license: String,
    repository: String,
    /// (全文の見出し, `texts` の添字)。全文が同梱されていないクレートは空。
    texts: Vec<(String, usize)>,
}

fn main() {
    let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let root = manifest_dir.join("..").join("..");
    let lock_path = root.join("Cargo.lock");
    let out_path = manifest_dir
        .join("assets")
        .join("third-party")
        .join("licenses.json");

    let raw = fs::read_to_string(&lock_path)
        .unwrap_or_else(|e| panic!("{} が読めない: {e}", lock_path.display()));
    let lock: Lockfile = toml::from_str(&raw)
        .unwrap_or_else(|e| panic!("{} を解釈できない: {e}", lock_path.display()));

    let home = CargoHome::discover();
    let mut texts: Vec<String> = Vec::new();
    let mut text_ids: HashMap<String, usize> = HashMap::new();
    let mut packages: Vec<OutPackage> = Vec::new();
    let mut missing: Vec<String> = Vec::new();

    for locked in &lock.package {
        // ソースの無いパッケージはこのワークスペース自身（アプリ本体は `licenses.rs` が
        // リポジトリの LICENSE から別に出す）なので、第三者ソフトウェアだけを集める。
        let Some(source) = locked.source.as_deref() else {
            continue;
        };
        let Some(dir) = home.find(source, &locked.name, &locked.version) else {
            missing.push(format!("{} {}", locked.name, locked.version));
            continue;
        };

        let manifest = read_manifest(&dir.join("Cargo.toml"));
        let license = license_expression(&manifest, &dir);
        let mut refs = Vec::new();
        for (label, text) in collect_license_texts(&dir, &manifest) {
            let id = match text_ids.get(&text) {
                Some(&id) => id,
                None => {
                    let id = texts.len();
                    texts.push(text.clone());
                    text_ids.insert(text, id);
                    id
                }
            };
            refs.push((label, id));
        }

        packages.push(OutPackage {
            name: locked.name.clone(),
            version: locked.version.clone(),
            license,
            repository: manifest.package.repository.clone().unwrap_or_default(),
            texts: refs,
        });
    }

    if !missing.is_empty() {
        // 不完全な一覧で上書きしない（差分をレビューできなくなるため）
        panic!(
            "{} 件のクレートのソースが見つからないため書き換えない:\n  {}\n\
             `cargo metadata --format-version 1 > /dev/null` を実行してから、もう一度どうぞ",
            missing.len(),
            missing.join("\n  ")
        );
    }

    packages.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.version.cmp(&b.version))
    });

    let catalog = OutCatalog { texts, packages };
    let json = serde_json::to_string(&catalog).expect("licenses.json にできない");
    fs::write(&out_path, json).unwrap_or_else(|e| panic!("{} を書けない: {e}", out_path.display()));
    println!(
        "{} を更新した（{} 件・全文 {} 件）",
        out_path.display(),
        catalog.packages.len(),
        catalog.texts.len()
    );
}

/// ローカルの cargo キャッシュ（依存のソースが展開されている場所）。
struct CargoHome {
    /// `registry/src/<index>/` の一覧。
    registries: Vec<PathBuf>,
    /// `git/checkouts/`。
    git_checkouts: PathBuf,
}

impl CargoHome {
    fn discover() -> Self {
        let home = env::var_os("CARGO_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                let user = env::var_os("HOME")
                    .or_else(|| env::var_os("USERPROFILE"))
                    .unwrap_or_default();
                PathBuf::from(user).join(".cargo")
            });
        let registries = fs::read_dir(home.join("registry").join("src"))
            .map(|entries| {
                entries
                    .flatten()
                    .map(|entry| entry.path())
                    .filter(|path| path.is_dir())
                    .collect()
            })
            .unwrap_or_default();
        Self {
            registries,
            git_checkouts: home.join("git").join("checkouts"),
        }
    }

    /// `Cargo.lock` の `source` から、パッケージのソースディレクトリを探す。
    fn find(&self, source: &str, name: &str, version: &str) -> Option<PathBuf> {
        if source.starts_with("git+") {
            self.find_git(source, name, version)
        } else if source.starts_with("registry+") {
            self.find_registry(name, version)
        } else {
            None
        }
    }

    /// crates.io のクレートは `registry/src/<index>/<name>-<version>/` に展開されている。
    fn find_registry(&self, name: &str, version: &str) -> Option<PathBuf> {
        let dir_name = format!("{name}-{version}");
        self.registries
            .iter()
            .map(|index| index.join(&dir_name))
            .find(|dir| dir.join("Cargo.toml").is_file())
    }

    /// git 依存は `git/checkouts/<repo>-<hash>/<rev>/` にチェックアウトされている。
    fn find_git(&self, source: &str, name: &str, version: &str) -> Option<PathBuf> {
        let (url, rev) = source.split_once('#')?;
        let url = url.strip_prefix("git+")?.split('?').next()?;
        let repo = url.rsplit(['/', ':']).next()?;
        // checkouts のディレクトリ名は短縮リビジョン
        let rev = &rev[..rev.len().min(7)];

        for checkout in fs::read_dir(&self.git_checkouts).ok()?.flatten() {
            let checkout_dir = checkout.path();
            if !checkout
                .file_name()
                .to_string_lossy()
                .starts_with(&format!("{repo}-"))
            {
                continue;
            }
            let Ok(revs) = fs::read_dir(&checkout_dir) else {
                continue;
            };
            for rev_dir in revs.flatten() {
                if !rev_dir.file_name().to_string_lossy().starts_with(rev) {
                    continue;
                }
                // 目的のクレートはそのリポジトリのワークスペースのメンバー
                if let Some(dir) = find_workspace_member(&rev_dir.path(), name, version) {
                    return Some(dir);
                }
            }
        }
        None
    }
}

/// ワークスペースの `members` から、名前とバージョンが一致するメンバーを探す。
fn find_workspace_member(root: &Path, name: &str, version: &str) -> Option<PathBuf> {
    let workspace = read_manifest(&root.join("Cargo.toml")).workspace?;
    for member in &workspace.members {
        // `crates/*` のようなワイルドカードは 1 階層だけ展開する
        let candidates = match member.rsplit_once("/*") {
            Some((parent, _)) => match fs::read_dir(root.join(parent)) {
                Ok(entries) => entries.flatten().map(|entry| entry.path()).collect(),
                Err(_) => continue,
            },
            None => vec![root.join(member)],
        };
        for dir in candidates {
            let manifest = read_manifest(&dir.join("Cargo.toml"));
            if manifest.package.name.as_deref() == Some(name)
                && manifest.package.version.as_deref() == Some(version)
            {
                return Some(dir);
            }
        }
    }
    None
}

fn read_manifest(path: &Path) -> Manifest {
    fs::read_to_string(path)
        .ok()
        .and_then(|raw| toml::from_str(&raw).ok())
        .unwrap_or_default()
}

/// クレートの `license` 表記。`license.workspace = true` はワークスペースの値を使う。
fn license_expression(manifest: &Manifest, dir: &Path) -> String {
    match manifest.package.license.as_ref() {
        Some(LicenseField::Spdx(spdx)) => spdx.clone(),
        Some(LicenseField::Inherited { .. }) => workspace_license(dir).unwrap_or_default(),
        None => String::new(),
    }
}

/// `license.workspace = true` のときの継承元（`[workspace.package] license`）。
fn workspace_license(dir: &Path) -> Option<String> {
    let root = workspace_root(dir)?;
    match read_manifest(&root.join("Cargo.toml"))
        .workspace?
        .package
        .license
    {
        Some(LicenseField::Spdx(spdx)) => Some(spdx),
        _ => None,
    }
}

/// 親ディレクトリをたどって、`[workspace]` を持つ `Cargo.toml`（git 依存のリポジトリ直下）を探す。
fn workspace_root(dir: &Path) -> Option<PathBuf> {
    let mut current = dir.parent();
    for _ in 0..8 {
        let parent = current?;
        let manifest_path = parent.join("Cargo.toml");
        if manifest_path.is_file() && read_manifest(&manifest_path).workspace.is_some() {
            return Some(parent.to_path_buf());
        }
        current = parent.parent();
    }
    None
}

/// クレートに同梱されているライセンス全文を (見出し, 全文) で集める。
///
/// ワークスペースのメンバー（git 依存）はリポジトリ直下に `LICENSE-APACHE` を
/// 置くことが多いので、クレートのディレクトリに無ければワークスペース直下も見る。
fn collect_license_texts(dir: &Path, manifest: &Manifest) -> Vec<(String, String)> {
    // (置かれているディレクトリ, ファイル名)。`license-file` は `../LICENSE` のような
    // 相対パスもありうる。
    let mut candidates: Vec<(PathBuf, String)> = Vec::new();
    if let Some(file) = manifest.package.license_file.as_deref() {
        candidates.push((dir.to_path_buf(), file.to_string()));
    }

    let mut bases = vec![dir.to_path_buf()];
    if let Some(root) = workspace_root(dir).filter(|root| root != dir) {
        bases.push(root);
    }
    for base in bases {
        let Ok(entries) = fs::read_dir(&base) else {
            continue;
        };
        let mut found: Vec<String> = entries
            .flatten()
            .filter(|entry| entry.file_type().map(|t| t.is_file()).unwrap_or(false))
            .map(|entry| entry.file_name().to_string_lossy().into_owned())
            .filter(|name| is_license_file(name))
            .collect();
        found.sort();
        for name in found {
            if !candidates.iter().any(|(_, file)| *file == name) {
                candidates.push((base.clone(), name));
            }
        }
    }

    let license = license_expression(manifest, dir);
    let mut texts = Vec::new();
    for (base, file) in candidates {
        let path = base.join(&file);
        let Ok(meta) = fs::metadata(&path) else {
            continue;
        };
        if meta.len() > MAX_LICENSE_BYTES {
            continue;
        }
        let Ok(bytes) = fs::read(&path) else {
            continue;
        };
        let text = normalize_text(&String::from_utf8_lossy(&bytes));
        if text.is_empty() {
            continue;
        }
        texts.push((label_for(&file, &license), text));
    }
    texts
}

/// ライセンス全文とみなすファイル名か。
fn is_license_file(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    LICENSE_FILE_PREFIXES
        .iter()
        .any(|prefix| lower.starts_with(prefix))
}

/// 全文の見出し。ファイル名からライセンスの種類が分かればそれを使い、
/// 分からなければクレートが宣言している表記を使う。
fn label_for(file_name: &str, license: &str) -> String {
    let lower = file_name.to_ascii_lowercase();
    for (needle, label) in [
        ("notice", "NOTICE"),
        ("copyright", "Copyright"),
        ("apache", "Apache-2.0"),
        ("mit", "MIT"),
        ("lucide", "ISC"),
        ("isc", "ISC"),
        ("zlib", "Zlib"),
        ("cc0", "CC0-1.0"),
        ("unlicense", "Unlicense"),
        ("bsd-3", "BSD-3-Clause"),
        ("bsd-2", "BSD-2-Clause"),
        ("0bsd", "0BSD"),
        ("bsl", "BSL-1.0"),
        ("mpl", "MPL-2.0"),
        ("agpl", "AGPL-3.0"),
        ("lgpl", "LGPL"),
        ("gpl", "GPL"),
        ("epl", "EPL"),
        ("wtfpl", "WTFPL"),
    ] {
        if lower.contains(needle) {
            return label.to_string();
        }
    }
    if license.is_empty() {
        file_name.to_string()
    } else {
        license.to_string()
    }
}

/// 改行コードをそろえる（クレートごとの差で同じ全文が別物として重複するのを避ける）。
fn normalize_text(text: &str) -> String {
    text.trim_start_matches('\u{feff}')
        .replace("\r\n", "\n")
        .trim_end()
        .to_string()
}
