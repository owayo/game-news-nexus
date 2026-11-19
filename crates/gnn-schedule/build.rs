//! 埋め込む辞書 (`dict/ipadic.hsd`) を、`dict/bundled.json` の大きさと SHA-256 で照合する。
//!
//! 通信はしない。辞書がないか中身が違うときは、`mise run dict` を案内してビルドを止める。

use std::env;
use std::fmt::Write as _;
use std::fs;
use std::io::Read as _;
use std::path::PathBuf;

use serde::Deserialize;
use sha2::{Digest as _, Sha256};

#[derive(Deserialize)]
struct Bundled {
    hasami_tag: String,
    dictionary: Dictionary,
}

#[derive(Deserialize)]
struct Dictionary {
    file: String,
    size: u64,
    sha256: String,
}

fn main() {
    println!("cargo:rerun-if-changed=dict/bundled.json");
    let dir =
        PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR")).join("dict");
    let manifest = dir.join("bundled.json");
    let text = fs::read_to_string(&manifest)
        .unwrap_or_else(|e| panic!("{} を読めません: {e}", manifest.display()));
    let bundled: Bundled = serde_json::from_str(&text)
        .unwrap_or_else(|e| panic!("{} の書き方が正しくありません: {e}", manifest.display()));
    let dict = bundled.dictionary;
    let path = dir.join(&dict.file);
    println!("cargo:rerun-if-changed=dict/{}", dict.file);

    let stop = |reason: String| -> ! {
        panic!(
            "{reason}。`mise run dict` を実行して辞書を用意してください \
             (crates/gnn-schedule/dict/README.md)"
        )
    };
    let metadata =
        fs::metadata(&path).unwrap_or_else(|_| stop(format!("{} がありません", path.display())));
    if metadata.len() != dict.size {
        stop(format!(
            "{} の大きさ ({}) が bundled.json ({}) と違います",
            path.display(),
            metadata.len(),
            dict.size
        ));
    }
    let actual = sha256(&path);
    if actual != dict.sha256 {
        stop(format!(
            "{} の SHA-256 ({actual}) が bundled.json と違います",
            path.display()
        ));
    }
    // 抽出器の識別子 (`extractor_id`) に入れる。辞書が変われば保存済みの記事を読み直す。
    println!("cargo:rustc-env=GNN_HASAMI_TAG={}", bundled.hasami_tag);
    println!("cargo:rustc-env=GNN_DICT_SHA256={}", dict.sha256);
}

fn sha256(path: &std::path::Path) -> String {
    let mut file =
        fs::File::open(path).unwrap_or_else(|e| panic!("{} を開けません: {e}", path.display()));
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = file
            .read(&mut buffer)
            .unwrap_or_else(|e| panic!("{} を読めません: {e}", path.display()));
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    hasher
        .finalize()
        .iter()
        .fold(String::new(), |mut hex, byte| {
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}
