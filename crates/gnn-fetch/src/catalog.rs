//! カタログ (game-news-nexus-config) の取り込み。
//!
//! 既定では GitHub の codeload から main の tar.gz を取り、`groups.yaml` と
//! `sources/<グループ>/<id>.yaml` だけを取り出す。ETag を覚えておき、変わっていなければ
//! 取り直さない。手元のディレクトリ (リポジトリを clone したもの) も指定できる。
//!
//! ここでは中身を解釈しない。解釈と検査は `gnn_core::catalog` が行う。

use std::io::Read as _;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context as _, Result, bail};
use reqwest::StatusCode;
use reqwest::header::{ETAG, IF_NONE_MATCH};

use gnn_core::catalog::{MAX_CATALOG_FILES, is_catalog_file};
use gnn_core::yaml::MAX_YAML_BYTES;

use crate::http::{HttpFetcher, NetworkPolicy, read_capped};

/// 取り込む tar.gz の上限。
const MAX_ARCHIVE_BYTES: usize = 16 * 1024 * 1024;

/// 取り込んだカタログのファイル。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogFiles {
    /// リポジトリの中のパス (`/` 区切り) と中身。
    pub files: Vec<(String, String)>,
    /// 次回の条件付き取得に使う ETag。
    pub etag: Option<String>,
    /// 取り込んだ版 (GitHub の tar.gz ならコミットの SHA)。
    pub revision: Option<String>,
}

/// 取り込みの結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogFetch {
    /// 前回から変わっていない。
    NotModified,
    Updated(CatalogFiles),
}

/// カタログの置き場所。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CatalogLocation {
    /// tar.gz の URL。
    Archive(String),
    /// 手元のディレクトリ。
    Directory(PathBuf),
}

impl CatalogLocation {
    /// 設定の文字列から決める。`http://` `https://` で始まれば URL、それ以外はディレクトリ。
    pub fn parse(text: &str) -> Self {
        let text = text.trim();
        if text.starts_with("https://") || text.starts_with("http://") {
            CatalogLocation::Archive(text.to_string())
        } else {
            CatalogLocation::Directory(PathBuf::from(text.trim_start_matches("file:///")))
        }
    }
}

/// カタログを取り込む。`etag` は前回の取り込みで返した値。
///
/// カタログの置き場所は利用者が決めるものなので、取得先は制限しない。
/// 取り込んだソースを実行するときは [`NetworkPolicy::PublicOnly`] で制限する。
pub async fn fetch_catalog(
    http: &HttpFetcher,
    location: &CatalogLocation,
    etag: Option<&str>,
) -> Result<CatalogFetch> {
    match location {
        CatalogLocation::Directory(dir) => {
            let dir = dir.clone();
            let files = tokio::task::spawn_blocking(move || read_directory(&dir))
                .await
                .context("カタログの読み込みが中断しました")??;
            Ok(CatalogFetch::Updated(CatalogFiles {
                files,
                etag: None,
                revision: None,
            }))
        }
        CatalogLocation::Archive(url) => {
            let mut request = http.client(NetworkPolicy::Any).get(url);
            if let Some(etag) = etag {
                request = request.header(IF_NONE_MATCH, etag);
            }
            let response = request
                .send()
                .await
                .with_context(|| format!("カタログ {url} に接続できません"))?;
            if response.status() == StatusCode::NOT_MODIFIED {
                return Ok(CatalogFetch::NotModified);
            }
            if !response.status().is_success() {
                bail!(
                    "カタログ {url} を取得できません (HTTP {})",
                    response.status().as_u16()
                );
            }
            let etag = response
                .headers()
                .get(ETAG)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let bytes = read_capped(response, MAX_ARCHIVE_BYTES)
                .await
                .with_context(|| format!("カタログ {url} を読めません"))?;
            let (files, revision) = tokio::task::spawn_blocking(move || unpack(&bytes))
                .await
                .context("カタログの展開が中断しました")??;
            Ok(CatalogFetch::Updated(CatalogFiles {
                files,
                etag,
                revision,
            }))
        }
    }
}

/// 展開したデータ全体の上限。読み飛ばすファイルや pax の拡張ヘッダも数える。
const MAX_UNPACKED_BYTES: u64 = 64 * 1024 * 1024;
/// tar の中の項目数の上限。
const MAX_ENTRIES: usize = 20_000;

/// 読んだ量が上限を超えたら誤りにする。展開すると膨れる tar.gz で手元の資源を使い切らないように。
struct Limited<R> {
    inner: R,
    remaining: u64,
}

impl<R: std::io::Read> std::io::Read for Limited<R> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.remaining == 0 {
            // ちょうど上限で終わっていれば終わり、まだ続くなら誤り。
            let mut probe = [0u8; 1];
            return match self.inner.read(&mut probe)? {
                0 => Ok(0),
                _ => Err(std::io::Error::other(format!(
                    "カタログの tar.gz を展開すると {} MB を超えます",
                    MAX_UNPACKED_BYTES / (1024 * 1024)
                ))),
            };
        }
        let max = buf
            .len()
            .min(self.remaining.min(usize::MAX as u64) as usize);
        let read = self.inner.read(&mut buf[..max])?;
        self.remaining -= read as u64;
        Ok(read)
    }
}

/// 展開したファイル (パスと中身) と版。
type Unpacked = (Vec<(String, String)>, Option<String>);

/// tar.gz を展開し、カタログのファイルだけを取り出す。
///
/// GitHub の tar.gz は `<リポジトリ>-<ブランチ>/` の下にファイルが入り、先頭の
/// pax グローバルヘッダの comment にコミットの SHA が入る。
fn unpack(bytes: &[u8]) -> Result<Unpacked> {
    unpack_with_limit(bytes, MAX_UNPACKED_BYTES)
}

fn unpack_with_limit(bytes: &[u8], limit: u64) -> Result<Unpacked> {
    let reader = Limited {
        inner: flate2::read::GzDecoder::new(bytes),
        remaining: limit,
    };
    let mut archive = tar::Archive::new(reader);
    let mut files = Vec::new();
    let mut revision = None;
    for (index, entry) in archive
        .entries()
        .context("カタログの tar.gz を読めません")?
        .enumerate()
    {
        if index >= MAX_ENTRIES {
            bail!("カタログの tar.gz の項目が多すぎます (上限 {MAX_ENTRIES})");
        }
        let mut entry = entry.context("カタログの tar.gz が壊れています")?;
        let kind = entry.header().entry_type();
        if kind == tar::EntryType::XGlobalHeader {
            if let Some(extensions) = entry.pax_extensions()? {
                for extension in extensions.flatten() {
                    if extension.key() == Ok("comment") {
                        revision = extension.value().ok().map(|value| value.trim().to_string());
                    }
                }
            }
            continue;
        }
        if !kind.is_file() {
            continue;
        }
        let path = entry.path()?.into_owned();
        let Some(relative) = strip_top_dir(&path) else {
            continue;
        };
        if !is_catalog_file(&relative) {
            continue;
        }
        if files.len() >= MAX_CATALOG_FILES {
            bail!("カタログのファイルが多すぎます (上限 {MAX_CATALOG_FILES})");
        }
        let mut text = String::new();
        (&mut entry)
            .take(MAX_YAML_BYTES as u64 + 1)
            .read_to_string(&mut text)
            .with_context(|| format!("{relative} を UTF-8 として読めません"))?;
        if text.len() > MAX_YAML_BYTES {
            bail!(
                "{relative} が大きすぎます (上限 {} KB)",
                MAX_YAML_BYTES / 1024
            );
        }
        files.push((relative, text));
    }
    files.sort();
    Ok((files, revision))
}

/// 先頭のディレクトリを外し、`/` 区切りのパスにする。`..` などを含むパスは捨てる。
fn strip_top_dir(path: &Path) -> Option<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => parts.push(part.to_str()?.to_string()),
            Component::CurDir => {}
            _ => return None,
        }
    }
    if parts.len() < 2 {
        return None;
    }
    Some(parts[1..].join("/"))
}

/// 手元のディレクトリからカタログのファイルを読む。
fn read_directory(root: &Path) -> Result<Vec<(String, String)>> {
    gnn_core::catalog::read_dir(root)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn archive(entries: &[(&str, &str)], comment: Option<&str>) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        if let Some(comment) = comment {
            let record = format!("comment={comment}\n");
            let record = format!(
                "{} {record}",
                record.len() + 1 + (record.len() + 1).to_string().len()
            );
            let mut header = tar::Header::new_ustar();
            header.set_entry_type(tar::EntryType::XGlobalHeader);
            header.set_size(record.len() as u64);
            header.set_cksum();
            builder
                .append_data(&mut header, "pax_global_header", record.as_bytes())
                .unwrap();
        }
        for (path, text) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(text.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, path, text.as_bytes())
                .unwrap();
        }
        let tar = builder.into_inner().unwrap();
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        std::io::Write::write_all(&mut encoder, &tar).unwrap();
        encoder.finish().unwrap()
    }

    #[test]
    fn unpacks_only_catalog_files() {
        let bytes = archive(
            &[
                ("config-main/README.md", "# readme"),
                ("config-main/groups.yaml", "groups: []"),
                ("config-main/sources/dmm/a.yaml", "id: a"),
                ("config-main/sources/dmm/b.yml", "id: b"),
                ("config-main/sources/dmm/notes.txt", "x"),
                ("config-main/schema/source.schema.json", "{}"),
            ],
            Some("0123456789abcdef"),
        );
        let (files, revision) = unpack(&bytes).unwrap();
        let paths: Vec<&str> = files.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(
            paths,
            vec!["groups.yaml", "sources/dmm/a.yaml", "sources/dmm/b.yml"]
        );
        assert_eq!(revision.as_deref(), Some("0123456789abcdef"));
    }

    #[test]
    fn stops_at_the_unpacked_size_limit() {
        let big = "x".repeat(200_000);
        let bytes = archive(&[("config-main/README.md", big.as_str())], None);
        assert!(unpack_with_limit(&bytes, 100_000).is_err());
        assert!(unpack_with_limit(&bytes, 10_000_000).is_ok());
    }

    #[test]
    fn rejects_paths_outside_the_archive() {
        assert_eq!(strip_top_dir(Path::new("repo/../../etc/passwd")), None);
        assert_eq!(strip_top_dir(Path::new("repo")), None);
        assert_eq!(
            strip_top_dir(Path::new("repo/sources/dmm/a.yaml")).as_deref(),
            Some("sources/dmm/a.yaml")
        );
    }

    #[test]
    fn reads_a_local_checkout() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sources/dmm")).unwrap();
        std::fs::write(dir.path().join("groups.yaml"), "groups: []").unwrap();
        std::fs::write(dir.path().join("sources/dmm/a.yaml"), "id: a").unwrap();
        std::fs::write(dir.path().join("README.md"), "x").unwrap();
        let files = read_directory(dir.path()).unwrap();
        let paths: Vec<&str> = files.iter().map(|(path, _)| path.as_str()).collect();
        assert_eq!(paths, vec!["groups.yaml", "sources/dmm/a.yaml"]);
        assert!(read_directory(&dir.path().join("missing")).is_err());
    }

    #[test]
    fn parses_locations() {
        assert_eq!(
            CatalogLocation::parse(" https://example.com/a.tar.gz "),
            CatalogLocation::Archive("https://example.com/a.tar.gz".into())
        );
        assert_eq!(
            CatalogLocation::parse(r"C:\GitHub\game-news-nexus-config"),
            CatalogLocation::Directory(PathBuf::from(r"C:\GitHub\game-news-nexus-config"))
        );
    }
}
