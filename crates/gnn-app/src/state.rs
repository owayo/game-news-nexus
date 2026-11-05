//! 画面全体で共有する状態。設定・カタログ・使うソース・取得状況・未読数・予定の表を持つ。

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration as StdDuration;

use anyhow::{Result, bail};
use chrono::{DateTime, Duration, Local, NaiveDate, Utc};
use gnn_core::agenda::{self, Agenda, Days, SourceMark};
use gnn_core::catalog::{Catalog, ResolvedSource, SourceOrigin};
use gnn_core::config::LocalSourceFile;
use gnn_core::source::Channel;
use gnn_core::{AppSettings, RunStatus, SourceConfig, SourceState, Subscriptions};
use gpui_kit::{App, AppContext as _, Context, Entity, EventEmitter, Global, Task};

use crate::services::{AddedEntry, CatalogStatus, ServiceEvent, Services};

/// 予定の表を読み直す知らせが続いたときに、まとめるために待つ時間。
const AGENDA_COALESCE: StdDuration = StdDuration::from_millis(300);
/// 予定の状態が変わる時刻を待つ間隔の上限。これより長く待たず、日付の変わり目と
/// スリープからの復帰 (時刻の飛び) を確かめる。
const CLOCK_MAX_WAIT: StdDuration = StdDuration::from_secs(60);
const CLOCK_MIN_WAIT: StdDuration = StdDuration::from_millis(500);
/// 待った時間よりこれ以上時刻が進んでいたら、スリープから戻ったとみなす。
const RESUME_SLACK: Duration = Duration::seconds(30);

/// 画面へ知らせる変化。
#[derive(Debug, Clone)]
pub enum AppEvent {
    /// 記事が増えた・変わった・既読になった。一覧を読み直す。
    EntriesChanged,
    /// 巡回で新着があった。
    NewEntries(Vec<AddedEntry>),
    /// 予定の表を読み直した。予定の開始・終了の時刻・日付の変わり目・スリープからの復帰でも
    /// 読み直す。一覧の並び (今日・明日の予定) とバッジを計算し直す。
    AgendaChanged,
}

pub struct AppState {
    services: Services,
    settings: AppSettings,
    /// 使うソース。off のものも含む。
    sources: Vec<ResolvedSource>,
    catalog: Arc<Catalog>,
    catalog_status: CatalogStatus,
    subscriptions: Subscriptions,
    local_files: Vec<LocalSourceFile>,
    states: HashMap<String, SourceState>,
    unread: HashMap<Channel, HashMap<String, usize>>,
    running: HashSet<String>,
    last_cycle_at: Option<DateTime<Utc>>,
    /// 次にウィンドウを開いたときに知らせること。
    notice: Option<String>,
    /// on のお知らせのソースの予定の表 (今の前後の範囲)。一覧のバッジと並び、サイドバーの印に使う。
    agenda: Arc<Agenda>,
    /// ソースごとのサイドバーの印 (予定の表と今の時刻から計算したもの)。
    marks: HashMap<String, SourceMark>,
    agenda_loading: bool,
    /// 読み込み中に、もう一度読み直す知らせが来た。
    agenda_dirty: bool,
    agenda_task: Task<()>,
    clock: Task<()>,
}

impl EventEmitter<AppEvent> for AppState {}

struct GlobalAppState(Entity<AppState>);

impl Global for GlobalAppState {}

impl AppState {
    pub fn init(services: Services, cx: &mut App) -> Entity<Self> {
        let state = cx.new(|cx| {
            let mut state = Self {
                settings: services.settings(),
                sources: services.sources(),
                catalog: services.catalog(),
                catalog_status: services.catalog_status(),
                subscriptions: services.subscriptions(),
                local_files: services.local_files(),
                services,
                states: HashMap::new(),
                unread: HashMap::new(),
                running: HashSet::new(),
                last_cycle_at: None,
                notice: None,
                agenda: Arc::default(),
                marks: HashMap::new(),
                agenda_loading: false,
                agenda_dirty: false,
                agenda_task: Task::ready(()),
                clock: Task::ready(()),
            };
            state.refresh_counts(cx);
            state.reload_agenda(cx);
            state
        });
        // スリープから戻ったら、新着がなくても予定の状態 (メンテ中かなど) を計算し直す。
        let weak = state.downgrade();
        cx.on_system_wake(move |cx| {
            let _ = weak.update(cx, |state, cx| state.reload_agenda(cx));
        })
        .detach();
        cx.set_global(GlobalAppState(state.clone()));
        state
    }

    pub fn global(cx: &App) -> Entity<Self> {
        cx.global::<GlobalAppState>().0.clone()
    }

    pub fn services(&self) -> &Services {
        &self.services
    }

    pub fn set_notice(&mut self, notice: String) {
        self.notice = Some(notice);
    }

    pub fn take_notice(&mut self) -> Option<String> {
        self.notice.take()
    }

    pub fn settings(&self) -> &AppSettings {
        &self.settings
    }

    /// 使うソース。off のものも含む。
    pub fn sources(&self) -> &[ResolvedSource] {
        &self.sources
    }

    /// on のソースの定義。
    pub fn enabled_sources(&self) -> impl Iterator<Item = &SourceConfig> {
        self.sources
            .iter()
            .filter(|source| source.enabled)
            .map(|source| &source.config)
    }

    /// on のソースの ID。`channel` で絞る。
    pub fn enabled_ids(&self, channel: Channel) -> Vec<String> {
        self.enabled_sources()
            .filter(|source| source.channel == channel)
            .map(|source| source.id.clone())
            .collect()
    }

    pub fn resolved(&self, id: &str) -> Option<&ResolvedSource> {
        self.sources.iter().find(|source| source.id() == id)
    }

    /// ソースの定義 (購読の上書きを反映したもの)。on/off は問わない。
    pub fn source(&self, id: &str) -> Option<&SourceConfig> {
        self.resolved(id).map(|source| &source.config)
    }

    pub fn catalog(&self) -> &Arc<Catalog> {
        &self.catalog
    }

    pub fn catalog_status(&self) -> &CatalogStatus {
        &self.catalog_status
    }

    pub fn subscriptions(&self) -> &Subscriptions {
        &self.subscriptions
    }

    /// 手元の定義のファイル。読めないものも含む。
    pub fn local_files(&self) -> &[LocalSourceFile] {
        &self.local_files
    }

    pub fn local_file(&self, id: &str) -> Option<&LocalSourceFile> {
        self.local_files.iter().find(|file| file.id == id)
    }

    pub fn state_of(&self, id: &str) -> Option<&SourceState> {
        self.states.get(id)
    }

    pub fn is_running(&self, id: &str) -> bool {
        self.running.contains(id)
    }

    pub fn running_count(&self) -> usize {
        self.running.len()
    }

    pub fn last_cycle_at(&self) -> Option<DateTime<Utc>> {
        self.last_cycle_at
    }

    pub fn unread(&self, channel: Channel, source_id: &str) -> usize {
        self.unread
            .get(&channel)
            .and_then(|counts| counts.get(source_id))
            .copied()
            .unwrap_or(0)
    }

    /// on のソースの未読の合計。
    pub fn unread_total(&self, channel: Channel) -> usize {
        self.enabled_ids(channel)
            .iter()
            .map(|id| self.unread(channel, id))
            .sum()
    }

    /// 失敗している on のソースの数。
    pub fn failing_count(&self) -> usize {
        self.enabled_sources()
            .filter(|source| {
                self.states
                    .get(&source.id)
                    .is_some_and(|state| state.last_status == Some(RunStatus::Failed))
            })
            .count()
    }

    /// 次に取得が回ってくる時刻。
    pub fn next_due(&self) -> Option<DateTime<Utc>> {
        self.enabled_sources()
            .map(|source| {
                let interval = Duration::minutes(self.settings.interval_for(source).into());
                match self
                    .states
                    .get(&source.id)
                    .and_then(|state| state.last_attempt_at)
                {
                    Some(last) => last + interval,
                    None => Utc::now(),
                }
            })
            .min()
    }

    /// 未読数と取得状況をストアから読み直す。
    pub fn refresh_counts(&mut self, cx: &mut Context<Self>) {
        let services = self.services.clone();
        cx.spawn(async move |this, cx| {
            let loaded = cx
                .background_spawn(async move {
                    services.with_store(|store| -> Result<_> {
                        let states = store.source_states()?;
                        let mut unread = HashMap::new();
                        for channel in [Channel::News, Channel::Releases] {
                            unread.insert(channel, store.unread_counts(channel)?);
                        }
                        Ok((states, unread))
                    })
                })
                .await;
            match loaded {
                Ok((states, unread)) => {
                    let _ = this.update(cx, |this, cx| {
                        this.states = states;
                        this.unread = unread;
                        cx.notify();
                    });
                }
                Err(err) => tracing::error!("取得状況を読めません: {err:#}"),
            }
        })
        .detach();
    }

    /// 記事が変わったことを知らせる。
    pub fn entries_changed(&mut self, cx: &mut Context<Self>) {
        self.refresh_counts(cx);
        cx.emit(AppEvent::EntriesChanged);
        self.reload_agenda(cx);
    }

    /// 予定の表 (on のお知らせのソース)。
    pub fn agenda(&self) -> &Arc<Agenda> {
        &self.agenda
    }

    /// サイドバーの印。今メンテナンスの予定の時間内か、今日メンテナンスがあるソースだけ。
    pub fn maintenance_mark(&self, source_id: &str) -> Option<&SourceMark> {
        self.marks.get(source_id)
    }

    /// 予定の表をストアから読み直し、サイドバーの印を計算し直して画面に知らせる。
    ///
    /// 知らせが続いたら少し待ってまとめて読む。読み込み中に呼ばれたら、読み終えてからもう一度読む
    /// (取り込み直後のように知らせが続いても、途中の結果を出しながら最後の状態まで追いつく)。
    pub fn reload_agenda(&mut self, cx: &mut Context<Self>) {
        if self.agenda_loading {
            self.agenda_dirty = true;
            return;
        }
        self.agenda_loading = true;
        let services = self.services.clone();
        let ids = self.enabled_ids(Channel::News);
        self.agenda_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(AGENDA_COALESCE).await;
            let now = Utc::now();
            let loaded = cx
                .background_spawn(async move {
                    services.with_store(|store| {
                        store.agenda(
                            Some(&ids),
                            now - agenda::LOAD_BEHIND,
                            now + agenda::LOAD_AHEAD,
                        )
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                this.agenda_loading = false;
                match loaded {
                    Ok(items) => this.agenda = Arc::new(Agenda::new(items)),
                    Err(err) => tracing::error!("予定の表を読めません: {err:#}"),
                }
                this.refresh_marks();
                this.clock = Self::spawn_clock(cx);
                cx.emit(AppEvent::AgendaChanged);
                cx.notify();
                if std::mem::take(&mut this.agenda_dirty) {
                    this.reload_agenda(cx);
                }
            });
        });
    }

    fn refresh_marks(&mut self) {
        let now = Utc::now();
        self.marks = self.agenda.marks(now, &Days::at(now, &Local), &Local);
    }

    /// 予定の状態が変わる次の時刻 (開始・終了など) か日付の変わり目を待ち、来たら予定の表を読み直す。
    /// 待った時間より大きく時刻が進んでいたら (スリープから戻った)、そのときも読み直す。
    fn spawn_clock(cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let started = Utc::now();
            let Ok(next) = this.update(cx, |this, _| this.agenda.next_boundary(started)) else {
                return;
            };
            let mut last = started;
            loop {
                let now = Utc::now();
                let wait = match next {
                    Some(next) if next <= now => CLOCK_MIN_WAIT,
                    Some(next) => (next - now).to_std().unwrap_or(CLOCK_MAX_WAIT),
                    None => CLOCK_MAX_WAIT,
                }
                .clamp(CLOCK_MIN_WAIT, CLOCK_MAX_WAIT);
                cx.background_executor().timer(wait).await;
                let now = Utc::now();
                let crossed = next.is_some_and(|next| next <= now);
                let new_day = local_date(now) != local_date(last);
                let resumed =
                    now - last > Duration::from_std(wait).unwrap_or_default() + RESUME_SLACK;
                last = now;
                if crossed || new_day || resumed {
                    // 読み直したら、新しい表で時計を作り直す。
                    let _ = this.update(cx, |this, cx| this.reload_agenda(cx));
                    return;
                }
            }
        })
    }

    /// サービスから使うソースを読み直す。
    fn reload_sources(&mut self, cx: &mut Context<Self>) {
        let enabled_before: Vec<String> = self
            .enabled_sources()
            .map(|source| source.id.clone())
            .collect();
        self.sources = self.services.sources();
        self.catalog = self.services.catalog();
        self.catalog_status = self.services.catalog_status();
        self.subscriptions = self.services.subscriptions();
        self.local_files = self.services.local_files();
        let enabled_after: Vec<String> = self
            .enabled_sources()
            .map(|source| source.id.clone())
            .collect();
        if enabled_before != enabled_after {
            // on/off が変わると、一覧に出す記事と予定の表が変わる。
            cx.emit(AppEvent::EntriesChanged);
            self.reload_agenda(cx);
        }
        self.prune_icons(cx);
        cx.notify();
    }

    pub fn handle_service_event(&mut self, event: ServiceEvent, cx: &mut Context<Self>) {
        match event {
            ServiceEvent::FetchStarted { source_id } => {
                self.running.insert(source_id);
                cx.notify();
            }
            ServiceEvent::FetchFinished(summary) => {
                self.running.remove(&summary.source_id);
                if summary.changed() {
                    cx.emit(AppEvent::EntriesChanged);
                    // 中身が変わった・消えた記事の古い予定は、表から外れている。
                    self.reload_agenda(cx);
                }
                self.refresh_counts(cx);
            }
            ServiceEvent::CycleFinished { added, pruned } => {
                self.last_cycle_at = Some(Utc::now());
                self.running = self.services.running();
                if pruned > 0 {
                    cx.emit(AppEvent::EntriesChanged);
                    self.reload_agenda(cx);
                }
                if !added.is_empty() {
                    cx.emit(AppEvent::NewEntries(added));
                }
                self.refresh_counts(cx);
            }
            ServiceEvent::ScheduleChanged { sources } => {
                // 画面に出していない (off の) ソースだけなら、読み直さなくてよい。
                let shown = self.enabled_ids(Channel::News);
                if sources.iter().any(|id| shown.contains(id)) {
                    self.reload_agenda(cx);
                }
            }
            ServiceEvent::SourcesChanged => self.reload_sources(cx),
            // 画像の読み直しと、ウィンドウの出し入れ・終了は app.rs が受け持つ。
            ServiceEvent::ImagesCached { .. }
            | ServiceEvent::RetryImages(_)
            | ServiceEvent::ImagesDiscarded(_)
            | ServiceEvent::ShowWindow
            | ServiceEvent::Quit => {}
        }
    }

    pub fn update_settings(&mut self, settings: AppSettings, cx: &mut Context<Self>) -> Result<()> {
        self.services.save_settings(settings.clone())?;
        self.settings = settings;
        cx.notify();
        Ok(())
    }

    /// ソースを on/off する。
    pub fn set_enabled(&mut self, id: &str, enabled: bool, cx: &mut Context<Self>) -> Result<()> {
        self.services
            .update_subscriptions(|subscriptions| subscriptions.set_enabled(id, enabled))?;
        self.reload_sources(cx);
        Ok(())
    }

    /// ソースのアイコンを変える。`None` で外す。
    pub fn set_icon(
        &mut self,
        id: &str,
        icon: Option<String>,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let enabled = self.resolved(id).is_some_and(|source| source.enabled);
        self.services.update_subscriptions(|subscriptions| {
            let existed = subscriptions.get(id).is_some();
            let entry = subscriptions.entry(id);
            entry.icon = icon;
            // アイコンのために足した購読で、off のソースを on にしない。
            if !existed {
                entry.enabled = enabled;
            }
        })?;
        self.reload_sources(cx);
        Ok(())
    }

    /// 手元のネットワーク (localhost・LAN) への取得を許す。手元の定義だけ。
    pub fn set_allow_private_network(
        &mut self,
        id: &str,
        allow: bool,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        if self
            .resolved(id)
            .is_some_and(|source| source.origin == SourceOrigin::Catalog)
        {
            bail!("カタログのソースは手元のネットワークに取得できません");
        }
        self.services.update_subscriptions(|subscriptions| {
            subscriptions.entry(id).allow_private_network = allow;
        })?;
        self.reload_sources(cx);
        Ok(())
    }

    /// 手元の定義を保存する。`is_new` なら on にする。
    pub fn save_local_source(
        &mut self,
        id: &str,
        text: &str,
        is_new: bool,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        self.services.save_local_source(id, text, is_new)?;
        self.reload_sources(cx);
        Ok(())
    }

    /// カタログのソースを、手元で直せるよう写す (上書き)。on/off とアイコンはそのまま。
    pub fn copy_to_local(&mut self, id: &str, cx: &mut Context<Self>) -> Result<()> {
        let Some(entry) = self.catalog.get(id) else {
            bail!("カタログに `{id}` がありません");
        };
        // カタログの中の相対パスの Schema の指定は、手元のファイルでは届かないので URL にする。
        let text: String = entry
            .text
            .lines()
            .map(|line| {
                if line.trim_start().starts_with("# yaml-language-server:") {
                    format!(
                        "# yaml-language-server: $schema={}",
                        gnn_core::config::SCHEMA_URL
                    )
                } else {
                    line.to_string()
                }
            })
            .collect::<Vec<_>>()
            .join("\n")
            + "\n";
        self.services.save_local_source(id, &text, false)?;
        self.reload_sources(cx);
        Ok(())
    }

    /// 手元の定義を消す。カタログの上書きならカタログの定義に戻し、手元だけのソースなら
    /// その記事も消す。
    ///
    /// `delete_entries` は利用者が記事も消すと確かめたか。確かめていなければ、手元だけの
    /// ソースになっていても記事は消さない (確認のあとでカタログが変わることがあるため)。
    pub fn delete_local_source(
        &mut self,
        id: &str,
        delete_entries: bool,
        cx: &mut Context<Self>,
    ) -> Result<()> {
        let local_only = self.catalog.get(id).is_none();
        self.services.delete_local_source(id)?;
        self.reload_sources(cx);
        if local_only && delete_entries {
            let services = self.services.clone();
            let id = id.to_string();
            cx.spawn(async move |this, cx| {
                let result = cx
                    .background_spawn(async move { services.delete_entries_of(&id) })
                    .await;
                if let Err(err) = result {
                    tracing::error!("ソースの記事を消せません: {err:#}");
                }
                let _ = this.update(cx, |this, cx| this.entries_changed(cx));
            })
            .detach();
        }
        Ok(())
    }

    /// カタログを今すぐ取り込む。
    pub fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        self.catalog_status.checking = true;
        self.services.refresh_catalog();
        cx.notify();
    }

    /// どのソースからも使われなくなったアイコンの画像を消す。
    fn prune_icons(&self, cx: &mut Context<Self>) {
        let icons_dir = self.services.paths().icons_dir();
        let mut in_use = self.subscriptions.icons();
        in_use.extend(
            self.local_files
                .iter()
                .filter_map(|file| file.source.as_ref().ok())
                .filter_map(|source| source.icon.clone()),
        );
        cx.background_executor()
            .spawn(async move {
                if let Err(err) = crate::icons::prune(&icons_dir, &in_use) {
                    tracing::warn!("使っていないアイコンを消せません: {err:#}");
                }
            })
            .detach();
    }

    /// 今すぐ取得する。`None` は on のすべて。
    pub fn fetch_now(&mut self, ids: Option<Vec<String>>, cx: &mut Context<Self>) {
        let targets: Vec<String> = match &ids {
            Some(ids) => ids.clone(),
            None => self
                .enabled_sources()
                .map(|source| source.id.clone())
                .collect(),
        };
        self.running.extend(targets);
        self.services.fetch_now(ids);
        cx.notify();
    }

    /// 表示の地域時刻の今日と明日。
    pub fn days(now: DateTime<Utc>) -> Days {
        Days::at(now, &Local)
    }

    /// 新しいソース ID を作る。`base` が空いていればそのまま使う。
    /// カタログと手元の定義の両方と重ならないようにする。
    pub fn unique_source_id(&self, base: &str) -> String {
        let taken = |id: &str| {
            self.catalog.get(id).is_some()
                || self.local_file(id).is_some()
                || self.resolved(id).is_some()
        };
        if !taken(base) {
            return base.to_string();
        }
        (2..)
            .map(|n| format!("{base}_{n}"))
            .find(|candidate| !taken(candidate))
            .expect("空いている ID")
    }
}

/// 表示の地域時刻の暦日。
fn local_date(at: DateTime<Utc>) -> NaiveDate {
    at.with_timezone(&Local).date_naive()
}
