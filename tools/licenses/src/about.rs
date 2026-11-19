//! cargo-about と cargo を呼び、結果と cargo-about のログを読む。

use std::ffi::OsString;
use std::fs;
use std::process::Command;

use anyhow::{Context as _, Result, bail};
use serde::Deserialize;

use crate::Paths;

/// mise.toml で cargo-about の版を固定しているツールの名前 (`[tasks.licenses]` の `tools`)。
const MISE_TOOL: &str = "github:EmbarkStudios/cargo-about";

/// cargo-about の 1 回の実行の結果。
pub struct AboutRun {
    /// `--format json` の出力 (ひな形に渡すデータと同じ形)。
    pub json: String,
    /// 標準エラーのログから読んだこと。
    pub log: Log,
}

/// cargo-about のログ (`-L debug`) から読んだこと。
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Log {
    /// 警告とエラー、ログの形でない出力 (設定の誤りなどの診断)。
    pub problems: Vec<String>,
    /// 本文が見つからず、SPDX の標準の文面に置き換えたライセンス。cargo-about はこれを debug のログに
    /// しか出さない。
    pub fallbacks: Vec<Fallback>,
}

/// 本文を SPDX の標準の文面に置き換えたクレートとライセンス。
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Fallback {
    pub name: String,
    pub version: String,
    pub license: String,
}

/// `cargo metadata` のうち、使うところ。
#[derive(Deserialize)]
pub struct Metadata {
    pub packages: Vec<MetadataPackage>,
    pub workspace_members: Vec<String>,
}

#[derive(Deserialize)]
pub struct MetadataPackage {
    pub id: String,
    pub name: String,
    pub version: String,
    /// `publish = false` は空の配列、指定なしは null。
    pub publish: Option<Vec<String>>,
}

/// mise.toml の `[tasks.licenses]` の `tools` に固定した cargo-about の版。
pub fn pinned_version(mise_toml: &str) -> Result<String> {
    let table: toml::Table = mise_toml
        .parse()
        .context("mise.toml の書き方が正しくありません")?;
    table
        .get("tasks")
        .and_then(|tasks| tasks.get("licenses"))
        .and_then(|task| task.get("tools"))
        .and_then(|tools| tools.get(MISE_TOOL))
        .and_then(toml::Value::as_str)
        .map(str::to_owned)
        .with_context(|| {
            format!("mise.toml の [tasks.licenses] の tools に \"{MISE_TOOL}\" の版がありません")
        })
}

/// 手元の cargo-about の版 (`cargo-about --version`)。
pub fn installed_version() -> Result<String> {
    let output = Command::new("cargo-about")
        .arg("--version")
        .output()
        .context("cargo-about を起動できません。mise run licenses で動かしてください (mise.toml が版を固定している)")?;
    if !output.status.success() {
        bail!("cargo-about --version が失敗しました ({})", output.status);
    }
    let text = String::from_utf8_lossy(&output.stdout);
    parse_version(&text)
        .with_context(|| format!("cargo-about --version の出力を読めません: {text}"))
}

/// `cargo-about 0.9.2` から版を取り出す。
pub fn parse_version(text: &str) -> Option<String> {
    let version = text.trim().strip_prefix("cargo-about ")?.trim();
    (!version.is_empty()).then(|| version.to_owned())
}

/// 依存のクレートのソースを取ってくる (cargo-about はソースの中のライセンスのファイルを読む)。
pub fn fetch(paths: &Paths) -> Result<()> {
    let status = Command::new(cargo())
        .args(["fetch", "--locked", "--quiet", "--manifest-path"])
        .arg(paths.root.join("Cargo.toml"))
        .status()
        .context("cargo fetch を起動できません")?;
    if !status.success() {
        bail!("cargo fetch --locked が失敗しました ({status})");
    }
    Ok(())
}

/// ワークスペースと依存のクレートの一覧。
pub fn metadata(paths: &Paths) -> Result<Metadata> {
    let output = Command::new(cargo())
        .args([
            "metadata",
            "--format-version",
            "1",
            "--locked",
            "--offline",
            "--manifest-path",
        ])
        .arg(paths.root.join("Cargo.toml"))
        .output()
        .context("cargo metadata を起動できません")?;
    if !output.status.success() {
        bail!(
            "cargo metadata が失敗しました ({}):\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        );
    }
    serde_json::from_slice(&output.stdout).context("cargo metadata の出力を読めません")
}

/// cargo-about で、配布物の実行ファイル (gnn-app) のクレートとライセンスを集める。
pub fn generate(paths: &Paths) -> Result<AboutRun> {
    let dir = tempfile::tempdir().context("作業用のフォルダを作れません")?;
    let out = dir.path().join("about.json");
    // 本文を標準の文面に置き換えたことは debug のログにしか出ないので、-L debug で読む。
    // 出力はファイルに書かせる (標準出力に書かせると、PowerShell の下では cargo-about が断る)
    let output = Command::new("cargo-about")
        .args([
            "-L", "debug", "--color", "never", "generate", "--locked", "--fail",
        ])
        .args(["--format", "json"])
        .arg("--manifest-path")
        .arg(&paths.manifest)
        .arg("--config")
        .arg(&paths.config)
        .arg("--output-file")
        .arg(&out)
        .current_dir(&paths.root)
        .output()
        .context("cargo-about を起動できません。mise run licenses で動かしてください")?;
    let log = parse_log(&String::from_utf8_lossy(&output.stderr));
    if !output.status.success() {
        bail!(
            "cargo-about が失敗しました ({}):\n{}",
            output.status,
            log.problems.join("\n")
        );
    }
    let json = fs::read_to_string(&out)
        .with_context(|| format!("cargo-about の出力 {} を読めません", out.display()))?;
    Ok(AboutRun { json, log })
}

/// `cargo run` から動かしたときは、同じ cargo を使う。
fn cargo() -> OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into())
}

/// cargo-about の標準エラーを読む。ログは `<時刻> [<レベル>] <本文>` の形 (色の制御文字が付く)。
pub fn parse_log(stderr: &str) -> Log {
    let mut log = Log::default();
    for raw in stderr.lines() {
        let line = strip_ansi(raw);
        let line = line.trim_end();
        if line.is_empty() {
            continue;
        }
        match split_record(line) {
            Some(("ERROR" | "WARN", _)) => log.problems.push(line.to_owned()),
            Some(("DEBUG", message)) => {
                if let Some(fallback) = parse_fallback(message) {
                    log.fallbacks.push(fallback);
                }
            }
            Some(_) => {}
            // 設定の誤りやライセンスの式の誤りの診断 (codespan の形) など
            None => log.problems.push(line.to_owned()),
        }
    }
    log.fallbacks.sort();
    log.fallbacks.dedup();
    log
}

/// ログの 1 行を、レベルと本文に分ける。ログの形でなければ None。
fn split_record(line: &str) -> Option<(&str, &str)> {
    let (_time, rest) = line.split_once(' ')?;
    let (level, message) = rest.strip_prefix('[')?.split_once(']')?;
    if level.is_empty() || !level.bytes().all(|b| b.is_ascii_uppercase()) {
        return None;
    }
    Some((level, message.trim_start()))
}

/// `unable to find text for license 'MIT' for crate 'mac 0.1.1', falling back to canonical text`
fn parse_fallback(message: &str) -> Option<Fallback> {
    let rest = message.strip_prefix("unable to find text for license '")?;
    let (license, rest) = rest.split_once("' for crate '")?;
    let krate = rest.strip_suffix("', falling back to canonical text")?;
    let (name, version) = krate.rsplit_once(' ')?;
    Some(Fallback {
        name: name.to_owned(),
        version: version.to_owned(),
        license: license.to_owned(),
    })
}

/// 端末の色などの制御文字 (ESC [ ... 終端) を除く。
fn strip_ansi(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\u{1b}' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if ('@'..='~').contains(&c) {
                        break;
                    }
                }
            }
            continue;
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_pinned_version_from_the_task() {
        let mise = r#"
[tools]
rust = "1.99.1"

[tasks.licenses]
tools = { "github:EmbarkStudios/cargo-about" = "0.9.2" }
run = "cargo run -q -p gnn-licenses --"
"#;
        assert_eq!(pinned_version(mise).unwrap(), "0.9.2");
        assert!(pinned_version("[tasks.licenses]\nrun = \"x\"\n").is_err());
    }

    #[test]
    fn reads_the_version_of_cargo_about() {
        assert_eq!(
            parse_version("cargo-about 0.9.2\n").as_deref(),
            Some("0.9.2")
        );
        assert_eq!(parse_version("cargo-deny 0.9.2"), None);
        assert_eq!(parse_version("cargo-about "), None);
    }

    #[test]
    fn reads_fallbacks_and_problems_from_the_log() {
        let stderr = concat!(
            "2026-11-06T09:15:21.4394197Z [\u{1b}[32mINFO\u{1b}[0m] gathering crates for crates/gnn-app/Cargo.toml\u{1b}[0m\n",
            "2026-11-06T09:15:22.5898469Z [\u{1b}[34mDEBUG\u{1b}[0m] filtered accesskit_unix 0.22.1\u{1b}[0m\n",
            "2026-11-06T09:15:25.5563739Z [\u{1b}[34mDEBUG\u{1b}[0m] unable to find text for license 'MIT' for crate 'block2 0.6.2', falling back to canonical text\u{1b}[0m\n",
            "2026-11-06T09:15:25.5566288Z [\u{1b}[34mDEBUG\u{1b}[0m] unable to find text for license 'MPL-2.0' for crate 'dwrote 0.11.5', falling back to canonical text\u{1b}[0m\n",
            "2026-11-06T09:15:25.6Z [\u{1b}[33mWARN\u{1b}[0m] failed to validate all files specified in clarification for crate chrono 0.4.45: checksum mismatch\u{1b}[0m\n",
            "warning: config found for a crate not present in the graph\n",
            "   ┌─ packaging/licenses/about.toml:10:1\n",
            "\n",
        );
        let log = parse_log(stderr);
        assert_eq!(
            log.fallbacks,
            vec![
                Fallback {
                    name: "block2".into(),
                    version: "0.6.2".into(),
                    license: "MIT".into()
                },
                Fallback {
                    name: "dwrote".into(),
                    version: "0.11.5".into(),
                    license: "MPL-2.0".into()
                },
            ]
        );
        assert_eq!(log.problems.len(), 3, "{:?}", log.problems);
        assert!(log.problems[0].contains("[WARN] failed to validate"));
        assert_eq!(
            log.problems[1],
            "warning: config found for a crate not present in the graph"
        );
    }

    #[test]
    fn strips_terminal_colors() {
        assert_eq!(
            strip_ansi("[\u{1b}[31mERROR\u{1b}[0m] x\u{1b}[0m"),
            "[ERROR] x"
        );
        assert_eq!(strip_ansi("プレーン"), "プレーン");
    }
}
