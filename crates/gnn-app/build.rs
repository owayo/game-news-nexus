use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=assets/app.rc");
    println!("cargo:rerun-if-changed=assets/icons/app.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        write_version_header();
        // gpui はウィンドウのアイコンとして実行ファイルのアイコン資源 1 番を読む。版の情報 (VERSIONINFO) は、
        // タスクマネージャー・「スタートアップ アプリ」・インストーラーが閉じるアプリの一覧に出る名前と版になる。
        embed_resource::compile("assets/app.rc", embed_resource::NONE)
            .manifest_optional()
            .expect("アイコンと版の資源を埋め込めません");
    }
}

/// app.rc が読む版の定義 (`version.rh`) を OUT_DIR に書く。embed-resource は OUT_DIR を rc.exe の
/// インクルードの場所に入れる。版は Cargo.toml の版 (`yy.m.連番`) から作る。
fn write_version_header() {
    let version = std::env::var("CARGO_PKG_VERSION").expect("CARGO_PKG_VERSION がありません");
    let parts: Vec<u16> = [
        "CARGO_PKG_VERSION_MAJOR",
        "CARGO_PKG_VERSION_MINOR",
        "CARGO_PKG_VERSION_PATCH",
    ]
    .iter()
    .map(|name| {
        std::env::var(name)
            .ok()
            .and_then(|value| value.parse().ok())
            .unwrap_or_else(|| panic!("{name} を版の数として読めません"))
    })
    .collect();
    let header = format!(
        "#define GNN_FILEVERSION {},{},{},0\n#define GNN_VERSION \"{version}\"\n",
        parts[0], parts[1], parts[2]
    );
    let out =
        PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR がありません")).join("version.rh");
    std::fs::write(&out, header).expect("版の定義を書けません");
}
