//! アプリ全体で 1 つのサービス。設定・カタログ・購読・ストア・取得エンジンを持ち、
//! 常駐の定期取得とカタログの取り込みを回す。
//!
//! 取得は画面とは独立した tokio ランタイムで動き、結果は [`ServiceEvent`] として
//! 画面側へ送る。ウィンドウを閉じてトレイに入っていても取得は続く。
//!
//! 予定の読み取り (`gnn-schedule`) も同じランタイムで回す。保存済みの記事のうち読み直しが要るもの
//! (未解析・抽出器が新しい・本文が変わった・ソースの地域時刻が変わった) を少しずつ取り出し、
//! ストアのロックを外して読み、記事の版を照合して保存する。
//!
//! 終わるときは [`Services::shutdown`] で止める。止める合図を立てると、新しい処理 (定期取得・カタログの
//! 取り込み・予定の読み取り・巡回のあとの片付け) を始めず、取得の通信を取り消す。始まっているストアへの
//! 書き込みは途中で切らずに終わるまで待ち、そのあとストアを閉じる。runtime の上の処理はすべて
//! [`Services::spawn`] (と `blocking`) で始め、終わるときに待つ処理として数える。

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::time::{Duration as StdDuration, Instant, SystemTime};

use anyhow::{Context as _, Result, bail};
use chrono::{DateTime, Duration, Utc};
use chrono_tz::Tz;
use futures::StreamExt as _;
use gnn_core::agenda::{self, AgendaDetail, AgendaItem, Resolution, SourceSchedule};
use gnn_core::catalog::{Catalog, ResolvedSource, resolve};
use gnn_core::config::{CachedFile, CatalogCache, LocalSourceFile};
use gnn_core::image_ref;
use gnn_core::source::Channel;
use gnn_core::store::NewEntry;
use gnn_core::{
    AnalysisInput, AnalysisResult, AppPaths, AppSettings, ConfigStore, EntryDraft, FetchRun,
    ImageHolder, ImageIdentity, RunStatus, SourceConfig, Store, Subscriptions,
};
use gnn_fetch::image_cache::{ImageFetchError, Refreshed};
use gnn_fetch::{
    CatalogFetch, CatalogLocation, Engine, HttpFetcher, HttpOptions, ImageCache, NetworkPolicy,
    RecheckMiss, RunInput, RunOutput,
};
use parking_lot::{Mutex, RwLock};
use tokio::sync::{Notify, Semaphore, oneshot};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::image_recovery::{RecoveryBatch, RecoveryItem, RecoveryQueue};

/// 定期取得の判定をする間隔。
const TICK: StdDuration = StdDuration::from_secs(60);
/// 投稿からこの時間内の記事は、毎回詳細を取り直す (追記に追いつくため)。
/// 予定の時間が近いメンテナンスの記事は、投稿からの時間によらず取り直す (`agenda::needs_recheck`)。
const RECHECK_HOURS: i64 = 48;
/// 1 回にまとめて予定を読む記事の数。読むあいだはストアのロックを外す。
const ANALYSIS_BATCH: usize = 48;
/// カタログを取り込み直す間隔。
const CATALOG_REFRESH_HOURS: i64 = 24;
/// 1 つのソースの取得にかけてよい時間。
const FETCH_TIMEOUT: StdDuration = StdDuration::from_secs(300);
/// 画像キャッシュの上限。
const IMAGE_CACHE_BYTES: u64 = 1024 * 1024 * 1024;
/// 画像キャッシュを残す日数。
const IMAGE_CACHE_DAYS: u64 = 180;
/// 1 枚の画像の上限。
pub const MAX_IMAGE_BYTES: usize = 15 * 1024 * 1024;
/// 終わるときに、動いている処理とストアへの書き込みを待つ上限。macOS は
/// `applicationWillTerminate` の中で待つので長くしない (`--quit` を呼ぶインストーラーは 10 秒待つ)。
pub const SHUTDOWN_TIMEOUT: StdDuration = StdDuration::from_secs(3);
/// 終わるときにストアを閉じたあとの、ストアの処理の誤り。
const STORE_CLOSED: &str = "終了の処理でストアを閉じたため、使えません";
/// 取得で見た画像の参照 (`url_identity.images`) を、配信元で確かめ直すまでの時間。参照は鍵が変わらないので、
/// 配信元が同じパスの画像を差し替えても、確かめ直さないと古い画像を出し続ける。
const IMAGE_REVALIDATE_AFTER: StdDuration = StdDuration::from_secs(24 * 3600);
/// 1 回の取得で確かめ直す画像の上限。残りは次の取得で確かめる。
const IMAGE_REVALIDATE_LIMIT: usize = 32;
/// 1 回の取得で確かめ直す画像の大きさの上限 (検証子のない配信元では、確かめ直すたびに全部を取る)。
const IMAGE_REVALIDATE_BYTES: u64 = 64 * 1024 * 1024;
/// 確かめ直しに失敗した画像を、次に確かめるまでの時間 (続けて失敗すると倍にし、[`IMAGE_RETRY_MAX`] まで)。
const IMAGE_RETRY_BASE: StdDuration = StdDuration::from_secs(30 * 60);
const IMAGE_RETRY_MAX: StdDuration = StdDuration::from_secs(24 * 3600);
/// 先読みで同時に取る画像の数。
const IMAGE_PREFETCH_CONCURRENCY: usize = 4;
/// 取得 URL で取れなかった本文の画像を取り直すときに、詳細を取り直す記事の数 (画像ごと)。
const RECOVERY_HOLDERS_PER_IMAGE: usize = 3;
/// 取り直し 1 回で、詳細を取り直す記事の数の上限。
const RECOVERY_RECHECK_LIMIT: usize = 10;
/// 取り直しても新しい取得 URL で取れない画像がこの数だけ重なったら、そのソースの取り直しを休む。
const RECOVERY_PAUSE_AFTER: usize = 2;
/// 取り直しを休む時間。
const RECOVERY_PAUSE: StdDuration = StdDuration::from_secs(3600);

/// サービスから画面への知らせ。
#[derive(Debug, Clone)]
pub enum ServiceEvent {
    FetchStarted {
        source_id: String,
    },
    FetchFinished(FetchSummary),
    /// 1 回の巡回が終わった。新着を通知に使う。`pruned` は保存期間を過ぎて消した記事の数。
    CycleFinished {
        added: Vec<AddedEntry>,
        pruned: usize,
    },
    /// カタログ・購読・手元の定義が変わり、使うソースが変わった。
    SourcesChanged,
    /// 予定の表が変わった (読み取りの結果を保存した・記事が消えて突き合わせを作り直した)。
    /// `sources` は表を作り直したソース。新着とは別の知らせで、既読や並びの時刻は変わっていない。
    ScheduleChanged {
        sources: Vec<String>,
    },
    /// 画像キャッシュに画像を入れた (先読み・取り直し) か、配信元で差し替わっていたので入れ直した
    /// (確かめ直し)。値はキャッシュの鍵 (画像の参照か URL)。画面は、失敗を覚えている画像を読み直し、
    /// 入れ直した画像は表示できていても読み直す。
    ImagesCached {
        available: Vec<String>,
        changed: Vec<String>,
    },
    /// 画面の HTTP クライアントが、一時的に取れなかった画像を読み直す時刻になった (値は画面が求めた URI と
    /// 予約の番号)。
    RetryImages(Vec<(String, u64)>),
    /// 画面の HTTP クライアントが、gpui が読めなかったキャッシュの画像 (確かめていないもの) を捨てた (値は
    /// キャッシュの鍵)。画面は、覚えている失敗を消して読み直させる (キャッシュにないので取り直す)。
    ImagesDiscarded(Vec<String>),
    /// 2 つ目に起動されたアプリやトレイからの「ウィンドウを出して」。
    ShowWindow,
    /// トレイからの「終了」と、`--quit` で起動されたアプリからの終了の依頼 (instance.rs)。
    Quit,
}

#[derive(Debug, Clone)]
pub struct FetchSummary {
    pub source_id: String,
    pub status: RunStatus,
    pub message: Option<String>,
    pub listed: usize,
    pub added: usize,
    pub updated: usize,
    /// 一覧から消えて削除した記事の数 (新作・近日リリース)。
    pub removed: usize,
}

impl FetchSummary {
    /// 記事が増えたり変わったり消えたりしたか。
    pub fn changed(&self) -> bool {
        self.added > 0 || self.updated > 0 || self.removed > 0
    }
}

/// 新しく保存された記事。
#[derive(Debug, Clone)]
pub struct AddedEntry {
    pub entry_id: i64,
    pub source_name: String,
    pub channel: Channel,
    pub title: String,
}

/// カタログの取り込みの状況。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CatalogStatus {
    /// 取り込む場所 (設定の値)。
    pub location: String,
    /// 取り込んだ版 (コミットの SHA など)。
    pub revision: Option<String>,
    /// 最後に取り込みを確かめた時刻。同梱版のままなら `None`。
    pub checked_at: Option<DateTime<Utc>>,
    /// 取り込み中か。
    pub checking: bool,
    /// 直前の取り込みの誤り。
    pub error: Option<String>,
}

/// 取得先の制限と秘密の値の扱いを、ソースの出どころから決める。
pub fn fetch_permissions(source: &ResolvedSource) -> (NetworkPolicy, bool) {
    let network = if source.allow_private_network {
        NetworkPolicy::Any
    } else {
        NetworkPolicy::PublicOnly
    };
    (network, !source.origin.is_catalog())
}

#[derive(Clone)]
pub struct Services(Arc<Inner>);

/// 取得用の tokio のランタイム。落とすときに、残っている処理を待たない。
///
/// 終わるときに待つのは [`Services::shutdown`] が期限を決めて行う。`Runtime` をそのまま落とすと、
/// 動いているブロッキングの処理 (待たないと決めた画像の掃除・取り消したカタログの読み込み・期限を
/// 過ぎた書き込み) が終わるまで待ち続け、runtime のスレッドの上で落とすと panic する。
struct FetchRuntime(Option<tokio::runtime::Runtime>);

impl std::ops::Deref for FetchRuntime {
    type Target = tokio::runtime::Runtime;

    fn deref(&self) -> &Self::Target {
        self.0.as_ref().expect("ランタイムは落とすときまである")
    }
}

impl Drop for FetchRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_background();
        }
    }
}

/// 止める合図のあとなので、ストアの処理を始めなかった ([`Services::blocking`])。
/// 終わる途中のことなので、誤りとして記録しない。
#[derive(Debug)]
struct Stopped;

impl std::fmt::Display for Stopped {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("終了の途中のため、ストアの処理を始めませんでした")
    }
}

impl std::error::Error for Stopped {}

fn is_stopped(err: &anyhow::Error) -> bool {
    err.is::<Stopped>()
}

/// テストで競合の順番を決めるため、処理を止めて待たせる場所 (`Services::pause`)。どれもその処理の直前。
#[cfg(test)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pause {
    /// `spawn` が止める合図を確かめたあと、処理を数える前 (受け付けの中)。
    Register,
    /// `blocking` がストアのロックを取りに行く前。
    StoreLock,
    /// `shutdown` がストアを閉じる前 (閉じる係の中)。
    Close,
}

#[cfg(test)]
type PauseHook = Arc<dyn Fn(Pause) + Send + Sync>;

/// 取得中の印 (`Inner::running`)。落とすと外す (取り消したとき・始めなかったときも残らない)。
struct Running {
    services: Services,
    id: String,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.services.0.running.lock().remove(&self.id);
    }
}

struct Inner {
    config: ConfigStore,
    runtime: FetchRuntime,
    /// 終わるときに閉じて `None` にする ([`Services::shutdown`])。
    store: Mutex<Option<Store>>,
    engine: Engine,
    images: ImageCache,
    events: flume::Sender<ServiceEvent>,
    shared: RwLock<Shared>,
    running: Mutex<HashSet<String>>,
    /// 同時に取得するソースの数を、手動の取得と定期の取得をまとめて抑える。
    permits: Mutex<(usize, Arc<Semaphore>)>,
    wake: Notify,
    /// 予定の読み取りを促す (取得の保存・旧形式の取り込み・ソースの変更・記事の削除のあと)。
    analysis_wake: Notify,
    last_image_prune: Mutex<Option<SystemTime>>,
    /// カタログの取り込みを同時に 1 つに限る。
    catalog_lock: tokio::sync::Mutex<()>,
    /// 止める合図 (終わるときに立てる)。立ったら新しい処理を始めず、取得の通信を取り消す。
    stop: CancellationToken,
    /// runtime の上で動いている処理 (ループ・取得・カタログの取り込み・予定の読み取り・ストアの処理)。
    /// 終わるときに、これが空になるのを待つ。
    tasks: TaskTracker,
    /// 処理の受け付け ([`Services::spawn`]) と止める合図を並べる。合図の直前に受け付けた処理を、
    /// 待つ処理に数えそこねないため。持ったまま待たない。
    admission: Mutex<()>,
    /// [`Services::shutdown`] の結果。終わる処理は 1 度だけ行い、2 回目からはこれを返す。
    shutdown: Mutex<Option<bool>>,
    /// テストで、決めた場所で処理を止める ([`Pause`])。
    #[cfg(test)]
    pause: RwLock<Option<PauseHook>>,
    /// 取得 URL で取れなかった画像の参照の、取り直しの待ち行列。
    recovery: Mutex<RecoveryQueue>,
    recovery_wake: Notify,
    /// 先読みで取れなかった画像 (キャッシュの鍵ごと)。取れたら消す。
    image_failures: Mutex<HashMap<String, ImageFailure>>,
}

/// 先読み・確かめ直しで取れなかった画像。
#[derive(Debug, Clone, Copy)]
struct ImageFailure {
    /// 時間をおけば取れそうか (通信の失敗・5xx・429)。
    transient: bool,
    /// 続けて失敗した回数。
    count: u32,
    /// これより前には確かめ直さない (キャッシュにない画像は、取得のたびに取りに行く)。
    retry_at: Instant,
    /// 配信元が指定した待ち時間 (Retry-After) が明ける時刻。キャッシュにない画像も、これより前には取りに行かない。
    not_before: Option<Instant>,
}

impl ImageFailure {
    /// 前の失敗 (あれば) に続けて失敗した。
    fn after(previous: Option<&ImageFailure>, err: &ImageFetchError, now: Instant) -> Self {
        let count = previous.map_or(1, |previous| previous.count.saturating_add(1));
        let backoff = IMAGE_RETRY_BASE
            .saturating_mul(1u32 << (count - 1).min(10))
            .min(IMAGE_RETRY_MAX);
        let not_before = err.retry_after().map(|wait| now + wait);
        Self {
            transient: err.is_transient(),
            count,
            retry_at: not_before.map_or(now + backoff, |at| at.max(now + backoff)),
            not_before,
        }
    }
}

/// 1 つのソースの取得の頼み。
struct FetchJob {
    source: ResolvedSource,
    /// 予定の近いメンテナンスの記事のほかに、詳細を取り直す記事の外部 ID
    /// (取得 URL で取れなかった本文の画像を持つ記事)。
    recheck: HashSet<String>,
}

impl From<ResolvedSource> for FetchJob {
    fn from(source: ResolvedSource) -> Self {
        Self {
            source,
            recheck: HashSet::new(),
        }
    }
}

/// 1 つのソースの取得の結果。
struct FetchOutcome {
    summary: FetchSummary,
    added: Vec<AddedEntry>,
    /// 詳細を取り直す記事のうち、取れなかったもの (`RunOutput::recheck_misses`)。
    recheck_misses: HashMap<String, RecheckMiss>,
}

/// ソースごとの、予定を読むときの文脈。
#[derive(Debug, Clone)]
struct AnalysisContext {
    /// 地域時刻の明記がない本文の日時を読む地域時刻。
    timezone: Tz,
    /// 「サーバー時間」の地域時刻。
    server_timezone: Option<Tz>,
    /// 保存した結果と比べる指紋。変われば読み直す。
    fingerprint: String,
}

impl AnalysisContext {
    fn of(source: &SourceConfig) -> Self {
        let timezone = source.schedule_timezone();
        let server_timezone = source.server_timezone();
        Self {
            fingerprint: format!(
                "timezone={};server_timezone={}",
                timezone.name(),
                server_timezone.map_or("-", |tz| tz.name())
            ),
            timezone,
            server_timezone,
        }
    }
}

struct Shared {
    settings: AppSettings,
    catalog: Arc<Catalog>,
    catalog_status: CatalogStatus,
    /// カタログの控えの ETag と、それを返した場所。場所を変えたら使わない。
    catalog_etag: Option<(String, String)>,
    subscriptions: Subscriptions,
    local: Vec<LocalSourceFile>,
    resolved: Vec<ResolvedSource>,
}

impl Shared {
    fn resolve(&mut self) {
        let local: Vec<SourceConfig> = self
            .local
            .iter()
            .filter_map(|file| file.source.as_ref().ok().cloned())
            .collect();
        self.resolved = resolve(&self.catalog, &self.subscriptions, &local);
    }
}

/// 取得の結果などを知らせる移行の結果。起動のあとで画面に出す。
#[derive(Debug, Clone, Default)]
pub struct StartupNotes {
    /// 旧形式の `sources.toml` を移した。
    pub migrated: Option<gnn_core::config::LegacyMigration>,
}

impl Services {
    /// 設定を読み、ストアを開き、ランタイムを立ち上げる。
    pub fn start(
        paths: AppPaths,
        events: flume::Sender<ServiceEvent>,
    ) -> Result<(Self, StartupNotes)> {
        paths.ensure()?;
        let config = ConfigStore::new(paths.clone());
        let settings = config.load_settings()?;
        let secrets = config.load_secrets()?;
        let store = Store::open(&paths.database_file())?;

        // カタログ: 同じ場所から取り込んだ控えがあればそれを、なければ同梱版を使う。
        let bundled = Catalog::bundled();
        let cache = config
            .load_catalog_cache()?
            .filter(|cache| cache.location == settings.catalog);
        let (catalog, catalog_status, catalog_etag) = match cache {
            Some(cache) => {
                let files = cache
                    .files
                    .iter()
                    .map(|file| (file.path.as_str(), file.text.as_str()));
                (
                    Catalog::from_files(files, Some(&bundled)),
                    CatalogStatus {
                        location: settings.catalog.clone(),
                        revision: cache.revision.clone(),
                        checked_at: Some(cache.fetched_at),
                        ..CatalogStatus::default()
                    },
                    // アプリの版が変わったら ETag は使わない (304 で読み直せなくなるため)。
                    cache
                        .etag
                        .filter(|_| {
                            cache.app_version.as_deref() == Some(gnn_core::source::APP_VERSION)
                        })
                        .map(|etag| (cache.location.clone(), etag)),
                )
            }
            None => (
                bundled,
                CatalogStatus {
                    location: settings.catalog.clone(),
                    ..CatalogStatus::default()
                },
                None,
            ),
        };

        // 購読: なければ旧形式から移すか、既定のグループを on にする。
        let mut notes = StartupNotes::default();
        let mut subscriptions = match config.load_subscriptions()? {
            Some(subscriptions) => subscriptions,
            None => {
                notes.migrated = config.migrate_legacy_sources(&catalog, &settings)?;
                match config.load_subscriptions()? {
                    Some(subscriptions) => subscriptions,
                    None => {
                        let defaults = catalog.default_subscriptions();
                        config.save_subscriptions(&defaults)?;
                        defaults
                    }
                }
            }
        };
        // 前の起動 (や旧形式) のあとでカタログに加わったソースを、既定どおりに足す。
        let before = subscriptions.items.len();
        let adopted = catalog.adopt_new_entries(&mut subscriptions);
        if subscriptions.items.len() != before {
            config.save_subscriptions(&subscriptions)?;
            if !adopted.is_empty() {
                tracing::info!(
                    "カタログに加わったソースを on にしました: {}",
                    adopted.join(", ")
                );
            }
        }
        let local = config.load_local_sources()?;
        for file in &local {
            if let Err(err) = &file.source {
                tracing::warn!("手元の定義 {} を読めません: {err}", file.path.display());
            }
        }

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(3)
            .thread_name("gnn-fetch")
            .enable_all()
            .build()
            .context("取得用のランタイムを起動できません")?;
        let http = HttpFetcher::new(
            HttpOptions {
                user_agent: settings
                    .user_agent
                    .clone()
                    .unwrap_or_else(|| gnn_fetch::http::DEFAULT_USER_AGENT.to_string()),
                ..HttpOptions::default()
            },
            secrets,
        )?;

        let mut shared = Shared {
            settings,
            catalog: Arc::new(catalog),
            catalog_status,
            catalog_etag,
            subscriptions,
            local,
            resolved: Vec::new(),
        };
        shared.resolve();

        Ok((
            Self(Arc::new(Inner {
                config,
                runtime: FetchRuntime(Some(runtime)),
                store: Mutex::new(Some(store)),
                engine: Engine::new(http),
                images: ImageCache::new(paths.images_dir()),
                events,
                shared: RwLock::new(shared),
                running: Mutex::new(HashSet::new()),
                permits: Mutex::new((0, Arc::new(Semaphore::new(1)))),
                wake: Notify::new(),
                analysis_wake: Notify::new(),
                last_image_prune: Mutex::new(None),
                catalog_lock: tokio::sync::Mutex::new(()),
                stop: CancellationToken::new(),
                tasks: TaskTracker::new(),
                admission: Mutex::new(()),
                shutdown: Mutex::new(None),
                #[cfg(test)]
                pause: RwLock::new(None),
                recovery: Mutex::new(RecoveryQueue::default()),
                recovery_wake: Notify::new(),
                image_failures: Mutex::new(HashMap::new()),
            })),
            notes,
        ))
    }

    pub fn paths(&self) -> &AppPaths {
        self.0.config.paths()
    }

    pub fn images(&self) -> &ImageCache {
        &self.0.images
    }

    pub fn runtime(&self) -> tokio::runtime::Handle {
        self.0.runtime.handle().clone()
    }

    /// 画像の取得に使う HTTP クライアント。本文の画像はどのソースのものでも
    /// 第三者の HTML から来るので、手元のネットワークへは取りに行かない。
    pub fn http_client(&self) -> reqwest::Client {
        self.0
            .engine
            .http()
            .client(NetworkPolicy::PublicOnly)
            .clone()
    }

    pub fn events(&self) -> flume::Sender<ServiceEvent> {
        self.0.events.clone()
    }

    /// ストアを使う。呼び出し側のスレッドで同期的に動くので、画面のスレッドからは呼ばない。
    /// 終わるときにストアを閉じたあと ([`Services::shutdown`]) は、`f` を呼ばずに誤りを返す。
    pub fn with_store<R>(&self, f: impl FnOnce(&mut Store) -> Result<R>) -> Result<R> {
        let mut store = self.0.store.lock();
        f(store.as_mut().context(STORE_CLOSED)?)
    }

    /// 止める合図が立っているか (終わる途中か)。
    fn stopping(&self) -> bool {
        self.0.stop.is_cancelled()
    }

    /// `future` を動かし、止める合図が来たら取り消して `None` を返す。合図と同時に済んだときも
    /// 合図を先に見る (`CancellationToken::run_until_cancelled` は済んだほうを先に見るので使わない)。
    async fn unless_stopped<F: Future>(&self, future: F) -> Option<F::Output> {
        tokio::select! {
            biased;
            () = self.0.stop.cancelled() => None,
            output = future => Some(output),
        }
    }

    /// runtime で処理を始め、終わるときに待つ処理として数える。止める合図のあとは始めずに `None`
    /// (`task` は落とす)。始まるのが合図のあとになった処理は、入口で合図を見て抜ける。
    fn spawn<F>(&self, task: F) -> Option<tokio::task::JoinHandle<F::Output>>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let _admission = self.0.admission.lock();
        if self.stopping() {
            return None;
        }
        #[cfg(test)]
        self.pause(Pause::Register);
        Some(self.0.tasks.spawn_on(task, self.0.runtime.handle()))
    }

    /// テストで、競合の順番を決めるために `at` で止まる ([`Pause`])。
    #[cfg(test)]
    fn pause(&self, at: Pause) {
        let hook = self.0.pause.read().clone();
        if let Some(hook) = hook {
            hook(at);
        }
    }

    /// 画面で使う止める合図 (画像の取得を取り消すため)。立てるのは [`Services::shutdown`] だけ。
    pub fn stop_signal(&self) -> CancellationToken {
        self.0.stop.child_token()
    }

    /// 画像の参照 (ソース ID, 鍵) を取りに行く配信元の URL (最後に見たもの)。ストアを読むので、
    /// 画面のスレッドでは呼ばない。
    pub fn image_fetch_url(&self, source_id: &str, key: &str) -> Option<String> {
        self.with_store(|store| store.image_fetch_url(source_id, key))
            .inspect_err(|err| tracing::warn!("画像の取得 URL を読めません: {err:#}"))
            .ok()
            .flatten()
    }

    pub fn settings(&self) -> AppSettings {
        self.0.shared.read().settings.clone()
    }

    /// 使うソース (カタログと手元の定義を購読で決めたもの)。off のものも含む。
    pub fn sources(&self) -> Vec<ResolvedSource> {
        self.0.shared.read().resolved.clone()
    }

    pub fn catalog(&self) -> Arc<Catalog> {
        self.0.shared.read().catalog.clone()
    }

    pub fn catalog_status(&self) -> CatalogStatus {
        self.0.shared.read().catalog_status.clone()
    }

    pub fn subscriptions(&self) -> Subscriptions {
        self.0.shared.read().subscriptions.clone()
    }

    /// 手元の定義のファイル。読めないものも含む。
    pub fn local_files(&self) -> Vec<LocalSourceFile> {
        self.0.shared.read().local.clone()
    }

    pub fn running(&self) -> HashSet<String> {
        self.0.running.lock().clone()
    }

    pub fn save_settings(&self, settings: AppSettings) -> Result<()> {
        self.0.config.save_settings(&settings)?;
        let location_changed = {
            let mut shared = self.0.shared.write();
            let changed = shared.settings.catalog != settings.catalog;
            shared.settings = settings;
            changed
        };
        if location_changed {
            self.refresh_catalog();
        }
        self.0.wake.notify_one();
        Ok(())
    }

    /// 購読を書き換えて保存し、使うソースを決め直す。
    pub fn update_subscriptions(&self, f: impl FnOnce(&mut Subscriptions)) -> Result<()> {
        {
            let mut shared = self.0.shared.write();
            let mut subscriptions = shared.subscriptions.clone();
            f(&mut subscriptions);
            self.0.config.save_subscriptions(&subscriptions)?;
            shared.subscriptions = subscriptions;
            shared.resolve();
        }
        self.sources_changed();
        Ok(())
    }

    /// 手元の定義を保存する (新規・上書き・カタログの写し)。`text` は YAML の全文。
    pub fn save_local_source(&self, id: &str, text: &str, enable: bool) -> Result<()> {
        self.0.config.save_local_source(id, text)?;
        let local = self.0.config.load_local_sources()?;
        {
            let mut shared = self.0.shared.write();
            shared.local = local;
            if enable {
                let mut subscriptions = shared.subscriptions.clone();
                subscriptions.set_enabled(id, true);
                self.0.config.save_subscriptions(&subscriptions)?;
                shared.subscriptions = subscriptions;
            }
            shared.resolve();
        }
        self.sources_changed();
        Ok(())
    }

    /// 手元の定義を消す。カタログを上書きしていたなら、カタログの定義に戻る。
    /// 手元だけのソースなら購読も消す。記事を消すかは呼び出し側が決める。
    pub fn delete_local_source(&self, id: &str) -> Result<()> {
        self.0.config.delete_local_source(id)?;
        let local = self.0.config.load_local_sources()?;
        {
            let mut shared = self.0.shared.write();
            shared.local = local;
            if shared.catalog.get(id).is_none() {
                let mut subscriptions = shared.subscriptions.clone();
                subscriptions.remove(id);
                self.0.config.save_subscriptions(&subscriptions)?;
                shared.subscriptions = subscriptions;
            }
            shared.resolve();
        }
        self.sources_changed();
        Ok(())
    }

    fn sources_changed(&self) {
        let _ = self.0.events.send(ServiceEvent::SourcesChanged);
        self.0.wake.notify_one();
        // 本文の地域時刻 (`schedule`) が変わったソースや、on にしたソースの記事を読み直す。
        self.wake_analysis();
    }

    /// 常駐の定期取得・カタログの取り込み・予定の読み取り・画像の取り直しを始める。
    pub fn spawn_scheduler(&self) {
        let services = self.clone();
        self.spawn(async move { services.scheduler_loop().await });
        let services = self.clone();
        self.spawn(async move { services.analysis_loop().await });
        let services = self.clone();
        self.spawn(async move { services.image_recovery_loop().await });
    }

    /// カタログを今すぐ取り込む。
    pub fn refresh_catalog(&self) {
        let services = self.clone();
        self.spawn(async move {
            services.refresh_catalog_inner().await;
        });
    }

    async fn refresh_catalog_inner(&self) {
        // 終わる途中なら、ほかの取り込みが終わるのも待たない。
        let Some(_guard) = self.unless_stopped(self.0.catalog_lock.lock()).await else {
            return;
        };
        let (location, etag, previous) = {
            let mut shared = self.0.shared.write();
            shared.catalog_status.checking = true;
            shared.catalog_status.location = shared.settings.catalog.clone();
            (
                shared.settings.catalog.clone(),
                // 場所を変えたら、前の場所の ETag は使わない。
                shared
                    .catalog_etag
                    .as_ref()
                    .filter(|(etag_location, _)| *etag_location == shared.settings.catalog)
                    .map(|(_, etag)| etag.clone()),
                shared.catalog.clone(),
            )
        };
        let _ = self.0.events.send(ServiceEvent::SourcesChanged);
        let fetched = self
            .unless_stopped(gnn_fetch::fetch_catalog(
                self.0.engine.http(),
                &CatalogLocation::parse(&location),
                etag.as_deref(),
            ))
            .await;
        // 取り込んだカタログを反映する前にも合図を見る。
        let Some(result) = fetched.filter(|_| !self.stopping()) else {
            // 終わる途中。取り込みを取り消し、控えも購読も前のまま残す (取り込みの中の読み込みは
            // 読むだけなので、取り消したあと残っても待たない)。
            self.0.shared.write().catalog_status.checking = false;
            return;
        };
        let now = Utc::now();
        let outcome: Result<()> = match result {
            Ok(CatalogFetch::NotModified) => {
                let files = previous
                    .files()
                    .into_iter()
                    .map(|(path, text)| CachedFile { path, text })
                    .collect();
                let revision = self.0.shared.read().catalog_status.revision.clone();
                self.0.config.save_catalog_cache(&CatalogCache {
                    location: location.clone(),
                    etag: etag.clone(),
                    revision,
                    app_version: Some(gnn_core::source::APP_VERSION.into()),
                    fetched_at: now,
                    files,
                })
            }
            Ok(CatalogFetch::Updated(download)) => {
                let catalog = Catalog::from_files(
                    download
                        .files
                        .iter()
                        .map(|(path, text)| (path.as_str(), text.as_str())),
                    Some(&previous),
                );
                if catalog.is_empty() && !previous.is_empty() {
                    Err(anyhow::anyhow!("取り込んだカタログにソースがありません"))
                } else {
                    for problem in catalog.problems() {
                        tracing::warn!("カタログ: {problem}");
                    }
                    let saved = self.0.config.save_catalog_cache(&CatalogCache {
                        location: location.clone(),
                        etag: download.etag.clone(),
                        revision: download.revision.clone(),
                        app_version: Some(gnn_core::source::APP_VERSION.into()),
                        fetched_at: now,
                        files: catalog
                            .files()
                            .into_iter()
                            .map(|(path, text)| CachedFile { path, text })
                            .collect(),
                    });
                    let mut shared = self.0.shared.write();
                    // カタログに加わったソースを、既定どおりに購読に足す。
                    let mut subscriptions = shared.subscriptions.clone();
                    let before = subscriptions.items.len();
                    let adopted = catalog.adopt_new_entries(&mut subscriptions);
                    let saved = saved.and_then(|()| {
                        if subscriptions.items.len() != before {
                            self.0.config.save_subscriptions(&subscriptions)?;
                            shared.subscriptions = subscriptions;
                        }
                        Ok(())
                    });
                    if !adopted.is_empty() {
                        tracing::info!(
                            "カタログに加わったソースを on にしました: {}",
                            adopted.join(", ")
                        );
                    }
                    shared.catalog = Arc::new(catalog);
                    shared.catalog_etag = download.etag.map(|etag| (location.clone(), etag));
                    shared.catalog_status.revision = download.revision;
                    shared.resolve();
                    saved
                }
            }
            Err(err) => Err(err),
        };
        {
            let mut shared = self.0.shared.write();
            shared.catalog_status.checking = false;
            match outcome {
                Ok(()) => {
                    shared.catalog_status.checked_at = Some(now);
                    shared.catalog_status.error = None;
                }
                Err(err) => {
                    tracing::warn!("カタログを取り込めません: {err:#}");
                    shared.catalog_status.error = Some(format!("{err:#}"));
                }
            }
        }
        self.sources_changed();
    }

    /// 指定したソース (省略時は on のすべて) を今すぐ取得する。
    pub fn fetch_now(&self, source_ids: Option<Vec<String>>) {
        let sources: Vec<ResolvedSource> = self
            .sources()
            .into_iter()
            .filter(|source| match &source_ids {
                Some(ids) => ids.iter().any(|id| id == source.id()),
                None => source.enabled,
            })
            .collect();
        let services = self.clone();
        self.spawn(async move {
            services
                .run_cycle(sources.into_iter().map(FetchJob::from).collect())
                .await;
        });
    }

    /// 保存せずに取得だけ試す。ソース画面の「テスト取得」に使う。
    ///
    /// `template` は取得先の制限を決めるためのソース (同じ ID の今の設定)。新しく作る
    /// ソースなら `None` で、手元の定義と同じ扱い (公開のネットワークだけ) にする。
    pub fn test_fetch(
        &self,
        source: SourceConfig,
        from_catalog: bool,
        limit: usize,
    ) -> oneshot::Receiver<RunOutput> {
        let (sender, receiver) = oneshot::channel();
        let services = self.clone();
        let settings = self.settings();
        let allow_private = !from_catalog
            && self
                .sources()
                .iter()
                .any(|resolved| resolved.id() == source.id && resolved.allow_private_network);
        self.spawn(async move {
            // クライアントを作るのは画面のスレッドの外で行う。
            let engine = services.isolated_engine();
            let known = HashMap::new();
            let run = engine.run(RunInput {
                max_age_days: settings.max_age_for(&source),
                source: &source,
                known: &known,
                now: Utc::now(),
                recheck: Duration::hours(RECHECK_HOURS),
                recheck_ids: HashSet::new(),
                limit: Some(limit),
                network: if allow_private {
                    NetworkPolicy::Any
                } else {
                    NetworkPolicy::PublicOnly
                },
                allow_secrets: !from_catalog,
                trace: true,
            });
            // 終わる途中なら取り消す。送り手を落とすので、画面は待つのをやめる。
            if let Some(output) = services.unless_stopped(run).await {
                let _ = sender.send(output);
            }
        });
        receiver
    }

    /// 画面なしで、on のソースを 1 度ずつ取得する。
    pub fn fetch_once_blocking(&self) -> Vec<FetchSummary> {
        let services = self.clone();
        self.0.runtime.block_on(async move {
            // 画面なしでも、取得の前にカタログを取り込む。
            services.refresh_catalog_inner().await;
            let sources: Vec<ResolvedSource> = services
                .sources()
                .into_iter()
                .filter(|source| source.enabled)
                .collect();
            let (outcomes, _) = services
                .run_cycle_inner(sources.into_iter().map(FetchJob::from).collect())
                .await;
            // 画面なしでも、取得した記事の予定を読んでおく (次に画面を開いたときにすぐ出せるように)。
            services.drain_analysis().await;
            outcomes
                .into_iter()
                .map(|outcome| outcome.summary)
                .collect()
        })
    }

    /// 終わる。止める合図を立てて新しい処理を始めず、取得の通信を取り消し、動いている処理が抜けるのを
    /// 待つ。始まっているストアへの書き込みは途中で切らず、画面からの書き込み (既読など) もロックを
    /// 持っていれば済むのを待ってから、ストアを閉じる。閉じたあとの [`Services::with_store`] は誤りを返す。
    ///
    /// 待つのは、ストアを閉じる (WAL を書き戻す) のも含めて `timeout` まで。すべて済めば `true`。
    /// 過ぎたら待つのをやめて `false` を返す。そのあとプロセスが終わって書き込みが切れても、SQLite は
    /// 確定していないトランザクションを巻き戻すだけでデータベースは壊れない (記事の保存と取得ログの記録は
    /// 別のトランザクションなので、記事だけが残ることはある。次の取得で取り直す)。終わる処理は 1 度だけ
    /// 行い、2 回目からは 1 回目の結果を返す。runtime の上の処理を待つので、runtime のスレッドからは呼ばない。
    pub fn shutdown(&self, timeout: StdDuration) -> bool {
        let mut outcome = self.0.shutdown.lock();
        if let Some(finished) = *outcome {
            return finished;
        }
        let started = Instant::now();
        let deadline = started + timeout;
        {
            // 受け付けと並べる。合図より前に受け付けた処理は待つ処理に入り、あとの処理は始まらない。
            let _admission = self.0.admission.lock();
            self.0.stop.cancel();
            self.0.tasks.close();
        }

        // 動いている処理が抜けるのを待つ。待つ係は runtime の上で動かし、待つ処理には数えない。
        let (done, finished) = flume::bounded(1);
        let tasks = self.0.tasks.clone();
        self.0.runtime.spawn(async move {
            tasks.wait().await;
            let _ = done.send(());
        });
        let tasks_finished = finished.recv_deadline(deadline).is_ok();
        let seconds = timeout.as_secs_f32();
        if !tasks_finished {
            tracing::warn!(
                "{seconds} 秒たっても取得か予定の読み取りが終わらないため、待たずに終わります"
            );
        }

        // ストアを取り出してロックを離し、閉じるのは別のスレッドで行う (WAL の書き戻しも期限で待つ)。
        let store = self
            .0
            .store
            .try_lock_until(deadline)
            .map(|mut store| store.take());
        let store_closed = match store {
            None => {
                tracing::warn!(
                    "{seconds} 秒たってもストアへの書き込みが終わらないため、ストアを閉じずに終わります"
                );
                false
            }
            Some(None) => true,
            Some(Some(store)) => {
                let (closed, result) = flume::bounded(1);
                #[cfg(test)]
                let services = self.clone();
                self.0.runtime.spawn_blocking(move || {
                    #[cfg(test)]
                    services.pause(Pause::Close);
                    let _ = closed.send(store.close());
                });
                match result.recv_deadline(deadline) {
                    Ok(Ok(())) => true,
                    Ok(Err(err)) => {
                        tracing::warn!("{err:#}");
                        false
                    }
                    Err(_) => {
                        tracing::warn!(
                            "{seconds} 秒たってもストアを閉じ終わらないため、待たずに終わります"
                        );
                        false
                    }
                }
            }
        };

        let finished = tasks_finished && store_closed;
        if finished {
            tracing::info!(
                "取得と予定の読み取りを止め、ストアを閉じました ({} ms)",
                started.elapsed().as_millis()
            );
        }
        *outcome = Some(finished);
        finished
    }

    async fn scheduler_loop(self) {
        let mut startup = true;
        let mut last_catalog_check: Option<DateTime<Utc>> = None;
        while !self.stopping() {
            let now = Utc::now();
            // カタログは起動時と、1 日ごとに取り込み直す。
            if last_catalog_check
                .is_none_or(|last| now - last >= Duration::hours(CATALOG_REFRESH_HOURS))
            {
                last_catalog_check = Some(now);
                self.refresh_catalog_inner().await;
            }

            let (settings, sources) = {
                let shared = self.0.shared.read();
                (shared.settings.clone(), shared.resolved.clone())
            };
            let states = self
                .blocking(|store| store.source_states())
                .await
                .unwrap_or_default();
            let now = Utc::now();
            let due: Vec<ResolvedSource> = sources
                .into_iter()
                .filter(|source| source.enabled)
                .filter(|source| {
                    if startup && settings.fetch_on_startup {
                        return true;
                    }
                    let interval = Duration::minutes(settings.interval_for(&source.config).into());
                    let state = states.get(source.id());
                    // 失敗が続くソースは間隔を延ばす (最大 8 倍)。
                    let backoff =
                        state.map_or(1, |state| 1i32 << state.consecutive_failures.min(3));
                    match state.and_then(|state| state.last_attempt_at) {
                        None => true,
                        Some(last) => now - last + Duration::seconds(30) >= interval * backoff,
                    }
                })
                .collect();
            startup = false;
            if !due.is_empty() {
                self.run_cycle(due.into_iter().map(FetchJob::from).collect())
                    .await;
            }
            tokio::select! {
                biased;
                () = self.0.stop.cancelled() => break,
                () = tokio::time::sleep(TICK) => {}
                () = self.0.wake.notified() => {}
            }
        }
    }

    /// 取得して、終わったことを画面に知らせる。取得したソースの結果を返す (取得中で飛ばしたソースと、
    /// 終わる途中で取り消した取得は含まない)。
    async fn run_cycle(&self, jobs: Vec<FetchJob>) -> Vec<FetchOutcome> {
        let (outcomes, pruned) = self.run_cycle_inner(jobs).await;
        // 終わる途中で取り消した巡回は、画面に知らせない (新着の通知も出さない)。
        if self.stopping() {
            return outcomes;
        }
        let added = outcomes
            .iter()
            .flat_map(|outcome| outcome.added.iter().cloned())
            .collect();
        let _ = self
            .0
            .events
            .send(ServiceEvent::CycleFinished { added, pruned });
        outcomes
    }

    /// 同時取得数の上限。設定が変わったら新しい上限で作り直す
    /// (取得中の分は古い上限のまま終わる)。
    fn permits(&self, concurrency: usize) -> Arc<Semaphore> {
        let concurrency = concurrency.max(1);
        let mut permits = self.0.permits.lock();
        if permits.0 != concurrency {
            *permits = (concurrency, Arc::new(Semaphore::new(concurrency)));
        }
        permits.1.clone()
    }

    /// 1 つのソースの取得に使うエンジン。Cookie をほかのソースと共有しない。
    /// 作れなければ (TLS の設定を読めないなど) 共有のエンジンを使う。
    fn isolated_engine(&self) -> Engine {
        match self.0.engine.isolated() {
            Ok(engine) => engine,
            Err(err) => {
                tracing::warn!(
                    "取得ごとのクライアントを作れないため、共有のものを使います: {err:#}"
                );
                self.0.engine.clone()
            }
        }
    }

    /// ソースが今も使われているか。
    fn has_source(&self, id: &str) -> bool {
        self.0
            .shared
            .read()
            .resolved
            .iter()
            .any(|source| source.id() == id)
    }

    /// 取得する。取得したソースの結果と、保存期間を過ぎて消した記事の数を返す。
    async fn run_cycle_inner(&self, jobs: Vec<FetchJob>) -> (Vec<FetchOutcome>, usize) {
        let settings = self.settings();
        let semaphore = self.permits(settings.concurrency);
        let mut handles = Vec::new();
        for FetchJob { source, recheck } in jobs {
            if !self.0.running.lock().insert(source.id().to_string()) {
                continue;
            }
            let running = Running {
                services: self.clone(),
                id: source.id().to_string(),
            };
            let services = self.clone();
            let semaphore = semaphore.clone();
            let settings = settings.clone();
            let handle = self.spawn(async move {
                let _running = running;
                // 終わる途中なら、順番を待っている取得は始めない。
                let permit = services.unless_stopped(semaphore.acquire_owned()).await?;
                let _permit = permit.ok();
                services.fetch_source(&source, &settings, recheck).await
            });
            handles.extend(handle);
        }
        if handles.is_empty() {
            // どれも取得中だった。片付けはその取得のあとで行う。
            return (Vec::new(), 0);
        }

        let mut outcomes = Vec::new();
        for handle in handles {
            if let Ok(Some(outcome)) = handle.await {
                outcomes.push(outcome);
            }
        }
        // 終わる途中なら、片付け (古い記事の削除・旧形式の書き出し・画像の掃除) は次の巡回に回す。
        let pruned = if self.stopping() {
            0
        } else {
            self.maintain(&settings).await
        };
        (outcomes, pruned)
    }

    /// 1 つのソースを取得して保存し、画像を先に取っておく。`extra_recheck` は、ほかに詳細を取り直す
    /// 記事の外部 ID (取得 URL で取れなかった本文の画像を持つ記事)。終わる途中で取り消したら `None`
    /// (保存も取得ログの記録もせず、前回の取得の時刻のままなので、次の取得 (起動時の取得か、間隔どおりの
    /// 取得) で取り直す)。
    async fn fetch_source(
        &self,
        resolved: &ResolvedSource,
        settings: &AppSettings,
        extra_recheck: HashSet<String>,
    ) -> Option<FetchOutcome> {
        // 順番待ちを抜けたところで合図が来ていたら、始めない。
        if self.stopping() {
            return None;
        }
        let source = &resolved.config;
        let _ = self.0.events.send(ServiceEvent::FetchStarted {
            source_id: source.id.clone(),
        });
        let started_at = Utc::now();
        let source_id = source.id.clone();
        let known = self
            .blocking(move |store| store.known_items(&source_id))
            .await
            .unwrap_or_default();
        // 詳細ページから本文を取るソースは、予定の時間が近いメンテナンスの記事の本文を取り直す
        // (1 週間前の予告に当日足された完了・延長の追記を拾うため)。
        let recheck_ids = if source.detail.is_some() {
            let mut ids = self.recheck_ids(&source.id, started_at).await;
            ids.extend(extra_recheck);
            ids
        } else {
            HashSet::new()
        };
        let (network, allow_secrets) = fetch_permissions(resolved);
        let engine = self.isolated_engine();
        let run = engine.run(RunInput {
            source,
            known: &known,
            max_age_days: settings.max_age_for(source),
            now: started_at,
            recheck: Duration::hours(RECHECK_HOURS),
            recheck_ids,
            limit: None,
            network,
            allow_secrets,
            trace: false,
        });
        let output = match self
            .unless_stopped(tokio::time::timeout(FETCH_TIMEOUT, run))
            .await
        {
            None => {
                tracing::info!(source = %source.id, "終了するため、取得を取り消しました");
                return None;
            }
            Some(Ok(output)) => output,
            Some(Err(_)) => RunOutput::failed(
                anyhow::anyhow!(
                    "{} 秒たっても終わらないため止めました",
                    FETCH_TIMEOUT.as_secs()
                ),
                Vec::new(),
            ),
        };

        let mut status = output.status;
        let mut message = output.message.clone();
        for warning in &output.warnings {
            tracing::warn!(source = %source.id, "{warning}");
        }

        // 保存と取得ログの記録を、ストアのロックを取ったまま行う。その中でソースが
        // まだ使われているかを確かめる。ソースを消すときは使うソースから外してから
        // ロックを取って記事を消すので、消したあとに古い取得が記事や状態を作り直すことはない。
        // 止める合図のあとにロックを取ったときは保存しない (`blocking`)。合図の前に保存を始めたら、
        // 記事の保存と取得ログの記録を最後まで行う。
        let services = self.clone();
        let source_id = source.id.clone();
        let channel = source.channel;
        let images = ImageIdentity::of(source);
        let save_images = images.clone();
        let drafts = output.drafts.clone();
        // 一部が失敗した巡回では一覧を置き換えない。取れなかった記事を消さないため。
        let keep = (source.is_snapshot() && output.status == RunStatus::Ok)
            .then(|| output.listed_ids.clone());
        let listed = output.listed;
        let fetch_status = output.status;
        let fetch_message = output.message.clone();
        let saved = self
            .blocking(move |store| {
                if !services.has_source(&source_id) {
                    return Ok(None);
                }
                let mut status = fetch_status;
                let mut message = fetch_message;
                let mut report = gnn_core::ApplyReport::default();
                if status != RunStatus::Failed {
                    match store.apply_fetch(
                        &source_id,
                        channel,
                        &save_images,
                        &drafts,
                        keep.as_deref(),
                        Utc::now(),
                    ) {
                        Ok(applied) => report = applied,
                        Err(err) => {
                            status = RunStatus::Failed;
                            message = Some(format!("保存に失敗しました: {err:#}"));
                        }
                    }
                }
                store.record_run(&FetchRun {
                    source_id: source_id.clone(),
                    started_at,
                    finished_at: Utc::now(),
                    status,
                    listed,
                    added: report.added.len(),
                    updated: report.updated,
                    message: message.clone(),
                })?;
                Ok(Some((status, message, report)))
            })
            .await;
        let report = match saved {
            Ok(Some((saved_status, saved_message, report))) => {
                status = saved_status;
                message = saved_message;
                report
            }
            Ok(None) => {
                tracing::info!(source = %source.id, "取得中にソースが外されたため、結果を捨てました");
                gnn_core::ApplyReport::default()
            }
            Err(err) if is_stopped(&err) => {
                tracing::info!(source = %source.id, "終了するため、取得の結果を保存せずに捨てました");
                return None;
            }
            Err(err) => {
                status = RunStatus::Failed;
                message = Some(format!("保存に失敗しました: {err:#}"));
                tracing::error!("取得結果を保存できません: {err:#}");
                gnn_core::ApplyReport::default()
            }
        };
        if status == RunStatus::Failed {
            tracing::warn!(source = %source.id, "取得に失敗: {}", message.as_deref().unwrap_or(""));
        }
        // 増えた・変わった記事の予定を読む。読み取りに失敗しても、記事の保存は成功のまま。
        if !report.added.is_empty() || report.updated > 0 || report.removed > 0 {
            self.wake_analysis();
        }

        // 終わる途中なら、画像の先読みはしない (取得中のものも取り消す)。保存は済んでいるので、
        // 取得そのものは取り消さない。
        let prefetched = self
            .unless_stopped(self.prefetch_images(&source.id, &images, &output.drafts))
            .await
            .unwrap_or_default();
        if !prefetched.available.is_empty() || !prefetched.changed.is_empty() {
            let _ = self.0.events.send(ServiceEvent::ImagesCached {
                available: prefetched.available,
                changed: prefetched.changed,
            });
        }

        let summary = FetchSummary {
            source_id: source.id.clone(),
            status,
            message,
            listed: output.listed,
            added: report.added.len(),
            updated: report.updated,
            removed: report.removed,
        };
        let _ = self
            .0
            .events
            .send(ServiceEvent::FetchFinished(summary.clone()));
        let added = report
            .added
            .into_iter()
            .map(|NewEntry { id, title }| AddedEntry {
                entry_id: id,
                source_name: source.name.clone(),
                channel: source.channel,
                title,
            })
            .collect();
        Some(FetchOutcome {
            summary,
            added,
            recheck_misses: output.recheck_misses,
        })
    }

    /// 本文の画像を先に取っておく。表示が速くなり、配信が消えても読める。
    ///
    /// 取得の定義の `url_identity.images` に合う画像は、保存した本文と同じ参照の鍵でキャッシュに入れる
    /// (取得には今回の配信元の URL を使う)。参照なら、同じ画像を署名が変わるたびに取り直さない。
    /// キャッシュにある画像も、確かめてから [`IMAGE_REVALIDATE_AFTER`] たてば今回の URL で条件付きに取り、
    /// 配信元で差し替わっていれば入れ直す (参照は鍵が変わらず、URL が同じまま中身を差し替える配信元もある)。
    /// キャッシュにない画像を先に取り、確かめ直しはそのあとで、上限は [`select_prefetch`] が決める。
    /// 取れなかった画像は [`ImageFailure`] に残し、確かめ直しを待たせる。
    async fn prefetch_images(
        &self,
        source_id: &str,
        identity: &ImageIdentity,
        drafts: &[EntryDraft],
    ) -> PrefetchReport {
        let images = self.0.images.clone();
        let targets = prefetch_targets(identity, drafts);
        let failures: HashMap<String, ImageFailure> = {
            let failures = self.0.image_failures.lock();
            targets
                .iter()
                .filter_map(|(key, _)| Some((key.clone(), *failures.get(key)?)))
                .collect()
        };
        // キャッシュのファイルの時刻と大きさを読むだけだが、ほかの処理と同じく終わるときに待つ処理に数える。
        let plan = self
            .0
            .tasks
            .spawn_blocking_on(
                move || {
                    select_prefetch(
                        &images,
                        targets,
                        SystemTime::now(),
                        &failures,
                        Instant::now(),
                    )
                },
                self.0.runtime.handle(),
            )
            .await
            .unwrap_or_default();
        let mut report = PrefetchReport::default();
        let (missing, stale) = (plan.missing.len(), plan.stale.len());
        let mut failed = 0;
        for batch in [plan.missing, plan.stale] {
            if batch.is_empty() {
                continue;
            }
            let results = self.download_images(batch).await;
            let now = Instant::now();
            let mut failures = self.0.image_failures.lock();
            for (key, url, result) in results {
                match result {
                    Ok(refreshed) => {
                        failures.remove(&key);
                        match refreshed {
                            Refreshed::Added => report.available.push(key),
                            Refreshed::Changed => report.changed.push(key),
                            Refreshed::Unchanged => {}
                        }
                    }
                    Err(err) => {
                        failed += 1;
                        tracing::debug!(source = source_id, "画像を取れません: {url}: {err:#}");
                        let failure = ImageFailure::after(failures.get(&key), &err, now);
                        failures.insert(key, failure);
                    }
                }
            }
        }
        if missing + stale > 0 {
            tracing::debug!(
                source = source_id,
                "画像: キャッシュになかった {missing} 枚と確かめ直す {stale} 枚を取り、入れた {} 枚・差し替わっていた {} 枚・取れなかった {failed} 枚",
                report.available.len(),
                report.changed.len()
            );
        }
        report
    }

    /// 画像をまとめて取る (同時に [`IMAGE_PREFETCH_CONCURRENCY`] 枚まで)。
    async fn download_images(
        &self,
        targets: Vec<(String, String)>,
    ) -> Vec<(String, String, Result<Refreshed, ImageFetchError>)> {
        let client = self.http_client();
        let images = self.0.images.clone();
        futures::stream::iter(targets)
            .map(|(key, url)| {
                let client = client.clone();
                let images = images.clone();
                async move {
                    let result = download_image(&client, &images, &url, &key).await;
                    (key, url, result)
                }
            })
            .buffer_unordered(IMAGE_PREFETCH_CONCURRENCY)
            .collect()
            .await
    }

    /// 巡回のあとの片付け。保存期間を過ぎた記事を消し、旧形式を書き出す。消した記事の数を返す。
    /// 止める合図のあとにロックを取った片付けは行わない (`blocking`)。
    async fn maintain(&self, settings: &AppSettings) -> usize {
        let mut pruned = 0;
        if settings.retention_days > 0 {
            let cutoff = Utc::now() - Duration::days(settings.retention_days.into());
            match self.blocking(move |store| store.prune(cutoff)).await {
                Ok(count) => pruned = count,
                Err(err) if is_stopped(&err) => {}
                Err(err) => tracing::error!("古い記事を消せません: {err:#}"),
            }
            if pruned > 0 {
                // 消した記事の報告で決まっていた結果があれば、突き合わせを作り直す。
                self.wake_analysis();
            }
        }
        if let Some(dir) = settings.legacy_export_dir.clone() {
            let result = self
                .blocking(move |store| -> Result<()> {
                    let rows = store.legacy_rows()?;
                    // 旧アプリの「今日・明日」の並べ替え用の日付は、予定の表から作る。
                    let now = Utc::now();
                    let items =
                        store.agenda(None, now - agenda::LOAD_BEHIND, now + agenda::LOAD_AHEAD)?;
                    let dates = legacy_dates(&items);
                    gnn_core::legacy::write_jsonl(&dir.join("game_info.jsonl"), &rows, &dates)
                })
                .await;
            if let Err(err) = result
                && !is_stopped(&err)
            {
                tracing::error!("旧形式を書き出せません: {err:#}");
            }
        }

        let now = SystemTime::now();
        let due = self.0.last_image_prune.lock().is_none_or(|last| {
            now.duration_since(last).unwrap_or_default() > StdDuration::from_secs(24 * 3600)
        });
        if due && !self.stopping() {
            *self.0.last_image_prune.lock() = Some(now);
            let images = self.0.images.clone();
            let older_than = now - StdDuration::from_secs(IMAGE_CACHE_DAYS * 24 * 3600);
            // ファイルを消すだけなので、終わるときに待つ処理には数えない (消しかけで終わっても害がない)。
            let prune =
                tokio::task::spawn_blocking(move || images.prune(older_than, IMAGE_CACHE_BYTES));
            let _ = self.unless_stopped(prune).await;
        }
        pruned
    }

    /// サービスの処理からストアを使う。tokio のブロッキング用スレッドで動かし、終わるときに待つ処理として
    /// 数える (待っている側が取り消されても、始まった書き込みは終わるまで待つ)。
    ///
    /// ロックを取ったあとで止める合図を確かめ、合図のあとなら `f` を呼ばずに [`Stopped`] の誤りを返す
    /// (終わる途中で書き込みを始めない。ストアを閉じたあとも、合図のあとなので同じ)。合図の前に
    /// ロックを取った `f` は、合図が来ても最後まで行う。
    async fn blocking<R: Send + 'static>(
        &self,
        f: impl FnOnce(&mut Store) -> Result<R> + Send + 'static,
    ) -> Result<R> {
        let services = self.clone();
        let work = move || {
            #[cfg(test)]
            services.pause(Pause::StoreLock);
            let mut store = services.0.store.lock();
            if services.stopping() {
                return Err(Stopped.into());
            }
            f(store.as_mut().context(STORE_CLOSED)?)
        };
        self.0
            .tasks
            .spawn_blocking_on(work, self.0.runtime.handle())
            .await
            .context("ストアの処理が中断されました")?
    }
}

// ---- 取得 URL で取れなかった画像の取り直し ----

impl Services {
    /// 記録した取得 URL (`failed_url`) で取れなかった (署名の期限が切れたかもしれない) 画像の参照の取り直しを
    /// 頼む。待たない。頼みのまとめ方と上限は [`RecoveryQueue`] が決める。
    pub fn request_image_recovery(&self, source_id: &str, key: &str, failed_url: Option<&str>) {
        let accepted = self
            .0
            .recovery
            .lock()
            .request(source_id, key, failed_url, Instant::now());
        if accepted {
            tracing::debug!(source = source_id, "画像の取り直しを受けました: {key}");
            self.0.recovery_wake.notify_one();
        }
    }

    /// 画像の取り直しの待ち行列を回す。止める合図で抜ける (取り直しの途中なら取り消す。始まった
    /// ストアへの書き込みと取得は、それぞれが合図を見て終わる)。
    async fn image_recovery_loop(self) {
        while !self.stopping() {
            let busy = self.running();
            let next = self.0.recovery.lock().next_due(Instant::now(), &busy);
            let wait = async {
                match next {
                    Some(at) => {
                        tokio::select! {
                            () = tokio::time::sleep_until(tokio::time::Instant::from_std(at)) => {}
                            () = self.0.recovery_wake.notified() => {}
                        }
                    }
                    None => self.0.recovery_wake.notified().await,
                }
            };
            if self.unless_stopped(wait).await.is_none() {
                break;
            }
            let busy = self.running();
            let batches = self.0.recovery.lock().take_due(Instant::now(), &busy);
            for batch in batches {
                if self
                    .unless_stopped(self.recover_images(batch))
                    .await
                    .is_none()
                {
                    return;
                }
            }
        }
    }

    /// 1 つのソースの画像を取り直す。
    ///
    /// 画面が取れなかったあとで新しい取得 URL が記録されていれば、まずその URL で取る。取れなければソースを
    /// 取り直す。詳細のあるソースは、その画像を持つ記事の詳細を取り直す (`RunInput::recheck_ids`。本文の画像は
    /// 詳細を取らないと新しい取得 URL が記録されない)。詳細のないソースは、普通の取得で一覧から記録される。
    /// 取得のあとの先読みが画像をキャッシュに入れ、画面に知らせる ([`ServiceEvent::ImagesCached`])。
    async fn recover_images(&self, batch: RecoveryBatch) {
        let source_id = batch.source_id.clone();
        let Some(source) = self
            .sources()
            .into_iter()
            .find(|source| source.id() == source_id && source.enabled)
        else {
            return;
        };
        let reference = |key: &str| image_ref::reference_url(&source_id, key);
        // 定期の取得の先読みなどで、もう入った画像は、画面に読み直させるだけでよい。
        let (cached, missing): (Vec<RecoveryItem>, Vec<RecoveryItem>) = batch
            .items
            .into_iter()
            .partition(|item| self.0.images.contains(&reference(&item.key)));
        self.announce_cached(cached.iter().map(|item| reference(&item.key)).collect());
        if missing.is_empty() {
            return;
        }
        // 取得に失敗しているソースは、その待ち時間が明けるまで待つ (取り直しても取れず、待ち時間も延びる)。
        if let Some(wait) = self.failure_backoff(&source).await {
            tracing::debug!(source = %source_id, "取得の失敗の待ち時間が明けるまで、画像の取り直しを待ちます");
            let now = Instant::now();
            self.0
                .recovery
                .lock()
                .put_back(&source_id, missing, now, Some(now + wait));
            return;
        }
        // 画面が取れなかったあとで新しい取得 URL が記録されていれば (定期の取得など)、まずそれで取る。
        let missing = self.try_recorded_urls(&source_id, missing).await;
        if missing.is_empty() {
            return;
        }
        let keys: Vec<String> = missing.iter().map(|item| item.key.clone()).collect();
        let holders = match self.image_holders(&source_id, &keys).await {
            Ok(holders) => holders,
            Err(err) => {
                if !is_stopped(&err) {
                    tracing::warn!(source = %source_id, "画像を持つ記事を読めません: {err:#}");
                }
                return;
            }
        };
        let has_detail = source.config.detail.is_some();
        let plan = recovery_plan(has_detail, &keys, &holders);
        let (deferred, tried): (Vec<RecoveryItem>, Vec<RecoveryItem>) = missing
            .into_iter()
            .partition(|item| plan.deferred.contains(&item.key));
        if !deferred.is_empty() {
            // 1 回の取得で詳細を取り直せる記事の数を超えた画像は、次の取り直しに回す (試みには数えない)。
            self.0
                .recovery
                .lock()
                .put_back(&source_id, deferred, Instant::now(), None);
            self.0.recovery_wake.notify_one();
        }
        if tried.is_empty() {
            return;
        }
        tracing::info!(
            source = %source_id,
            "取得 URL で取れなかった画像 {} 件のため取り直します (詳細を取り直す記事 {} 件)",
            tried.len(),
            plan.recheck.len()
        );
        let outcomes = self
            .run_cycle(vec![FetchJob {
                source: source.clone(),
                recheck: plan.recheck.clone(),
            }])
            .await;
        let Some(outcome) = outcomes.into_iter().next() else {
            // 取り出したあとで定期の取得などが始まっていた (か、終わる途中で取り消した)。それが終わって
            // から見直す (試みには数えない)。
            self.0
                .recovery
                .lock()
                .put_back(&source_id, tried, Instant::now(), None);
            self.0.recovery_wake.notify_one();
            return;
        };
        self.0.recovery.lock().finished(&source_id, Instant::now());

        let still: Vec<String> = tried
            .into_iter()
            .map(|item| item.key)
            .filter(|key| !self.0.images.contains(&reference(key)))
            .collect();
        if still.is_empty() {
            return;
        }
        let after = match self.image_holders(&source_id, &still).await {
            Ok(after) => after,
            Err(err) => {
                if !is_stopped(&err) {
                    tracing::warn!(source = %source_id, "画像を持つ記事を読めません: {err:#}");
                }
                return;
            }
        };
        let decisions: Vec<(String, RecoveryOutcome)> = {
            let failures = self.0.image_failures.lock();
            still
                .into_iter()
                .map(|key| {
                    let misses: Vec<RecheckMiss> = plan
                        .rechecked
                        .get(&key)
                        .into_iter()
                        .flatten()
                        .filter_map(|id| outcome.recheck_misses.get(id).copied())
                        .collect();
                    let decision = recovery_outcome(&RecoveryFacts {
                        renewed: latest_seen(&after, &key) > latest_seen(&holders, &key),
                        failure: failures.get(&reference(&key)).copied(),
                        status: outcome.summary.status,
                        has_detail,
                        misses: &misses,
                    });
                    (key, decision)
                })
                .collect()
        };
        let now = Instant::now();
        let mut retry = Vec::new();
        let mut not_before: Option<Instant> = None;
        let mut blocked = 0;
        let mut recovery = self.0.recovery.lock();
        for (key, decision) in decisions {
            match decision {
                RecoveryOutcome::Transient { not_before: wait } => {
                    not_before = not_before.max(wait);
                    retry.push(key);
                }
                RecoveryOutcome::Blocked => {
                    blocked += 1;
                    recovery.give_up(&source_id, &key);
                    tracing::info!(source = %source_id, "新しい取得 URL でも画像を取れません: {key}");
                }
                RecoveryOutcome::NotFound => {
                    recovery.give_up(&source_id, &key);
                    tracing::info!(
                        source = %source_id,
                        "画像の新しい取得 URL が見つかりません (記事が消えた・本文から画像が外れたなど): {key}"
                    );
                }
            }
        }
        if blocked >= RECOVERY_PAUSE_AFTER {
            // WAF などで、取り直しても取れないらしい。しばらく頼みを受けない。
            tracing::warn!(
                source = %source_id,
                "新しい取得 URL でも取れない画像が {blocked} 件あったため、画像の取り直しを休みます"
            );
            recovery.pause(&source_id, now + RECOVERY_PAUSE);
        } else if !retry.is_empty() {
            let retried = recovery.retry_later(&source_id, &retry, now, not_before);
            tracing::info!(
                source = %source_id,
                "時間をおけば取れそうな画像 {} 件のうち {retried} 件を、あとで取り直します",
                retry.len()
            );
        }
        drop(recovery);
        self.0.recovery_wake.notify_one();
    }

    /// 画像がキャッシュに入ったことを画面に知らせる (失敗を覚えている画像を読み直させる)。
    fn announce_cached(&self, available: Vec<String>) {
        if !available.is_empty() {
            let _ = self.0.events.send(ServiceEvent::ImagesCached {
                available,
                changed: Vec::new(),
            });
        }
    }

    /// 記録した取得 URL が、画面が取れなかった URL と違う画像 (失敗のあとで取得があった。予約し直した頼みは
    /// 記録した URL そのもの) を、ソースを取り直す前に、その URL で取ってみる。取れなかった画像を返す。
    async fn try_recorded_urls(
        &self,
        source_id: &str,
        items: Vec<RecoveryItem>,
    ) -> Vec<RecoveryItem> {
        let id = source_id.to_string();
        let keys: Vec<String> = items.iter().map(|item| item.key.clone()).collect();
        let recorded: HashMap<String, String> = self
            .blocking(move |store| {
                let mut recorded = HashMap::new();
                for key in keys {
                    if let Some(url) = store.image_fetch_url(&id, &key)? {
                        recorded.insert(key, url);
                    }
                }
                Ok(recorded)
            })
            .await
            .unwrap_or_default();
        let attempts: Vec<(String, String)> = items
            .iter()
            .filter_map(|item| {
                let url = recorded.get(&item.key)?;
                (item.failed_url.as_deref() != Some(url.as_str()))
                    .then(|| (image_ref::reference_url(source_id, &item.key), url.clone()))
            })
            .collect();
        if attempts.is_empty() {
            return items;
        }
        let results = self.download_images(attempts).await;
        let mut available = Vec::new();
        {
            let now = Instant::now();
            let mut failures = self.0.image_failures.lock();
            for (key, url, result) in results {
                match result {
                    Ok(_) => {
                        failures.remove(&key);
                        available.push(key);
                    }
                    Err(err) => {
                        tracing::debug!(
                            source = source_id,
                            "記録した取得 URL でも画像を取れません: {url}: {err:#}"
                        );
                        let failure = ImageFailure::after(failures.get(&key), &err, now);
                        failures.insert(key, failure);
                    }
                }
            }
        }
        let got: HashSet<String> = available.iter().cloned().collect();
        self.announce_cached(available);
        items
            .into_iter()
            .filter(|item| !got.contains(&image_ref::reference_url(source_id, &item.key)))
            .collect()
    }

    /// 取得に失敗しているソースの、次の定期の取得までの待ち時間 (失敗が続くと延ばした間隔)。
    async fn failure_backoff(&self, source: &ResolvedSource) -> Option<StdDuration> {
        let id = source.id().to_string();
        let state = self
            .blocking(move |store| Ok(store.source_states()?.remove(&id)))
            .await
            .ok()
            .flatten()?;
        if state.consecutive_failures == 0 {
            return None;
        }
        let interval = Duration::minutes(self.settings().interval_for(&source.config).into());
        let backoff = interval * (1i32 << state.consecutive_failures.min(3));
        let next = state.last_attempt_at? + backoff - Duration::seconds(30);
        (next - Utc::now()).to_std().ok()
    }

    /// 画像の参照を持つ記事 (鍵ごと)。
    async fn image_holders(
        &self,
        source_id: &str,
        keys: &[String],
    ) -> Result<HashMap<String, Vec<ImageHolder>>> {
        let source_id = source_id.to_string();
        let keys = keys.to_vec();
        self.blocking(move |store| {
            keys.into_iter()
                .map(|key| {
                    let holders = store.image_holders(&source_id, &key)?;
                    Ok((key, holders))
                })
                .collect()
        })
        .await
    }
}

/// 画面の HTTP クライアントは、画像の参照の取得 URL をストアで引き、取れなければ取り直しを頼む。
impl crate::http_client::ReferenceImages for Services {
    fn fetch_url(&self, source_id: &str, key: &str) -> Option<String> {
        self.image_fetch_url(source_id, key)
    }

    fn request_recovery(&self, source_id: &str, key: &str, failed_url: Option<&str>) {
        self.request_image_recovery(source_id, key, failed_url);
    }
}

/// 取り直したあとも取れなかった画像の扱い。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RecoveryOutcome {
    /// 時間をおけば取れそう (一覧・詳細・画像の通信の失敗・5xx・429)。回数の上限の中で、あとで試す
    /// (`not_before` は配信元が指定した待ち時間)。
    Transient { not_before: Option<Instant> },
    /// 新しい取得 URL でも取れない (403 など)。期限切れではないらしい (配信元の WAF・権限など)。諦める。
    Blocked,
    /// 新しい取得 URL が記録されなかった (記事が一覧からも詳細からも消えた・本文から画像が外れた)。諦める。
    NotFound,
}

/// 取り直しの結果を決める材料。
struct RecoveryFacts<'a> {
    /// 取り直しで、新しい取得 URL が記録されたか。
    renewed: bool,
    /// 先読みで取れなかった理由 (取り直しの取得のもの)。
    failure: Option<ImageFailure>,
    /// 取り直しの取得の結果。
    status: RunStatus,
    has_detail: bool,
    /// 詳細を取り直した記事のうち、取れなかったものの理由。
    misses: &'a [RecheckMiss],
}

fn recovery_outcome(facts: &RecoveryFacts<'_>) -> RecoveryOutcome {
    if facts.renewed {
        return match facts.failure {
            Some(failure) if failure.transient => RecoveryOutcome::Transient {
                not_before: failure.not_before,
            },
            _ => RecoveryOutcome::Blocked,
        };
    }
    // 新しい取得 URL が記録されなかった。一覧 (詳細のないソースでは一覧の一部) や詳細を取れなかったなら、
    // 時間をおけば記録されるかもしれない。
    let list_failed = facts.status == RunStatus::Failed
        || (!facts.has_detail && facts.status == RunStatus::Partial);
    if list_failed || facts.misses.contains(&RecheckMiss::Failed) {
        return RecoveryOutcome::Transient { not_before: None };
    }
    RecoveryOutcome::NotFound
}

/// 画像の参照の取得 URL を、どれかの記事で最後に記録した時刻。
fn latest_seen(holders: &HashMap<String, Vec<ImageHolder>>, key: &str) -> Option<DateTime<Utc>> {
    holders
        .get(key)
        .and_then(|held| held.iter().map(|holder| holder.seen_at).max())
}

/// 取り直しの取得で詳細を取り直す記事。
#[derive(Debug, Default, PartialEq, Eq)]
struct RecoveryPlan {
    /// 詳細を取り直す記事の外部 ID。
    recheck: HashSet<String>,
    /// 画像ごとの、詳細を取り直す記事。
    rechecked: HashMap<String, Vec<String>>,
    /// 1 回の取得で詳細を取り直せる記事の数を超えたため、次の取り直しに回す画像。
    deferred: HashSet<String>,
}

/// 詳細のあるソースでは、画像を持つ記事を、取得 URL を新しく記録した記事から画像ごとに
/// [`RECOVERY_HOLDERS_PER_IMAGE`] 件、合わせて [`RECOVERY_RECHECK_LIMIT`] 件まで詳細を取り直す。記事の `image_url`
/// にある画像でも詳細から取ったものかもしれず、一覧の取得だけで新しい取得 URL が記録されるとは限らないので、
/// 一覧の画像かどうかでは分けない (取り直しはまれで、詳細の取得は少ない)。詳細のないソースは、普通の取得で
/// 一覧から新しい取得 URL が記録される。
fn recovery_plan(
    has_detail: bool,
    keys: &[String],
    holders: &HashMap<String, Vec<ImageHolder>>,
) -> RecoveryPlan {
    let mut plan = RecoveryPlan::default();
    if !has_detail {
        return plan;
    }
    for key in keys {
        let chosen: Vec<String> = holders
            .get(key)
            .into_iter()
            .flatten()
            .take(RECOVERY_HOLDERS_PER_IMAGE)
            .map(|holder| holder.external_id.clone())
            .collect();
        let added = chosen
            .iter()
            .filter(|id| !plan.recheck.contains(*id))
            .count();
        if plan.recheck.len() + added > RECOVERY_RECHECK_LIMIT {
            plan.deferred.insert(key.clone());
            continue;
        }
        plan.recheck.extend(chosen.iter().cloned());
        plan.rechecked.insert(key.clone(), chosen);
    }
    plan
}

// ---- 予定の読み取り ----

impl Services {
    /// 予定の読み取りを促す。読み直しが要る記事がなければ何もしない。
    fn wake_analysis(&self) {
        self.0.analysis_wake.notify_one();
    }

    /// 予定の読み取りの待ち行列。起動時と、促されるたびに、読み直しが要る記事がなくなるまで読む。
    /// 止める合図で抜ける。
    async fn analysis_loop(self) {
        while !self.stopping() {
            self.drain_analysis().await;
            tokio::select! {
                biased;
                () = self.0.stop.cancelled() => break,
                () = self.0.analysis_wake.notified() => {}
            }
        }
    }

    /// 読み直しが要る記事がなくなるまで読む。止める合図のあとは次の回を読まない。
    async fn drain_analysis(&self) {
        while !self.stopping() {
            match self.analyze_batch().await {
                Ok(true) => {}
                Ok(false) => break,
                Err(err) if is_stopped(&err) => break,
                Err(err) => {
                    // 次に促されたときにやり直す。記事の保存には関わらない。
                    tracing::error!("予定を読み取れません: {err:#}");
                    break;
                }
            }
        }
    }

    /// 読み直しが要る記事を 1 回分 (新しい順に [`ANALYSIS_BATCH`] 件まで) 読み、保存する。
    /// 読んだ記事があれば `true` (続きがあるかもしれない)。
    ///
    /// 読むあいだはストアのロックを外す。保存のときに記事の版を照合し、読んでいるあいだに中身が
    /// 変わった記事・消えた記事の結果は捨てる (変わった記事は次の回に読み直す)。
    ///
    /// 止める合図が立ったら、記事 1 件の切れ目で読むのをやめ、その回の結果は保存しない
    /// (保存されていないので、次の起動のときに読み直す)。
    async fn analyze_batch(&self) -> Result<bool> {
        let contexts = self.analysis_contexts();
        let fingerprints: Vec<(String, String)> = contexts
            .iter()
            .map(|(id, context)| (id.clone(), context.fingerprint.clone()))
            .collect();
        let extractor = gnn_schedule::extractor_id();
        let (inputs, stale) = {
            let extractor = extractor.clone();
            self.blocking(move |store| {
                Ok((
                    store.pending_analyses(&extractor, &fingerprints, ANALYSIS_BATCH)?,
                    store.has_stale_schedules()?,
                ))
            })
            .await?
        };
        if inputs.is_empty() && !stale {
            return Ok(false);
        }
        let count = inputs.len();
        let stop = self.0.stop.clone();
        let results = self
            .0
            .tasks
            .spawn_blocking_on(
                move || analyze(&inputs, &contexts, &extractor, &stop),
                self.0.runtime.handle(),
            )
            .await
            .context("予定の読み取りが中断されました")?;
        // 途中でやめた回の結果は、止める合図のあとなので保存されない (`blocking`)。
        let saved = self
            .blocking(move |store| store.save_analyses(&results, &mut reconcile_source))
            .await?;
        if saved.saved + saved.discarded > 0 {
            tracing::debug!(
                "予定を {} 件読み取りました (中身が変わった・消えた記事 {} 件の結果は捨てました)",
                saved.saved,
                saved.discarded
            );
        }
        if !saved.rebuilt.is_empty() {
            let _ = self.0.events.send(ServiceEvent::ScheduleChanged {
                sources: saved.rebuilt,
            });
        }
        Ok(count > 0)
    }

    /// 予定を読むソース (on のお知らせのソース) と、その文脈。
    fn analysis_contexts(&self) -> HashMap<String, AnalysisContext> {
        self.0
            .shared
            .read()
            .resolved
            .iter()
            .filter(|source| source.enabled && source.config.channel == Channel::News)
            .map(|source| (source.id().to_string(), AnalysisContext::of(&source.config)))
            .collect()
    }

    /// 本文を取り直して追記 (完了・延長・中止など) を確かめたいメンテナンスの記事の外部 ID。
    async fn recheck_ids(&self, source_id: &str, now: DateTime<Utc>) -> HashSet<String> {
        let source_id = source_id.to_string();
        // 細かい判断は `agenda::needs_recheck` が行う。ここでは表から範囲で候補を絞る。
        let from = now - agenda::LOAD_BEHIND;
        let to = now + agenda::RECHECK_AHEAD + Duration::days(1);
        let candidates = self
            .blocking(move |store| store.recheck_candidates(&source_id, from, to))
            .await;
        match candidates {
            Ok(candidates) => candidates
                .into_iter()
                .filter(|(_, item)| match &item.detail {
                    AgendaDetail::Maintenance(maintenance) => {
                        agenda::needs_recheck(maintenance, now)
                    }
                    AgendaDetail::Period(_) => false,
                })
                .map(|(external_id, _)| external_id)
                .collect(),
            Err(err) if is_stopped(&err) => HashSet::new(),
            Err(err) => {
                tracing::warn!("本文を取り直す記事を決められません: {err:#}");
                HashSet::new()
            }
        }
    }
}

/// 記事の予定を読む。スレッドごとに抽出器を 1 つ持つ (辞書はプロセスで共有される)。
///
/// 抽出器が止まった (panic) 記事は失敗として残し、その抽出器は作り直す。`stop` が立ったら、
/// 残りの記事は読まずに戻る (呼び出し側はその回の結果を保存しない)。
fn analyze(
    inputs: &[AnalysisInput],
    contexts: &HashMap<String, AnalysisContext>,
    extractor_id: &str,
    stop: &CancellationToken,
) -> Vec<AnalysisResult> {
    thread_local! {
        static EXTRACTOR: RefCell<Option<gnn_schedule::Extractor>> = const { RefCell::new(None) };
    }
    EXTRACTOR.with(|cell| {
        let mut slot = cell.borrow_mut();
        inputs
            .iter()
            .take_while(|_| !stop.is_cancelled())
            .filter_map(|input| {
                let context = contexts.get(&input.source_id)?;
                let article = gnn_schedule::Article {
                    title: &input.title,
                    body_html: &input.body_html,
                    body_text: &input.body_text,
                    context: gnn_schedule::Context {
                        published: input.published,
                        first_seen_at: input.first_seen_at,
                        timezone: context.timezone,
                        server_timezone: context.server_timezone,
                    },
                };
                let extractor = slot.get_or_insert_with(gnn_schedule::Extractor::new);
                let outcome = match catch_unwind(AssertUnwindSafe(|| extractor.extract(&article))) {
                    Ok(schedule) => Ok(schedule),
                    Err(panic) => {
                        // 途中で止まった解析器の作業領域は使い回さない。
                        *slot = None;
                        let message = panic_message(panic.as_ref());
                        tracing::error!(
                            entry_id = input.entry_id,
                            "予定の読み取りに失敗しました: {message}"
                        );
                        Err(message)
                    }
                };
                Some(AnalysisResult {
                    entry_id: input.entry_id,
                    revision: input.revision,
                    extractor: extractor_id.to_string(),
                    context: context.fingerprint.clone(),
                    analyzed_at: Utc::now(),
                    outcome,
                })
            })
            .collect()
    })
}

/// 同じソースの記事を突き合わせる。突き合わせが止まったら (panic)、記事の記述のままにする。
fn reconcile_source(articles: &[SourceSchedule]) -> Vec<Resolution> {
    let input: Vec<gnn_schedule::SourceArticle<'_>> = articles
        .iter()
        .map(|article| gnn_schedule::SourceArticle {
            entry_id: article.entry_id,
            published: article.published,
            first_seen_at: article.first_seen_at,
            schedule: &article.schedule,
        })
        .collect();
    match catch_unwind(AssertUnwindSafe(|| gnn_schedule::reconcile(&input))) {
        Ok(resolved) => resolved
            .into_iter()
            .map(|resolved| Resolution {
                entry_id: resolved.entry_id,
                index: resolved.index,
                disposition: resolved.disposition,
                planned: resolved.planned,
                actual: resolved.actual,
                changes: resolved.changes,
                ongoing: resolved.ongoing,
                resolved_by: resolved.resolved_by,
            })
            .collect(),
        Err(panic) => {
            tracing::error!(
                "予定の突き合わせに失敗しました: {}",
                panic_message(panic.as_ref())
            );
            Vec::new()
        }
    }
}

fn panic_message(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|message| message.to_string())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "理由は分かりません".to_string())
}

/// 旧形式の `time_range` に書く日付 (この PC の地域時刻の暦日)。
fn legacy_dates(items: &[AgendaItem]) -> HashMap<i64, Vec<chrono::NaiveDate>> {
    let mut by_entry: HashMap<i64, Vec<&AgendaItem>> = HashMap::new();
    for item in items {
        by_entry.entry(item.entry_id).or_default().push(item);
    }
    by_entry
        .into_iter()
        .map(|(id, items)| (id, agenda::legacy_dates(items, &chrono::Local)))
        .filter(|(_, dates)| !dates.is_empty())
        .collect()
}

/// 旧形式を取り込んだ結果。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LegacyImportSummary {
    pub total: usize,
    pub added: usize,
    pub skipped: usize,
    /// 画像キャッシュに入れた手元の画像の数。
    pub images: usize,
}

impl Services {
    /// 旧形式 (dmm-games-info の `game_info.jsonl`) を取り込む。同期的に動くので画面のスレッドでは呼ばない。
    pub fn import_legacy(&self, path: &std::path::Path) -> Result<LegacyImportSummary> {
        let import = gnn_core::legacy::read_jsonl(path)?;
        let mut images = 0;
        let rows: Vec<(String, EntryDraft)> = import
            .rows
            .into_iter()
            .map(|(source_id, mut draft)| {
                if let Some(body) = draft.body_html.as_mut() {
                    let (localized, count) = localize_legacy_images(body, &self.0.images);
                    *body = localized;
                    images += count;
                }
                (source_id, draft)
            })
            .collect();
        let added = self.with_store(|store| store.import_legacy(&rows, Utc::now()))?;
        if added > 0 {
            // 取り込んだ本文から予定を読む (旧形式の `time_range` は使わない)。
            self.wake_analysis();
        }
        Ok(LegacyImportSummary {
            total: rows.len(),
            added,
            skipped: import.skipped.len(),
            images,
        })
    }

    /// 手元だけのソースの記事を消す。同期的に動くので画面のスレッドでは呼ばない。
    pub fn delete_entries_of(&self, id: &str) -> Result<usize> {
        if self.has_source(id) {
            bail!("使っているソースの記事は消せません");
        }
        self.with_store(|store| store.delete_source(id))
    }
}

/// 旧形式から取り込んだ画像の URL (`https://legacy-image.invalid/<鍵>`) のホスト。手元のファイルから取り込んだ
/// もので、キャッシュから消えると取り直せない。
pub const LEGACY_IMAGE_HOST: &str = "legacy-image.invalid";

/// 旧形式の本文にある手元の画像 (`http://asset.localhost/C:/...` や `file:///C:/...`) を
/// 画像キャッシュに入れ、`src` をキャッシュの URL に置き換える。
///
/// gpui の画像は URL として読み込まれるため、手元のパスのままでは表示できない。
/// キャッシュの URL は `.invalid` ドメインなので、キャッシュになければ外へは取りに行かない。
fn localize_legacy_images(body: &str, images: &ImageCache) -> (String, usize) {
    static IMG_SRC: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"(<img\b[^>]*?\bsrc=")([^"]+)(")"#).expect("regex")
    });
    let mut count = 0;
    let localized = IMG_SRC.replace_all(body, |caps: &regex::Captures<'_>| {
        let src = gnn_core::text::decode_entities(&caps[2]);
        let replaced = legacy_local_path(&src).and_then(|path| {
            let bytes = std::fs::read(&path).ok()?;
            let digest = sha2::Digest::finalize(<sha2::Sha256 as sha2::Digest>::new_with_prefix(
                path.display().to_string().as_bytes(),
            ));
            let url = format!("https://{LEGACY_IMAGE_HOST}/{}", hex::encode(&digest[..16]));
            images.write(&url, &bytes).ok()?;
            Some(url)
        });
        match replaced {
            Some(url) => {
                count += 1;
                format!("{}{url}{}", &caps[1], &caps[3])
            }
            None => caps[0].to_string(),
        }
    });
    (localized.into_owned(), count)
}

/// 旧形式の画像の場所を手元のパスにする。手元の画像でなければ `None`。
fn legacy_local_path(src: &str) -> Option<std::path::PathBuf> {
    let file_url = if let Some(rest) = src.strip_prefix("http://asset.localhost/") {
        format!("file:///{rest}")
    } else if src.starts_with("file:///") {
        src.to_string()
    } else if src.len() > 2 && src.as_bytes()[1] == b':' && src.as_bytes()[0].is_ascii_alphabetic()
    {
        return Some(std::path::PathBuf::from(src));
    } else {
        return None;
    };
    url::Url::parse(&file_url).ok()?.to_file_path().ok()
}

/// 本文 HTML の `<img src>` を集める。
pub fn image_sources(html: &str) -> Vec<String> {
    static SRC: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| {
        regex::Regex::new(r#"<img\b[^>]*?\bsrc="([^"]+)""#).expect("regex")
    });
    SRC.captures_iter(html)
        .map(|caps| gnn_core::text::decode_entities(&caps[1]))
        .collect()
}

/// 下書きの画像のうち先読みするもの (キャッシュの鍵, 取りに行く URL)。鍵は、宣言
/// (`url_identity.images`) に合う画像なら保存する本文と同じ参照、ほかは URL そのもの。同じ鍵は 1 つにする。
pub fn prefetch_targets(identity: &ImageIdentity, drafts: &[EntryDraft]) -> Vec<(String, String)> {
    let mut targets: Vec<(String, String)> = Vec::new();
    for draft in drafts {
        let urls = draft.image_url.iter().cloned().chain(
            draft
                .body_html
                .as_deref()
                .map(image_sources)
                .unwrap_or_default(),
        );
        for url in urls {
            // 保存済みの参照 (詳細を取り直さなかった記事の画像) や旧形式の画像は、ここでは取れない。
            if !url.starts_with("http") || ImageCache::is_internal(&url) {
                continue;
            }
            let key = identity.identify(&url).into_owned();
            if !targets.iter().any(|(known, _)| *known == key) {
                targets.push((key, url));
            }
        }
    }
    targets
}

/// 先読みで入れた・入れ直した画像の鍵。
#[derive(Debug, Default)]
struct PrefetchReport {
    /// キャッシュになかった画像を入れた。
    available: Vec<String>,
    /// 配信元で差し替わっていたので入れ直した。
    changed: Vec<String>,
}

/// 先読みで取りに行く画像 (キャッシュの鍵, 取りに行く URL)。
#[derive(Debug, Default, PartialEq, Eq)]
struct PrefetchPlan {
    /// キャッシュにない画像。
    missing: Vec<(String, String)>,
    /// 確かめ直す画像。
    stale: Vec<(String, String)>,
}

/// 確かめた時刻が今より先 (時計を戻したなど) でも、この幅までは確かめ直さない。
const CLOCK_SKEW: StdDuration = StdDuration::from_secs(3600);

/// 先読みで取りに行く画像 (キャッシュの鍵, 取りに行く URL)。キャッシュにない画像と、確かめ直す画像を分けて返す。
///
/// 確かめ直すのは、確かめてから [`IMAGE_REVALIDATE_AFTER`] たった画像 (確かめた時刻が [`CLOCK_SKEW`] を超えて
/// 未来のものも、確かめて今に直す)。前に取れなかった画像 (`failures`) は、次に試してよい時刻 (`at` と比べる) まで
/// 除き、確かめた時刻の古いものから [`IMAGE_REVALIDATE_LIMIT`] 件・キャッシュの大きさで [`IMAGE_REVALIDATE_BYTES`]
/// まで。キャッシュにない画像は取得のたびに取りに行く (署名付きの URL は取得のたびに新しくなる) が、配信元が
/// 待ち時間 (Retry-After) を指定していれば、それが明けるまで待つ。
fn select_prefetch(
    images: &ImageCache,
    targets: Vec<(String, String)>,
    now: SystemTime,
    failures: &HashMap<String, ImageFailure>,
    at: Instant,
) -> PrefetchPlan {
    let mut missing = Vec::new();
    let mut stale = Vec::new();
    for (key, url) in targets {
        let failure = failures.get(&key);
        let Some(entry) = images.entry(&key) else {
            if failure
                .and_then(|failure| failure.not_before)
                .is_none_or(|not_before| not_before <= at)
            {
                missing.push((key, url));
            }
            continue;
        };
        let due = match now.duration_since(entry.checked_at) {
            Ok(age) => age >= IMAGE_REVALIDATE_AFTER,
            Err(ahead) => ahead.duration() > CLOCK_SKEW,
        };
        if due && failure.is_none_or(|failure| failure.retry_at <= at) {
            stale.push((entry, key, url));
        }
    }
    stale.sort_by_key(|(entry, _, _)| entry.checked_at);
    let mut bytes = 0u64;
    let mut picked = Vec::new();
    for (entry, key, url) in stale {
        if picked.len() >= IMAGE_REVALIDATE_LIMIT
            || (!picked.is_empty() && bytes + entry.size > IMAGE_REVALIDATE_BYTES)
        {
            break;
        }
        bytes += entry.size;
        picked.push((key, url));
    }
    PrefetchPlan {
        missing,
        stale: picked,
    }
}

/// `url` の画像を取り、キャッシュの `key` (画像の参照か、`url` そのもの) に入れる。キャッシュにあれば
/// 入れたときの検証子 (ETag・Last-Modified) で条件付きに取り、変わっていなければ書き直さない。
pub async fn download_image(
    client: &reqwest::Client,
    images: &ImageCache,
    url: &str,
    key: &str,
) -> Result<Refreshed, ImageFetchError> {
    // 本文の画像は第三者の HTML から来るので、手元のネットワークへは取りに行かない。
    // 名前のホストは名前解決のときに確かめ、IP アドレスで書かれたホストはここで確かめる。
    let parsed = url::Url::parse(url)
        .map_err(|_| ImageFetchError::Refused(format!("URL `{url}` を解釈できません")))?;
    gnn_core::net::check_public_url(&parsed).map_err(ImageFetchError::Refused)?;
    gnn_fetch::image_cache::fetch_image(client, images, url, key, MAX_IMAGE_BYTES).await
}

/// 状態ごとの最終取得日時などを表示用にまとめる。
pub fn describe_last_attempt(at: Option<DateTime<Utc>>) -> String {
    match at {
        Some(at) => crate::format::relative_time(at, Utc::now()),
        None => "未取得".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn localizes_legacy_images_into_the_cache() {
        let dir = tempfile::tempdir().unwrap();
        let image = dir.path().join("a b.png");
        std::fs::write(&image, b"png-bytes").unwrap();
        let cache = ImageCache::new(dir.path().join("cache"));
        let asset = format!(
            "http://asset.localhost/{}",
            image
                .display()
                .to_string()
                .replace('\\', "/")
                .replace(' ', "%20")
        );
        let body = format!(
            r#"<p><img src="{asset}"><img src="https://example.com/x.png"><img src="file:///Z:/missing.png"></p>"#
        );
        let (localized, count) = localize_legacy_images(&body, &cache);
        assert_eq!(count, 1);
        assert!(!localized.contains("asset.localhost"), "{localized}");
        assert!(localized.contains("https://example.com/x.png"));
        assert!(localized.contains("file:///Z:/missing.png"));
        let url = image_sources(&localized).into_iter().next().unwrap();
        assert!(url.starts_with("https://legacy-image.invalid/"));
        assert_eq!(cache.read(&url).as_deref(), Some(&b"png-bytes"[..]));
    }

    #[test]
    fn collects_image_sources() {
        let html = r#"<p><img src="https://a/x.png?a=1&amp;b=2" alt="x"></p><img alt="y" src="https://a/y.png">"#;
        assert_eq!(
            image_sources(html),
            vec!["https://a/x.png?a=1&b=2", "https://a/y.png"]
        );
    }

    /// `https://cdn.example.com/secure/` の画像の署名 (`s`) と期限 (`t`) を見ない決まり。
    fn signed_images() -> ImageIdentity {
        ImageIdentity::new(
            "gc",
            &gnn_core::source::UrlIdentity {
                images: vec![gnn_core::source::UrlIdentityRule {
                    origin: "https://cdn.example.com".into(),
                    path_prefix: Some("/secure/".into()),
                    ignore_query: vec!["s".into(), "t".into()],
                }],
            },
        )
    }

    #[test]
    fn prefetches_signed_images_under_their_references() {
        let identity = signed_images();
        let banner = "https://cdn.example.com/secure/banner.png?s=A&t=1";
        let mut first = draft(
            "1",
            "告知",
            &format!(
                r#"<p><img src="{}"></p><p><img src="https://other.example.com/x.png?v=2"><img src="https://legacy-image.invalid/0123456789abcdef"></p>"#,
                gnn_core::text::escape_html(banner)
            ),
        );
        first.image_url = Some(banner.into());
        // 詳細を取り直さなかった記事は、保存済みの参照を持つ (ここでは取れない)。
        let mut skipped = draft("2", "告知 2", "");
        skipped.body_html = None;
        skipped.image_url = identity.reference("https://cdn.example.com/secure/b.png?s=B&t=1");
        let mut second = draft("3", "告知 3", "");
        second.body_html = None;
        second.image_url = Some("https://cdn.example.com/secure/banner.png?s=C&t=2".into());

        // 同じ画像 (同じ参照) は 1 度だけ、配信元の URL (署名付き) で取りに行く。宣言の外の画像は
        // URL そのものが鍵。
        assert_eq!(
            prefetch_targets(&identity, &[first, skipped, second]),
            vec![
                (identity.reference(banner).unwrap(), banner.to_string()),
                (
                    "https://other.example.com/x.png?v=2".to_string(),
                    "https://other.example.com/x.png?v=2".to_string()
                ),
            ]
        );
    }

    #[test]
    fn selects_missing_images_first_and_stale_ones_within_limits() {
        let dir = tempfile::tempdir().unwrap();
        let images = ImageCache::new(dir.path().join("images"));
        let now = SystemTime::now();
        let at = Instant::now();
        let hours = |hours: u64| StdDuration::from_secs(hours * 3600);
        let put = |url: &str, size: usize, checked_at: SystemTime| {
            images.write(url, &vec![0u8; size]).unwrap();
            std::fs::File::options()
                .write(true)
                .open(images.path_for(url))
                .unwrap()
                .set_modified(checked_at)
                .unwrap();
        };
        let reference = |name: &str| image_ref::reference_url("gc", &format!("{name:0>32}"));
        put(&reference("1"), 10, now - hours(1));
        put(&reference("2"), 10, now - hours(30));
        put(&reference("3"), 10, now - hours(50));
        put("https://cdn.example.com/plain.png", 10, now - hours(40));
        put(&reference("4"), 10, now - hours(60));
        put(&reference("7"), 10, now - hours(70));
        // 時計を戻して、確かめた時刻が今より先になった画像。
        put(&reference("5"), 10, now + hours(5));
        put(&reference("6"), 10, now + StdDuration::from_secs(60));
        let target = |key: String| (key.clone(), format!("https://cdn.example.com/{key}"));
        let targets: Vec<(String, String)> = [
            reference("1"),
            reference("2"),
            reference("3"),
            "https://cdn.example.com/plain.png".to_string(),
            reference("4"),
            reference("7"),
            reference("5"),
            reference("6"),
            reference("missing"),
            reference("busy"),
            reference("failed"),
        ]
        .into_iter()
        .map(target)
        .collect();
        let failure = |retry_at: Instant, not_before: Option<Instant>| ImageFailure {
            transient: true,
            count: 1,
            retry_at,
            not_before,
        };
        let failures = HashMap::from([
            // 確かめ直しに失敗して、次に試す時刻を待っている。
            (reference("4"), failure(at + hours(1), None)),
            // 待つ時刻を過ぎた。
            (reference("7"), failure(at - hours(1), None)),
            // キャッシュにない画像でも、配信元が指定した待ち時間 (Retry-After) が明けるまでは取りに行かない。
            (
                reference("busy"),
                failure(at + hours(1), Some(at + hours(1))),
            ),
            // キャッシュにない画像は、失敗していても取得のたびに取りに行く (署名付きの URL は新しくなる)。
            (reference("failed"), failure(at + hours(1), None)),
        ]);
        let plan = select_prefetch(&images, targets.clone(), now, &failures, at);
        assert_eq!(
            plan.missing,
            vec![target(reference("missing")), target(reference("failed"))]
        );
        // 確かめた時刻の古いものから (普通の URL も確かめ直す)。少し先の時刻は時計のずれとして待つ。
        assert_eq!(
            plan.stale,
            vec![
                target(reference("7")),
                target(reference("3")),
                target("https://cdn.example.com/plain.png".to_string()),
                target(reference("2")),
                target(reference("5")),
            ]
        );
    }

    #[test]
    fn revalidations_stop_at_the_count_and_size_limits() {
        let dir = tempfile::tempdir().unwrap();
        let images = ImageCache::new(dir.path().join("images"));
        let now = SystemTime::now();
        let old = now - IMAGE_REVALIDATE_AFTER * 2;
        let mut targets = Vec::new();
        for index in 0..IMAGE_REVALIDATE_LIMIT + 5 {
            let url = format!("https://cdn.example.com/{index}.png");
            images.write(&url, b"x").unwrap();
            std::fs::File::options()
                .write(true)
                .open(images.path_for(&url))
                .unwrap()
                .set_modified(old)
                .unwrap();
            targets.push((url.clone(), url));
        }
        let plan = select_prefetch(&images, targets, now, &HashMap::new(), Instant::now());
        assert_eq!(plan.stale.len(), IMAGE_REVALIDATE_LIMIT);

        // 大きさの上限を超える分は次の取得に回す (1 枚目は上限より大きくても確かめる)。
        let big = "https://cdn.example.com/big.png";
        let half = "https://cdn.example.com/half.png";
        let rest = "https://cdn.example.com/rest.png";
        for (url, size, age) in [
            (big, IMAGE_REVALIDATE_BYTES + 1, 3),
            (half, IMAGE_REVALIDATE_BYTES / 2, 2),
            (rest, IMAGE_REVALIDATE_BYTES / 2, 1),
        ] {
            images.write(url, &vec![0u8; size as usize]).unwrap();
            std::fs::File::options()
                .write(true)
                .open(images.path_for(url))
                .unwrap()
                .set_modified(old - StdDuration::from_secs(age))
                .unwrap();
        }
        let targets = [big, half, rest]
            .iter()
            .map(|url| (url.to_string(), url.to_string()))
            .collect();
        let plan = select_prefetch(&images, targets, now, &HashMap::new(), Instant::now());
        assert_eq!(plan.stale, vec![(big.to_string(), big.to_string())]);
    }

    #[test]
    fn failed_downloads_wait_longer_each_time() {
        let now = Instant::now();
        let status = |code: u16, retry_after: Option<u64>| ImageFetchError::Status {
            status: reqwest::StatusCode::from_u16(code).unwrap(),
            retry_after: retry_after.map(StdDuration::from_secs),
        };
        let first = ImageFailure::after(None, &status(403, None), now);
        assert!(!first.transient);
        assert_eq!(first.retry_at, now + IMAGE_RETRY_BASE);
        assert_eq!(first.not_before, None);
        let second = ImageFailure::after(Some(&first), &status(503, None), now);
        assert!(second.transient);
        assert_eq!(second.retry_at, now + IMAGE_RETRY_BASE * 2);
        // Retry-After が長ければそれまで待つ (キャッシュにない画像も)。続けて失敗しても 1 日まで。
        let third = ImageFailure::after(Some(&second), &status(429, Some(4 * 3600)), now);
        assert_eq!(third.retry_at, now + StdDuration::from_secs(4 * 3600));
        assert_eq!(
            third.not_before,
            Some(now + StdDuration::from_secs(4 * 3600))
        );
        let mut failure = third;
        for _ in 0..20 {
            failure = ImageFailure::after(Some(&failure), &status(503, None), now);
        }
        assert_eq!(failure.retry_at, now + IMAGE_RETRY_MAX);
        assert_eq!(failure.not_before, None);
    }

    #[test]
    fn plans_detail_rechecks_for_entries_holding_images() {
        let at = |minutes: i64| Utc::now() - Duration::minutes(minutes);
        let holder = |id: &str, is_image_url: bool, minutes: i64| ImageHolder {
            external_id: id.into(),
            is_image_url,
            seen_at: at(minutes),
        };
        let holders = HashMap::from([
            // 多くの記事で使われる画像は、取得 URL を新しく記録した記事から 3 件まで。
            (
                "shared".to_string(),
                vec![
                    holder("1", false, 1),
                    holder("2", false, 2),
                    holder("3", false, 3),
                    holder("4", false, 4),
                ],
            ),
            // 記事の image_url の画像も、詳細から取ったものかもしれないので詳細を取り直す。
            (
                "banner".to_string(),
                vec![holder("1", true, 1), holder("9", true, 9)],
            ),
            ("gone".to_string(), Vec::new()),
        ]);
        let keys: Vec<String> = ["shared", "banner", "gone"].map(String::from).to_vec();
        let plan = recovery_plan(true, &keys, &holders);
        let mut recheck: Vec<&String> = plan.recheck.iter().collect();
        recheck.sort();
        assert_eq!(recheck, ["1", "2", "3", "9"]);
        assert_eq!(plan.rechecked["shared"], ["1", "2", "3"]);
        assert_eq!(plan.rechecked["banner"], ["1", "9"]);
        assert!(plan.rechecked["gone"].is_empty());
        assert!(plan.deferred.is_empty());
        // 詳細のないソースは、普通の取得で一覧から新しい取得 URL が記録される。
        assert_eq!(
            recovery_plan(false, &keys, &holders),
            RecoveryPlan::default()
        );

        // 1 回の取り直しで詳細を取り直す記事は 10 件まで。残りの画像は次の取り直しに回す。
        let many: HashMap<String, Vec<ImageHolder>> = (0..12)
            .map(|index| {
                (
                    format!("k{index:02}"),
                    vec![holder(&format!("e{index}"), false, 1)],
                )
            })
            .collect();
        let mut keys: Vec<String> = many.keys().cloned().collect();
        keys.sort();
        let plan = recovery_plan(true, &keys, &many);
        assert_eq!(plan.recheck.len(), RECOVERY_RECHECK_LIMIT);
        let mut deferred: Vec<&String> = plan.deferred.iter().collect();
        deferred.sort();
        assert_eq!(deferred, ["k10", "k11"]);
    }

    #[test]
    fn decides_what_to_do_with_images_still_missing_after_a_recovery() {
        let failure = |transient, not_before| ImageFailure {
            transient,
            count: 1,
            retry_at: Instant::now(),
            not_before,
        };
        let facts = |renewed, failure, status, has_detail, misses: &'static [RecheckMiss]| {
            recovery_outcome(&RecoveryFacts {
                renewed,
                failure,
                status,
                has_detail,
                misses,
            })
        };
        let wait = Some(Instant::now() + StdDuration::from_secs(600));
        // 新しい取得 URL でも 403 など: 期限切れではないらしい。
        assert_eq!(
            facts(true, Some(failure(false, None)), RunStatus::Ok, true, &[]),
            RecoveryOutcome::Blocked
        );
        assert_eq!(
            facts(true, None, RunStatus::Ok, true, &[]),
            RecoveryOutcome::Blocked
        );
        // 新しい取得 URL で、通信の失敗・5xx・429: あとで試す (配信元が指定した待ち時間は守る)。
        assert_eq!(
            facts(true, Some(failure(true, wait)), RunStatus::Ok, true, &[]),
            RecoveryOutcome::Transient { not_before: wait }
        );
        // 新しい取得 URL が記録されなかった: 詳細が消えた・本文から画像が外れたなら諦める。
        assert_eq!(
            facts(false, None, RunStatus::Ok, true, &[RecheckMiss::Gone]),
            RecoveryOutcome::NotFound
        );
        assert_eq!(
            facts(false, None, RunStatus::Ok, true, &[]),
            RecoveryOutcome::NotFound
        );
        assert_eq!(
            facts(false, None, RunStatus::Partial, false, &[]),
            RecoveryOutcome::Transient { not_before: None }
        );
        // 詳細や一覧を取れなかっただけなら、あとで試す (一覧にある記事も一覧にない記事も)。
        assert_eq!(
            facts(
                false,
                None,
                RunStatus::Partial,
                true,
                &[RecheckMiss::Failed, RecheckMiss::Gone]
            ),
            RecoveryOutcome::Transient { not_before: None }
        );
        assert_eq!(
            facts(false, None, RunStatus::Ok, true, &[RecheckMiss::Failed]),
            RecoveryOutcome::Transient { not_before: None }
        );
        assert_eq!(
            facts(false, None, RunStatus::Failed, true, &[]),
            RecoveryOutcome::Transient { not_before: None }
        );
    }

    /// 作業用ディレクトリでサービスを立ち上げる (通信はしない)。
    fn start_services() -> (tempfile::TempDir, Services, flume::Receiver<ServiceEvent>) {
        let dir = tempfile::tempdir().unwrap();
        let (sender, receiver) = flume::unbounded();
        let (services, _) =
            Services::start(AppPaths::under(dir.path().to_path_buf()), sender).unwrap();
        (dir, services, receiver)
    }

    fn news_source(services: &Services) -> ResolvedSource {
        services
            .sources()
            .into_iter()
            .find(|source| source.enabled && source.config.channel == Channel::News)
            .expect("既定で on のお知らせのソース")
    }

    fn draft(id: &str, title: &str, body: &str) -> EntryDraft {
        EntryDraft {
            external_id: id.into(),
            title: title.into(),
            url: None,
            published: Some(gnn_core::Published::datetime(Utc::now())),
            body_html: Some(body.into()),
            image_url: None,
            note: None,
            list_signature: String::new(),
        }
    }

    /// 予定の読み直しを待っている記事。
    fn pending(services: &Services) -> Vec<AnalysisInput> {
        let contexts: Vec<(String, String)> = services
            .analysis_contexts()
            .into_iter()
            .map(|(id, context)| (id, context.fingerprint))
            .collect();
        services
            .with_store(|store| {
                store.pending_analyses(&gnn_schedule::extractor_id(), &contexts, 100)
            })
            .unwrap()
    }

    // ---- 終わるとき ----

    /// 通信しないサービス。カタログの場所は `sources` のない空のディレクトリ (取り込みに失敗して
    /// 同梱版のまま) にし、ソースはすべて off にする。
    fn offline_services() -> (tempfile::TempDir, Services, flume::Receiver<ServiceEvent>) {
        let dir = tempfile::tempdir().unwrap();
        let paths = AppPaths::under(dir.path().join("home"));
        paths.ensure().unwrap();
        let catalog = dir.path().join("catalog");
        std::fs::create_dir_all(&catalog).unwrap();
        ConfigStore::new(paths.clone())
            .save_settings(&AppSettings {
                catalog: catalog.display().to_string(),
                fetch_on_startup: false,
                ..AppSettings::default()
            })
            .unwrap();
        let (sender, receiver) = flume::unbounded();
        let (services, _) = Services::start(paths, sender).unwrap();
        let ids: Vec<String> = services
            .sources()
            .iter()
            .map(|source| source.id().to_string())
            .collect();
        services
            .update_subscriptions(|subscriptions| {
                for id in &ids {
                    subscriptions.set_enabled(id, false);
                }
            })
            .unwrap();
        assert!(services.sources().iter().all(|source| !source.enabled));
        (dir, services, receiver)
    }

    /// 手元の定義のお知らせのソース (RSS) を足して on にし、localhost への取得を許す。
    fn add_local_feed(services: &Services, id: &str, port: u16) {
        use gnn_core::source::{ListKind, ListSpec};
        let mut source = gnn_core::templates::blank(ListKind::Feed, id.into());
        source.lists = vec![ListSpec::empty(
            ListKind::Feed,
            format!("http://127.0.0.1:{port}/feed.xml"),
        )];
        services
            .save_local_source(id, &source.to_yaml().unwrap(), true)
            .unwrap();
        services
            .update_subscriptions(|subscriptions| {
                subscriptions.entry(id).allow_private_network = true;
            })
            .unwrap();
    }

    /// `condition` が成り立つまで待つ (10 秒まで)。
    fn wait_until(mut condition: impl FnMut() -> bool) {
        let deadline = Instant::now() + StdDuration::from_secs(10);
        while !condition() {
            assert!(Instant::now() < deadline, "10 秒待っても成り立ちません");
            std::thread::sleep(StdDuration::from_millis(10));
        }
    }

    /// 閉じたあとのデータベースを開き直して、記事の数を数える。
    fn saved_entries(services: &Services) -> usize {
        Store::open(&services.paths().database_file())
            .unwrap()
            .query(&gnn_core::EntryQuery::channel(Channel::News))
            .unwrap()
            .len()
    }

    /// 合っている実装なら止まったままのところで、戻ってこないことを確かめる時間。誤った実装はこの間に
    /// 戻ってしまう (合っている実装は、どれだけ待っても戻らないので、遅い CI でも誤って落ちない)。
    const STILL_WAITING: StdDuration = StdDuration::from_millis(200);
    /// 期限に引っかからないよう、テストで長めに渡す `shutdown` の期限。
    const LONG: StdDuration = StdDuration::from_secs(30);
    /// 誤った実装が待ち続けても、テストが止まらないための外の期限。
    const WATCHDOG: StdDuration = StdDuration::from_secs(10);

    /// 画面からの書き込みの代わり。ストアのロックを取って知らせ、`release` に知らされるまでロックを
    /// 持ったまま待ってから、記事を 1 件書き込む。ロックを取ったら戻る。
    fn hold_store(services: &Services) -> (flume::Sender<()>, std::thread::JoinHandle<Result<()>>) {
        let (locked, is_locked) = flume::bounded(1);
        let (release, released) = flume::bounded::<()>(1);
        let holder = {
            let services = services.clone();
            std::thread::spawn(move || {
                services.with_store(|store| {
                    locked.send(()).unwrap();
                    released.recv().unwrap();
                    store
                        .apply_fetch(
                            "game",
                            Channel::News,
                            &ImageIdentity::default(),
                            &[draft("1", "お知らせ", "<p>本文</p>")],
                            None,
                            Utc::now(),
                        )
                        .map(|_| ())
                })
            })
        };
        is_locked.recv().unwrap();
        (release, holder)
    }

    /// 別のスレッドで `shutdown` を呼ぶ。結果はチャネルで受け取る。
    fn spawn_shutdown(services: &Services, timeout: StdDuration) -> flume::Receiver<bool> {
        let (sender, receiver) = flume::bounded(1);
        let services = services.clone();
        std::thread::spawn(move || {
            let _ = sender.send(services.shutdown(timeout));
        });
        receiver
    }

    /// `at` で 1 度だけ止める。止まったら 1 つ目に知らせ、2 つ目に知らされるまで待つ。
    fn pause_once(services: &Services, at: Pause) -> (flume::Receiver<()>, flume::Sender<()>) {
        let (paused, is_paused) = flume::bounded(1);
        let (resume, resumed) = flume::bounded::<()>(1);
        let armed = Mutex::new(Some((paused, resumed)));
        *services.0.pause.write() = Some(Arc::new(move |point| {
            if point != at {
                return;
            }
            let Some((paused, resumed)) = armed.lock().take() else {
                return;
            };
            let _ = paused.send(());
            let _ = resumed.recv();
        }));
        (is_paused, resume)
    }

    /// サービスからのストアの処理で、記事を 1 件書き込む。
    fn write_one(services: &Services) -> tokio::task::JoinHandle<Result<gnn_core::ApplyReport>> {
        let worker = services.clone();
        services
            .spawn(async move {
                worker
                    .blocking(|store| {
                        store.apply_fetch(
                            "game",
                            Channel::News,
                            &ImageIdentity::default(),
                            &[draft("1", "お知らせ", "<p>本文</p>")],
                            None,
                            Utc::now(),
                        )
                    })
                    .await
            })
            .expect("合図の前なので受け付ける")
    }

    #[test]
    fn shutdown_stops_the_loops_and_starts_nothing_afterwards() {
        let (_dir, services, events) = offline_services();
        services.spawn_scheduler();
        // 起動時のカタログの取り込み (空のディレクトリなので失敗する) が済むまで待つ。
        wait_until(|| services.catalog_status().error.is_some());
        assert!(
            services.0.tasks.len() >= 2,
            "定期取得と予定の読み取りのループ"
        );

        // ループは合図ですぐ抜ける。ストアを閉じるのはテストを並べると遅れることがあるので、期限は長めにする。
        assert!(services.shutdown(LONG));
        assert!(services.0.tasks.is_empty(), "ループは抜けた");
        // ストアは閉じている。サービスの処理からは、合図のあとなので記録しない誤りになる。
        let closed = services.with_store(|store| store.source_states());
        assert!(closed.is_err());
        let refused = services
            .runtime()
            .block_on(services.blocking(|store| store.source_states()));
        assert!(is_stopped(&refused.unwrap_err()));

        // 合図の前の知らせは読み捨て、合図のあとに何も始まらないことを確かめる。
        while events.try_recv().is_ok() {}
        assert!(services.spawn(async {}).is_none());
        services.fetch_now(None);
        services.refresh_catalog();
        services.wake_analysis();
        services.0.wake.notify_one();
        let test = services.test_fetch(services.catalog().entries()[0].source.clone(), true, 5);
        assert!(
            services.runtime().block_on(test).is_err(),
            "テスト取得は取得せずに送り手を落とす"
        );
        assert!(services.0.tasks.is_empty());
        assert!(events.recv_timeout(STILL_WAITING).is_err());
        // 2 回目は 1 回目の結果を返す。
        assert!(services.shutdown(LONG));
    }

    #[test]
    fn shutdown_waits_for_a_store_write_in_progress() {
        let (_dir, services, _events) = offline_services();
        // 画面からの書き込み (gpui の background で with_store を呼ぶのと同じ) が、ロックを持ったまま
        // 書き込みの途中にいる。
        let (release, holder) = hold_store(&services);
        let shutdown = spawn_shutdown(&services, LONG);
        wait_until(|| services.stopping());
        assert!(
            shutdown.recv_timeout(STILL_WAITING).is_err(),
            "書き込みの途中でストアを閉じない"
        );
        release.send(()).unwrap();
        holder.join().unwrap().unwrap();
        assert!(shutdown.recv_timeout(WATCHDOG).unwrap());
        assert_eq!(
            saved_entries(&services),
            1,
            "閉じる前に書き込みが済んでいる"
        );
    }

    #[test]
    fn service_writes_started_before_the_stop_are_finished() {
        let (_dir, services, _events) = offline_services();
        let (locked, is_locked) = flume::bounded(1);
        let (release, released) = flume::bounded::<()>(1);
        let worker = services.clone();
        let write = services
            .spawn(async move {
                worker
                    .blocking(move |store| {
                        locked.send(()).unwrap();
                        released.recv().unwrap();
                        store.apply_fetch(
                            "game",
                            Channel::News,
                            &ImageIdentity::default(),
                            &[draft("1", "お知らせ", "<p>本文</p>")],
                            None,
                            Utc::now(),
                        )
                    })
                    .await
            })
            .unwrap();
        // 書き込みは合図の前にロックを取り、合図を確かめ終えている。
        is_locked.recv().unwrap();
        let shutdown = spawn_shutdown(&services, LONG);
        wait_until(|| services.stopping());
        assert!(
            shutdown.recv_timeout(STILL_WAITING).is_err(),
            "書き込みの途中で戻らない"
        );
        release.send(()).unwrap();
        assert!(shutdown.recv_timeout(WATCHDOG).unwrap());
        assert!(
            services.runtime().block_on(write).unwrap().is_ok(),
            "合図が来ても最後まで書き込む"
        );
        assert_eq!(saved_entries(&services), 1);
    }

    #[test]
    fn service_store_work_is_waited_for_and_dropped_after_the_stop() {
        let (_dir, services, _events) = offline_services();
        // サービスの書き込みが、合図の前にストアのロックを取りに行く手前で止まっている。
        let (paused, resume) = pause_once(&services, Pause::StoreLock);
        let caller = write_one(&services);
        paused.recv_timeout(WATCHDOG).unwrap();
        // 待つ側を取り消しても、ストアの処理は待つ処理として数えたまま残る。
        caller.abort();
        assert!(
            services
                .runtime()
                .block_on(caller)
                .unwrap_err()
                .is_cancelled()
        );
        let shutdown = spawn_shutdown(&services, LONG);
        wait_until(|| services.stopping());
        assert!(
            shutdown.recv_timeout(STILL_WAITING).is_err(),
            "数えているストアの処理が済むまで戻らない"
        );
        resume.send(()).unwrap();
        assert!(shutdown.recv_timeout(WATCHDOG).unwrap());
        assert_eq!(
            saved_entries(&services),
            0,
            "合図のあとにロックを取ったので書き込まない"
        );
    }

    #[test]
    fn shutdown_gives_up_at_the_deadline() {
        let (_dir, services, _events) = offline_services();
        let (release, holder) = hold_store(&services);
        let started = Instant::now();
        let first = spawn_shutdown(&services, StdDuration::from_millis(300));
        assert!(!first.recv_timeout(WATCHDOG).expect("期限で戻る"));
        assert!(
            started.elapsed() >= StdDuration::from_millis(250),
            "期限までは待つ"
        );
        // 2 回目は待たずに 1 回目の結果を返す (待つなら、書き込みが続くあいだ戻らない)。
        let again = spawn_shutdown(&services, LONG);
        assert!(!again.recv_timeout(WATCHDOG).expect("すぐ戻る"));
        release.send(()).unwrap();
        holder.join().unwrap().unwrap();
        // 書き込みが続いていたのでストアは閉じず、書き込みは済んでいる。
        assert!(services.with_store(|store| store.source_states()).is_ok());
    }

    #[test]
    fn shutdown_does_not_wait_past_the_deadline_for_a_slow_close() {
        let (_dir, services, _events) = offline_services();
        // ストアを閉じる係 (WAL の書き戻し) が止まっている。
        let (paused, resume) = pause_once(&services, Pause::Close);
        let shutdown = spawn_shutdown(&services, StdDuration::from_millis(300));
        paused.recv_timeout(WATCHDOG).unwrap();
        let finished = shutdown.recv_timeout(WATCHDOG);
        resume.send(()).unwrap();
        assert!(
            !finished.expect("閉じ終わるのを期限より長く待たない"),
            "閉じ終わっていない"
        );
        // ストアは取り出したので、閉じ終わっていなくても、もう使えない。2 回目も同じ結果を返す。
        assert!(services.with_store(|store| store.source_states()).is_err());
        assert!(!services.shutdown(LONG));
    }

    #[test]
    fn shutdown_cancels_a_fetch_in_progress() {
        let (_dir, services, events) = offline_services();
        // 接続を受けたら知らせ、何も返さない HTTP サーバー。
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let (accepted, is_accepted) = flume::unbounded();
        std::thread::spawn(move || {
            let mut held = Vec::new();
            for stream in listener.incoming().flatten() {
                held.push(stream);
                let _ = accepted.send(());
            }
        });
        add_local_feed(&services, "stalled", port);
        services.fetch_now(Some(vec!["stalled".into()]));
        is_accepted
            .recv_timeout(StdDuration::from_secs(10))
            .expect("取得が通信を始める");
        assert!(services.running().contains("stalled"));

        // HTTP の期限 (30 秒) を待たずに取り消す (取り消さなければ、この期限で false になる)。
        assert!(services.shutdown(WATCHDOG));
        assert!(services.running().is_empty());
        // 取り消した取得は、保存も取得ログの記録もせず (次の取得で取り直す)、画面に終わりを知らせない。
        let store = Store::open(&services.paths().database_file()).unwrap();
        assert!(!store.source_states().unwrap().contains_key("stalled"));
        assert!(!events.try_iter().any(|event| matches!(
            event,
            ServiceEvent::FetchFinished(_) | ServiceEvent::CycleFinished { .. }
        )));
    }

    #[test]
    fn work_accepted_before_the_stop_is_waited_for() {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (_dir, services, _events) = offline_services();
        // 受け付けが合図を確かめ終え、まだ処理を数えていないところで止める。
        let (paused, resume) = pause_once(&services, Pause::Register);
        let finished = Arc::new(AtomicBool::new(false));
        let spawner = {
            let services = services.clone();
            let finished = finished.clone();
            std::thread::spawn(move || {
                services
                    .spawn(async move { finished.store(true, Ordering::SeqCst) })
                    .is_some()
            })
        };
        paused.recv_timeout(WATCHDOG).unwrap();
        // 合図は受け付けが済むまで立たない (受け付けの途中で立つと、受け付けた処理を待ちそこねる)。
        let shutdown = spawn_shutdown(&services, LONG);
        assert!(shutdown.recv_timeout(STILL_WAITING).is_err());
        assert!(!services.stopping());
        resume.send(()).unwrap();
        assert!(spawner.join().unwrap(), "合図の前に受け付けた");
        assert!(shutdown.recv_timeout(WATCHDOG).unwrap());
        assert!(
            finished.load(Ordering::SeqCst),
            "受け付けた処理が済んでから戻る"
        );
    }

    #[test]
    fn analysis_stops_at_the_stop_signal_without_saving() {
        let (_dir, services, _events) = start_services();
        let id = news_source(&services).id().to_string();
        services
            .with_store(|store| {
                store.apply_fetch(
                    &id,
                    Channel::News,
                    &ImageIdentity::default(),
                    &[
                        draft(
                            "1",
                            "定期メンテナンスのお知らせ",
                            "<p>メンテナンスを実施します。</p>",
                        ),
                        draft("2", "新キャラクター登場", "<p>新しい仲間が加わります。</p>"),
                    ],
                    None,
                    Utc::now(),
                )
            })
            .unwrap();
        let inputs = pending(&services);
        assert_eq!(inputs.len(), 2);
        // 合図が立っていれば、残りの記事は読まずに戻る。
        let stop = CancellationToken::new();
        stop.cancel();
        let contexts = services.analysis_contexts();
        assert!(analyze(&inputs, &contexts, &gnn_schedule::extractor_id(), &stop).is_empty());

        // 合図のあとは、待ち行列も読まずに抜け、サービスからのストアの処理は始めない。
        services.0.stop.cancel();
        services.runtime().block_on(services.drain_analysis());
        assert_eq!(pending(&services).len(), 2);
        let refused = services
            .runtime()
            .block_on(services.blocking(|store| store.prune(Utc::now())));
        assert!(is_stopped(&refused.unwrap_err()));
        assert_eq!(pending(&services).len(), 2, "古い記事として消していない");
    }

    #[test]
    fn fetch_once_reads_every_schedule_and_then_closes_the_store() {
        let (_dir, services, _events) = offline_services();
        // 取得先は閉じたポートなので、取得はすぐ失敗する (外へは通信しない)。
        let port = std::net::TcpListener::bind(("127.0.0.1", 0))
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        add_local_feed(&services, "local_news", port);
        services
            .with_store(|store| {
                store.apply_fetch(
                    "local_news",
                    Channel::News,
                    &ImageIdentity::default(),
                    &[draft(
                        "1",
                        "定期メンテナンスのお知らせ",
                        "<p>2026年11月12日(木) 14:00～17:00 にメンテナンスを実施いたします。</p>",
                    )],
                    None,
                    Utc::now(),
                )
            })
            .unwrap();
        assert_eq!(pending(&services).len(), 1);

        let summaries = services.fetch_once_blocking();
        assert_eq!(summaries.len(), 1);
        assert_eq!(summaries[0].status, RunStatus::Failed);
        // 取得に失敗しても、保存済みの記事の予定は読み終えている (止める合図は立てない)。
        assert!(pending(&services).is_empty());
        assert!(
            services.0.tasks.is_empty(),
            "取得と予定の読み取りは済んでいる"
        );
        // ストアを閉じる (WAL の書き戻し) のは、テストを並べて動かすと 1 秒ほどかかることがあるので、
        // 期限は長めにする。
        assert!(services.shutdown(LONG));
        assert!(services.with_store(|store| store.source_states()).is_err());
    }

    #[test]
    fn analysis_queue_reads_every_entry_and_rereads_when_context_changes() {
        let (_dir, services, _events) = start_services();
        let source = news_source(&services);
        let id = source.id().to_string();
        services
            .with_store(|store| {
                store.apply_fetch(
                    &id,
                    Channel::News, &ImageIdentity::default(),
                    &[
                        draft(
                            "1",
                            "定期メンテナンスのお知らせ",
                            "<p>2026年11月12日(木) 14:00～17:00 にメンテナンスを実施いたします。</p>",
                        ),
                        draft("2", "新キャラクター登場", "<p>新しい仲間が加わります。</p>"),
                    ],
                    None,
                    Utc::now(),
                )
            })
            .unwrap();
        assert_eq!(pending(&services).len(), 2);

        services.runtime().block_on(services.drain_analysis());
        assert!(pending(&services).is_empty());
        let entries = services
            .with_store(|store| store.query(&gnn_core::EntryQuery::channel(Channel::News)))
            .unwrap();
        // 何を読み取るかは抽出器が決める。ここではすべての記事に今の中身の結果があることだけを見る。
        for entry in &entries {
            let schedule = services
                .with_store(|store| store.entry_schedule(entry.id))
                .unwrap()
                .expect("読み取った結果");
            assert!(schedule.current);
        }

        // 本文の地域時刻を変えたら、保存済みの記事を読み直す (取り直しはしない)。
        let mut changed = source.config.clone();
        changed.schedule.timezone = Some("Asia/Shanghai".into());
        services
            .save_local_source(&id, &changed.to_yaml().unwrap(), false)
            .unwrap();
        assert_eq!(pending(&services).len(), 2);
        services.runtime().block_on(services.drain_analysis());
        assert!(pending(&services).is_empty());
    }

    #[test]
    fn rechecks_entries_with_upcoming_or_running_maintenance() {
        use gnn_core::schedule::{
            Certainty, Disposition, Maintenance, Schedule, Scope, Slot, TimeValue, When, Window,
            Zone,
        };
        let (_dir, services, _events) = start_services();
        let id = news_source(&services).id().to_string();
        let now = Utc::now();
        let at = |offset: Duration| {
            Slot::Value(TimeValue {
                when: When::Instant {
                    at: now + offset,
                    zone: Zone::Named("Asia/Tokyo".into()),
                },
                certainty: Certainty::Explicit,
                evidence: Vec::new(),
            })
        };
        let schedule = |start: Duration, end: Duration, disposition| Schedule {
            maintenance: vec![Maintenance {
                scope: Scope::Unknown,
                planned: Window {
                    start: at(start),
                    end: at(end),
                },
                actual: Window::default(),
                disposition,
                ongoing: None,
                changes: Vec::new(),
                evidence: Vec::new(),
            }],
            ..Schedule::default()
        };
        // 突き合わせで結び付かないよう、時間帯はすべて違えておく。
        let outcomes = [
            (
                "soon",
                schedule(
                    Duration::days(2),
                    Duration::days(2) + Duration::hours(3),
                    Disposition::Scheduled,
                ),
            ),
            (
                "running",
                schedule(
                    -Duration::hours(1),
                    Duration::hours(2),
                    Disposition::Scheduled,
                ),
            ),
            (
                "done",
                schedule(
                    -Duration::hours(6),
                    -Duration::hours(4),
                    Disposition::Completed,
                ),
            ),
            (
                "long_ago",
                schedule(
                    -Duration::days(2),
                    -Duration::days(2) + Duration::hours(3),
                    Disposition::Scheduled,
                ),
            ),
        ];
        services
            .with_store(|store| -> Result<()> {
                let drafts: Vec<EntryDraft> = outcomes
                    .iter()
                    .map(|(external_id, _)| draft(external_id, external_id, "<p>メンテナンス</p>"))
                    .collect();
                store.apply_fetch(
                    &id,
                    Channel::News,
                    &ImageIdentity::default(),
                    &drafts,
                    None,
                    now,
                )?;
                let inputs = store.pending_analyses("test", &[(id.clone(), "ctx".into())], 10)?;
                let results: Vec<AnalysisResult> = inputs
                    .iter()
                    .map(|input| AnalysisResult {
                        entry_id: input.entry_id,
                        revision: input.revision,
                        extractor: "test".into(),
                        context: "ctx".into(),
                        analyzed_at: now,
                        outcome: Ok(outcomes
                            .iter()
                            .find(|(external_id, _)| *external_id == input.title)
                            .unwrap()
                            .1
                            .clone()),
                    })
                    .collect();
                store.save_analyses(&results, &mut reconcile_source)?;
                Ok(())
            })
            .unwrap();
        let mut ids: Vec<String> = services
            .runtime()
            .block_on(services.recheck_ids(&id, now))
            .into_iter()
            .collect();
        ids.sort();
        assert_eq!(ids, vec!["running", "soon"]);
    }

    #[test]
    fn exports_legacy_dates_per_entry() {
        use gnn_core::schedule::{Disposition, Window};
        let item = |entry_id: i64, disposition| AgendaItem {
            entry_id,
            source_id: "game".into(),
            detail: AgendaDetail::from(agenda::ResolvedMaintenance {
                index: 0,
                scope: Default::default(),
                disposition,
                planned: Window {
                    start: gnn_core::schedule::Slot::Value(gnn_core::schedule::TimeValue {
                        when: gnn_core::schedule::When::Instant {
                            at: Utc::now(),
                            zone: gnn_core::schedule::Zone::Offset(0),
                        },
                        certainty: gnn_core::schedule::Certainty::Explicit,
                        evidence: Vec::new(),
                    }),
                    end: Default::default(),
                },
                actual: Window::default(),
                changes: Vec::new(),
                ongoing: None,
                resolved_by: None,
            }),
        };
        let dates = legacy_dates(&[
            item(1, Disposition::Scheduled),
            item(2, Disposition::Completed),
        ]);
        assert_eq!(dates.len(), 1);
        assert_eq!(dates[&1], vec![chrono::Local::now().date_naive()]);
    }

    #[test]
    fn context_fingerprint_names_both_timezones() {
        let mut source = Catalog::bundled().entries()[0].source.clone();
        source.timezone = Some("Asia/Shanghai".into());
        source.schedule = Default::default();
        assert_eq!(
            AnalysisContext::of(&source).fingerprint,
            "timezone=Asia/Shanghai;server_timezone=-"
        );
        source.schedule.timezone = Some("Asia/Tokyo".into());
        source.schedule.server_timezone = Some("Asia/Shanghai".into());
        let context = AnalysisContext::of(&source);
        assert_eq!(context.timezone, chrono_tz::Asia::Tokyo);
        assert_eq!(context.server_timezone, Some(chrono_tz::Asia::Shanghai));
        assert_eq!(
            context.fingerprint,
            "timezone=Asia/Tokyo;server_timezone=Asia/Shanghai"
        );
    }

    #[test]
    fn catalog_sources_are_public_only_without_secrets() {
        let catalog = Catalog::bundled();
        let id = catalog.entries()[0].source.id.clone();
        let mut local = catalog.entries()[1].source.clone();
        local.id = "mine".into();
        let subscriptions = Subscriptions {
            items: vec![
                gnn_core::Subscription {
                    allow_private_network: true,
                    ..gnn_core::Subscription::new(id.clone())
                },
                gnn_core::Subscription {
                    allow_private_network: true,
                    ..gnn_core::Subscription::new("mine")
                },
            ],
        };
        let resolved = resolve(&catalog, &subscriptions, &[local]);
        let catalog_source = resolved.iter().find(|source| source.id() == id).unwrap();
        assert_eq!(
            fetch_permissions(catalog_source),
            (NetworkPolicy::PublicOnly, false)
        );
        let mine = resolved
            .iter()
            .find(|source| source.id() == "mine")
            .unwrap();
        assert_eq!(fetch_permissions(mine), (NetworkPolicy::Any, true));
    }
}
