//! `gnn-schedule` に埋め込む hasami の辞書を用意する。`mise run dict` で動かす。
//!
//! `crates/gnn-schedule/dict/bundled.json` の固定情報 (リリースのタグ・大きさ・SHA-256) で、hasami の
//! リリースから辞書を取得して `crates/gnn-schedule/dict/` に置く。正しいファイルがすでにあれば通信しない。
//! 中身の違うファイル (前の版の辞書や壊れたもの) は取り直して置き換える。

use std::fs;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use hasami::download::{self, DistributedDict, DownloadOptions, Outcome, Verification};
use serde::Deserialize;

/// `bundled.json` の中身。
#[derive(Deserialize)]
struct Bundled {
    /// 辞書を添付した hasami のリリースのタグ。
    hasami_tag: String,
    /// 辞書の形式の版。依存の hasami が読める版と同じでなければならない。
    format_version: u32,
    /// リリースの `dictionaries.json` の項目をそのまま写したもの。
    dictionary: DistributedDict,
}

fn main() -> Result<()> {
    let dir = dict_dir();
    let path = dir.join("bundled.json");
    let text =
        fs::read_to_string(&path).with_context(|| format!("{} を読めません", path.display()))?;
    let bundled: Bundled = serde_json::from_str(&text)
        .with_context(|| format!("{} の書き方が正しくありません", path.display()))?;
    if bundled.format_version != hasami::hsd::FORMAT_VERSION {
        bail!(
            "bundled.json の辞書の形式 ({}) を、依存の hasami (形式 {}) では読めません。\
             Cargo.toml の hasami のタグと bundled.json を同じリリースにそろえてください",
            bundled.format_version,
            hasami::hsd::FORMAT_VERSION
        );
    }
    let dict = &bundled.dictionary;
    dict.validate().map_err(|problem| {
        anyhow::anyhow!("bundled.json の辞書の項目が正しくありません: {problem}")
    })?;

    let target = dir.join(&dict.file);
    let verification = download::verify(&target, dict)
        .with_context(|| format!("{} を確かめられません", target.display()))?;
    if verification == Verification::Differs {
        println!("{} が固定した辞書と違うので取り直します", target.display());
    }
    let base_url = download::release_url(&bundled.hasami_tag);
    let mut shown = 0;
    let mut progress = |received: u64, total: u64| {
        // 10% ごとに進み具合を出す。
        let percent = received.saturating_mul(100).checked_div(total).unwrap_or(0);
        if percent >= shown {
            print!("\r{} を取得中 {percent:>3}%", dict.file);
            let _ = std::io::stdout().flush();
            shown = percent - percent % 10 + 10;
        }
    };
    let outcome = download::download(
        dict,
        &dir,
        DownloadOptions {
            base_url: Some(&base_url),
            force: verification == Verification::Differs,
            progress: Some(&mut progress),
            ..Default::default()
        },
    )
    .with_context(|| format!("{base_url} から {} を取得できません", dict.file))?;
    match outcome {
        Outcome::Present(path) => println!("辞書はそろっています: {}", path.display()),
        Outcome::Downloaded(path) => println!("\n辞書を取得しました: {}", path.display()),
    }
    Ok(())
}

/// 辞書の置き場所 (このクレートは `tools/dict` にある)。
fn dict_dir() -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("ワークスペースの中にある");
    root.join("crates").join("gnn-schedule").join("dict")
}
