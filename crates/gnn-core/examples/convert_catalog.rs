//! 書式 1 (TOML) のカタログを書式 2 (YAML) に変換する。移行のための一度きりの道具。
//!
//! `cargo run -p gnn-core --example convert_catalog -- <カタログのディレクトリ>`
//!
//! `sources/<グループ>/<id>.toml` を同じ場所の `<id>.yaml` にし、元の TOML を消す。
//! 先頭のコメント行 (名前) は残し、エディタの補完用の Schema の指定を足す。

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use gnn_core::source_v1;

fn main() -> Result<()> {
    let root = PathBuf::from(
        std::env::args()
            .nth(1)
            .context("カタログのディレクトリを指定してください")?,
    );
    let mut converted = 0;
    for group in fs::read_dir(root.join("sources"))? {
        let group = group?.path();
        if !group.is_dir() {
            continue;
        }
        for file in fs::read_dir(&group)? {
            let path = file?.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("toml") {
                continue;
            }
            convert(&path)?;
            converted += 1;
        }
    }
    println!("{converted} 件を変換しました");
    Ok(())
}

fn convert(path: &Path) -> Result<()> {
    let text = fs::read_to_string(path)?;
    let old: source_v1::SourceConfig =
        toml::from_str(&text).with_context(|| format!("{} を読めません", path.display()))?;
    let converted = old.to_v2();
    if converted.local_icon.is_some() {
        bail!("{}: カタログに手元のアイコンがあります", path.display());
    }
    let source = converted.source;
    let issues = source.validate();
    if !issues.is_empty() {
        bail!("{}: {issues:?}", path.display());
    }
    // 先頭のコメント (名前や説明) はそのまま残す。
    let comments: Vec<&str> = text
        .lines()
        .take_while(|line| line.starts_with('#'))
        .collect();
    let mut out = String::new();
    for line in &comments {
        out.push_str(line);
        out.push('\n');
    }
    out.push_str("# yaml-language-server: $schema=../../schema/source.schema.json\n");
    out.push_str(&source.to_yaml().map_err(|err| anyhow::anyhow!("{err}"))?);
    let again = gnn_core::SourceConfig::from_yaml(&out).map_err(|err| anyhow::anyhow!("{err}"))?;
    if again != source {
        bail!("{}: YAML を読み直すと内容が変わります", path.display());
    }
    let target = path.with_extension("yaml");
    fs::write(&target, out)?;
    fs::remove_file(path)?;
    println!("{}", target.display());
    Ok(())
}
