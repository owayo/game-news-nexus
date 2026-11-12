//! 配布物 (Windows のインストーラーと macOS の DMG) に入れる、実行ファイルに組み込んだ Rust のクレートの
//! ライセンスの一覧 (`crates/gnn-app/assets/licenses/THIRD_PARTY_LICENSES.md`) を作る。
//! `mise run licenses` で作り直し、`mise run licenses -- --check` で古くないかだけを確かめる (CI)。
//!
//! cargo-about (版は mise.toml が固定) で gnn-app のクレートとライセンスを集め、次を確かめてから
//! ひな形 (`packaging/licenses/about.hbs`) に当てはめる。問題があれば、全部を出して止まる。
//!
//! - 使ってよいライセンス (`about.toml` の accepted) だけか (cargo-about が止まる)
//! - cargo-about が警告を出していないか (clarify の SHA-256 の食い違い・グラフにないクレートの設定など)
//! - 本文が SPDX の標準の文面に置き換わったクレートが `allowlist.toml` の standard-text にあるか
//! - 一覧に載せるライセンスが、各クレートの Cargo.toml の式を満たすか (clarify が古い式を隠していないか)
//! - publish = false (一覧から外れる) が自分のクレートだけか
//! - 直下の NOTICE と下の階層のライセンスのファイルが、一覧に入るか、入れない理由が allowlist.toml にあるか
//! - 各クレートのソースの入手先 (crates.io のページか repository) を示せるか

mod about;
mod allowlist;
mod checks;
mod render;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result, bail};
use serde_json::Value;

use crate::allowlist::Allowlist;
use crate::checks::Report;

/// リポジトリの中の、使うファイルの場所。
pub struct Paths {
    pub root: PathBuf,
    /// 配布物の実行ファイルのクレート。
    pub manifest: PathBuf,
    pub config: PathBuf,
    pub template: PathBuf,
    pub allowlist: PathBuf,
    pub mise: PathBuf,
    pub output: PathBuf,
}

impl Paths {
    fn new() -> Self {
        // このクレートは tools/licenses にある
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .ancestors()
            .nth(2)
            .expect("ワークスペースの中にある")
            .to_path_buf();
        let packaging = root.join("packaging").join("licenses");
        Self {
            manifest: root.join("crates").join("gnn-app").join("Cargo.toml"),
            config: packaging.join("about.toml"),
            template: packaging.join("about.hbs"),
            allowlist: packaging.join("allowlist.toml"),
            mise: root.join("mise.toml"),
            output: root
                .join("crates")
                .join("gnn-app")
                .join("assets")
                .join("licenses")
                .join("THIRD_PARTY_LICENSES.md"),
            root,
        }
    }

    /// リポジトリからの相対パス (区切りは OS によらず `/`)。
    fn relative(&self, path: &Path) -> String {
        path.strip_prefix(&self.root)
            .unwrap_or(path)
            .to_string_lossy()
            .replace('\\', "/")
    }
}

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error:#}");
            ExitCode::FAILURE
        }
    }
}

fn run() -> Result<ExitCode> {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let check = match args.as_slice() {
        [] => false,
        [arg] if arg == "--check" => true,
        [arg] if arg == "-h" || arg == "--help" => {
            println!(
                "使い方: mise run licenses [-- --check]\n\
                 配布物に入れる Rust のクレートのライセンスの一覧を作り直す。--check は作り直さずに、古くないかだけを確かめる"
            );
            return Ok(ExitCode::SUCCESS);
        }
        _ => bail!(
            "不明な引数です: {} (mise run licenses -- --help)",
            args.join(" ")
        ),
    };
    let paths = Paths::new();

    let pinned = about::pinned_version(&read(&paths.mise)?)?;
    let installed = about::installed_version()?;
    if installed != pinned {
        bail!(
            "cargo-about の版 ({installed}) が mise.toml の固定 ({pinned}) と違います。mise run licenses で動かしてください"
        );
    }
    let allowlist = Allowlist::load(&paths.allowlist)?;
    let clarified = checks::clarified_files(&read(&paths.config)?)?;

    about::fetch(&paths)?;
    let metadata = about::metadata(&paths)?;
    let run = about::generate(&paths)?;
    let data: Value =
        serde_json::from_str(&run.json).context("cargo-about の出力 (JSON) を読めません")?;
    let report: Report = serde_json::from_value(data.clone())
        .context("cargo-about の出力 (JSON) の形が想定と違います")?;

    let mut problems = Vec::new();
    if !run.log.problems.is_empty() {
        problems.push(format!(
            "cargo-about が警告を出しました:\n{}",
            run.log.problems.join("\n")
        ));
    }
    problems.extend(allowlist.problems());
    problems.extend(checks::standard_texts(&run.log.fallbacks, &allowlist));
    problems.extend(checks::expressions(&report));
    problems.extend(checks::unpublished(&metadata));
    problems.extend(checks::links(&report));
    problems.extend(checks::fences(&report));
    problems.extend(checks::unlisted_files(&report, &clarified, &allowlist)?);
    if !problems.is_empty() {
        for problem in &problems {
            eprintln!("- {problem}");
        }
        bail!(
            "ライセンスの一覧を作れません ({} 件)。packaging/licenses/ の about.toml と allowlist.toml を直してください",
            problems.len()
        );
    }

    let rendered = render::render(&read(&paths.template)?, data, &allowlist.standard_text)?;
    let output = paths.relative(&paths.output);
    let current = fs::read_to_string(&paths.output)
        .ok()
        .map(|text| render::normalize_newlines(&text));
    println!(
        "cargo-about {installed}: {} 個のクレート、{} 個の本文 (標準の文面は {} 個)",
        report.crates.len(),
        report.licenses.len(),
        run.log.fallbacks.len()
    );
    if current.as_deref() == Some(rendered.as_str()) {
        println!("{output} は最新です");
        return Ok(ExitCode::SUCCESS);
    }
    if check {
        match current.as_deref() {
            None => eprintln!("{output} がありません"),
            Some(current) => {
                if let Some((line, old, new)) = first_difference(current, &rendered) {
                    eprintln!("{output} の {line} 行目から違います:\n- {old}\n+ {new}");
                }
            }
        }
        eprintln!(
            "{output} が依存や設定と合っていません。mise run licenses で作り直してコミットしてください"
        );
        return Ok(ExitCode::FAILURE);
    }
    fs::write(&paths.output, &rendered).with_context(|| format!("{output} を書けません"))?;
    println!("{output} を作り直しました");
    Ok(ExitCode::SUCCESS)
}

fn read(path: &Path) -> Result<String> {
    fs::read_to_string(path).with_context(|| format!("{} を読めません", path.display()))
}

/// 最初に違う行 (1 から数えた行番号と、それぞれの行)。
fn first_difference<'a>(old: &'a str, new: &'a str) -> Option<(usize, &'a str, &'a str)> {
    let mut old_lines = old.lines();
    let mut new_lines = new.lines();
    let mut number = 0;
    loop {
        number += 1;
        match (old_lines.next(), new_lines.next()) {
            (None, None) => return None,
            (old, new) if old == new => {}
            (old, new) => return Some((number, old.unwrap_or(""), new.unwrap_or(""))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_first_different_line() {
        assert_eq!(first_difference("a\nb\n", "a\nb\n"), None);
        assert_eq!(first_difference("a\nb\n", "a\nc\n"), Some((2, "b", "c")));
        assert_eq!(first_difference("a\n", "a\nb\n"), Some((2, "", "b")));
    }

    #[test]
    fn paths_point_into_the_repository() {
        let paths = Paths::new();
        assert!(paths.manifest.is_file(), "{}", paths.manifest.display());
        assert!(paths.config.is_file(), "{}", paths.config.display());
        assert!(paths.template.is_file(), "{}", paths.template.display());
        assert!(paths.allowlist.is_file(), "{}", paths.allowlist.display());
        assert!(paths.mise.is_file(), "{}", paths.mise.display());
    }
}
