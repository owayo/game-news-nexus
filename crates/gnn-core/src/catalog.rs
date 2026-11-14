//! 取得定義のカタログ。
//!
//! ゲームごとの取得定義は、別のリポジトリ (game-news-nexus-config) で 1 ソース 1 ファイル
//! (`sources/<グループ>/<id>.yaml`) として管理する。アプリはその内容を取り込み、利用者は
//! 好きなソースを on/off する。ビルド時に埋め込んだ同梱版を初期値にし、起動後に
//! リポジトリから新しい版を取り込む。
//!
//! 取り込みはファイルごとに判断する。読めないファイルや、このアプリより新しい版を求める
//! ファイル (`schema`・`min_app_version`) は、同じ ID の前の版を使い続ける。
//!
//! カタログには第三者の PR で入った設定が載るため、秘密の値の参照と、手元のネットワーク
//! (localhost・プライベートアドレス) への取得を禁じる ([`check_entry`])。実行時にも
//! 取得先を制限する (`gnn-fetch` の `NetworkPolicy::PublicOnly`)。

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use crate::net;
use crate::source::{
    APP_VERSION, ConfigIssue, SCHEMA_VERSION, SourceConfig, TemplatePart, compare_versions,
    is_valid_id, template_parts,
};
use crate::subscription::{Subscription, Subscriptions};
use crate::yaml;

/// 取得先を固定したカタログ。GitHub のリポジトリの main を tar.gz で取る。
pub const CATALOG_URL: &str =
    "https://codeload.github.com/owayo/game-news-nexus-config/tar.gz/refs/heads/main";
/// カタログのリポジトリ。案内に使う。
pub const CATALOG_REPOSITORY: &str = "https://github.com/owayo/game-news-nexus-config";
/// グループの一覧のファイル。
pub const GROUPS_FILE: &str = "groups.yaml";
/// カタログの読み方の版。読み方を変えて、前の読み方で取り込んだ控え (`catalog.json`) を使い続けて
/// はいけなくなったら上げる (控えを捨てて同梱版から始め、ETag を使わずに取り直す)。
///
/// 2: 成人向けのソース (`adult`・`general_edition`) を読むようになった。前の読み方では、成人向けの
/// 取得先を一般向けのソースとして取り込んでいた。
pub const CATALOG_READER_VERSION: u32 = 2;

include!(concat!(env!("OUT_DIR"), "/bundled_catalog.rs"));

/// カタログのグループ。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogGroup {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// 初めて起動したときに、このグループのソースを on にする。
    #[serde(default)]
    pub default: bool,
}

/// `groups.yaml`。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupsFile {
    #[serde(default)]
    pub groups: Vec<CatalogGroup>,
}

/// カタログの 1 ソース。
#[derive(Debug, Clone, PartialEq)]
pub struct CatalogEntry {
    /// グループの ID (`sources/<グループ>/` のディレクトリ名)。
    pub group: String,
    /// リポジトリの中のパス。
    pub path: String,
    pub source: SourceConfig,
    /// ファイルの中身。コメントを含めて画面に出し、「自分用にコピー」に使う。
    pub text: String,
    /// 中身の SHA-256 (先頭 12 文字)。
    pub hash: String,
    /// この ID の新しい版は成人向け (`adult: true`) だが、このアプリでは読めないので前の版 (この定義) を
    /// 使い続けている。前の版が成人向けと書いていなくても成人向けとして扱う (`source.adult` も `true`
    /// にする)。カタログの控えにも残す ([`Catalog::forced_adult_ids`])。
    pub forced_adult: bool,
}

/// このアプリでは使えないソース (より新しいアプリが必要)。
#[derive(Debug, Clone, PartialEq)]
pub struct UnavailableEntry {
    pub group: String,
    pub path: String,
    pub id: String,
    pub name: String,
    /// 必要なアプリの版。書式の版が新しいだけなら `None`。
    pub required_app_version: Option<String>,
    /// 成人向け (`adult: true`) と書かれている。成人向けの版を使わない利用者には、名前も出さない。
    pub adult: bool,
}

/// 問題の種類。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProblemKind {
    /// 書式の誤りや決まりに合わない設定。
    Invalid,
    /// このアプリより新しいアプリが必要。
    NeedsNewerApp,
}

/// 読めなかったファイルや、決まりに合わないソース。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CatalogProblem {
    pub path: String,
    pub kind: ProblemKind,
    pub message: String,
    /// 前の版を使い続けているか。
    pub kept_previous: bool,
}

impl std::fmt::Display for CatalogProblem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.path, self.message)?;
        if self.kept_previous {
            f.write_str(" (前の版を使っています)")?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalog {
    groups: Vec<CatalogGroup>,
    groups_text: Option<String>,
    entries: Vec<CatalogEntry>,
    unavailable: Vec<UnavailableEntry>,
    problems: Vec<CatalogProblem>,
    editions: Editions,
}

/// 成人向けの版と一般向けの版の組。カタログの定義だけで決める (手元の定義では変えない)。
#[derive(Debug, Clone, Default, PartialEq)]
struct Editions {
    /// 一般向けの版の代わりになる成人向けの版の ID → 一般向けの版の ID。
    pairs: HashMap<String, String>,
    /// `general_edition` が決まりに合わない成人向けの版。同意していても使わない。
    invalid: HashSet<String>,
}

impl Catalog {
    /// 一般向けの版の記事を引き継ぐ成人向けの版の組 (一般向けの版の ID, 成人向けの版の ID)。
    /// 決まりに合う組で、成人向けの版が `inherit_articles` を書いたものだけ
    /// ([`crate::Store::inherit_articles`] で、組ごとに 1 度だけ記事を移す)。
    pub fn inherited_editions(&self) -> Vec<(String, String)> {
        let mut editions: Vec<(String, String)> = self
            .entries
            .iter()
            .filter(|entry| entry.source.inherit_articles)
            .filter_map(|entry| {
                self.editions
                    .pairs
                    .get(&entry.source.id)
                    .map(|general| (general.clone(), entry.source.id.clone()))
            })
            .collect();
        editions.sort();
        editions
    }
}

/// 互換性の判断のために先に読む項目。未知の項目は無視する。
#[derive(Debug, Default, Deserialize)]
struct Header {
    #[serde(default)]
    schema: Option<u32>,
    #[serde(default)]
    min_app_version: Option<String>,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    name: Option<String>,
    /// 読めない (より新しいアプリが要る) ソースでも、成人向けかは分かるようにする。
    #[serde(default)]
    adult: Option<bool>,
}

enum Loaded {
    Ok(Box<CatalogEntry>),
    Unavailable(UnavailableEntry, String),
    /// 読めない理由と、互換性の項目から読めた成人向けの区分。
    Invalid(String, bool),
}

impl Catalog {
    /// リポジトリのファイル (パスと中身) から作る。
    ///
    /// 対象は `groups.yaml` と `sources/<グループ>/<id>.yaml`。それ以外のファイルは無視する。
    /// `previous` を渡すと、読めなかったソースと新しいアプリが必要なソースは、
    /// `previous` の同じ ID の版を使い続ける。
    pub fn from_files<P, T>(
        files: impl IntoIterator<Item = (P, T)>,
        previous: Option<&Catalog>,
    ) -> Self
    where
        P: AsRef<str>,
        T: AsRef<str>,
    {
        Self::from_files_for(files, previous, APP_VERSION)
    }

    /// `app_version` のアプリとして読む。テストと検査用。
    pub fn from_files_for<P, T>(
        files: impl IntoIterator<Item = (P, T)>,
        previous: Option<&Catalog>,
        app_version: &str,
    ) -> Self
    where
        P: AsRef<str>,
        T: AsRef<str>,
    {
        let mut catalog = Catalog::default();
        let mut seen_groups = false;
        for (path, text) in files {
            let path = path.as_ref().replace('\\', "/");
            let text = text.as_ref();
            if path == GROUPS_FILE {
                seen_groups = true;
                catalog.load_groups(path, text, previous);
                continue;
            }
            let Some((group, id)) = source_path(&path) else {
                if path.starts_with("sources/")
                    && (path.ends_with(".yaml") || path.ends_with(".yml"))
                {
                    catalog.problems.push(CatalogProblem {
                        path,
                        kind: ProblemKind::Invalid,
                        message: "sources/<グループ>/<id>.yaml に置いてください".into(),
                        kept_previous: false,
                    });
                }
                continue;
            };
            let (group, id) = (group.to_string(), id.to_string());
            match load_entry(&path, &group, &id, text, app_version) {
                Loaded::Ok(entry) => catalog.push_entry(*entry),
                Loaded::Unavailable(unavailable, message) => {
                    let kept = catalog.keep_previous(previous, &id, unavailable.adult);
                    if !kept {
                        catalog.unavailable.push(unavailable);
                    }
                    catalog.problems.push(CatalogProblem {
                        path,
                        kind: ProblemKind::NeedsNewerApp,
                        message,
                        kept_previous: kept,
                    });
                }
                Loaded::Invalid(message, adult) => {
                    let kept = catalog.keep_previous(previous, &id, adult);
                    catalog.problems.push(CatalogProblem {
                        path,
                        kind: ProblemKind::Invalid,
                        message,
                        kept_previous: kept,
                    });
                }
            }
        }
        if !seen_groups && let Some(previous) = previous {
            catalog.groups = previous.groups.clone();
            catalog.groups_text = previous.groups_text.clone();
        }
        catalog.sort();
        catalog.check_editions();
        catalog
    }

    /// 成人向けの版の組を決め、決まりに合わないものを問題として出す。
    fn check_editions(&mut self) {
        let (editions, problems) = self.editions_and_problems();
        self.editions = editions;
        self.problems.extend(problems);
    }

    /// 成人向けの版の `general_edition` は、同じグループ・同じお知らせの出し先の、成人向けでない
    /// カタログのソースを指し、1 つの一般向けの版を指す成人向けの版は 1 つだけにする。
    /// 合わないものは組にせず、同意していても使わない (一般向けの版はそのまま使う)。
    /// 指す先が重なるときは、どれが正しいか決められないので、どれも使わない。
    fn editions_and_problems(&self) -> (Editions, Vec<CatalogProblem>) {
        let mut problems = Vec::new();
        let mut invalid = HashSet::new();
        let mut claims: BTreeMap<&str, Vec<&CatalogEntry>> = BTreeMap::new();
        for entry in &self.entries {
            let Some(general) = entry.source.general_edition.as_deref() else {
                continue;
            };
            let message = match self.get(general) {
                None => Some(format!(
                    "general_edition の `{general}` がカタログにありません"
                )),
                Some(target) if target.source.adult => Some(format!(
                    "general_edition の `{general}` は成人向けのソースです。一般向けの版を指してください"
                )),
                Some(target) if target.group != entry.group => Some(format!(
                    "general_edition の `{general}` はグループ `{}` にあります。同じグループに置いてください",
                    target.group
                )),
                Some(target) if target.source.channel != entry.source.channel => Some(format!(
                    "general_edition の `{general}` とお知らせの出し先 (channel) を同じにしてください"
                )),
                Some(_) => {
                    claims.entry(general).or_default().push(entry);
                    None
                }
            };
            if let Some(message) = message {
                invalid.insert(entry.source.id.clone());
                problems.push(CatalogProblem {
                    path: entry.path.clone(),
                    kind: ProblemKind::Invalid,
                    message,
                    kept_previous: false,
                });
            }
        }
        let mut pairs = HashMap::new();
        for (general, entries) in claims {
            if let [entry] = entries.as_slice() {
                pairs.insert(entry.source.id.clone(), general.to_string());
                continue;
            }
            let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
            for entry in entries {
                invalid.insert(entry.source.id.clone());
                problems.push(CatalogProblem {
                    path: entry.path.clone(),
                    kind: ProblemKind::Invalid,
                    message: format!(
                        "`{general}` の成人向けの版が複数あります ({})。1 つにしてください",
                        paths.join("・")
                    ),
                    kept_previous: false,
                });
            }
        }
        (Editions { pairs, invalid }, problems)
    }

    fn load_groups(&mut self, path: String, text: &str, previous: Option<&Catalog>) {
        match yaml::from_str::<GroupsFile>(text) {
            Ok(file) => {
                self.groups = file.groups;
                self.groups_text = Some(text.to_string());
            }
            Err(err) => {
                let previous = previous.filter(|previous| previous.groups_text.is_some());
                if let Some(previous) = previous {
                    self.groups = previous.groups.clone();
                    self.groups_text = previous.groups_text.clone();
                }
                self.problems.push(CatalogProblem {
                    path,
                    kind: ProblemKind::Invalid,
                    message: format!("書式が誤っています: {err}"),
                    kept_previous: previous.is_some(),
                });
            }
        }
    }

    fn push_entry(&mut self, entry: CatalogEntry) {
        if let Some(existing) = self
            .entries
            .iter()
            .find(|existing| existing.source.id == entry.source.id)
        {
            self.problems.push(CatalogProblem {
                path: entry.path.clone(),
                kind: ProblemKind::Invalid,
                message: format!(
                    "ID `{}` が {} と重なっています",
                    entry.source.id, existing.path
                ),
                kept_previous: false,
            });
            return;
        }
        self.entries.push(entry);
    }

    /// 前の版の同じ ID のソースを使い続ける。あれば `true`。新しい版が成人向け (`adult`) なら、
    /// 前の版も成人向けとして扱う (同意していない利用者に、前の版で取得し続けない)。
    fn keep_previous(&mut self, previous: Option<&Catalog>, id: &str, adult: bool) -> bool {
        match previous.and_then(|previous| previous.get(id)) {
            Some(entry) => {
                let mut entry = entry.clone();
                if adult && !entry.source.adult {
                    entry.source.adult = true;
                    entry.forced_adult = true;
                }
                self.push_entry(entry);
                true
            }
            None => false,
        }
    }

    /// 新しい版が成人向けなので、前の版を成人向けとして使っているソースの ID。カタログの控えに残し、
    /// 控えから読み直したときに [`Catalog::force_adult`] で戻す。
    pub fn forced_adult_ids(&self) -> Vec<String> {
        self.entries
            .iter()
            .filter(|entry| entry.forced_adult)
            .map(|entry| entry.source.id.clone())
            .collect()
    }

    /// 控えに残した ID のソースを成人向けとして扱う ([`Catalog::forced_adult_ids`])。
    pub fn force_adult(&mut self, ids: &[String]) {
        for entry in &mut self.entries {
            if ids.contains(&entry.source.id) && !entry.source.adult {
                entry.source.adult = true;
                entry.forced_adult = true;
            }
        }
        // 成人向けになったソースを指す組は、決まりに合わなくなる。
        self.editions = self.editions_and_problems().0;
    }

    /// グループを `groups.yaml` の順に、グループの中は名前の順に並べる。
    fn sort(&mut self) {
        let order: HashMap<String, usize> = self
            .groups
            .iter()
            .enumerate()
            .map(|(index, group)| (group.id.clone(), index))
            .collect();
        let rank = |group: &str| order.get(group).copied().unwrap_or(usize::MAX);
        self.entries.sort_by(|a, b| {
            rank(&a.group)
                .cmp(&rank(&b.group))
                .then_with(|| a.group.cmp(&b.group))
                .then_with(|| a.source.name.cmp(&b.source.name))
                .then_with(|| a.source.id.cmp(&b.source.id))
        });
        self.unavailable.sort_by(|a, b| {
            rank(&a.group)
                .cmp(&rank(&b.group))
                .then_with(|| a.name.cmp(&b.name))
        });
    }

    /// ビルド時に埋め込んだカタログ。
    pub fn bundled() -> Self {
        Self::from_files(BUNDLED_FILES.iter().copied(), None)
    }

    pub fn groups(&self) -> &[CatalogGroup] {
        &self.groups
    }

    pub fn entries(&self) -> &[CatalogEntry] {
        &self.entries
    }

    pub fn unavailable(&self) -> &[UnavailableEntry] {
        &self.unavailable
    }

    pub fn problems(&self) -> &[CatalogProblem] {
        &self.problems
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, id: &str) -> Option<&CatalogEntry> {
        self.entries.iter().find(|entry| entry.source.id == id)
    }

    /// グループの表示名。`groups.yaml` になければ ID。
    pub fn group_name<'a>(&'a self, id: &'a str) -> &'a str {
        self.groups
            .iter()
            .find(|group| group.id == id)
            .map_or(id, |group| group.name.as_str())
    }

    pub fn group(&self, id: &str) -> Option<&CatalogGroup> {
        self.groups.iter().find(|group| group.id == id)
    }

    /// グループの ID を `groups.yaml` の順に。`groups.yaml` にないグループは後ろに付ける。
    pub fn group_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.groups.iter().map(|group| group.id.clone()).collect();
        let used = self
            .entries
            .iter()
            .map(|entry| &entry.group)
            .chain(self.unavailable.iter().map(|entry| &entry.group));
        for group in used {
            if !ids.contains(group) {
                ids.push(group.clone());
            }
        }
        ids
    }

    /// 初めて起動したときの購読。`default: true` のグループのソースを on にする。
    pub fn default_subscriptions(&self) -> Subscriptions {
        let mut subscriptions = Subscriptions::default();
        self.adopt_new_entries(&mut subscriptions);
        subscriptions
    }

    /// まだ購読にないカタログのソースを購読に足す。`default: true` のグループのソースは on、
    /// ほかは off で足す。on にしたソースの ID を返す。
    ///
    /// 足したソースは「見た」ことになり、利用者が off にすればそのまま off で残る。
    /// カタログに後から加わったゲーム (例: 既定のグループに入った新作) を、初めて見たときだけ
    /// 既定どおりに on にするために使う。
    pub fn adopt_new_entries(&self, subscriptions: &mut Subscriptions) -> Vec<String> {
        let defaults: Vec<&str> = self
            .groups
            .iter()
            .filter(|group| group.default)
            .map(|group| group.id.as_str())
            .collect();
        let mut enabled = Vec::new();
        for entry in &self.entries {
            let id = &entry.source.id;
            // 一般向けの版の代わりに使う成人向けの版は、一般向けの版の購読を使うので足さない。
            if subscriptions.get(id).is_some() || entry.source.general_edition.is_some() {
                continue;
            }
            // 成人向けだけのゲームは、既定のグループでも off で足す (成人向けの版を使う同意で、
            // ほかのゲームまでまとめて取得し始めないように)。
            let on = defaults.contains(&entry.group.as_str()) && !entry.source.adult;
            subscriptions.items.push(Subscription {
                enabled: on,
                ..Subscription::new(id.clone())
            });
            if on {
                enabled.push(id.clone());
            }
        }
        enabled
    }

    /// 控えとして保存するファイル。前の版を使い続けているソースも含む。
    pub fn files(&self) -> Vec<(String, String)> {
        let mut files = Vec::new();
        if let Some(text) = &self.groups_text {
            files.push((GROUPS_FILE.to_string(), text.clone()));
        }
        files.extend(
            self.entries
                .iter()
                .map(|entry| (entry.path.clone(), entry.text.clone())),
        );
        files
    }
}

/// カタログに含めるファイルか。`path` はリポジトリの中のパス (`/` 区切り)。
pub fn is_catalog_file(path: &str) -> bool {
    path == GROUPS_FILE
        || (path.starts_with("sources/") && (path.ends_with(".yaml") || path.ends_with(".yml")))
}

/// 読み込むファイル数の上限。
pub const MAX_CATALOG_FILES: usize = 5000;

/// 手元のディレクトリ (リポジトリを clone したもの) からカタログのファイルを読む。
pub fn read_dir(root: &std::path::Path) -> anyhow::Result<Vec<(String, String)>> {
    use anyhow::{Context as _, bail};

    if !root.join("sources").is_dir() {
        bail!("{} に sources ディレクトリがありません", root.display());
    }
    let mut files = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let entries =
            std::fs::read_dir(&dir).with_context(|| format!("{} を読めません", dir.display()))?;
        for entry in entries {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            if kind.is_dir() {
                // `.git` などの隠しディレクトリは見ない。
                if !entry.file_name().to_string_lossy().starts_with('.') {
                    stack.push(path);
                }
                continue;
            }
            if !kind.is_file() {
                continue;
            }
            let Some(relative) = path
                .strip_prefix(root)
                .ok()
                .and_then(|relative| relative.to_str())
                .map(|relative| relative.replace('\\', "/"))
            else {
                continue;
            };
            if !is_catalog_file(&relative) {
                continue;
            }
            if files.len() >= MAX_CATALOG_FILES {
                bail!("カタログのファイルが多すぎます (上限 {MAX_CATALOG_FILES})");
            }
            if entry.metadata()?.len() > yaml::MAX_YAML_BYTES as u64 {
                bail!(
                    "{relative} が大きすぎます (上限 {} KB)",
                    yaml::MAX_YAML_BYTES / 1024
                );
            }
            let text = std::fs::read_to_string(&path)
                .with_context(|| format!("{relative} を UTF-8 として読めません"))?;
            files.push((relative, text));
        }
    }
    files.sort();
    Ok(files)
}

/// `sources/<グループ>/<id>.yaml` からグループと ID を取る。
fn source_path(path: &str) -> Option<(&str, &str)> {
    let rest = path.strip_prefix("sources/")?;
    let stem = rest
        .strip_suffix(".yaml")
        .or_else(|| rest.strip_suffix(".yml"))?;
    let (group, id) = stem.split_once('/')?;
    (!id.contains('/')).then_some((group, id))
}

fn load_entry(path: &str, group: &str, id: &str, text: &str, app_version: &str) -> Loaded {
    // 1. 互換性に関わる項目だけを先に読む。新しい書式の未知の項目があっても読める。
    let header = match yaml::from_str::<Header>(text) {
        Ok(header) => header,
        Err(err) => return Loaded::Invalid(format!("書式が誤っています: {err}"), false),
    };
    let adult = header.adult.unwrap_or(false);
    let unavailable = |required: Option<String>| UnavailableEntry {
        group: group.to_string(),
        path: path.to_string(),
        id: header.id.clone().unwrap_or_else(|| id.to_string()),
        name: header.name.clone().unwrap_or_else(|| id.to_string()),
        required_app_version: required,
        adult,
    };
    if let Some(required) = &header.min_app_version
        && compare_versions(required, app_version) == Ordering::Greater
    {
        return Loaded::Unavailable(
            unavailable(Some(required.clone())),
            format!("アプリ {required} 以降が必要です (このアプリは {app_version})"),
        );
    }
    if let Some(schema) = header.schema
        && schema > SCHEMA_VERSION
    {
        return Loaded::Unavailable(
            unavailable(None),
            format!(
                "書式の版 {schema} はこのアプリでは読めません (読めるのは {SCHEMA_VERSION} まで)"
            ),
        );
    }
    // 2. 厳格な型で全体を読む。
    let source = match SourceConfig::from_yaml(text) {
        Ok(source) => source,
        Err(err) => return Loaded::Invalid(format!("書式が誤っています: {err}"), adult),
    };
    let entry = CatalogEntry {
        group: group.to_string(),
        path: path.to_string(),
        source,
        text: text.to_string(),
        hash: hash(text),
        forced_adult: false,
    };
    // 3. 中身とカタログの決まりを確かめる。
    let issues = check_entry(&entry, id, app_version);
    if !issues.is_empty() {
        return Loaded::Invalid(
            issues
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(" / "),
            adult,
        );
    }
    Loaded::Ok(Box::new(entry))
}

fn hash(text: &str) -> String {
    let digest = Sha256::digest(text.as_bytes());
    hex::encode(digest)[..12].to_string()
}

/// カタログのソースに課す決まり。`expected_id` はファイル名から決まる ID、
/// `app_version` は読むアプリ (検査なら検査に使うツール) の版。
///
/// CSS セレクタは `gnn-core` では検査できないので、`gnn-fetch` の検査と合わせて使う。
pub fn check_entry(entry: &CatalogEntry, expected_id: &str, app_version: &str) -> Vec<ConfigIssue> {
    let source = &entry.source;
    let mut issues = source.validate();
    let mut push = |field: &str, message: String| {
        issues.push(ConfigIssue {
            field: field.to_string(),
            message,
        })
    };

    if source.id != expected_id {
        push(
            "id",
            format!(
                "ファイル名と同じ `{expected_id}` にしてください (今は `{}`)",
                source.id
            ),
        );
    }
    if !is_valid_id(&entry.group) {
        push(
            "group",
            format!(
                "グループのディレクトリ名 `{}` は英小文字・数字・_・- で",
                entry.group
            ),
        );
    }

    // 版: 使っている機能に必要な版以上で、読むアプリの版以下にする。
    let required = source.required_app_version();
    match &source.min_app_version {
        None => push(
            "min_app_version",
            format!("カタログのソースには min_app_version を書いてください (`'{required}'` 以上)"),
        ),
        Some(version) => {
            if compare_versions(version, required) == Ordering::Less {
                let features = source.later_features().join("・");
                push(
                    "min_app_version",
                    format!("使っている機能 ({features}) には {required} 以上が必要です"),
                );
            }
            if compare_versions(version, app_version) == Ordering::Greater {
                push(
                    "min_app_version",
                    format!("{version} はこのアプリ ({app_version}) より新しい版です"),
                );
            }
        }
    }

    // 秘密の値をほかのサイトへ送らせないよう、カタログでは参照を禁じる。
    if source.uses_secrets() {
        push(
            "request.headers",
            "カタログのソースでは {{secret:...}} を使えません (利用者の秘密の値を送ってしまうため)"
                .into(),
        );
    }

    for (field, template) in source.request_urls() {
        if let Some(message) = check_template_url(template) {
            push(&field, message);
        }
    }
    for (field, value) in [("homepage", &source.homepage), ("icon", &source.icon)] {
        if let Some(value) = value
            && let Some(message) = check_template_url(value)
        {
            push(field, message);
        }
    }
    issues
}

/// テンプレートの URL が、インターネット上の http / https を指すか。問題があれば説明を返す。
fn check_template_url(template: &str) -> Option<String> {
    let Ok(parts) = template_parts(template) else {
        return None; // テンプレートの誤りは validate が報告する。
    };
    // `{{item.url}}` のように URL 全体を差し込む形は、取得のときに確かめる。
    if matches!(parts.as_slice(), [TemplatePart::Placeholder(_)]) {
        return None;
    }
    let resolved: String = parts
        .iter()
        .map(|part| match part {
            TemplatePart::Text(text) => *text,
            TemplatePart::Placeholder(_) => "x",
        })
        .collect();
    let url = url::Url::parse(&resolved).ok()?;
    net::check_public_url(&url).err()
}

/// 使う候補 (手元の上書きを反映した定義・出どころ・グループ)。
///
/// 成人向けの区分と版の組はカタログの定義で決める: カタログが成人向けなら、手元の上書きで
/// `adult` を外しても成人向けのまま (上書きで成人向けにはできる)。`general_edition` は
/// カタログの定義のものにそろえる。手元だけのソースは `general_edition` を使えない (組にしない)。
fn candidates(
    catalog: &Catalog,
    local: &[SourceConfig],
) -> Vec<(SourceConfig, SourceOrigin, Option<String>)> {
    let local_by_id: BTreeMap<&str, &SourceConfig> = local
        .iter()
        .map(|source| (source.id.as_str(), source))
        .collect();
    let mut candidates = Vec::new();
    for entry in catalog.entries() {
        let id = entry.source.id.as_str();
        candidates.push(match local_by_id.get(id) {
            Some(local) => {
                let mut config = (*local).clone();
                config.adult |= entry.source.adult;
                config.general_edition = entry.source.general_edition.clone();
                (config, SourceOrigin::Override, Some(entry.group.clone()))
            }
            None => (
                entry.source.clone(),
                SourceOrigin::Catalog,
                Some(entry.group.clone()),
            ),
        });
    }
    for source in local {
        if catalog.get(&source.id).is_none() {
            candidates.push((source.clone(), SourceOrigin::Local, None));
        }
    }
    candidates
}

/// 成人向けのソースのうち、使わない (同意していない・版の組が決まりに合わない) もの。記事を
/// 開く・通知する・書き出すときにも隠す。`resolved` は同じカタログと手元の定義での [`resolve`] の結果。
pub fn hidden_adult_sources(
    catalog: &Catalog,
    local: &[SourceConfig],
    resolved: &[ResolvedSource],
) -> BTreeSet<String> {
    candidates(catalog, local)
        .into_iter()
        .filter(|(config, _, _)| config.adult)
        .map(|(config, _, _)| config.id)
        .filter(|id| resolved.iter().all(|source| source.id() != id))
        .collect()
}

/// 購読・手元の定義と、成人向けの版を使うか (`adult`。設定で同意したか) から、使うソースを決める。
///
/// カタログのソースをカタログの並びで、続けて手元のソースを並べる。カタログと同じ ID の
/// 手元のソースは、カタログの定義の代わりに使う (定義全体を置き換える)。
/// カタログのソースは購読で on にしたものだけ、手元のソースは購読で off にしない限り使う。
///
/// 成人向けのソース (`adult: true`) は、`adult` のときだけ入れる。一般向けの版
/// (`general_edition`) の代わりになる成人向けの版は、`adult` のときに一般向けの版の位置に入り、
/// 一般向けの版の購読 (on/off・アイコン・取得間隔など) を使う ([`ResolvedSource::subscription_id`])。
/// そのとき一般向けの版は入れない。版の組が決まりに合わない成人向けの版は使わない
/// (一般向けの版をそのまま使う)。
pub fn resolve(
    catalog: &Catalog,
    subscriptions: &Subscriptions,
    local: &[SourceConfig],
    adult: bool,
) -> Vec<ResolvedSource> {
    let candidates = candidates(catalog, local);
    let editions = &catalog.editions;
    // 一般向けの版の ID → 代わりに使う成人向けの版 (候補の位置)。
    let replacements: HashMap<&str, usize> = if adult {
        candidates
            .iter()
            .enumerate()
            .filter(|(_, (config, origin, _))| *origin != SourceOrigin::Local && config.adult)
            .filter_map(|(index, (config, _, _))| {
                editions
                    .pairs
                    .get(&config.id)
                    .map(|general| (general.as_str(), index))
            })
            .collect()
    } else {
        HashMap::new()
    };

    let mut resolved = Vec::new();
    for (index, (config, origin, _)) in candidates.iter().enumerate() {
        let id = config.id.as_str();
        // 使わない成人向けのソース・一般向けの版の位置に入れる成人向けの版・組の誤った成人向けの版・
        // 版の組を書いた手元だけのソース。
        let skip = config.adult
            && (!adult
                || editions.pairs.contains_key(id)
                || editions.invalid.contains(id)
                || (*origin == SourceOrigin::Local && config.general_edition.is_some()));
        if skip {
            continue;
        }
        // `id` の購読で入れる。手元だけのソースは購読で off にしない限り使い、カタログのソースは
        // on にしたものだけ使う。
        let subscription = subscriptions.get(id);
        let enabled = if *origin == SourceOrigin::Local {
            subscription.is_none_or(|subscription| subscription.enabled)
        } else {
            subscription.is_some_and(|subscription| subscription.enabled)
        };
        // 成人向けの版を使うなら、定義はその版のもの。
        let (config, origin, group) = &candidates[replacements.get(id).copied().unwrap_or(index)];
        let mut source = ResolvedSource::new(
            config.clone(),
            *origin,
            group.clone(),
            id,
            subscription,
            enabled,
        );
        // 通信の許可は、一般向けの版の購読から引き継がず、実際に使う定義の購読で決める。
        source.allow_private_network = !origin.is_catalog()
            && subscriptions
                .get(&config.id)
                .is_some_and(|subscription| subscription.allow_private_network);
        resolved.push(source);
    }
    resolved
}

/// どこから来たソースか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceOrigin {
    /// カタログの定義をそのまま使う。
    Catalog,
    /// カタログのソースを、手元の定義で置き換えて使う。
    Override,
    /// 手元だけのソース。
    Local,
}

impl SourceOrigin {
    /// 第三者が書いた定義か。取得先を公開のネットワークに限り、秘密の値を渡さない。
    pub fn is_catalog(self) -> bool {
        self == SourceOrigin::Catalog
    }

    pub fn label(self) -> &'static str {
        match self {
            SourceOrigin::Catalog => "カタログ",
            SourceOrigin::Override => "カタログを手元で上書き",
            SourceOrigin::Local => "手元のソース",
        }
    }
}

/// 実際に取得と表示に使うソース。
#[derive(Debug, Clone, PartialEq)]
pub struct ResolvedSource {
    /// 購読の上書き (アイコン・取得間隔・期間) を反映した定義。
    pub config: SourceConfig,
    pub origin: SourceOrigin,
    /// カタログのグループ。手元だけのソースは `None`。
    pub group: Option<String>,
    /// 購読 (on/off・アイコン・取得間隔などの上書き) の ID。ふつうはソースの ID と同じで、
    /// 一般向けの版の代わりに使う成人向けの版では、一般向けの版の ID。
    pub subscription_id: String,
    pub enabled: bool,
    /// 手元のネットワークへの取得を許すか (手元の定義だけ)。
    pub allow_private_network: bool,
}

impl ResolvedSource {
    fn new(
        mut config: SourceConfig,
        origin: SourceOrigin,
        group: Option<String>,
        subscription_id: &str,
        subscription: Option<&Subscription>,
        enabled: bool,
    ) -> Self {
        if let Some(subscription) = subscription {
            if subscription.icon.is_some() {
                config.icon = subscription.icon.clone();
            }
            if subscription.interval_minutes.is_some() {
                config.interval_minutes = subscription.interval_minutes;
            }
            if subscription.max_age_days.is_some() {
                config.max_age_days = subscription.max_age_days;
            }
        }
        Self {
            config,
            origin,
            group,
            subscription_id: subscription_id.to_string(),
            enabled,
            allow_private_network: !origin.is_catalog()
                && subscription.is_some_and(|subscription| subscription.allow_private_network),
        }
    }

    pub fn id(&self) -> &str {
        &self.config.id
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const MIST: &str = r#"
schema: 2
min_app_version: '26.11.6'
id: mist
name: ミスト
lists:
  - kind: json
    request:
      url: 'https://example.com/api/news'
    items: /articles
    fields:
      id: /id
      title: /title
"#;

    fn with_id(text: &str, id: &str) -> String {
        text.replace("id: mist", &format!("id: {id}"))
    }

    const GROUPS: &str = "groups:\n  - id: dmm\n    name: DMM GAMES\n    default: true\n  - id: media\n    name: ゲームメディア\n";

    #[test]
    fn bundled_catalog_is_complete_and_valid() {
        let catalog = Catalog::bundled();
        assert_eq!(catalog.problems(), &[], "{:#?}", catalog.problems());
        assert!(catalog.entries().len() >= 20);
        assert!(catalog.get("genshin_impact").is_some());
        assert_eq!(catalog.group_name("dmm"), "DMM GAMES");
        let defaults = catalog.default_subscriptions();
        let enabled = |id: &str| defaults.get(id).is_some_and(|item| item.enabled);
        for id in [
            "mist_train_girls",
            "genshin_impact",
            "honkai_star_rail",
            "zenless_zone_zero",
        ] {
            assert!(enabled(id), "{id}");
        }
        // 既定でないグループのソースも「見た」ものとして off で入る。成人向けだけのゲームも off で入る。
        assert!(defaults.get("4gamer").is_some_and(|item| !item.enabled));
        assert!(
            defaults
                .get("escalation_heroines")
                .is_some_and(|item| !item.enabled)
        );
        // 一般向けの版の代わりに使う成人向けの版は、一般向けの版の購読を使うので入れない。
        let editions = catalog.inherited_editions();
        assert!(editions.contains(&("dot_abyss".to_string(), "dot_abyss_r18".to_string())));
        let paired = catalog
            .entries()
            .iter()
            .filter(|entry| entry.source.general_edition.is_some())
            .count();
        assert!(paired >= 7);
        assert_eq!(defaults.items.len(), catalog.entries().len() - paired);
        for entry in catalog.entries() {
            assert!(entry.source.min_app_version.is_some(), "{}", entry.path);
            assert!(
                defaults.get(&entry.source.id).is_some() != entry.source.general_edition.is_some()
            );
        }
        // 同意していなければ、成人向けのソースは使わない。
        let resolved = resolve(&catalog, &defaults, &[], false);
        assert!(resolved.iter().all(|source| !source.config.adult));
        assert!(resolved.iter().any(|source| source.id() == "dot_abyss"));
        // 同意していれば、一般向けの版の代わりに成人向けの版を一般向けの版の購読で使う。
        let resolved = resolve(&catalog, &defaults, &[], true);
        let dot = resolved
            .iter()
            .find(|source| source.id() == "dot_abyss_r18")
            .unwrap();
        assert_eq!(dot.subscription_id, "dot_abyss");
        assert!(dot.enabled);
        assert!(resolved.iter().all(|source| source.id() != "dot_abyss"));
    }

    #[test]
    fn adopts_new_entries_once() {
        let catalog = Catalog::from_files_for(
            [
                ("groups.yaml", GROUPS.to_string()),
                ("sources/dmm/mist.yaml", MIST.to_string()),
                ("sources/dmm/brand_new.yaml", with_id(MIST, "brand_new")),
                ("sources/media/feed.yaml", with_id(MIST, "feed")),
            ],
            None,
            "26.11.6",
        );
        // 前から使っていた人: mist を off にしている。brand_new と feed は初めて見る。
        let mut subscriptions = Subscriptions {
            items: vec![Subscription {
                enabled: false,
                ..Subscription::new("mist")
            }],
        };
        assert_eq!(
            catalog.adopt_new_entries(&mut subscriptions),
            vec!["brand_new".to_string()]
        );
        assert!(!subscriptions.get("mist").unwrap().enabled);
        assert!(subscriptions.get("brand_new").unwrap().enabled);
        assert!(!subscriptions.get("feed").unwrap().enabled);
        // 2 回目は何もしない。off にしたものは off のまま。
        subscriptions.set_enabled("brand_new", false);
        assert!(catalog.adopt_new_entries(&mut subscriptions).is_empty());
        assert!(!subscriptions.get("brand_new").unwrap().enabled);
    }

    #[test]
    fn reports_problems_per_file() {
        let secret = MIST.replace(
            "      url: 'https://example.com/api/news'",
            "      url: 'https://example.com/api/news'\n      headers:\n        Cookie: '{{secret:dmm}}'",
        );
        let local = MIST.replace(
            "https://example.com/api/news",
            "http://127.0.0.1:8080/admin",
        );
        let no_version = MIST.replace("min_app_version: '26.11.6'\n", "");
        let catalog = Catalog::from_files_for(
            [
                ("groups.yaml", GROUPS.to_string()),
                ("sources/dmm/mist.yaml", MIST.to_string()),
                ("sources/dmm/wrong.yaml", MIST.to_string()),
                ("sources/dmm/secret.yaml", with_id(&secret, "secret")),
                ("sources/dmm/local.yaml", with_id(&local, "local")),
                ("sources/dmm/broken.yaml", "id: [".to_string()),
                (
                    "sources/dmm/noversion.yaml",
                    with_id(&no_version, "noversion"),
                ),
                ("sources/flat.yaml", MIST.to_string()),
                ("README.md", "# ignored".to_string()),
            ],
            None,
            "26.11.6",
        );
        assert_eq!(catalog.entries().len(), 1);
        let paths: Vec<&str> = catalog
            .problems()
            .iter()
            .map(|problem| problem.path.as_str())
            .collect();
        assert_eq!(
            paths,
            vec![
                "sources/dmm/wrong.yaml",
                "sources/dmm/secret.yaml",
                "sources/dmm/local.yaml",
                "sources/dmm/broken.yaml",
                "sources/dmm/noversion.yaml",
                "sources/flat.yaml",
            ]
        );
        let message = |index: usize| catalog.problems()[index].message.clone();
        assert!(message(0).contains("ファイル名と同じ"), "{}", message(0));
        assert!(message(1).contains("secret"), "{}", message(1));
        assert!(message(2).contains("手元のネットワーク"), "{}", message(2));
        assert!(message(4).contains("min_app_version"), "{}", message(4));
    }

    #[test]
    fn keeps_previous_versions_of_unreadable_or_newer_sources() {
        let previous = Catalog::from_files_for(
            [
                ("groups.yaml", GROUPS.to_string()),
                ("sources/dmm/mist.yaml", MIST.to_string()),
                ("sources/dmm/other.yaml", with_id(MIST, "other")),
                ("sources/dmm/gone.yaml", with_id(MIST, "gone")),
            ],
            None,
            "26.11.6",
        );
        assert_eq!(previous.entries().len(), 3);

        // 新しいアプリが必要な版と、未知の項目を含む版が来た。消えたソースは消す。
        let newer = MIST
            .replace("min_app_version: '26.11.6'", "min_app_version: '27.1.0'")
            .replace(
                "    items: /articles",
                "    items: /articles\n    future_option: 1",
            );
        let unknown =
            with_id(MIST, "other").replace("    items: /articles", "    itemz: /articles");
        let brand_new = with_id(&newer, "brand_new");
        let catalog = Catalog::from_files_for(
            [
                ("groups.yaml", "groups: [".to_string()),
                ("sources/dmm/mist.yaml", newer),
                ("sources/dmm/other.yaml", unknown),
                ("sources/dmm/brand_new.yaml", brand_new),
            ],
            Some(&previous),
            "26.11.6",
        );
        let ids: Vec<&str> = catalog
            .entries()
            .iter()
            .map(|entry| entry.source.id.as_str())
            .collect();
        assert_eq!(ids, vec!["mist", "other"]);
        assert_eq!(catalog.get("mist").unwrap().text, MIST);
        assert_eq!(catalog.group_name("dmm"), "DMM GAMES");
        let unavailable: Vec<(&str, Option<&str>)> = catalog
            .unavailable()
            .iter()
            .map(|entry| (entry.id.as_str(), entry.required_app_version.as_deref()))
            .collect();
        assert_eq!(unavailable, vec![("brand_new", Some("27.1.0"))]);
        let kinds: Vec<(&str, &ProblemKind, bool)> = catalog
            .problems()
            .iter()
            .map(|problem| (problem.path.as_str(), &problem.kind, problem.kept_previous))
            .collect();
        assert_eq!(
            kinds,
            vec![
                ("groups.yaml", &ProblemKind::Invalid, true),
                ("sources/dmm/mist.yaml", &ProblemKind::NeedsNewerApp, true),
                ("sources/dmm/other.yaml", &ProblemKind::Invalid, true),
                (
                    "sources/dmm/brand_new.yaml",
                    &ProblemKind::NeedsNewerApp,
                    false
                ),
            ]
        );
        // 控えには使い続けている版が入る。
        let files = catalog.files();
        assert_eq!(files[0].0, "groups.yaml");
        assert!(
            files
                .iter()
                .any(|(path, text)| path == "sources/dmm/mist.yaml" && text == MIST)
        );
    }

    #[test]
    fn resolves_subscriptions_overrides_and_local_sources() {
        let catalog = Catalog::from_files_for(
            [
                ("sources/dmm/mist.yaml", MIST.to_string()),
                ("sources/dmm/other.yaml", with_id(MIST, "other")),
                ("sources/media/off.yaml", with_id(MIST, "off")),
            ],
            None,
            "26.11.6",
        );
        let mut override_source = catalog.get("other").unwrap().source.clone();
        override_source.name = "手元で直した".into();
        let mut mine = override_source.clone();
        mine.id = "mine".into();
        let mut paused = override_source.clone();
        paused.id = "paused".into();
        let subscriptions = Subscriptions {
            items: vec![
                Subscription {
                    icon: Some("C:/icons/mist.png".into()),
                    interval_minutes: Some(15),
                    allow_private_network: true,
                    ..Subscription::new("mist")
                },
                Subscription::new("other"),
                Subscription {
                    allow_private_network: true,
                    ..Subscription::new("mine")
                },
                Subscription {
                    enabled: false,
                    ..Subscription::new("paused")
                },
                Subscription::new("removed_upstream"),
            ],
        };
        let resolved = resolve(
            &catalog,
            &subscriptions,
            &[override_source, mine, paused],
            false,
        );
        let summary: Vec<(&str, SourceOrigin, bool, bool)> = resolved
            .iter()
            .map(|source| {
                (
                    source.id(),
                    source.origin,
                    source.enabled,
                    source.allow_private_network,
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                ("mist", SourceOrigin::Catalog, true, false),
                ("other", SourceOrigin::Override, true, false),
                ("off", SourceOrigin::Catalog, false, false),
                ("mine", SourceOrigin::Local, true, true),
                ("paused", SourceOrigin::Local, false, false),
            ]
        );
        assert_eq!(
            resolved[0].config.icon.as_deref(),
            Some("C:/icons/mist.png")
        );
        assert_eq!(resolved[0].config.interval_minutes, Some(15));
        assert_eq!(resolved[1].config.name, "手元で直した");
        assert_eq!(resolved[1].group.as_deref(), Some("dmm"));
        assert!(
            resolved
                .iter()
                .all(|source| source.subscription_id == source.id())
        );
    }

    /// 成人向けの版のファイル。`general` があれば `general_edition` に書く。
    fn adult(id: &str, general: Option<&str>) -> String {
        let mut text = with_id(MIST, id).replace("name: ミスト", "name: ミスト (成人向け)");
        text.push_str("adult: true\n");
        if let Some(general) = general {
            text.push_str(&format!("general_edition: {general}\n"));
        }
        text
    }

    fn edition_catalog() -> Catalog {
        Catalog::from_files_for(
            [
                ("groups.yaml", GROUPS.to_string()),
                ("sources/dmm/mist.yaml", MIST.to_string()),
                ("sources/dmm/mist_r18.yaml", adult("mist_r18", Some("mist"))),
                ("sources/dmm/only_r18.yaml", adult("only_r18", None)),
                ("sources/dmm/plain.yaml", with_id(MIST, "plain")),
            ],
            None,
            "26.11.6",
        )
    }

    #[test]
    fn adult_editions_are_used_only_with_consent_and_share_the_general_subscription() {
        let catalog = edition_catalog();
        assert_eq!(catalog.problems(), &[]);
        // 一般向けの版の代わりになる成人向けの版は購読に足さない。成人向けだけのゲームは、既定の
        // グループでも off で足す (同意しても、ほかのゲームまでまとめて取得し始めない)。
        let mut subscriptions = catalog.default_subscriptions();
        let mut adopted: Vec<(&str, bool)> = subscriptions
            .items
            .iter()
            .map(|item| (item.id.as_str(), item.enabled))
            .collect();
        adopted.sort_unstable();
        assert_eq!(
            adopted,
            vec![("mist", true), ("only_r18", false), ("plain", true)]
        );
        subscriptions.set_enabled("only_r18", true);
        subscriptions.entry("mist").icon = Some("C:/icons/mist.png".into());
        subscriptions.set_enabled("plain", false);

        let summary = |adult: bool| -> Vec<(String, String, bool)> {
            resolve(&catalog, &subscriptions, &[], adult)
                .iter()
                .map(|source| {
                    (
                        source.id().to_string(),
                        source.subscription_id.clone(),
                        source.enabled,
                    )
                })
                .collect()
        };
        // 同意していなければ、成人向けのソースは入れない (取得もソース画面への表示もしない)。
        assert_eq!(
            summary(false),
            vec![
                ("mist".into(), "mist".into(), true),
                ("plain".into(), "plain".into(), false),
            ]
        );
        // 同意していれば、一般向けの版の位置に成人向けの版が入り、一般向けの版の購読を使う。
        assert_eq!(
            summary(true),
            vec![
                ("mist_r18".into(), "mist".into(), true),
                ("plain".into(), "plain".into(), false),
                ("only_r18".into(), "only_r18".into(), true),
            ]
        );
        let resolved = resolve(&catalog, &subscriptions, &[], true);
        assert_eq!(resolved[0].config.name, "ミスト (成人向け)");
        assert_eq!(
            resolved[0].config.icon.as_deref(),
            Some("C:/icons/mist.png")
        );
        // 一般向けの版を off にすると、成人向けの版も off。
        subscriptions.set_enabled("mist", false);
        assert!(!resolve(&catalog, &subscriptions, &[], true)[0].enabled);
        // 使わない成人向けのソースは、記事を開く・通知するときにも隠す。
        assert_eq!(
            hidden_adult_sources(
                &catalog,
                &[],
                &resolve(&catalog, &subscriptions, &[], false)
            ),
            BTreeSet::from(["mist_r18".to_string(), "only_r18".to_string()])
        );
        assert!(
            hidden_adult_sources(&catalog, &[], &resolve(&catalog, &subscriptions, &[], true))
                .is_empty()
        );
    }

    #[test]
    fn local_definitions_cannot_lift_or_invent_editions() {
        let catalog = edition_catalog();
        let mut subscriptions = catalog.default_subscriptions();
        subscriptions.set_enabled("only_r18", true);
        // 手元だけの成人向けのソースも、同意したときだけ使う。版の組 (general_edition) は書けない。
        let mut mine = catalog.get("plain").unwrap().source.clone();
        mine.id = "mine".into();
        mine.adult = true;
        let mut linked = mine.clone();
        linked.id = "linked".into();
        linked.general_edition = Some("plain".into());
        // カタログの成人向けのソースを、手元の上書きで一般向けにはできない。
        let mut lifted = catalog.get("only_r18").unwrap().source.clone();
        lifted.adult = false;
        let local = [mine, linked, lifted];
        let ids = |adult: bool| -> Vec<String> {
            resolve(&catalog, &subscriptions, &local, adult)
                .iter()
                .map(|source| source.id().to_string())
                .collect()
        };
        assert_eq!(ids(false), vec!["mist", "plain"]);
        assert_eq!(ids(true), vec!["mist_r18", "plain", "only_r18", "mine"]);
        let hidden = hidden_adult_sources(
            &catalog,
            &local,
            &resolve(&catalog, &subscriptions, &local, false),
        );
        assert!(
            hidden.contains("only_r18") && hidden.contains("mine") && hidden.contains("linked")
        );
    }

    #[test]
    fn edition_links_must_point_to_one_general_source_in_the_same_group() {
        let catalog = Catalog::from_files_for(
            [
                ("groups.yaml", GROUPS.to_string()),
                ("sources/dmm/mist.yaml", MIST.to_string()),
                ("sources/dmm/a_r18.yaml", adult("a_r18", Some("mist"))),
                ("sources/dmm/b_r18.yaml", adult("b_r18", Some("mist"))),
                ("sources/dmm/c_r18.yaml", adult("c_r18", Some("missing"))),
                ("sources/dmm/d_r18.yaml", adult("d_r18", Some("solo"))),
                ("sources/dmm/solo.yaml", with_id(MIST, "solo")),
                (
                    "sources/dmm/paired_r18.yaml",
                    adult("paired_r18", Some("solo2")),
                ),
                ("sources/dmm/solo2.yaml", with_id(MIST, "solo2")),
                ("sources/media/e_r18.yaml", adult("e_r18", Some("solo"))),
                (
                    "sources/dmm/general.yaml",
                    format!("{}general_edition: mist\n", with_id(MIST, "general")),
                ),
            ],
            None,
            "26.11.6",
        );
        let problems: Vec<(&str, &str)> = catalog
            .problems()
            .iter()
            .map(|problem| (problem.path.as_str(), problem.message.as_str()))
            .collect();
        // 成人向けでないソースの general_edition は、ファイルの検査で断る (取り込まない)。
        assert!(problems[0].0.ends_with("general.yaml"));
        assert!(problems[0].1.contains("adult: true"));
        assert!(catalog.get("general").is_none());
        // 指す先がない・別のグループ・重なるものは、取り込んだうえで問題として出す。重なるときは
        // どれが正しいか決められないので、先に並んだものも使わない。
        let mut rest: Vec<&str> = problems[1..].iter().map(|(path, _)| *path).collect();
        rest.sort_unstable();
        assert_eq!(
            rest,
            vec![
                "sources/dmm/a_r18.yaml",
                "sources/dmm/b_r18.yaml",
                "sources/dmm/c_r18.yaml",
                "sources/media/e_r18.yaml",
            ],
            "{problems:#?}"
        );
        let mut subscriptions = catalog.default_subscriptions();
        for id in [
            "mist", "solo", "solo2", "a_r18", "b_r18", "c_r18", "d_r18", "e_r18",
        ] {
            subscriptions.set_enabled(id, true);
        }
        // 組の誤った成人向けの版は、同意していても使わず、一般向けの版をそのまま使う。
        let ids: Vec<String> = resolve(&catalog, &subscriptions, &[], true)
            .iter()
            .map(|source| format!("{}<{}", source.id(), source.subscription_id))
            .collect();
        assert_eq!(
            ids,
            vec!["mist<mist", "d_r18<solo", "paired_r18<solo2"],
            "d_r18 と paired_r18 だけが組になる"
        );
    }

    #[test]
    fn previous_versions_stay_adult_when_the_new_version_is_adult() {
        // 前の版は一般向け。新しい版は成人向けだが、このアプリでは読めない (より新しいアプリが要る)。
        let old = Catalog::from_files_for(
            [
                ("groups.yaml", GROUPS.to_string()),
                ("sources/dmm/mist.yaml", MIST.to_string()),
            ],
            None,
            "26.11.6",
        );
        let newer = adult("mist", None).replace("'26.11.6'", "'99.1.0'");
        let mut catalog = Catalog::from_files_for(
            [
                ("groups.yaml", GROUPS.to_string()),
                ("sources/dmm/mist.yaml", newer),
            ],
            Some(&old),
            "26.11.6",
        );
        let entry = catalog.get("mist").unwrap();
        assert!(entry.source.adult && entry.forced_adult);
        assert_eq!(catalog.forced_adult_ids(), vec!["mist".to_string()]);
        let mut subscriptions = Subscriptions::default();
        subscriptions.set_enabled("mist", true);
        assert!(resolve(&catalog, &subscriptions, &[], false).is_empty());
        // 控えには前の版の中身を残すので、読み直したら成人向けの印を戻す。
        let mut reread = Catalog::from_files_for(catalog.files(), None, "26.11.6");
        assert!(!reread.get("mist").unwrap().source.adult);
        reread.force_adult(&catalog.forced_adult_ids());
        assert!(reread.get("mist").unwrap().source.adult);
        assert!(resolve(&reread, &subscriptions, &[], false).is_empty());
        catalog.force_adult(&[]);
        assert!(catalog.get("mist").unwrap().source.adult);
    }

    #[test]
    fn unavailable_adult_sources_are_marked() {
        let newer = adult("later", None).replace("'26.11.6'", "'99.1.0'");
        let catalog = Catalog::from_files_for([("sources/dmm/later.yaml", newer)], None, "26.11.6");
        assert_eq!(catalog.unavailable().len(), 1);
        assert!(catalog.unavailable()[0].adult);
    }
}
