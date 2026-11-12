//! アプリ設定・購読・手元の取得定義・秘密情報のファイルと保存場所。
//!
//! ```text
//! config/
//!   settings.toml        アプリ設定
//!   subscriptions.toml   ソースの on/off と手元だけの上書き
//!   secrets.toml         ヘッダの {{secret:名前}} に入れる値 (共有しない)
//!   sources/<id>.yaml    手元の取得定義 (カタログと同じ形式。同じ ID ならカタログを置き換える)
//! data/
//!   catalog.json         取り込んだカタログの控え
//! ```
//!
//! 書き出すファイル (ここのファイルと旧形式の `game_info.jsonl`) は `write_atomic` で書く。書き込みごとに
//! 別の一時ファイルに書いてから置き換えるので、書き込みの途中で落ちても元のファイルは壊れず、読み手は
//! 書きかけを見ない。落ちたときに残った一時ファイルは、同じファイルを次に書いたときに消す。

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{self, Write as _};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime};

use anyhow::{Context as _, Result, bail};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::catalog::Catalog;
use crate::source::SourceConfig;
use crate::source_v1;
use crate::subscription::{Subscription, Subscriptions};

/// 手元の定義に書く、エディタの補完用の JSON Schema の場所。
pub const SCHEMA_URL: &str =
    "https://raw.githubusercontent.com/owayo/game-news-nexus-config/main/schema/source.schema.json";

/// アプリが使うディレクトリ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppPaths {
    config_dir: PathBuf,
    data_dir: PathBuf,
    cache_dir: PathBuf,
}

impl AppPaths {
    /// 環境変数 `GNN_HOME` があればその下、なければ OS 標準の場所を使う。
    pub fn resolve() -> Result<Self> {
        if let Some(home) = std::env::var_os("GNN_HOME") {
            return Ok(Self::under(PathBuf::from(home)));
        }
        let dirs = directories::ProjectDirs::from("", "", "game-news-nexus")
            .context("ホームディレクトリを特定できません")?;
        Ok(Self {
            config_dir: dirs.config_dir().to_path_buf(),
            data_dir: dirs.data_dir().to_path_buf(),
            cache_dir: dirs.cache_dir().to_path_buf(),
        })
    }

    /// 1 つのディレクトリの下にまとめる。持ち運び用とテスト用。
    pub fn under(root: PathBuf) -> Self {
        Self {
            config_dir: root.join("config"),
            data_dir: root.join("data"),
            cache_dir: root.join("cache"),
        }
    }

    pub fn ensure(&self) -> Result<()> {
        for dir in [
            &self.config_dir,
            &self.data_dir,
            &self.cache_dir,
            &self.local_sources_dir(),
            &self.images_dir(),
            &self.icons_dir(),
            &self.logs_dir(),
        ] {
            fs::create_dir_all(dir)
                .with_context(|| format!("{} を作成できません", dir.display()))?;
        }
        Ok(())
    }

    pub fn config_dir(&self) -> &Path {
        &self.config_dir
    }

    pub fn data_dir(&self) -> &Path {
        &self.data_dir
    }

    pub fn cache_dir(&self) -> &Path {
        &self.cache_dir
    }

    pub fn settings_file(&self) -> PathBuf {
        self.config_dir.join("settings.toml")
    }

    pub fn subscriptions_file(&self) -> PathBuf {
        self.config_dir.join("subscriptions.toml")
    }

    /// 手元の取得定義 (`<id>.yaml`) を置くディレクトリ。
    pub fn local_sources_dir(&self) -> PathBuf {
        self.config_dir.join("sources")
    }

    pub fn local_source_file(&self, id: &str) -> PathBuf {
        self.local_sources_dir().join(format!("{id}.yaml"))
    }

    /// 書式 1 の取得定義。移行のためだけに読む。
    pub fn legacy_sources_file(&self) -> PathBuf {
        self.config_dir.join("sources.toml")
    }

    pub fn secrets_file(&self) -> PathBuf {
        self.config_dir.join("secrets.toml")
    }

    pub fn database_file(&self) -> PathBuf {
        self.data_dir.join("nexus.db")
    }

    /// 取り込んだカタログの控え。
    pub fn catalog_cache_file(&self) -> PathBuf {
        self.data_dir.join("catalog.json")
    }

    pub fn images_dir(&self) -> PathBuf {
        self.cache_dir.join("images")
    }

    /// 利用者が登録したゲームのアイコン。
    pub fn icons_dir(&self) -> PathBuf {
        self.data_dir.join("icons")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    /// 二重起動を防ぐロック。起動中のアプリが持ち続ける。
    pub fn lock_file(&self) -> PathBuf {
        self.data_dir.join("instance.lock")
    }

    /// 起動中のアプリが、後から起動したアプリからの依頼を待ち受けるポート。
    pub fn instance_port_file(&self) -> PathBuf {
        self.data_dir.join("instance.port")
    }
}

/// アプリ設定 (`settings.toml`)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct AppSettings {
    /// 取得間隔の既定値 (分)。
    pub interval_minutes: u32,
    /// これより古い記事は取り込まない (日)。
    pub max_age_days: u32,
    /// 記事を残す日数。0 なら消さない。
    pub retention_days: u32,
    /// 起動したらすぐ取得する。
    pub fetch_on_startup: bool,
    /// 起動時にウィンドウを出さず、トレイに入れる。
    pub start_in_tray: bool,
    /// ウィンドウを閉じてもトレイに残り、定期取得を続ける。
    pub close_to_tray: bool,
    /// 新着を Windows の通知で知らせる。
    pub notify_new_entries: bool,
    /// 今日・明日の日付が本文にある記事を一覧の上に出す。
    pub prioritize_today: bool,
    pub theme: ThemePreference,
    /// 同時に取得するソースの数。
    pub concurrency: usize,
    /// 既定の User-Agent。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_agent: Option<String>,
    /// 指定すると、取得のたびに旧形式の `game_info.jsonl` をこのディレクトリに書き出す。
    #[serde(skip_serializing_if = "Option::is_none")]
    pub legacy_export_dir: Option<PathBuf>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            interval_minutes: 60,
            max_age_days: 60,
            retention_days: 365,
            fetch_on_startup: true,
            start_in_tray: false,
            close_to_tray: true,
            notify_new_entries: true,
            prioritize_today: true,
            theme: ThemePreference::System,
            concurrency: 4,
            user_agent: None,
            legacy_export_dir: None,
        }
    }
}

impl AppSettings {
    pub fn interval_for(&self, source: &SourceConfig) -> u32 {
        source
            .interval_minutes
            .unwrap_or(self.interval_minutes)
            .max(1)
    }

    pub fn max_age_for(&self, source: &SourceConfig) -> u32 {
        source.max_age_days.unwrap_or(self.max_age_days)
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThemePreference {
    #[default]
    System,
    Light,
    Dark,
}

/// `secrets.toml`。ヘッダの `{{secret:名前}}` に入れる値。共有しない前提のファイル。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Secrets {
    pub values: BTreeMap<String, String>,
}

impl Secrets {
    /// `{{secret:名前}}` を値に置き換える。
    pub fn resolve(&self, text: &str) -> Result<String> {
        let mut out = String::with_capacity(text.len());
        let mut rest = text;
        while let Some(start) = rest.find("{{secret:") {
            out.push_str(&rest[..start]);
            let after = &rest[start + "{{secret:".len()..];
            let end = after
                .find("}}")
                .with_context(|| format!("`{{{{secret:` が閉じていません: {text}"))?;
            let name = after[..end].trim();
            let value = self
                .values
                .get(name)
                .with_context(|| format!("secrets.toml に `{name}` がありません"))?;
            out.push_str(value);
            rest = &after[end + 2..];
        }
        out.push_str(rest);
        Ok(out)
    }
}

/// 手元の取得定義のファイル。
#[derive(Debug, Clone, PartialEq)]
pub struct LocalSourceFile {
    pub path: PathBuf,
    /// ファイル名から決まる ID。
    pub id: String,
    pub text: String,
    /// 読めた定義。読めなければ理由。
    pub source: Result<SourceConfig, String>,
}

/// 取り込んだカタログの控え (`catalog.json`)。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CatalogCache {
    /// 取り込んだ取得先。固定のカタログと一致する控えだけを使う。
    pub location: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub etag: Option<String>,
    /// 取り込んだ版 (コミットの SHA など)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
    /// 取り込んだアプリの版。アプリを新しくしたら ETag を使わずに取り直し、前のアプリでは
    /// 読めなかった (新しいアプリが必要だった) ファイルを読み直す。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub app_version: Option<String>,
    pub fetched_at: DateTime<Utc>,
    pub files: Vec<CachedFile>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CachedFile {
    pub path: String,
    pub text: String,
}

/// 書式 1 の `sources.toml` から移した結果。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyMigration {
    /// 購読にしたソースの数。
    pub subscriptions: usize,
    /// 手元の定義として書き出したソースの ID。
    pub local_sources: Vec<String>,
    /// 元のファイルを移した先。
    pub backup: PathBuf,
}

/// 設定ファイルの読み書き。
#[derive(Debug, Clone)]
pub struct ConfigStore {
    paths: AppPaths,
}

struct SubscriptionLock {
    file: File,
}

impl Drop for SubscriptionLock {
    fn drop(&mut self) {
        // 子プロセスなどに複製されたハンドルが残っても、ここで排他区間を終える。
        let _ = self.file.unlock();
    }
}

/// 起動時に読み込んだ購読と、移行・カタログ追加の結果。
#[derive(Debug)]
pub struct SubscriptionSetup {
    pub subscriptions: Subscriptions,
    pub migrated: Option<LegacyMigration>,
    pub adopted: Vec<String>,
}

impl ConfigStore {
    pub fn new(paths: AppPaths) -> Self {
        Self { paths }
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    pub fn load_settings(&self) -> Result<AppSettings> {
        let path = self.paths.settings_file();
        let Some(mut values) = load_toml::<toml::Table>(&path)? else {
            return Ok(AppSettings::default());
        };
        // 保存済みの取得先は読み飛ばす。カタログの取得先はアプリで固定する。
        values.remove("catalog");
        values
            .try_into()
            .with_context(|| format!("{} の書式が誤っています", path.display()))
    }

    pub fn save_settings(&self, settings: &AppSettings) -> Result<()> {
        write_toml(&self.paths.settings_file(), settings, SETTINGS_HEADER)
    }

    /// 購読を読む。ファイルがなければ `None` (初めての起動)。
    pub fn load_subscriptions(&self) -> Result<Option<Subscriptions>> {
        let _lock = self.lock_subscriptions()?;
        load_toml(&self.paths.subscriptions_file())
    }

    pub fn save_subscriptions(&self, subscriptions: &Subscriptions) -> Result<()> {
        let _lock = self.lock_subscriptions()?;
        self.write_subscriptions(subscriptions)
    }

    /// 最新の保存内容を読み直して変更する。別プロセスの変更を古い控えで上書きしない。
    pub fn update_subscriptions(
        &self,
        update: impl FnOnce(&mut Subscriptions),
    ) -> Result<Subscriptions> {
        let _lock = self.lock_subscriptions()?;
        let mut subscriptions: Subscriptions =
            load_toml(&self.paths.subscriptions_file())?.context("購読が初期化されていません")?;
        let before = subscriptions.clone();
        update(&mut subscriptions);
        if subscriptions != before {
            self.write_subscriptions(&subscriptions)?;
        }
        Ok(subscriptions)
    }

    fn write_subscriptions(&self, subscriptions: &Subscriptions) -> Result<()> {
        write_toml(
            &self.paths.subscriptions_file(),
            subscriptions,
            SUBSCRIPTIONS_HEADER,
        )
    }

    /// 購読がなければ移行または初期化し、新しいカタログ項目を補う。読み取りから保存まで同じロックで守る。
    pub fn initialize_subscriptions(
        &self,
        catalog: &Catalog,
        settings: &AppSettings,
    ) -> Result<SubscriptionSetup> {
        let _lock = self.lock_subscriptions()?;
        let mut migrated = None;
        let mut created = false;
        let mut subscriptions = match load_toml(&self.paths.subscriptions_file())? {
            Some(subscriptions) => subscriptions,
            None => {
                migrated = self.migrate_legacy_sources_locked(catalog, settings)?;
                match load_toml(&self.paths.subscriptions_file())? {
                    Some(subscriptions) => subscriptions,
                    None => {
                        created = true;
                        catalog.default_subscriptions()
                    }
                }
            }
        };
        let before = subscriptions.items.len();
        let adopted = catalog.adopt_new_entries(&mut subscriptions);
        if created || subscriptions.items.len() != before {
            self.write_subscriptions(&subscriptions)?;
        }
        Ok(SubscriptionSetup {
            subscriptions,
            migrated,
            adopted,
        })
    }

    /// 置き換えない別ファイルで、スレッド・プロセスをまたいで購読の読み書きを排他する。
    /// ロック中は待ち続けず誤りを返す。ファイルは消さず、ガードの破棄時に明示的にロックを外す。
    fn lock_subscriptions(&self) -> Result<SubscriptionLock> {
        let dir = self.paths.config_dir();
        fs::create_dir_all(dir).with_context(|| format!("{} を作成できません", dir.display()))?;
        let path = dir.join("subscriptions.lock");
        let file = File::options()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .with_context(|| format!("{} を開けません", path.display()))?;
        file.try_lock().context(
            "購読をロックできません。ほかの処理が使用中なら、少し待ってからやり直してください",
        )?;
        Ok(SubscriptionLock { file })
    }

    pub fn load_secrets(&self) -> Result<Secrets> {
        load_toml(&self.paths.secrets_file()).map(Option::unwrap_or_default)
    }

    /// 手元の取得定義をすべて読む。読めないファイルも理由と一緒に返す。
    pub fn load_local_sources(&self) -> Result<Vec<LocalSourceFile>> {
        let dir = self.paths.local_sources_dir();
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err).with_context(|| format!("{} を読めません", dir.display())),
        };
        let mut files = Vec::new();
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("yaml") {
                continue;
            }
            let Some(id) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .map(str::to_string)
            else {
                continue;
            };
            let text = fs::read_to_string(&path)
                .with_context(|| format!("{} を読めません", path.display()))?;
            let source = SourceConfig::from_yaml(&text)
                .map_err(|err| err.to_string())
                .and_then(|source| {
                    if source.id == id {
                        Ok(source)
                    } else {
                        Err(format!(
                            "ファイル名 ({id}.yaml) と id ({}) を同じにしてください",
                            source.id
                        ))
                    }
                });
            files.push(LocalSourceFile {
                path,
                id,
                text,
                source,
            });
        }
        files.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(files)
    }

    /// 手元の取得定義を書く。`text` は YAML の全文 (コメントを含めて利用者が書いたもの)。
    pub fn save_local_source(&self, id: &str, text: &str) -> Result<PathBuf> {
        let source = SourceConfig::from_yaml(text).map_err(|err| anyhow::anyhow!("{err}"))?;
        if source.id != id {
            bail!("ID を `{id}` から変えることはできません");
        }
        let path = self.paths.local_source_file(id);
        write_atomic(&path, text.as_bytes())?;
        Ok(path)
    }

    /// 手元の取得定義を消す。なければ何もしない。
    pub fn delete_local_source(&self, id: &str) -> Result<()> {
        match fs::remove_file(self.paths.local_source_file(id)) {
            Err(err) if err.kind() != std::io::ErrorKind::NotFound => {
                Err(err).context("手元の定義を消せません")
            }
            _ => Ok(()),
        }
    }

    pub fn load_catalog_cache(&self) -> Result<Option<CatalogCache>> {
        let path = self.paths.catalog_cache_file();
        let text = match fs::read_to_string(&path) {
            Ok(text) => text,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => {
                return Err(err).with_context(|| format!("{} を読めません", path.display()));
            }
        };
        match serde_json::from_str(&text) {
            Ok(cache) => Ok(Some(cache)),
            Err(err) => {
                // 控えなので、壊れていれば使わずに取り直す。
                tracing::warn!("カタログの控えを読めないため使いません: {err}");
                Ok(None)
            }
        }
    }

    pub fn save_catalog_cache(&self, cache: &CatalogCache) -> Result<()> {
        let text = serde_json::to_string(cache).context("カタログの控えを作れません")?;
        write_atomic(&self.paths.catalog_cache_file(), text.as_bytes())
    }

    /// 書式 1 の `sources.toml` を、購読と手元の定義 (書式 2) に移す。
    ///
    /// `subscriptions.toml` がまだなく、`sources.toml` があるときだけ行う。カタログと同じ
    /// ID のソースはカタログの定義を使い、on/off・アイコン・取得間隔・期間だけを購読に移す
    /// (書式 1 の定義は同梱のプリセットを写したもので、手元で写しを残すとカタログの修正が
    /// 届かなくなるため)。カタログにないソースは手元の定義にする。元のファイルは
    /// `sources.toml.bak` に移すので、手で直していた定義はそこから戻せる。
    ///
    /// 書き出す定義はすべて読み直して元と同じか確かめ、ファイルを書く前に誤りを見つける。
    /// `settings` は移す前のアプリ設定 (取得間隔・期間を引き継ぐために使う)。
    pub fn migrate_legacy_sources(
        &self,
        catalog: &Catalog,
        settings: &AppSettings,
    ) -> Result<Option<LegacyMigration>> {
        let _lock = self.lock_subscriptions()?;
        self.migrate_legacy_sources_locked(catalog, settings)
    }

    fn migrate_legacy_sources_locked(
        &self,
        catalog: &Catalog,
        settings: &AppSettings,
    ) -> Result<Option<LegacyMigration>> {
        let legacy = self.paths.legacy_sources_file();
        if !legacy.exists() || self.paths.subscriptions_file().exists() {
            return Ok(None);
        }
        let text = fs::read_to_string(&legacy)
            .with_context(|| format!("{} を読めません", legacy.display()))?;
        let file = source_v1::SourcesFile::parse(&text)
            .with_context(|| format!("{} の書式が誤っています", legacy.display()))?;

        // 書式 1 で未指定ならアプリ設定の値で動いていた。カタログに値があると、未指定のまま
        // 移すと変わってしまうので、今のアプリ設定の値を購読の上書きにする。
        let carry = |old: Option<u32>, catalog: Option<u32>, app: u32| match (old, catalog) {
            (old, catalog) if old == catalog => None,
            (None, Some(_)) => Some(app),
            (old, _) => old,
        };

        let mut subscriptions = Subscriptions::default();
        let mut local_files = Vec::new();
        for old in &file.sources {
            let converted = old.to_v2();
            let source = converted.source;
            let mut subscription = Subscription {
                enabled: converted.enabled,
                icon: converted.local_icon,
                ..Subscription::new(source.id.clone())
            };
            let keep_local = match catalog.get(&source.id) {
                Some(entry) => {
                    // 取得間隔と期間は購読の上書きにする。
                    subscription.interval_minutes = carry(
                        source.interval_minutes,
                        entry.source.interval_minutes,
                        settings.interval_minutes,
                    );
                    subscription.max_age_days = carry(
                        source.max_age_days,
                        entry.source.max_age_days,
                        settings.max_age_days,
                    );
                    false
                }
                None => true,
            };
            if keep_local {
                let text = source_file_text(&source)?;
                let reread = SourceConfig::from_yaml(&text).map_err(|err| {
                    anyhow::anyhow!(
                        "{} を移せません (書き出した定義を読めません): {err}",
                        source.id
                    )
                })?;
                if reread != source {
                    bail!(
                        "{} を移せません (書き出した定義を読み直すと内容が変わります)",
                        source.id
                    );
                }
                local_files.push((source.id.clone(), text));
            }
            subscriptions.items.push(subscription);
        }
        let mut local_sources = Vec::new();
        for (id, text) in local_files {
            write_atomic(&self.paths.local_source_file(&id), text.as_bytes())?;
            local_sources.push(id);
        }
        self.write_subscriptions(&subscriptions)?;

        let mut backup = legacy.with_extension("toml.bak");
        if backup.exists() {
            backup =
                legacy.with_extension(format!("toml.{}.bak", Utc::now().format("%Y%m%d%H%M%S")));
        }
        fs::rename(&legacy, &backup).with_context(|| {
            format!("{} を {} に移せません", legacy.display(), backup.display())
        })?;
        Ok(Some(LegacyMigration {
            subscriptions: subscriptions.items.len(),
            local_sources,
            backup,
        }))
    }
}

/// 手元の定義のファイルの中身。名前のコメントと、エディタの補完用の Schema の指定を付ける。
pub fn source_file_text(source: &SourceConfig) -> Result<String> {
    let body = source.to_yaml().map_err(|err| anyhow::anyhow!("{err}"))?;
    Ok(format!(
        "# {}\n# yaml-language-server: $schema={SCHEMA_URL}\n{body}",
        source.name.replace('\n', " ")
    ))
}

const SETTINGS_HEADER: &str =
    "# Game News Nexus のアプリ設定。アプリの設定画面からも変更できる。\n\n";
const SUBSCRIPTIONS_HEADER: &str = "# Game News Nexus の購読。どのソースを使うか (enabled) と、手元だけの上書き。\n# アプリのソース画面からも変更できる。\n\n";

fn load_toml<T: for<'de> Deserialize<'de>>(path: &Path) -> Result<Option<T>> {
    let text = match fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(err).with_context(|| format!("{} を読めません", path.display())),
    };
    let value = toml::from_str(&text)
        .with_context(|| format!("{} の書式が誤っています", path.display()))?;
    Ok(Some(value))
}

fn write_toml<T: Serialize>(path: &Path, value: &T, header: &str) -> Result<()> {
    let body = toml::to_string_pretty(value).context("設定を TOML にできません")?;
    write_atomic(path, format!("{header}{body}").as_bytes())
}

/// 一時ファイルの名前の終わり。一時ファイルは `.<名前>.<PID>.<通し番号>.tmp` (前の書き方は `.<名前>.<PID>.tmp`)。
/// この形の名前は、アプリの一時ファイルの名前として扱う (古くなったら消す)。
const TEMP_SUFFIX: &str = ".tmp";

/// 一時ファイルを作り直す回数の上限。名前が重なるのは、落ちたプロセスの残りと PID が重なったときだけ。
const TEMP_ATTEMPTS: usize = 16;

/// 消す一時ファイルの古さ。書き込みはメモリーの中身を 1 回書いて sync するだけなので、これより古い一時ファイルは、
/// 書き込みの途中で落ちた (止められた) プロセスの残りとみなす。新しいものは、ほかのプロセス (`--fetch-once`) の
/// 書き込みの途中かもしれないので残す。目安であって、書き込みの途中でないことの保証ではない (長く止まっていた
/// プロセスや、時計が進んだとき)。そのときは、その書き込みの置き換えが失敗するだけで、ファイルは壊れない。
const STALE_TEMP_AGE: Duration = Duration::from_secs(60 * 60);

/// 一時ファイルの名前に付ける、プロセスの中の通し番号。
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// `path` を `bytes` に置き換える。書き込みごとに別の一時ファイル (同じディレクトリの
/// `.<名前>.<PID>.<通し番号>.tmp`) に書いて sync し、閉じてから置き換える。
///
/// - 置き換えは原子的なので、書き込みの途中でプロセスが落ちても (止められても)、`path` は前の中身か新しい中身の
///   どちらかそのもので、壊れない (置き換える前に止まれば前の中身)。読み手も書きかけを見ない。
/// - 同じ `path` を同時に書いても (同じプロセスでもほかのプロセスでも)、一時ファイルを共有しない。残るのは、
///   置き換えに成功した書き込みのどれか 1 つの中身そのもの。どれが残るか (呼び出しの順・中身の新しさ) は
///   約束しないので、決めたいときは呼び出し側で並べる。
/// - 失敗したら、一時ファイルを閉じてから消す。置き換えたあと、同じ `path` の古い一時ファイル (落ちたプロセスの
///   残り) を消す。どちらの後始末もできる範囲で行い、できなくても結果を変えない。
/// - 電源断・OS の落ちへの備えは、置き換える前に中身を sync するところまで。置き換え自体がディスクに残ったかは
///   (ディレクトリを sync しないので) ファイルシステムに任せ、電源断のあとに新しい中身が残っていることは約束しない。
///
/// Windows は、置き換える先をほかのプログラム (ウイルス対策の検査、旧形式を読むアプリなど) が開いていると、
/// 置き換えを断ることがある (std の rename は `MoveFileExW` が断ったら POSIX の置き換えでやり直すが、それでも
/// `ERROR_ACCESS_DENIED` になることがある)。そのときは前の中身が残るだけなので、待ってやり直さない
/// (呼び出し元は画面のスレッドやロックの中で、同期のまま呼ぶ)。また、開かれているファイルの置き換え (その
/// POSIX の置き換え) と重なって、ほかのスレッド・プロセスが `path` を開くと、見つからない (`NotFound`) ことがある
/// のを、この PC の実験で見ている (置き換えが戻った直後に同じスレッドで開く実験では起きなかった)。書き込みと
/// 重なりうる読み手は、見つからないことを「ない」と決めつけない。
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> Result<()> {
    let dir = path.parent().context("保存先のディレクトリがありません")?;
    fs::create_dir_all(dir).with_context(|| format!("{} を作成できません", dir.display()))?;
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .context("ファイル名がありません")?;
    let (tmp, file) = create_temp(dir, file_name, &TEMP_SEQUENCE)?;
    #[cfg(test)]
    run_after_temp_created();
    replace_with(file, bytes, &tmp, path)?;
    remove_stale_temps(dir, file_name, SystemTime::now());
    Ok(())
}

#[cfg(test)]
thread_local! {
    /// テストで、一時ファイルを作ってから書く前に止める場所 (競合の順番を決める・途中で止まった書き込みをまねる)。
    /// 置いたスレッドの次の `write_atomic` で 1 度だけ呼ぶ。
    static AFTER_TEMP_CREATED: std::cell::RefCell<Option<Box<dyn FnOnce()>>> =
        const { std::cell::RefCell::new(None) };
}

#[cfg(test)]
fn run_after_temp_created() {
    // 外してから呼ぶ (中で書いても、もう呼ばない)。
    if let Some(hook) = AFTER_TEMP_CREATED.with(|hook| hook.borrow_mut().take()) {
        hook();
    }
}

/// 書き込みごとに別の一時ファイル `<dir>/.<file_name>.<PID>.<通し番号>.tmp` を作る。同じプロセスの書き込みどうしは
/// 通し番号 (`counter` から取る) で、ほかのプロセス (`--fetch-once`) とは PID で分かれる。落ちたプロセスの残りと
/// 名前が重なったら (PID が使い回されたとき)、残りには触らずに (`create_new`) 次の番号で作り直す。
fn create_temp(dir: &Path, file_name: &str, counter: &AtomicU64) -> Result<(PathBuf, File)> {
    let pid = std::process::id();
    for _ in 0..TEMP_ATTEMPTS {
        let sequence = counter.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".{file_name}.{pid}.{sequence}{TEMP_SUFFIX}"));
        match File::create_new(&tmp) {
            Ok(file) => return Ok((tmp, file)),
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => {
                return Err(err).with_context(|| format!("{} を作成できません", tmp.display()));
            }
        }
    }
    bail!(
        "{} の一時ファイルを作成できません",
        dir.join(file_name).display()
    )
}

/// 一時ファイル `tmp` (開いた `file`) に書いて sync し、閉じてから `path` に置き換える。書く段か置き換える段で
/// 失敗したら、閉じた一時ファイルを消す (書きかけや、置き換えられなかった一時ファイルを残さない)。消せなくても、
/// 返すのは元の誤り。
fn replace_with(mut file: File, bytes: &[u8], tmp: &Path, path: &Path) -> Result<()> {
    let written = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .with_context(|| format!("{} に書き込めません", tmp.display()));
    drop(file);
    let result = written.and_then(|()| {
        fs::rename(tmp, path).with_context(|| format!("{} を置き換えられません", path.display()))
    });
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}

/// `dir` にある `file_name` の一時ファイルのうち、最終更新が `now` の `STALE_TEMP_AGE` より前のもの (書き込みの
/// 途中で落ちたプロセスの残り。前の書き方のものも) を消す。最終更新が分からない・先の時刻のものは残す。普通の
/// ファイルだけを見て (`DirEntry::metadata` はシンボリックリンクをたどらない)、ディレクトリ・リンク・ほかの名前の
/// ファイルには触らない (旧形式を書き出すディレクトリは利用者のもの)。消せなくても誤りにしない (次に書いたときに
/// また消す)。ディレクトリを読み通すので、頻度を下げるならここを呼ぶ回数で決める。
fn remove_stale_temps(dir: &Path, file_name: &str, now: SystemTime) {
    let Some(cutoff) = now.checked_sub(STALE_TEMP_AGE) else {
        return;
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if !entry
            .file_name()
            .to_str()
            .is_some_and(|name| is_temp_of(name, file_name))
        {
            continue;
        }
        let stale = entry.metadata().is_ok_and(|metadata| {
            metadata.is_file() && metadata.modified().is_ok_and(|modified| modified < cutoff)
        });
        if stale {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// `name` が `file_name` の一時ファイル (`.<名前>.<PID>.<通し番号>.tmp`、前の書き方の `.<名前>.<PID>.tmp`) の名前か。
fn is_temp_of(name: &str, file_name: &str) -> bool {
    let Some(numbers) = name
        .strip_prefix('.')
        .and_then(|rest| rest.strip_prefix(file_name))
        .and_then(|rest| rest.strip_prefix('.'))
        .and_then(|rest| rest.strip_suffix(TEMP_SUFFIX))
    else {
        return false;
    };
    let numbers: Vec<&str> = numbers.split('.').collect();
    numbers.len() <= 2
        && numbers
            .iter()
            .all(|number| !number.is_empty() && number.bytes().all(|byte| byte.is_ascii_digit()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::CATALOG_URL;
    use pretty_assertions::assert_eq;
    use std::cell::RefCell;
    use std::io::Read as _;
    use std::rc::Rc;
    use std::sync::Barrier;
    use std::sync::atomic::AtomicBool;

    fn store() -> (tempfile::TempDir, ConfigStore) {
        let dir = tempfile::tempdir().unwrap();
        let store = ConfigStore::new(AppPaths::under(dir.path().to_path_buf()));
        store.paths().ensure().unwrap();
        (dir, store)
    }

    /// 親の書き込み中に別プロセスで購読へ触れる。通常のテスト実行では何もしない。
    #[test]
    fn subscriptions_process_lock_probe() {
        let Some(home) = std::env::var_os("GNN_TEST_SUBSCRIPTIONS_HOME") else {
            return;
        };
        let store = ConfigStore::new(AppPaths::under(home.into()));
        assert!(
            store
                .initialize_subscriptions(&Catalog::bundled(), &AppSettings::default())
                .is_err()
        );
        assert!(store.load_subscriptions().is_err());
        assert!(store.save_subscriptions(&Subscriptions::default()).is_err());
        assert!(
            store
                .update_subscriptions(|_| panic!("ロック中に変更しない"))
                .is_err()
        );
        assert!(
            store
                .migrate_legacy_sources(&Catalog::bundled(), &AppSettings::default())
                .is_err()
        );
    }

    /// Windows の置き換え中に名前が一瞬消える状況を、待ち時間に頼らず再現する。
    fn probe_during_subscription_write(paths: AppPaths, missing: bool) {
        on_temp_created(move || {
            let path = paths.subscriptions_file();
            let backup = path.with_extension("test-backup");
            if missing {
                fs::rename(&path, &backup).unwrap();
            }
            let mut child = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "config::tests::subscriptions_process_lock_probe",
                    "--nocapture",
                ])
                .env(
                    "GNN_TEST_SUBSCRIPTIONS_HOME",
                    paths.config_dir().parent().unwrap(),
                )
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            // ロックを待ち続ける実装に退行しても、子プロセスを残さずテストを失敗させる。
            let deadline = std::time::Instant::now() + Duration::from_secs(10);
            let timed_out = loop {
                if child.try_wait().unwrap().is_some() {
                    break false;
                }
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    break true;
                }
                std::thread::sleep(Duration::from_millis(10));
            };
            let output = child.wait_with_output().unwrap();
            if missing {
                fs::rename(backup, path).unwrap();
            }
            assert!(
                !timed_out && output.status.success(),
                "{}\n{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        });
    }

    #[test]
    fn subscription_replacement_cannot_be_mistaken_for_first_start() {
        let (_dir, store) = store();
        let catalog = Catalog::bundled();
        let mut subscriptions = catalog.default_subscriptions();
        let custom = subscriptions.entry("genshin_impact");
        custom.enabled = false;
        custom.interval_minutes = Some(37);
        custom.icon = Some("custom.png".into());
        store.save_subscriptions(&subscriptions).unwrap();

        probe_during_subscription_write(store.paths().clone(), true);
        store.save_subscriptions(&subscriptions).unwrap();
        let setup = store
            .initialize_subscriptions(&catalog, &AppSettings::default())
            .unwrap();
        assert_eq!(setup.subscriptions, subscriptions);
        assert_eq!(setup.migrated, None);
        assert!(setup.adopted.is_empty());
        assert_eq!(store.load_subscriptions().unwrap(), Some(subscriptions));
    }

    #[test]
    fn first_subscription_initialization_excludes_other_processes() {
        let (_dir, store) = store();
        let catalog = Catalog::bundled();
        probe_during_subscription_write(store.paths().clone(), false);
        let setup = store
            .initialize_subscriptions(&catalog, &AppSettings::default())
            .unwrap();
        assert_eq!(setup.subscriptions, catalog.default_subscriptions());
        assert_eq!(setup.migrated, None);
        assert_eq!(
            store.load_subscriptions().unwrap(),
            Some(setup.subscriptions)
        );
    }

    #[test]
    fn invalid_subscriptions_are_not_reinitialized() {
        let (_dir, store) = store();
        let path = store.paths().subscriptions_file();
        fs::write(&path, "broken = [").unwrap();
        assert!(
            store
                .initialize_subscriptions(&Catalog::bundled(), &AppSettings::default())
                .is_err()
        );
        assert_eq!(fs::read_to_string(path).unwrap(), "broken = [");
    }

    #[test]
    fn subscription_updates_read_the_latest_saved_values() {
        let (_dir, store) = store();
        store.save_subscriptions(&Subscriptions::default()).unwrap();
        let other = ConfigStore::new(store.paths().clone());
        store
            .update_subscriptions(|subscriptions| {
                let item = subscriptions.entry("first");
                item.enabled = false;
                item.interval_minutes = Some(37);
            })
            .unwrap();
        let updated = other
            .update_subscriptions(|subscriptions| {
                subscriptions.entry("second").icon = Some("custom.png".into());
            })
            .unwrap();
        assert!(!updated.get("first").unwrap().enabled);
        assert_eq!(updated.get("first").unwrap().interval_minutes, Some(37));
        assert_eq!(
            updated.get("second").unwrap().icon.as_deref(),
            Some("custom.png")
        );
        assert_eq!(store.load_subscriptions().unwrap(), Some(updated));
    }

    #[test]
    fn subscription_lock_is_released_before_inherited_handles_close() {
        let (_dir, store) = store();
        store.save_subscriptions(&Subscriptions::default()).unwrap();
        let lock = store.lock_subscriptions().unwrap();
        let inherited = lock.file.try_clone().unwrap();
        assert!(store.load_subscriptions().is_err());
        drop(lock);

        let updated = store
            .update_subscriptions(|subscriptions| {
                subscriptions.entry("first").enabled = false;
            })
            .unwrap();
        assert!(!updated.get("first").unwrap().enabled);
        assert_eq!(store.load_subscriptions().unwrap(), Some(updated));
        drop(inherited);
    }

    #[test]
    fn unchanged_subscriptions_are_not_rewritten() {
        let (_dir, store) = store();
        store.save_subscriptions(&Subscriptions::default()).unwrap();
        let path = store.paths().subscriptions_file();
        let modified = SystemTime::now() - Duration::from_secs(60);
        set_modified(&path, modified);
        let modified = fs::metadata(&path).unwrap().modified().unwrap();
        store.update_subscriptions(|_| {}).unwrap();
        assert_eq!(fs::metadata(path).unwrap().modified().unwrap(), modified);
    }

    #[test]
    fn subscription_lock_failure_does_not_change_saved_values() {
        let (_dir, store) = store();
        let path = store.paths().subscriptions_file();
        fs::write(&path, "# 購読を保持する\n").unwrap();
        fs::create_dir(store.paths().config_dir().join("subscriptions.lock")).unwrap();
        assert!(
            store
                .initialize_subscriptions(&Catalog::bundled(), &AppSettings::default())
                .is_err()
        );
        assert!(store.save_subscriptions(&Subscriptions::default()).is_err());
        assert_eq!(fs::read_to_string(path).unwrap(), "# 購読を保持する\n");
    }

    /// 同じ購読を再保存しない効果と、変更時の退行を手元の一時ディレクトリで測る。
    #[test]
    #[ignore = "手動の性能計測。通常のテストでは時間を評価しない"]
    fn benchmark_subscription_updates() {
        const ITERATIONS: usize = 100;
        const SAMPLES: usize = 7;
        // 変更の有無によらず保存する比較用の経路。排他・読み取り・複製は更新後とそろえる。
        fn update(store: &ConfigStore, changed: bool, always_save: bool) -> Subscriptions {
            let change = |subscriptions: &mut Subscriptions| {
                if changed {
                    subscriptions.items[0].enabled = !subscriptions.items[0].enabled;
                }
            };
            if !always_save {
                return store.update_subscriptions(change).unwrap();
            }
            let _lock = store.lock_subscriptions().unwrap();
            let mut subscriptions: Subscriptions = load_toml(&store.paths.subscriptions_file())
                .unwrap()
                .unwrap();
            let before = subscriptions.clone();
            change(&mut subscriptions);
            std::hint::black_box(subscriptions == before);
            store.write_subscriptions(&subscriptions).unwrap();
            subscriptions
        }
        let mut results = Vec::new();
        for count in [24, 1000] {
            for changed in [false, true] {
                let (_dir, store) = store();
                let subscriptions = Subscriptions {
                    items: (0..count)
                        .map(|index| Subscription::new(format!("source_{index}")))
                        .collect(),
                };
                store.save_subscriptions(&subscriptions).unwrap();
                let mut samples = [Vec::new(), Vec::new()];
                // 最初の 2 回はウォームアップ。すべて同じビルド・入力・回数で測る。
                for sample in 0..SAMPLES + 2 {
                    let mut elapsed = [0.0, 0.0];
                    // I/O の時間変動が片方に偏らないよう、操作ごとに順番を交互にする。
                    for iteration in 0..ITERATIONS {
                        let first = (sample + iteration) % 2;
                        for index in [first, 1 - first] {
                            let start = std::time::Instant::now();
                            std::hint::black_box(update(&store, changed, index == 0));
                            elapsed[index] += start.elapsed().as_secs_f64() * 1000.0;
                        }
                    }
                    if sample >= 2 {
                        for index in 0..2 {
                            samples[index].push(elapsed[index]);
                        }
                    }
                }
                assert_eq!(store.load_subscriptions().unwrap(), Some(subscriptions));
                results.push(serde_json::json!({
                    "subscriptions": count,
                    "changed": changed,
                    "always_save_ms": samples[0],
                    "skip_unchanged_ms": samples[1],
                }));
            }
        }
        println!(
            "{}",
            serde_json::json!({
                "os": std::env::consts::OS,
                "arch": std::env::consts::ARCH,
                "iterations": ITERATIONS,
                "warmup_samples": 2,
                "samples": SAMPLES,
                "sampling": "interleaved_per_operation",
                "cases": results,
            })
        );
    }

    #[test]
    fn settings_default_when_missing_and_roundtrip() {
        let (_dir, store) = store();
        assert_eq!(store.load_settings().unwrap(), AppSettings::default());

        let settings = AppSettings {
            interval_minutes: 15,
            theme: ThemePreference::Dark,
            ..AppSettings::default()
        };
        store.save_settings(&settings).unwrap();
        assert_eq!(store.load_settings().unwrap(), settings);

        let mut source = crate::templates::blank(crate::ListKind::Feed, "b".into());
        source.interval_minutes = Some(5);
        assert_eq!(settings.interval_for(&source), 5);
        source.interval_minutes = None;
        assert_eq!(settings.interval_for(&source), 15);
    }

    #[test]
    fn settings_ignore_catalog_location_and_do_not_save_it() {
        let (_dir, store) = store();
        fs::write(
            store.paths().settings_file(),
            "catalog = 'https://example.com/catalog.tar.gz'\ninterval_minutes = 37\n",
        )
        .unwrap();
        let settings = store.load_settings().unwrap();
        assert_eq!(settings.interval_minutes, 37);
        store.save_settings(&settings).unwrap();
        let saved: toml::Table =
            toml::from_str(&fs::read_to_string(store.paths().settings_file()).unwrap()).unwrap();
        assert!(!saved.contains_key("catalog"));
        assert_eq!(store.load_settings().unwrap(), settings);
    }

    #[test]
    fn settings_reject_unknown_options() {
        let (_dir, store) = store();
        fs::write(store.paths().settings_file(), "unknown_option = true\n").unwrap();
        assert!(store.load_settings().is_err());
    }

    #[test]
    fn saves_and_loads_local_sources() {
        let (_dir, store) = store();
        let source = crate::templates::blank(crate::ListKind::Json, "mine".into());
        let text = source_file_text(&source).unwrap();
        assert!(text.starts_with("# 新しいソース\n# yaml-language-server: $schema="));
        store.save_local_source("mine", &text).unwrap();
        // ファイル名と ID が違うもの、読めないものも理由と一緒に返す。
        fs::write(store.paths().local_source_file("other"), &text).unwrap();
        fs::write(store.paths().local_source_file("broken"), "schema: [").unwrap();
        let files = store.load_local_sources().unwrap();
        let summary: Vec<(&str, bool)> = files
            .iter()
            .map(|file| (file.id.as_str(), file.source.is_ok()))
            .collect();
        assert_eq!(
            summary,
            vec![("broken", false), ("mine", true), ("other", false)]
        );
        assert_eq!(files[1].text, text);
        assert!(store.save_local_source("renamed", &text).is_err());
        store.delete_local_source("mine").unwrap();
        store.delete_local_source("mine").unwrap();
        assert_eq!(store.load_local_sources().unwrap().len(), 2);
    }

    #[test]
    fn migrates_legacy_sources() {
        let (_dir, store) = store();
        let catalog = Catalog::bundled();
        let legacy = r#"
[[source]]
id = "genshin_impact"
name = "原神"
enabled = false
icon = 'C:\icons\genshin.png'
interval_minutes = 15
homepage = "https://genshin.hoyoverse.com/ja/news"
tags = ["hoyoverse"]
note = "公式サイトのニュース一覧が裏で読んでいる HoYoverse のコンテンツ API (getContentList) から取る"
[source.request]
url = "https://sg-public-api-static.hoyoverse.com/content_v2_user/app/a1b1f9d3315447cc/getContentList?iAppId=32&iChanId=395&iPageSize=30&iPage=1&sLangKey=ja-jp"
[source.list]
kind = "json"
items = "/data/list"
[source.list.fields]
id = "/iInfoId"
title = "/sTitle"
url = "https://genshin.hoyoverse.com/ja/news/detail/{/iInfoId}"
published = "/dtStartTime"
body = "/sContent"

[[source]]
id = "my_blog"
name = "自分のブログ"
[source.request]
url = "https://example.com/feed.xml"
[source.list]
kind = "feed"
"#;
        fs::write(store.paths().legacy_sources_file(), legacy).unwrap();
        let migration = store
            .migrate_legacy_sources(&catalog, &AppSettings::default())
            .unwrap()
            .unwrap();
        assert_eq!(migration.subscriptions, 2);
        assert_eq!(migration.local_sources, vec!["my_blog".to_string()]);
        assert!(migration.backup.exists());
        assert!(!store.paths().legacy_sources_file().exists());

        let subscriptions = store.load_subscriptions().unwrap().unwrap();
        let genshin = subscriptions.get("genshin_impact").unwrap();
        assert!(!genshin.enabled);
        assert_eq!(genshin.icon.as_deref(), Some(r"C:\icons\genshin.png"));
        assert_eq!(genshin.interval_minutes, Some(15));
        assert!(subscriptions.get("my_blog").unwrap().enabled);
        let local = store.load_local_sources().unwrap();
        assert_eq!(local.len(), 1);
        assert_eq!(local[0].source.as_ref().unwrap().name, "自分のブログ");

        // 2 回目は何もしない。
        assert_eq!(
            store
                .migrate_legacy_sources(&catalog, &AppSettings::default())
                .unwrap(),
            None
        );
    }

    #[test]
    fn migration_keeps_effective_intervals() {
        let (_dir, store) = store();
        let catalog = Catalog::from_files_for(
            [(
                "sources/feeds/paced.yaml",
                "schema: 2\nmin_app_version: '26.11.6'\nid: paced\nname: 間隔あり\ninterval_minutes: 360\nmax_age_days: 30\nlists:\n  - kind: feed\n    request:\n      url: 'https://example.com/feed.xml'\n",
            )],
            None,
            "26.11.6",
        );
        // 書式 1 では間隔を書かず、アプリ設定 (45 分) で動いていた。期間はカタログと同じ。
        let legacy = "[[source]]\nid = \"paced\"\nname = \"間隔あり\"\nmax_age_days = 30\n[source.request]\nurl = \"https://example.com/feed.xml\"\n[source.list]\nkind = \"feed\"\n";
        fs::write(store.paths().legacy_sources_file(), legacy).unwrap();
        let settings = AppSettings {
            interval_minutes: 45,
            ..AppSettings::default()
        };
        let migration = store
            .migrate_legacy_sources(&catalog, &settings)
            .unwrap()
            .unwrap();
        assert!(migration.local_sources.is_empty());
        let subscriptions = store.load_subscriptions().unwrap().unwrap();
        let paced = subscriptions.get("paced").unwrap();
        assert_eq!(paced.interval_minutes, Some(45));
        assert_eq!(paced.max_age_days, None);
    }

    #[test]
    fn roundtrips_catalog_cache() {
        let (_dir, store) = store();
        assert_eq!(store.load_catalog_cache().unwrap(), None);
        let cache = CatalogCache {
            location: CATALOG_URL.into(),
            etag: Some("\"abc\"".into()),
            revision: Some("0123".into()),
            app_version: Some(crate::source::APP_VERSION.into()),
            fetched_at: Utc::now(),
            files: vec![CachedFile {
                path: "groups.yaml".into(),
                text: "groups: []".into(),
            }],
        };
        store.save_catalog_cache(&cache).unwrap();
        assert_eq!(store.load_catalog_cache().unwrap(), Some(cache));
        fs::write(store.paths().catalog_cache_file(), "{").unwrap();
        assert_eq!(store.load_catalog_cache().unwrap(), None);
    }

    #[test]
    fn resolves_secret_placeholders() {
        let secrets = Secrets {
            values: BTreeMap::from([("cookie".to_string(), "sid=1".to_string())]),
        };
        assert_eq!(
            secrets.resolve("a={{secret:cookie}};b").unwrap(),
            "a=sid=1;b"
        );
        assert_eq!(secrets.resolve("plain").unwrap(), "plain");
        assert!(secrets.resolve("{{secret:missing}}").is_err());
        assert!(secrets.resolve("{{secret:cookie").is_err());
    }

    // ---- write_atomic ----

    const ERROR_ACCESS_DENIED: i32 = 5;
    const ERROR_SHARING_VIOLATION: i32 = 32;

    /// ディレクトリにあるもの (名前の順)。
    fn listing(dir: &Path) -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect();
        paths.sort();
        paths
    }

    fn set_modified(path: &Path, modified: SystemTime) {
        File::options()
            .write(true)
            .open(path)
            .unwrap()
            .set_modified(modified)
            .unwrap();
    }

    /// このスレッドの次の `write_atomic` が一時ファイルを作ったところ (書く前) で `hook` を呼ぶ。
    fn on_temp_created(hook: impl FnOnce() + 'static) {
        AFTER_TEMP_CREATED.with(|slot| *slot.borrow_mut() = Some(Box::new(hook)));
    }

    /// 終わったら読み手を止める。書き手の確かめが失敗しても、読み手を待ち続けない。
    struct StopOnDrop<'a>(&'a AtomicBool);

    impl Drop for StopOnDrop<'_> {
        fn drop(&mut self) {
            self.0.store(true, Ordering::Relaxed);
        }
    }

    /// Windows が置き換えを断っただけの失敗か。Windows は、置き換える先をほかのプログラム (ウイルス対策の
    /// 検査など) が開いていると置き換えを断る (`ERROR_ACCESS_DENIED`・`ERROR_SHARING_VIOLATION`)。同じファイルの
    /// 置き換えが重なった直後に起きやすい。前の中身かほかの書き込みの中身が残るだけで、ファイルは壊れない。
    /// macOS の rename は、開かれているファイルも置き換えるので断られない。
    fn replace_refused(err: &anyhow::Error, path: &Path) -> bool {
        cfg!(windows)
            && err.to_string() == format!("{} を置き換えられません", path.display())
            && err
                .root_cause()
                .downcast_ref::<io::Error>()
                .and_then(io::Error::raw_os_error)
                .is_some_and(|code| matches!(code, ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION))
    }

    /// 置き換えと重なって開けなかっただけの失敗か (Windows だけ)。Windows は、ほかのプログラムが開いているあいだ
    /// 開くのを断ることがある (`ERROR_ACCESS_DENIED`・`ERROR_SHARING_VIOLATION`) ほか、開かれているファイルの
    /// 置き換え (std の rename は `MoveFileExW` に断られると POSIX の置き換えでやり直す) と重なってその名前で開くと、
    /// 見つからない (`NotFound`) ことがあるのを、この PC で見ている。読み手が開き続けている間に書き手 1 つが置き換えを
    /// 2 万回くり返すと、POSIX の置き換えでは 5 回とも、開いた回の 1 % 足らずから 7 割ほどが見つからず (std の
    /// rename でも同じように起きた)、`MoveFileExW` だけ (読み手が開いていると断られる) では一度も起きなかった。
    /// 置き換えた直後に同じスレッドで開く実験では 4 万回とも開け、中身が壊れたこともない。macOS の rename は
    /// 名前を空けないので、どれも許さない。
    fn read_interrupted(err: &io::Error) -> bool {
        cfg!(windows)
            && (err.kind() == io::ErrorKind::NotFound
                || err.raw_os_error().is_some_and(|code| {
                    matches!(code, ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION)
                }))
    }

    /// `write` を行い、Windows が置き換えを断ったら少し待ってやり直す。書いたばかりのファイルはウイルス対策が
    /// 検査していて、続けて置き換えると断られることがある。ほかの失敗はそのまま返す。
    fn retry_refused(path: &Path, mut write: impl FnMut() -> Result<()>) -> Result<()> {
        let mut attempts = 0;
        loop {
            match write() {
                Err(err) if attempts < 10 && replace_refused(&err, path) => {
                    attempts += 1;
                    std::thread::sleep(Duration::from_millis(50));
                }
                result => return result,
            }
        }
    }

    /// 同じパスに同時に書くスレッドの数。
    const WRITERS: usize = 8;

    /// `round` 回目の書き手 `writer` の中身。書き込みごとに長さ (16〜128 KiB) と中身を変え、混ざったら
    /// 分かるようにする。先頭の 8 バイトが回と書き手。
    fn contents(round: usize, writer: usize) -> Vec<u8> {
        let mut bytes = Vec::new();
        bytes.extend_from_slice(&(round as u32).to_le_bytes());
        bytes.extend_from_slice(&(writer as u32).to_le_bytes());
        bytes.resize(16 * 1024 * (writer + 1), (round * WRITERS + writer) as u8);
        bytes
    }

    /// どの書き込みの中身そのものか (回, 書き手)。どれとも違えば (書きかけ・混ざったもの) `None`。
    fn identify(bytes: &[u8]) -> Option<(usize, usize)> {
        let field = |at: usize| -> Option<usize> {
            let field: [u8; 4] = bytes.get(at..at + 4)?.try_into().ok()?;
            Some(u32::from_le_bytes(field) as usize)
        };
        let (round, writer) = (field(0)?, field(4)?);
        (writer < WRITERS && bytes == contents(round, writer)).then_some((round, writer))
    }

    /// `WRITERS` 個のスレッドが同じパスに違う中身をそろって書くことを `rounds` 回くり返し、並行して読み手が
    /// 読む。読めた中身・残った中身はどれか 1 つの書き込みの中身そのもので、一時ファイルは残らない。
    fn write_concurrently(rounds: usize) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game_info.jsonl");
        // 前の中身。どの書き込みも断られた回は、前の中身が残る。
        write_atomic(&path, &contents(0, 0)).unwrap();
        let mut kept = (0, 0);
        let mut succeeded = 0;
        let stop = AtomicBool::new(false);
        let first_read = AtomicBool::new(false);
        std::thread::scope(|scope| {
            // 書いている間も読む (旧形式を読むアプリ、設定を読み直す画面)。
            let reader = scope.spawn(|| {
                let mut reads = 0;
                while !stop.load(Ordering::Relaxed) {
                    match fs::read(&path) {
                        Ok(bytes) => {
                            assert!(
                                identify(&bytes).is_some(),
                                "書きかけか、混ざった中身を読んだ ({} バイト)",
                                bytes.len()
                            );
                            reads += 1;
                            first_read.store(true, Ordering::Release);
                        }
                        // 置き換えと重なると開けないことは Windows ではありうる (開き直せばよい)。macOS では
                        // 開けないことはない。
                        Err(err) => assert!(read_interrupted(&err), "読めなかった: {err}"),
                    }
                }
                reads
            });

            {
                let _stop = StopOnDrop(&stop);
                // 読み手が一度読んでから書き始める (読み手が動く前に書き終わって止めることがないように)。
                // 読み手が先に落ちたら待たずに進み、最後の join で落ちたことを伝える (待ち続けない)。
                while !first_read.load(Ordering::Acquire) && !reader.is_finished() {
                    std::thread::yield_now();
                }
                for round in 1..=rounds {
                    let start = Barrier::new(WRITERS);
                    let results: Vec<Result<()>> = std::thread::scope(|writers| {
                        let handles: Vec<_> = (0..WRITERS)
                            .map(|writer| {
                                let (path, start) = (&path, &start);
                                writers.spawn(move || {
                                    let bytes = contents(round, writer);
                                    start.wait();
                                    write_atomic(path, &bytes)
                                })
                            })
                            .collect();
                        handles
                            .into_iter()
                            .map(|handle| handle.join().unwrap())
                            .collect()
                    });
                    // 失敗してよいのは、Windows が置き換えを断ったときだけ。
                    let mut written = Vec::new();
                    for (writer, result) in results.iter().enumerate() {
                        match result {
                            Ok(()) => written.push(writer),
                            Err(err) => assert!(
                                replace_refused(err, &path),
                                "{round} 回目の書き手 {writer}: {err:#}"
                            ),
                        }
                    }
                    // 残るのは、この回に書けたどれか 1 つの中身そのもの (どれも断られたら前の中身)。
                    let bytes = fs::read(&path).unwrap();
                    let current = identify(&bytes);
                    if written.is_empty() {
                        assert_eq!(current, Some(kept), "{round} 回目に前の中身が残らなかった");
                    } else {
                        assert!(
                            current.is_some_and(
                                |(at, writer)| at == round && written.contains(&writer)
                            ),
                            "{round} 回目に、書けた中身でないもの ({} バイト) が残った",
                            bytes.len()
                        );
                    }
                    kept = current.unwrap();
                    succeeded += written.len();
                    // 一時ファイルは残らない。
                    assert_eq!(listing(dir.path()), std::slice::from_ref(&path));
                }
            }
            assert!(reader.join().unwrap() > 0, "一度も読めなかった");
        });
        assert!(succeeded > 0, "どの書き込みも成功しなかった");
    }

    #[test]
    fn concurrent_writes_to_one_path_leave_one_whole_file() {
        // 同じファイルを同時に書いても、一時ファイルを共有しない。
        write_concurrently(5);
    }

    /// 重いので手で回す (`cargo test -p gnn-core --lib -- --ignored config`)。
    #[test]
    #[ignore = "重い。手で回すストレステスト"]
    fn many_concurrent_writes_to_one_path_leave_one_whole_file() {
        write_concurrently(1000);
    }

    #[test]
    fn a_write_in_progress_keeps_its_own_temp_file() {
        // 1 つ目の書き込みが一時ファイルを作ったところで、同じファイルへの 2 つ目の書き込みを最後まで行う。
        // 一時ファイルを共有すると、2 つ目が 1 つ目の一時ファイルを切り詰めて置き換えてしまい、1 つ目は置き換えた
        // 先に書き込んでから、置き換えで NotFound になる。
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game_info.jsonl");
        write_atomic(&path, b"old").unwrap();
        type During = (Result<()>, Vec<u8>, Vec<(PathBuf, Vec<u8>)>);
        let during: Rc<RefCell<Option<During>>> = Rc::default();
        {
            let (path, during) = (path.clone(), during.clone());
            on_temp_created(move || {
                let second = retry_refused(&path, || write_atomic(&path, b"second"));
                let files = listing(path.parent().unwrap())
                    .into_iter()
                    .map(|file| {
                        let bytes = fs::read(&file).unwrap();
                        (file, bytes)
                    })
                    .collect();
                *during.borrow_mut() = Some((second, fs::read(&path).unwrap(), files));
            });
        }
        retry_refused(&path, || write_atomic(&path, b"first")).unwrap();

        let (second, replaced, files) = during.take().expect("止め場所を通らなかった");
        second.unwrap();
        // 2 つ目は自分の一時ファイルで置き換え、1 つ目の一時ファイル (まだ空) には触れない。
        assert_eq!(replaced, b"second");
        assert_eq!(files.len(), 2, "{files:?}");
        let (temp, bytes) = files.iter().find(|(file, _)| *file != path).unwrap();
        let name = temp.file_name().unwrap().to_str().unwrap();
        assert!(is_temp_of(name, "game_info.jsonl"), "{name}");
        assert!(bytes.is_empty());
        // 1 つ目も置き換えに成功し、最後に置き換えた 1 つ目の中身が残る。一時ファイルは残らない。
        assert_eq!(fs::read(&path).unwrap(), b"first");
        assert_eq!(listing(dir.path()), [path]);
    }

    #[test]
    fn a_stopped_write_keeps_the_old_file() {
        // 一時ファイルを作ったところで止まった書き込み (終わるときの待ちを過ぎた・強制終了された) をまねる。
        // 元のファイルは前の中身のまま。残った一時ファイルは、ほかの書き込みの途中かもしれないあいだは残し
        // (同じ名前も使わない)、古くなったら次の書き込みで消す。
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        write_atomic(&path, b"old").unwrap();
        on_temp_created(|| panic!("一時ファイルを作ったところで止まった"));
        let stopped = std::panic::catch_unwind(|| write_atomic(&path, b"new"));
        assert!(stopped.is_err());
        assert_eq!(fs::read(&path).unwrap(), b"old");
        let files = listing(dir.path());
        assert_eq!(files.len(), 2, "{files:?}");
        let leftover = files.into_iter().find(|file| *file != path).unwrap();

        retry_refused(&path, || write_atomic(&path, b"next")).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"next");
        assert_eq!(listing(dir.path()), [leftover.clone(), path.clone()]);
        assert!(fs::read(&leftover).unwrap().is_empty());

        set_modified(
            &leftover,
            SystemTime::now() - STALE_TEMP_AGE - Duration::from_secs(60),
        );
        retry_refused(&path, || write_atomic(&path, b"last")).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"last");
        assert_eq!(listing(dir.path()), [path]);
    }

    #[test]
    fn replaces_a_file_while_it_is_being_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game_info.jsonl");
        write_atomic(&path, b"old").unwrap();
        // 読み手が開いている間 (旧形式を読むアプリの読み込みの途中) でも置き換えられる (Windows は std の rename
        // が POSIX の置き換えに切り替える)。読み手は前の中身を読み続け、新しく開くと新しい中身を読む。読み手が
        // 開いているせいで断られるなら、何度やり直しても断られて落ちる。
        let mut reading = File::open(&path).unwrap();
        retry_refused(&path, || write_atomic(&path, b"new")).unwrap();
        let mut before = Vec::new();
        reading.read_to_end(&mut before).unwrap();
        assert_eq!(before, b"old");
        assert_eq!(fs::read(&path).unwrap(), b"new");
        drop(reading);
        assert_eq!(listing(dir.path()), [path]);
    }

    #[test]
    fn failed_replacements_leave_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        // 置き換える先が空でないディレクトリだと、置き換えに失敗する。
        let path = dir.path().join("settings.toml");
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("inner"), b"x").unwrap();
        let err = write_atomic(&path, b"aaaa").unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("{} を置き換えられません", path.display())
        );
        assert_eq!(listing(dir.path()), [path]);
    }

    #[test]
    fn failed_writes_to_the_temp_file_leave_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("settings.toml");
        fs::write(&path, b"old").unwrap();
        let (tmp, file) = create_temp(dir.path(), "settings.toml", &AtomicU64::new(0)).unwrap();
        drop(file);
        // 書けないハンドル (読むだけで開いたもの) を渡すと、書く段で失敗する (ディスクがいっぱいのときなど)。
        let read_only = File::open(&tmp).unwrap();
        let err = replace_with(read_only, b"new", &tmp, &path).unwrap_err();
        assert_eq!(
            err.to_string(),
            format!("{} に書き込めません", tmp.display())
        );
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert_eq!(listing(dir.path()), [path]);
    }

    /// ほかのアプリが消すこと・名前を変えることを許さずに開いていると (共有モードに
    /// `FILE_SHARE_DELETE` がない)、置き換えに失敗する。そのときは前の中身が残り、一時ファイルは残らない。
    #[cfg(windows)]
    #[test]
    fn failed_replacements_keep_the_old_contents() {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_SHARE_READ: u32 = 0x1;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game_info.jsonl");
        write_atomic(&path, b"old").unwrap();
        let held = File::options()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&path)
            .unwrap();
        assert!(write_atomic(&path, b"new").is_err());
        drop(held);
        assert_eq!(fs::read(&path).unwrap(), b"old");
        assert_eq!(listing(dir.path()), [path]);
    }

    #[test]
    fn temp_files_never_reuse_leftovers() {
        let dir = tempfile::tempdir().unwrap();
        let pid = std::process::id();
        let temp = |sequence: usize| {
            dir.path()
                .join(format!(".settings.toml.{pid}.{sequence}.tmp"))
        };

        // 落ちたプロセスの残りと名前が重なったら (PID が使い回されたとき)、次の番号で作る。
        fs::write(temp(0), b"left").unwrap();
        fs::write(temp(1), b"left").unwrap();
        let (created, _file) =
            create_temp(dir.path(), "settings.toml", &AtomicU64::new(0)).unwrap();
        assert_eq!(created, temp(2));

        // 上限まで重なったら作らない。どちらでも、残りは書き換えず、消さない。
        for sequence in 3..3 + TEMP_ATTEMPTS {
            fs::write(temp(sequence), b"left").unwrap();
        }
        assert!(create_temp(dir.path(), "settings.toml", &AtomicU64::new(3)).is_err());
        for sequence in (0..2).chain(3..3 + TEMP_ATTEMPTS) {
            assert_eq!(fs::read(temp(sequence)).unwrap(), b"left");
        }
        assert_eq!(listing(dir.path()).len(), 3 + TEMP_ATTEMPTS);
    }

    #[test]
    fn recognizes_only_temp_files_of_the_same_file() {
        for name in [
            ".settings.toml.4242.0.tmp",
            ".settings.toml.4242.17.tmp",
            // 前の書き方。
            ".settings.toml.4242.tmp",
        ] {
            assert!(is_temp_of(name, "settings.toml"), "{name}");
        }
        for name in [
            "settings.toml",
            "settings.toml.4242.0.tmp",
            ".settings.toml.tmp",
            ".settings.toml..tmp",
            ".settings.toml.4242..tmp",
            ".settings.toml.4242.0.1.tmp",
            ".settings.toml.42a.0.tmp",
            ".settings.toml.4242.0.tmp.bak",
            ".subscriptions.toml.4242.0.tmp",
            ".settings.tomlx.4242.0.tmp",
        ] {
            assert!(!is_temp_of(name, "settings.toml"), "{name}");
        }
    }

    #[test]
    fn removes_only_stale_temp_files_of_the_written_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("game_info.jsonl");
        let at = |name: &str| dir.path().join(name);
        // 書き込みの途中で落ちたプロセスの残り (前の書き方のものも)、ほかの書き込みの途中のもの、
        // 最終更新が先の時刻のもの (時計を戻したとき)、ほかのファイルの古い一時ファイル、利用者のファイル。
        let stale = at(".game_info.jsonl.4242.0.tmp");
        let legacy = at(".game_info.jsonl.4242.tmp");
        let fresh = at(".game_info.jsonl.4242.1.tmp");
        let future = at(".game_info.jsonl.4242.2.tmp");
        let other = at(".settings.toml.4242.0.tmp");
        let unrelated = at(".game_info.jsonl.backup.tmp");
        for file in [&stale, &legacy, &fresh, &future, &other, &unrelated] {
            fs::write(file, b"half").unwrap();
        }
        // 一時ファイルの名前のディレクトリには触らない。
        let directory = at(".game_info.jsonl.4242.3.tmp");
        fs::create_dir(&directory).unwrap();
        let old = SystemTime::now() - STALE_TEMP_AGE - Duration::from_secs(60);
        for file in [&stale, &legacy, &other, &unrelated] {
            set_modified(file, old);
        }
        set_modified(&future, SystemTime::now() + Duration::from_secs(24 * 3600));

        write_atomic(&path, b"rows").unwrap();
        let mut expected = vec![path.clone(), fresh, future, other, unrelated, directory];
        expected.sort();
        assert_eq!(listing(dir.path()), expected);
        assert_eq!(fs::read(&path).unwrap(), b"rows");
    }

    #[test]
    fn stale_temp_files_are_judged_by_their_age() {
        let dir = tempfile::tempdir().unwrap();
        let temp = dir.path().join(".catalog.json.4242.0.tmp");
        fs::write(&temp, b"half").unwrap();
        let modified = SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000);
        set_modified(&temp, modified);
        let sweep = |now: SystemTime| remove_stale_temps(dir.path(), "catalog.json", now);
        // 新しいもの、最終更新が先の時刻のもの、ちょうど STALE_TEMP_AGE たったものは残す。
        sweep(modified);
        sweep(modified - Duration::from_secs(24 * 3600));
        sweep(modified + STALE_TEMP_AGE);
        assert!(temp.exists());
        // それより古くなったら消す。
        sweep(modified + STALE_TEMP_AGE + Duration::from_secs(1));
        assert!(!temp.exists());
    }

    /// 残りを消せなくても (ほかのアプリが削除を許さずに開いている)、書き込みは成功する。次に書いたときに消す。
    #[cfg(windows)]
    #[test]
    fn writes_succeed_even_if_leftovers_cannot_be_removed() {
        use std::os::windows::fs::OpenOptionsExt as _;
        const FILE_SHARE_READ: u32 = 0x1;

        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("subscriptions.toml");
        let leftover = dir.path().join(".subscriptions.toml.4242.0.tmp");
        fs::write(&leftover, b"half").unwrap();
        set_modified(
            &leftover,
            SystemTime::now() - STALE_TEMP_AGE - Duration::from_secs(60),
        );
        let held = File::options()
            .read(true)
            .share_mode(FILE_SHARE_READ)
            .open(&leftover)
            .unwrap();
        write_atomic(&path, b"items").unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"items");
        assert!(leftover.exists());
        drop(held);
        retry_refused(&path, || write_atomic(&path, b"items")).unwrap();
        assert_eq!(listing(dir.path()), [path]);
    }

    #[test]
    fn local_sources_ignore_temp_files() {
        let (_dir, store) = store();
        let source = crate::templates::blank(crate::ListKind::Json, "mine".into());
        let text = source_file_text(&source).unwrap();
        store.save_local_source("mine", &text).unwrap();
        // 書き込みの途中の一時ファイルは、手元の定義として読まない。
        let dir = store.paths().local_sources_dir();
        fs::write(dir.join(".mine.yaml.4242.0.tmp"), &text).unwrap();
        fs::write(dir.join(".other.yaml.4242.0.tmp"), "schema: [").unwrap();
        let ids: Vec<String> = store
            .load_local_sources()
            .unwrap()
            .into_iter()
            .map(|file| file.id)
            .collect();
        assert_eq!(ids, vec!["mine".to_string()]);
    }
}
