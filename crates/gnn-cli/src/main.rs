//! `gnn`: 取得定義のカタログを検査し、実際のサイトに対して試すコマンド。
//!
//! game-news-nexus-config の CI と、手元で取得定義を書くときに使う。
//!
//! ```text
//! gnn check <カタログのディレクトリ>          決まりと書式を検査する
//! gnn try <カタログのディレクトリ> [ID ...]   実際のサイトから取得してみる
//!         [--adult-consent]                   成人向けのソースも試す (18 歳以上で同意する場合だけ)
//! gnn schema [--out <ファイル>]                取得定義の JSON Schema を書き出す
//! gnn convert <sources.toml> <出力先>         書式 1 (TOML) の定義を書式 2 (YAML) にする
//! ```

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::{Context as _, Result};
use chrono::{Duration, Utc};
use clap::{Parser, Subcommand};
use gnn_core::catalog::{Catalog, GROUPS_FILE, ProblemKind};
use gnn_core::source::{APP_VERSION, SourceConfig};
use gnn_core::{EntryDraft, ImageIdentity, ImageObservation, RunStatus, Secrets, source_v1};
use gnn_fetch::{Engine, HttpFetcher, HttpOptions, NetworkPolicy, RunInput};

/// カタログの中の JSON Schema の置き場所。
const SCHEMA_PATH: &str = "schema/source.schema.json";

#[derive(Parser)]
#[command(
    name = "gnn",
    version,
    about = "Game News Nexus の取得定義を検査・試行する"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// カタログを検査する。問題があれば終了コード 1。
    Check {
        /// カタログのディレクトリ (groups.yaml と sources/ がある場所)。
        dir: PathBuf,
    },
    /// 実際のサイトから取得してみる。失敗したソースがあれば終了コード 1。
    Try {
        dir: PathBuf,
        /// 試すソースの ID。省略時はすべて。
        ids: Vec<String>,
        /// 1 ソースで読む記事の数。
        #[arg(long, default_value_t = 3)]
        limit: usize,
        /// 成人向けのソース (`adult: true`) も試す。18 歳以上で、成人向けの内容を取得することに
        /// 同意する場合だけ付ける。付けなければ成人向けのソースは試さず、ID で指定すると誤りにする。
        /// GitHub Actions の中では、成人向けのソースの記事の題名と本文を出さない。
        #[arg(long)]
        adult_consent: bool,
    },
    /// 取得定義の JSON Schema を書き出す。
    Schema {
        /// 書き出すファイル。省略時は標準出力。
        #[arg(long)]
        out: Option<PathBuf>,
    },
    /// 書式 1 の `sources.toml` を、書式 2 の YAML (1 ソース 1 ファイル) にする。
    Convert { input: PathBuf, out_dir: PathBuf },
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();
    let result = match cli.command {
        Command::Check { dir } => check(&dir),
        Command::Try {
            dir,
            ids,
            limit,
            adult_consent,
        } => try_sources(&dir, &ids, limit, adult_consent).await,
        Command::Schema { out } => schema(out.as_deref()).map(|()| true),
        Command::Convert { input, out_dir } => convert(&input, &out_dir).map(|()| true),
    };
    match result {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(err) => {
            eprintln!("エラー: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// GitHub Actions の注釈を出すか。
fn in_github_actions() -> bool {
    std::env::var_os("GITHUB_ACTIONS").is_some()
}

fn report(path: &str, message: &str) {
    if in_github_actions() {
        // 注釈の本文は 1 行にする。
        println!("::error file={path}::{}", message.replace('\n', " "));
    } else {
        println!("  ✗ {path}: {message}");
    }
}

fn read_catalog(dir: &Path) -> Result<(Catalog, Vec<(String, String)>)> {
    let files = gnn_core::catalog::read_dir(dir)?;
    let catalog = Catalog::from_files(files.iter().map(|(path, text)| (path, text)), None);
    Ok((catalog, files))
}

/// 決まりと書式を検査する。問題がなければ `true`。
fn check(dir: &Path) -> Result<bool> {
    let (catalog, files) = read_catalog(dir)?;
    let mut ok = true;

    if !files.iter().any(|(path, _)| path == GROUPS_FILE) {
        report(GROUPS_FILE, "groups.yaml がありません");
        ok = false;
    }
    for problem in catalog.problems() {
        // 検査ではこのツールの版で読むので、新しい版を求めるファイルも誤りとして扱う。
        let message = match problem.kind {
            ProblemKind::NeedsNewerApp => format!(
                "{} (このツールは {APP_VERSION}。gnn-cli を更新してください)",
                problem.message
            ),
            ProblemKind::Invalid => problem.message.clone(),
        };
        report(&problem.path, &message);
        ok = false;
    }
    let groups: Vec<&str> = catalog
        .groups()
        .iter()
        .map(|group| group.id.as_str())
        .collect();
    for entry in catalog.entries() {
        if !groups.contains(&entry.group.as_str()) {
            report(
                &entry.path,
                &format!("グループ `{}` が groups.yaml にありません", entry.group),
            );
            ok = false;
        }
        // gnn-core の検査の分は Catalog の読み込みで報告済み。gnn-fetch だけが見る問題
        // (CSS セレクタ・文字コードの名前) を足す。
        let reported: Vec<String> = entry
            .source
            .validate()
            .iter()
            .map(ToString::to_string)
            .collect();
        for issue in gnn_fetch::validate(&entry.source) {
            if !reported.contains(&issue.to_string()) {
                report(&entry.path, &issue.to_string());
                ok = false;
            }
        }
    }

    // JSON Schema がこのツールの版と同じか。
    let schema_file = dir.join(SCHEMA_PATH);
    if schema_file.exists() {
        let current = std::fs::read_to_string(&schema_file)?;
        if normalize_newlines(&current) != normalize_newlines(&schema_text()?) {
            report(
                SCHEMA_PATH,
                "取得定義の JSON Schema が古くなっています。`gnn schema --out schema/source.schema.json` で作り直してください",
            );
            ok = false;
        }
    }

    println!(
        "{} 件のソースを検査しました ({} グループ){}",
        catalog.entries().len(),
        catalog.groups().len(),
        if ok {
            "。問題はありません"
        } else {
            "。問題があります"
        }
    );
    Ok(ok)
}

fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").trim_end().to_string()
}

/// 実際のサイトから取得してみる。失敗がなければ `true`。
///
/// 成人向けのソース (`adult: true`) は、`adult_consent` (`--adult-consent`) のときだけ試す。
/// ID で指定していて印がなければ、通信する前に誤りにする。
async fn try_sources(
    dir: &Path,
    ids: &[String],
    limit: usize,
    adult_consent: bool,
) -> Result<bool> {
    let (catalog, _) = read_catalog(dir)?;
    for problem in catalog.problems() {
        report(&problem.path, &problem.message);
    }
    if !adult_consent {
        let named: Vec<&str> = catalog
            .entries()
            .iter()
            .filter(|entry| entry.source.adult && ids.contains(&entry.source.id))
            .map(|entry| entry.source.id.as_str())
            .collect();
        if !named.is_empty() {
            anyhow::bail!(
                "{} は成人向けのソースです。試すには --adult-consent を付けてください (18 歳以上で、成人向けの内容を取得することに同意する場合だけ)",
                named.join(", ")
            );
        }
    }
    let engine = Engine::new(HttpFetcher::new(
        HttpOptions::default(),
        Secrets::default(),
    )?);
    let known = HashMap::new();
    let mut ok = catalog.problems().is_empty();
    let mut tried = 0;
    let mut skipped_adult = 0;
    for entry in catalog.entries() {
        let source = &entry.source;
        if !ids.is_empty() && !ids.contains(&source.id) {
            continue;
        }
        if source.adult && !adult_consent {
            skipped_adult += 1;
            continue;
        }
        tried += 1;
        let started = std::time::Instant::now();
        let output = engine
            .run(RunInput {
                source,
                known: &known,
                max_age_days: 0,
                now: Utc::now(),
                recheck: Duration::hours(24),
                recheck_ids: HashSet::new(),
                limit: Some(limit),
                // アプリと同じく、カタログのソースは公開のネットワークだけに取得する。
                network: NetworkPolicy::PublicOnly,
                allow_secrets: false,
                trace: true,
            })
            .await;
        println!(
            "== {} [{}] {:?} 一覧 {} 件・読んだ {} 件 {:.1} 秒 {}",
            source.id,
            source.kind_label(),
            output.status,
            output.listed,
            output.drafts.len(),
            started.elapsed().as_secs_f32(),
            output.message.as_deref().unwrap_or_default()
        );
        for step in &output.trace {
            println!(
                "   {} {}: {} {}",
                if step.ok { "·" } else { "✗" },
                step.stage.label(),
                step.title,
                step.detail
            );
        }
        if let Some(line) = image_identity_summary(source, &output.drafts) {
            println!("   {line}");
        }
        // 公開される CI の記録には、成人向けのソースの記事の題名と本文を残さない。
        if source.adult && in_github_actions() {
            println!("   (成人向けのソースなので、記事の題名と本文は出しません)");
        }
        let previews = if source.adult && in_github_actions() {
            0
        } else {
            2
        };
        for draft in output.drafts.iter().take(previews) {
            let body = draft.body_html.as_deref().unwrap_or("");
            let preview: String = gnn_core::text::html_to_text(body)
                .chars()
                .take(60)
                .collect();
            println!(
                "   - {} | {} | 画像 {} | 本文 {} バイト: {}",
                draft.title,
                draft
                    .published
                    .map(|published| published
                        .at
                        .with_timezone(&chrono_tz::Asia::Tokyo)
                        .format("%Y-%m-%d %H:%M")
                        .to_string())
                    .unwrap_or_else(|| "日時なし".into()),
                if draft.image_url.is_some() {
                    "あり"
                } else {
                    "なし"
                },
                body.len(),
                preview.replace('\n', " / ")
            );
        }
        match output.status {
            RunStatus::Failed => {
                report(
                    &entry.path,
                    &format!("取得に失敗: {}", output.message.unwrap_or_default()),
                );
                ok = false;
            }
            RunStatus::Partial if in_github_actions() => {
                println!(
                    "::warning file={}::一部を取得できませんでした: {}",
                    entry.path,
                    output.message.unwrap_or_default().replace('\n', " ")
                );
            }
            _ => {}
        }
    }
    if skipped_adult > 0 {
        println!(
            "成人向けのソース {skipped_adult} 件は試していません (18 歳以上で、成人向けの内容を取得することに同意する場合は --adult-consent)"
        );
    }
    if tried == 0 && skipped_adult == 0 && !ids.is_empty() {
        anyhow::bail!("ID {} のソースはカタログにありません", ids.join(", "));
    }
    Ok(ok)
}

/// 画像の同一性 (`url_identity.images`) を書いたソースで、読んだ記事の画像のうち宣言に合ったものの数。
/// 1 つも合わなければ、`origin` と `path_prefix` の書き誤りを疑う印を付ける。
fn image_identity_summary(source: &SourceConfig, drafts: &[EntryDraft]) -> Option<String> {
    let identity = ImageIdentity::of(source);
    if identity.is_empty() {
        return None;
    }
    let mut matched = 0;
    let mut references = HashSet::new();
    for draft in drafts {
        let mut observed: Vec<ImageObservation> = draft
            .image_url
            .as_deref()
            .and_then(|url| identity.observe(url))
            .into_iter()
            .collect();
        if let Some(body) = draft.body_html.as_deref() {
            observed.extend(identity.rewrite_html(body).1);
        }
        matched += observed.len();
        references.extend(observed.into_iter().map(|observed| observed.reference));
    }
    Some(if matched == 0 {
        "✗ 画像の同一性: 宣言に合う画像がありません (origin と path_prefix を確かめてください)"
            .into()
    } else {
        format!(
            "· 画像の同一性: 宣言に合う画像 {matched} 件 (同じ画像を除いて {} 件)",
            references.len()
        )
    })
}

fn schema_text() -> Result<String> {
    let schema = schemars::schema_for!(SourceConfig);
    let mut text = serde_json::to_string_pretty(&schema)?;
    text.push('\n');
    Ok(text)
}

fn schema(out: Option<&Path>) -> Result<()> {
    let text = schema_text()?;
    match out {
        Some(path) => {
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            std::fs::write(path, text)
                .with_context(|| format!("{} に書けません", path.display()))?;
            println!("{} に書き出しました", path.display());
        }
        None => print!("{text}"),
    }
    Ok(())
}

fn convert(input: &Path, out_dir: &Path) -> Result<()> {
    let text = std::fs::read_to_string(input)
        .with_context(|| format!("{} を読めません", input.display()))?;
    let file = source_v1::SourcesFile::parse(&text)
        .with_context(|| format!("{} の書式が誤っています", input.display()))?;
    std::fs::create_dir_all(out_dir)?;
    for old in &file.sources {
        let converted = old.to_v2();
        let text = gnn_core::config::source_file_text(&converted.source)?;
        let path = out_dir.join(format!("{}.yaml", converted.source.id));
        std::fs::write(&path, text)?;
        let issues = gnn_fetch::validate(&converted.source);
        println!(
            "{}{}",
            path.display(),
            if issues.is_empty() {
                String::new()
            } else {
                format!(
                    " (要確認: {})",
                    issues
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(" / ")
                )
            }
        );
    }
    Ok(())
}
