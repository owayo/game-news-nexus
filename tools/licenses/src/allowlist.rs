//! packaging/licenses/allowlist.toml: cargo-about の設定 (about.toml) では原文を指定できないものの扱い。

use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::{Context as _, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Allowlist {
    /// 原文を取得できないので、SPDX の標準の文面を載せるクレート。
    #[serde(default, rename = "standard-text")]
    pub standard_text: Vec<StandardText>,
    /// 一覧に入れない、クレートの中のライセンスのファイル。
    #[serde(default, rename = "unlisted-file")]
    pub unlisted_file: Vec<UnlistedFile>,
}

/// ひな形にもそのまま渡す (標準の文面を載せたクレートの節)。
#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StandardText {
    pub crates: Vec<String>,
    /// SPDX の識別子。
    pub license: String,
    pub reason: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UnlistedFile {
    #[serde(rename = "crate")]
    pub krate: String,
    /// クレートのフォルダからの相対パス (区切りは `/`)。
    pub paths: Vec<String>,
    pub reason: String,
}

impl Allowlist {
    pub fn load(path: &Path) -> Result<Self> {
        let text =
            fs::read_to_string(path).with_context(|| format!("{} を読めません", path.display()))?;
        Self::parse(&text).with_context(|| format!("{} の書き方が正しくありません", path.display()))
    }

    pub fn parse(text: &str) -> Result<Self> {
        Ok(toml::from_str(text)?)
    }

    /// 書き方の誤り (理由がない・同じものが 2 回ある) を返す。
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        let mut standard = BTreeSet::new();
        for entry in &self.standard_text {
            if entry.reason.trim().is_empty() || entry.crates.is_empty() {
                problems.push(format!(
                    "allowlist.toml の standard-text ({}) にクレートか理由がありません",
                    entry.license
                ));
            }
            for name in &entry.crates {
                if !standard.insert((name.as_str(), entry.license.as_str())) {
                    problems.push(format!(
                        "allowlist.toml の standard-text に {name} ({}) が 2 回あります",
                        entry.license
                    ));
                }
            }
        }
        let mut unlisted = BTreeSet::new();
        for entry in &self.unlisted_file {
            if entry.reason.trim().is_empty() || entry.paths.is_empty() {
                problems.push(format!(
                    "allowlist.toml の unlisted-file ({}) にパスか理由がありません",
                    entry.krate
                ));
            }
            for path in &entry.paths {
                if !unlisted.insert((entry.krate.as_str(), path.as_str())) {
                    problems.push(format!(
                        "allowlist.toml の unlisted-file に {} の {path} が 2 回あります",
                        entry.krate
                    ));
                }
            }
        }
        problems
    }

    /// 標準の文面を載せてよいか。
    pub fn allows_standard_text(&self, name: &str, license: &str) -> bool {
        self.standard_text
            .iter()
            .any(|entry| entry.license == license && entry.crates.iter().any(|c| c == name))
    }

    /// 一覧に入れなくてよいファイルか。
    pub fn allows_unlisted(&self, name: &str, path: &str) -> bool {
        self.unlisted_file
            .iter()
            .any(|entry| entry.krate == name && entry.paths.iter().any(|p| p == path))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_finds_entries() {
        let allowlist = Allowlist::parse(
            r#"
[[standard-text]]
crates = ["mac", "seahash"]
license = "MIT"
reason = "原文がない"

[[unlisted-file]]
crate = "fontdb"
paths = ["tests/fonts/LICENSE.txt"]
reason = "テストのデータ"
"#,
        )
        .unwrap();
        assert!(allowlist.problems().is_empty());
        assert!(allowlist.allows_standard_text("seahash", "MIT"));
        assert!(!allowlist.allows_standard_text("seahash", "Apache-2.0"));
        assert!(allowlist.allows_unlisted("fontdb", "tests/fonts/LICENSE.txt"));
        assert!(!allowlist.allows_unlisted("fontdb", "LICENSE"));
    }

    #[test]
    fn reports_duplicates_and_missing_reasons() {
        let allowlist = Allowlist::parse(
            r#"
[[standard-text]]
crates = ["mac"]
license = "MIT"
reason = ""

[[standard-text]]
crates = ["mac"]
license = "MIT"
reason = "2 回目"
"#,
        )
        .unwrap();
        assert_eq!(allowlist.problems().len(), 2, "{:?}", allowlist.problems());
    }

    #[test]
    fn rejects_unknown_keys() {
        assert!(Allowlist::parse("[[standard-text]]\ncrates = []\nlicense = \"MIT\"\nreason = \"x\"\nversion = \"1\"\n").is_err());
    }
}
