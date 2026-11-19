//! cargo-about の結果の検査。どれも見つけた問題をすべて返す (1 つ目で止めない)。

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result};
use serde::Deserialize;

use crate::about::{Fallback, Metadata};
use crate::allowlist::Allowlist;

/// crates.io のクレートの `source`。
pub const CRATES_IO: &str = "registry+https://github.com/rust-lang/crates.io-index";

/// cargo-about の JSON (ひな形に渡すデータ) のうち、検査に使うところ。
#[derive(Deserialize)]
pub struct Report {
    /// 本文ごとの項。同じライセンスでも本文 (著作権表示など) が違えば別の項になる。
    pub licenses: Vec<LicenseText>,
    /// 一覧に載せるクレート (自分のクレートは除く)。
    pub crates: Vec<CrateLicense>,
}

#[derive(Deserialize)]
pub struct LicenseText {
    /// SPDX の識別子。
    pub id: String,
    pub text: String,
    pub used_by: Vec<UsedBy>,
}

#[derive(Deserialize)]
pub struct UsedBy {
    #[serde(rename = "crate")]
    pub krate: Package,
}

#[derive(Deserialize)]
pub struct CrateLicense {
    pub package: Package,
}

/// cargo metadata のパッケージのうち、使うところ。
#[derive(Deserialize)]
pub struct Package {
    pub id: String,
    pub name: String,
    pub version: String,
    pub source: Option<String>,
    /// Cargo.toml の license (cargo-about が `/` を ` OR ` に直したもの)。
    pub license: Option<String>,
    pub repository: Option<String>,
    pub manifest_path: PathBuf,
}

impl Package {
    fn label(&self) -> String {
        format!("{} {}", self.name, self.version)
    }
}

/// about.toml の clarify の files に書いた、クレートごとのパッケージの中のパス。
pub fn clarified_files(about_toml: &str) -> Result<BTreeMap<String, BTreeSet<String>>> {
    let table: toml::Table = about_toml
        .parse()
        .context("about.toml の書き方が正しくありません")?;
    let mut files = BTreeMap::new();
    for (name, value) in &table {
        let Some(list) = value
            .get("clarify")
            .and_then(|clarify| clarify.get("files"))
            .and_then(toml::Value::as_array)
        else {
            continue;
        };
        let paths: BTreeSet<String> = list
            .iter()
            .filter_map(|file| file.get("path").and_then(toml::Value::as_str))
            .map(|path| path.replace('\\', "/"))
            .collect();
        files.insert(name.clone(), paths);
    }
    Ok(files)
}

/// 標準の文面に置き換わったクレートが allowlist.toml の standard-text にあるか。使われていない項も問題にする。
pub fn standard_texts(fallbacks: &[Fallback], allowlist: &Allowlist) -> Vec<String> {
    let mut problems = Vec::new();
    for fallback in fallbacks {
        if !allowlist.allows_standard_text(&fallback.name, &fallback.license) {
            problems.push(format!(
                "{} {}: {} の本文が見つからず、SPDX の標準の文面に置き換わりました。about.toml の clarify で原文を\
                 指定するか、原文がないことを確かめて allowlist.toml の standard-text に足してください",
                fallback.name, fallback.version, fallback.license
            ));
        }
    }
    for entry in &allowlist.standard_text {
        for name in &entry.crates {
            let used = fallbacks
                .iter()
                .any(|f| &f.name == name && f.license == entry.license);
            if !used {
                problems.push(format!(
                    "allowlist.toml の standard-text の {name} ({}) は、もう標準の文面に置き換わっていません。消してください",
                    entry.license
                ));
            }
        }
    }
    problems
}

/// 一覧に載せるライセンスが、各クレートの Cargo.toml の式を満たすか。about.toml の clarify は元の式の代わりに
/// 使われるので、上流が式を変えたのに古い clarify で隠していないかをここで確かめる。
pub fn expressions(report: &Report) -> Vec<String> {
    let mut chosen: HashMap<&str, BTreeSet<&str>> = HashMap::new();
    for license in &report.licenses {
        for used in &license.used_by {
            chosen
                .entry(used.krate.id.as_str())
                .or_default()
                .insert(license.id.as_str());
        }
    }
    let mut problems = Vec::new();
    for krate in &report.crates {
        let package = &krate.package;
        let Some(ids) = chosen.get(package.id.as_str()) else {
            problems.push(format!(
                "{}: 一覧に本文が 1 つもありません",
                package.label()
            ));
            continue;
        };
        // license のないクレートは、cargo-about がファイルから式を作る (--fail なので、ファイルもなければ止まる)
        let Some(text) = &package.license else {
            continue;
        };
        match spdx::Expression::parse_mode(text, spdx::ParseMode::LAX) {
            Ok(expression) => {
                let satisfied = expression
                    .evaluate(|req| req.license.id().is_some_and(|id| ids.contains(id.name)));
                if !satisfied {
                    problems.push(format!(
                        "{}: 一覧に載せるライセンス ({}) が Cargo.toml の式 ({text}) を満たしません。\
                         about.toml の clarify が古くないか確かめてください",
                        package.label(),
                        ids.iter().copied().collect::<Vec<_>>().join(", ")
                    ));
                }
            }
            Err(error) => problems.push(format!(
                "{}: Cargo.toml のライセンスの式 ({text}) を読めません: {error}",
                package.label()
            )),
        }
    }
    problems
}

/// cargo-about は publish = false のクレートを一覧から外す (自分のクレートを外すため)。第三者のクレートが
/// 外れないよう、publish = false がワークスペースのクレートだけかを確かめる。
pub fn unpublished(metadata: &Metadata) -> Vec<String> {
    let members: BTreeSet<&str> = metadata
        .workspace_members
        .iter()
        .map(String::as_str)
        .collect();
    metadata
        .packages
        .iter()
        .filter(|package| {
            package.publish.as_ref().is_some_and(Vec::is_empty)
                && !members.contains(package.id.as_str())
        })
        .map(|package| {
            format!(
                "{} {}: ワークスペースの外のクレートが publish = false で、一覧から外れてしまいます",
                package.name, package.version
            )
        })
        .collect()
}

/// クレートごとにソースの入手先 (crates.io のページか repository) を示せるか。
pub fn links(report: &Report) -> Vec<String> {
    report
        .crates
        .iter()
        .map(|krate| &krate.package)
        .filter(|package| {
            package.source.as_deref() != Some(CRATES_IO)
                && package.repository.as_deref().is_none_or(str::is_empty)
        })
        .map(|package| {
            format!(
                "{}: crates.io のクレートでなく repository もないので、ソースの入手先を示せません",
                package.label()
            )
        })
        .collect()
}

/// ひな形は本文を 4 つのバッククォートの囲みに入れるので、本文に 4 つ以上で始まる行があると囲みが壊れる。
pub fn fences(report: &Report) -> Vec<String> {
    report
        .licenses
        .iter()
        .filter(|license| license.text.lines().any(breaks_fence))
        .map(|license| {
            let crates: Vec<String> = license.used_by.iter().map(|u| u.krate.label()).collect();
            format!(
                "{} の本文 ({}) に ```` で始まる行があり、一覧の囲みが壊れます。about.hbs の囲みを長くしてください",
                license.id,
                crates.join(", ")
            )
        })
        .collect()
}

fn breaks_fence(line: &str) -> bool {
    let trimmed = line.trim_start_matches(' ');
    line.len() - trimmed.len() <= 3 && trimmed.starts_with("````")
}

/// 一覧に入らない、クレートの中のライセンスのファイル (直下の NOTICE と、cargo-about が見ない下の階層の
/// もの) が、about.toml の clarify で入れてあるか allowlist.toml の unlisted-file にあるか。
/// unlisted-file のうち、どの版のクレートにもないパスも問題にする。
pub fn unlisted_files(
    report: &Report,
    clarified: &BTreeMap<String, BTreeSet<String>>,
    allowlist: &Allowlist,
) -> Result<Vec<String>> {
    let mut problems = Vec::new();
    // クレートの名前ごとの、どれかの版にあるライセンスのファイル
    let mut found: BTreeMap<&str, BTreeSet<String>> = BTreeMap::new();
    let mut seen = BTreeSet::new();
    for krate in &report.crates {
        let package = &krate.package;
        if !seen.insert(&package.manifest_path) {
            continue;
        }
        let root = package
            .manifest_path
            .parent()
            .with_context(|| format!("{} のフォルダが分かりません", package.label()))?;
        let files = license_files(root)?;
        let empty = BTreeSet::new();
        let included = clarified.get(&package.name).unwrap_or(&empty);
        for path in &files {
            if !included.contains(path) && !allowlist.allows_unlisted(&package.name, path) {
                problems.push(format!(
                    "{}: ライセンスのファイル {path} が一覧に入りません。実行ファイルに入るものなら about.toml の \
                     clarify の files に足し (選ばなかった方のライセンスの本文も、そのライセンスで書く)、入らないなら \
                     理由を allowlist.toml の unlisted-file に書いてください",
                    package.label()
                ));
            }
        }
        found
            .entry(package.name.as_str())
            .or_default()
            .extend(files);
    }
    for entry in &allowlist.unlisted_file {
        for path in &entry.paths {
            let exists = found
                .get(entry.krate.as_str())
                .is_some_and(|paths| paths.contains(path));
            if !exists {
                problems.push(format!(
                    "allowlist.toml の unlisted-file の {} の {path} は、もうありません。消してください",
                    entry.krate
                ));
            }
        }
    }
    Ok(problems)
}

/// クレートのフォルダの中の、ライセンスらしいファイル (区切りが `/` の相対パス)。統合テスト・ベンチマーク・
/// 例 (直下の tests・benches・examples) は依存先の実行ファイルに入らないので見ない。
fn license_files(root: &Path) -> Result<BTreeSet<String>> {
    let mut found = BTreeSet::new();
    let mut stack = vec![(root.to_path_buf(), String::new())];
    while let Some((dir, prefix)) = stack.pop() {
        let entries =
            fs::read_dir(&dir).with_context(|| format!("{} を読めません", dir.display()))?;
        for entry in entries {
            let entry = entry.with_context(|| format!("{} を読めません", dir.display()))?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let file_type = entry.file_type()?;
            let relative = format!("{prefix}{name}");
            if file_type.is_dir() {
                let skip = matches!(name.as_str(), ".git" | "target")
                    || (prefix.is_empty()
                        && matches!(name.as_str(), "tests" | "benches" | "examples"));
                if !skip {
                    stack.push((entry.path(), format!("{relative}/")));
                }
            } else if file_type.is_file() && is_license_file(&relative) {
                found.insert(relative);
            }
        }
    }
    Ok(found)
}

/// ソースや画像は名前が似ていても除く (copying.rs など)。
const NOT_LICENSE_EXTENSIONS: &[&str] = &[
    ".rs", ".c", ".h", ".cc", ".cpp", ".hpp", ".s", ".asm", ".py", ".js", ".ts", ".go", ".java",
    ".svg", ".png", ".json", ".toml", ".html", ".css",
];

/// 直下の NOTICE (cargo-about は本文でないので入れない) と、下の階層のライセンスのファイル。
/// 直下のライセンスのファイルは cargo-about が見る。
fn is_license_file(relative: &str) -> bool {
    let (dirs, name) = match relative.rsplit_once('/') {
        Some((dirs, name)) => (Some(dirs), name),
        None => (None, relative),
    };
    let lower = name.to_ascii_lowercase();
    if NOT_LICENSE_EXTENSIONS
        .iter()
        .any(|ext| lower.ends_with(ext))
    {
        return false;
    }
    if lower.starts_with("notice") {
        return true;
    }
    let Some(dirs) = dirs else {
        return false;
    };
    // REUSE の LICENSES フォルダなど
    let in_license_dir = dirs.split('/').any(|dir| {
        matches!(
            dir.to_ascii_lowercase().as_str(),
            "license" | "licenses" | ".licenses" | "licence" | "licences"
        )
    });
    in_license_dir
        || ["license", "licence", "copying", "unlicense", "unlicence"]
            .iter()
            .any(|prefix| lower.starts_with(prefix))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::allowlist::StandardText;

    fn package(name: &str, license: Option<&str>) -> Package {
        Package {
            id: format!("{CRATES_IO}#{name}@1.0.0"),
            name: name.into(),
            version: "1.0.0".into(),
            source: Some(CRATES_IO.into()),
            license: license.map(str::to_owned),
            repository: None,
            manifest_path: PathBuf::from(format!("/registry/{name}-1.0.0/Cargo.toml")),
        }
    }

    /// その本文を使うクレート (名前と Cargo.toml の license)。
    type Users<'a> = &'a [(&'a str, Option<&'a str>)];

    fn report(entries: &[(&str, Users<'_>)]) -> Report {
        let mut crates = BTreeMap::new();
        let licenses = entries
            .iter()
            .map(|(id, users)| LicenseText {
                id: (*id).into(),
                text: format!("{id} text"),
                used_by: users
                    .iter()
                    .map(|(name, license)| {
                        crates.insert(*name, *license);
                        UsedBy {
                            krate: package(name, *license),
                        }
                    })
                    .collect(),
            })
            .collect();
        Report {
            licenses,
            crates: crates
                .into_iter()
                .map(|(name, license)| CrateLicense {
                    package: package(name, license),
                })
                .collect(),
        }
    }

    #[test]
    fn chosen_licenses_must_satisfy_the_manifest() {
        let ok = report(&[
            (
                "MIT",
                &[
                    ("a", Some("MIT OR Apache-2.0")),
                    ("b", Some("MIT/Apache-2.0")),
                ],
            ),
            ("BSD-3-Clause", &[("c", Some("BSD-3-Clause OR MIT"))]),
            (
                "Apache-2.0",
                &[("d", Some("Apache-2.0 WITH LLVM-exception"))],
            ),
        ]);
        assert!(expressions(&ok).is_empty(), "{:?}", expressions(&ok));

        // 上流が AND で GPL を足したのに、clarify が MIT だけにしている
        let masked = report(&[("MIT", &[("e", Some("MIT AND GPL-3.0-only"))])]);
        assert_eq!(expressions(&masked).len(), 1);
    }

    #[test]
    fn crates_without_texts_are_reported() {
        let mut report = report(&[("MIT", &[("a", Some("MIT"))])]);
        report.crates.push(CrateLicense {
            package: package("lonely", Some("MIT")),
        });
        let problems = expressions(&report);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].starts_with("lonely 1.0.0:"));
    }

    #[test]
    fn standard_texts_need_an_entry_and_entries_must_be_used() {
        let allowlist = Allowlist {
            standard_text: vec![StandardText {
                crates: vec!["mac".into(), "gone".into()],
                license: "MIT".into(),
                reason: "原文がない".into(),
            }],
            unlisted_file: Vec::new(),
        };
        let fallbacks = vec![
            Fallback {
                name: "mac".into(),
                version: "0.1.1".into(),
                license: "MIT".into(),
            },
            Fallback {
                name: "new-crate".into(),
                version: "1.0.0".into(),
                license: "MIT".into(),
            },
        ];
        let problems = standard_texts(&fallbacks, &allowlist);
        assert_eq!(problems.len(), 2, "{problems:?}");
        assert!(problems[0].starts_with("new-crate 1.0.0:"));
        assert!(problems[1].contains("gone"));
    }

    #[test]
    fn links_need_crates_io_or_a_repository() {
        let mut report = report(&[("MIT", &[("a", Some("MIT"))])]);
        assert!(links(&report).is_empty());
        report.crates[0].package.source = Some("git+https://example.com/a?tag=v1#abc".into());
        assert_eq!(links(&report).len(), 1);
        report.crates[0].package.repository = Some("https://example.com/a".into());
        assert!(links(&report).is_empty());
    }

    #[test]
    fn long_backtick_runs_break_the_fence() {
        assert!(breaks_fence("````"));
        assert!(breaks_fence("   `````text"));
        assert!(!breaks_fence("    ````"));
        assert!(!breaks_fence("```"));
        assert!(!breaks_fence("~~~~"));
    }

    #[test]
    fn third_party_unpublished_crates_are_reported() {
        let metadata: Metadata = serde_json::from_value(serde_json::json!({
            "packages": [
                { "id": "path+file:///w/app#0.1.0", "name": "app", "version": "0.1.0", "publish": [] },
                { "id": "git+https://example.com/x#1.0.0", "name": "x", "version": "1.0.0", "publish": [] },
                { "id": "registry+r#y@1.0.0", "name": "y", "version": "1.0.0", "publish": null },
            ],
            "workspace_members": ["path+file:///w/app#0.1.0"],
        }))
        .unwrap();
        let problems = unpublished(&metadata);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].starts_with("x 1.0.0:"));
    }

    #[test]
    fn classifies_license_files() {
        assert!(is_license_file("NOTICE"));
        assert!(is_license_file("NOTICE.txt"));
        assert!(!is_license_file("LICENSE-MIT"), "直下は cargo-about が見る");
        assert!(is_license_file("src/spin/LICENSE"));
        assert!(is_license_file("src/unicode_tables/LICENSE-UNICODE"));
        assert!(is_license_file("third_party/COPYING"));
        assert!(is_license_file(".licenses/ChenYuheng-MIT"));
        assert!(is_license_file("LICENSES/MIT.txt"));
        assert!(!is_license_file("src/copying.rs"));
        assert!(!is_license_file("assets/icons/copyright.svg"));
        assert!(!is_license_file("src/lib.rs"));
    }

    #[test]
    fn reads_clarified_files() {
        let files = clarified_files(
            r#"
accepted = ["MIT"]

[tracing-core.clarify]
license = "MIT"
files = [
    { path = "LICENSE", license = "MIT", checksum = "0" },
    { path = "src/spin/LICENSE", license = "MIT", checksum = "1" },
]

[lyon.clarify]
license = "MIT"
git = [{ path = "LICENSE-MIT", license = "MIT", checksum = "2" }]

[hasami]
accepted = ["NAIST-2003"]
"#,
        )
        .unwrap();
        assert_eq!(
            files
                .get("tracing-core")
                .unwrap()
                .iter()
                .collect::<Vec<_>>(),
            ["LICENSE", "src/spin/LICENSE"]
        );
        assert!(
            !files.contains_key("lyon"),
            "git のパスはパッケージの中ではない"
        );
        assert!(!files.contains_key("hasami"));
    }
}
