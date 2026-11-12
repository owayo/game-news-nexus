//! `catalog/` (game-news-nexus-config の写し) をビルド時に埋め込む。

use std::fs;
use std::path::{Path, PathBuf};

fn main() {
    let root = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"))
        .join("catalog");
    println!("cargo:rerun-if-changed=catalog");

    let mut files = Vec::new();
    collect(&root, &root, &mut files);
    files.sort();

    let mut code = String::from(
        "/// ビルド時に埋め込んだカタログのファイル (パスと中身)。\npub const BUNDLED_FILES: &[(&str, &str)] = &[\n",
    );
    for relative in files {
        let absolute = root.join(&relative);
        code.push_str(&format!(
            "    ({:?}, include_str!({:?})),\n",
            relative,
            absolute.display().to_string()
        ));
    }
    code.push_str("];\n");
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("bundled_catalog.rs");
    fs::write(out, code).expect("埋め込むカタログを書き出せません");
}

/// `groups.yaml` と `sources/**/*.yaml` を集める。
fn collect(root: &Path, dir: &Path, files: &mut Vec<String>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            println!("cargo:rerun-if-changed={}", path.display());
            collect(root, &path, files);
            continue;
        }
        let relative = path
            .strip_prefix(root)
            .expect("catalog の中")
            .to_string_lossy()
            .replace('\\', "/");
        let is_source = relative.starts_with("sources/") && relative.ends_with(".yaml");
        if relative == "groups.yaml" || is_source {
            files.push(relative);
        }
    }
}
