//! お知らせの一覧と本文。
//!
//! 左に記事の一覧、右に本文 ([`ArticleView`]) を出す (狭いときはどちらか一方)。一覧の行は
//! 「ゲームと日時」「題名 (2 行まで)」「予定のチップ (代表 1 つと残りの数)」の 3 段で、高さは中身で
//! 変わる (gpui の `list`)。未読は題名の太字とローズの点、選んだ記事は淡いローズの背景と左の線で示す。
//! 見出しの「予定」のメニューで、今日・明日に関係する・始まる・終わる予定のある記事に絞る
//! (`agenda::ScheduleFilter`)。同じメニューの「今日・明日の予定がある記事を上にまとめる」を入れると、
//! 絞っていないときに、今日・明日の予定がある記事を見出しの付いたまとまりで上に出す。
//!
//! 取得で一覧が変わっても、見ている行は動かさない (先頭に見えていた記事の位置を保つ)。見ている位置より
//! 上に新しい記事が入ったら「新しいお知らせが N 件あります」を出す。

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::time::Duration;

use chrono::{Local, Utc};
use gnn_core::agenda::{Day, Priority, ScheduleFilter};
use gnn_core::source::Channel;
use gnn_core::text::find_matches;
use gnn_core::{EntryQuery, EntrySummary};
use gpui_kit::assets::IconName;
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    ActiveTheme as _, Icon, Selectable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, FocusHandle, Focusable,
    HighlightStyle, InteractiveElement as _, IntoElement, ListAlignment, ListOffset, ListState,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    StyledText, Subscription, Task, Window, div, list, px,
};

use crate::format;
use crate::state::{AppEvent, AppState};
use crate::ui::Layout;
use crate::ui::actions::{
    FocusSearch, MarkAllRead, OpenOriginal, SelectNext, SelectPrev, ToggleRead,
};
use crate::ui::article::{ArticleEvent, ArticleView};
use crate::ui::components::{EmptyState, MoreChip, ScheduleChip, SourceAvatar, segmented};
use crate::ui::theme::Palette;

const SEARCH_DELAY: Duration = Duration::from_millis(250);
/// 一覧に読み込む記事の上限 (新しい順)。今日・明日の予定がある記事は、この外にあっても足す。
const LIST_LIMIT: usize = 5000;
/// まだ測っていない行の高さのめやす (スクロールバーの大きさに使う)。
const ROW_HEIGHT_HINT: f32 = 76.;

/// 「予定」のメニューの絞り込み (メニューの文言と、選んだときのボタンの文言)。規則は
/// `gnn_core::agenda::ScheduleFilter`。
const SCHEDULE_FILTERS: [(ScheduleFilter, &str, &str); 6] = [
    (
        ScheduleFilter::Related(Day::Today),
        "今日に関係する予定",
        "今日に関係",
    ),
    (
        ScheduleFilter::Related(Day::Tomorrow),
        "明日に関係する予定",
        "明日に関係",
    ),
    (
        ScheduleFilter::Starts(Day::Today),
        "今日始まる予定",
        "今日始まる",
    ),
    (
        ScheduleFilter::Starts(Day::Tomorrow),
        "明日始まる予定",
        "明日始まる",
    ),
    (
        ScheduleFilter::Ends(Day::Today),
        "今日終わる予定",
        "今日終わる",
    ),
    (
        ScheduleFilter::Ends(Day::Tomorrow),
        "明日終わる予定",
        "明日終わる",
    ),
];

/// 絞り込みの文言 (メニュー・ボタン)。
fn schedule_labels(filter: ScheduleFilter) -> (&'static str, &'static str) {
    SCHEDULE_FILTERS
        .iter()
        .find(|(candidate, _, _)| *candidate == filter)
        .map(|(_, menu, button)| (*menu, *button))
        .unwrap_or(("予定", "予定"))
}

/// 一覧の 1 行。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Row {
    /// まとまりの見出し (予定のある記事を上にまとめるとき)。選べない。
    Header(Priority),
    /// 記事 (`entries` の位置)。
    Entry(usize),
}

/// 一覧の行を作り直しても見ている位置を保つための、先頭に見えていた行の目印。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Anchor {
    Header(Priority),
    Entry(i64),
}

pub struct NewsPage {
    app: Entity<AppState>,
    article: Entity<ArticleView>,
    filter: Option<String>,
    search: Entity<InputState>,
    query: String,
    unread_only: bool,
    /// 予定で絞る条件 (「今日始まる」など)。`None` は絞らない。
    schedule_filter: Option<ScheduleFilter>,
    entries: Vec<EntrySummary>,
    rows: Vec<Row>,
    /// まとまりごとの記事の数 (見出しに出す)。
    group_counts: HashMap<Priority, usize>,
    selected: Option<i64>,
    /// 本文を読み込んだら既読にする記事。
    read_on_open: Option<i64>,
    /// 狭いときに本文を出しているか。
    showing_article: bool,
    /// 見ている位置より上に入った新しい記事の数。
    new_above: usize,
    /// 次の読み込みのあとで、選んだ記事が見えるところまでスクロールする (通知から開いたとき・開き直したとき)。
    reveal_after_load: bool,
    loaded: bool,
    list_focus: FocusHandle,
    list_state: ListState,
    load_task: Task<()>,
    search_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl NewsPage {
    pub fn new(app: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder(search_placeholder(None)));
        let article = cx.new(|cx| ArticleView::new(app.clone(), cx));
        let subscriptions = vec![
            cx.subscribe_in(&search, window, |this, state, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let query = state.read(cx).value().to_string();
                    this.schedule_search(query, cx);
                }
            }),
            cx.subscribe(&app, |this, _, event, cx| match event {
                // 予定の表が変わった・予定の状態が変わる時刻が来たときも、上限の外の予定のある記事と
                // 並びを決め直すために読み直す。
                AppEvent::EntriesChanged | AppEvent::AgendaChanged => this.reload(cx),
                AppEvent::ReadChanged { ids, read } => {
                    // 未読だけの一覧でも、読んだ記事は次に読み直すまで残す (見ている行を消さない)。
                    for id in ids {
                        this.set_read_local(*id, *read, cx);
                    }
                }
                AppEvent::NewEntries(_) => {}
            }),
            cx.observe(&app, |_, _, cx| cx.notify()),
            cx.subscribe(&article, |this, _, event, cx| match event {
                ArticleEvent::Opened(id) => {
                    // 本文を出せてから既読にする (自動で先頭を出しただけでは既読にしない)。
                    if this.read_on_open == Some(*id) {
                        this.read_on_open = None;
                        this.set_read(*id, true, cx);
                    }
                }
                ArticleEvent::Back => {
                    this.showing_article = false;
                    cx.notify();
                }
            }),
        ];
        let mut page = Self {
            app,
            article,
            filter: None,
            search,
            query: String::new(),
            unread_only: false,
            schedule_filter: None,
            entries: Vec::new(),
            rows: Vec::new(),
            group_counts: HashMap::new(),
            selected: None,
            read_on_open: None,
            showing_article: false,
            new_above: 0,
            reveal_after_load: false,
            loaded: false,
            list_focus: cx.focus_handle(),
            list_state: ListState::new(0, ListAlignment::Top, px(600.)),
            load_task: Task::ready(()),
            search_task: Task::ready(()),
            _subscriptions: subscriptions,
        };
        page.reload(cx);
        page
    }

    pub fn filter(&self) -> Option<&str> {
        self.filter.as_deref()
    }

    /// 表示するソースを絞る。`None` はすべて。
    pub fn set_filter(
        &mut self,
        filter: Option<String>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.filter != filter {
            let remembered = filter.clone();
            self.app.update(cx, |app, _| {
                app.remember_view(|view| view.news_filter = remembered)
            });
            self.filter = filter;
            self.new_above = 0;
            self.showing_article = false;
            self.list_state.scroll_to(ListOffset::default());
            let placeholder = search_placeholder(self.filter.as_deref());
            self.search.update(cx, |search, cx| {
                search.set_placeholder(placeholder, window, cx)
            });
            self.reload(cx);
        }
    }

    /// 開き直したウィンドウで、前に開いていた記事を選ぶ (既読にはしない)。
    pub fn restore(
        &mut self,
        filter: Option<String>,
        schedule: Option<ScheduleFilter>,
        selected: Option<i64>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.set_filter(filter, window, cx);
        self.set_schedule_filter(schedule, cx);
        if let Some(id) = selected {
            self.selected = Some(id);
            self.article.update(cx, |article, cx| article.show(id, cx));
            self.reveal_after_load = true;
            self.reload(cx);
        }
    }

    /// 記事を開く。通知から呼ばれるので、絞り込みと検索を外してから選ぶ。
    pub fn reveal(&mut self, entry_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.set_filter(None, window, cx);
        self.set_schedule_filter(None, cx);
        self.unread_only = false;
        self.query.clear();
        self.search_task = Task::ready(());
        self.search
            .update(cx, |search, cx| search.set_value("", window, cx));
        self.article
            .update(cx, |article, cx| article.set_query("", cx));
        self.selected = Some(entry_id);
        self.reveal_after_load = true;
        self.reload(cx);
        self.select(entry_id, true, cx);
        self.showing_article = true;
    }

    fn schedule_search(&mut self, query: String, cx: &mut Context<Self>) {
        self.search_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                if this.query != query {
                    this.query = query;
                    this.article
                        .update(cx, |article, cx| article.set_query(&this.query, cx));
                    this.new_above = 0;
                    this.list_state.scroll_to(ListOffset::default());
                    this.reload(cx);
                }
            });
        });
    }

    /// 予定で絞る (`None` は絞らない)。予定の表と今の時刻から、合う記事を読み込みのたびに決め直す。
    pub fn set_schedule_filter(&mut self, filter: Option<ScheduleFilter>, cx: &mut Context<Self>) {
        if self.schedule_filter == filter {
            return;
        }
        self.schedule_filter = filter;
        self.app.update(cx, |app, _| {
            app.remember_view(|view| view.news_schedule = filter)
        });
        self.new_above = 0;
        self.list_state.scroll_to(ListOffset::default());
        self.reload(cx);
    }

    /// 今の絞り込み (ソース・検索・未読・予定) の条件。
    fn entry_query(&self, cx: &App) -> EntryQuery {
        let app = self.app.read(cx);
        let ids = self.schedule_filter.map(|filter| {
            let now = Utc::now();
            app.agenda()
                .filter_entries(filter, now, &AppState::days(now))
        });
        EntryQuery {
            channel: Channel::News,
            source_ids: Some(match &self.filter {
                Some(id) => vec![id.clone()],
                None => app.enabled_ids(Channel::News),
            }),
            search: (!self.query.trim().is_empty()).then(|| self.query.clone()),
            unread_only: self.unread_only,
            limit: ids.as_ref().map_or(LIST_LIMIT, Vec::len),
            ids,
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let app = self.app.read(cx);
        let services = app.services().clone();
        // 今日・明日の予定がある記事は、新しい順の上限の外にあっても一覧に足す
        // (1 か月前の告知に書かれた明日のメンテナンスなど)。予定で絞っているときは、絞った記事だけを出す。
        let prioritized = if app.settings().prioritize_today && self.schedule_filter.is_none() {
            let now = Utc::now();
            app.agenda().prioritized(now, &AppState::days(now))
        } else {
            Vec::new()
        };
        let query = self.entry_query(cx);
        self.load_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    services.with_store(|store| -> anyhow::Result<Vec<EntrySummary>> {
                        let mut entries = store.query(&query)?;
                        let listed: HashSet<i64> = entries.iter().map(|entry| entry.id).collect();
                        let missing: Vec<i64> = prioritized
                            .into_iter()
                            .filter(|id| !listed.contains(id))
                            .collect();
                        if !missing.is_empty() {
                            // 絞り込み (ソース・検索・未読) は同じものを使う。
                            let extra = EntryQuery {
                                limit: missing.len(),
                                ids: Some(missing),
                                ..query
                            };
                            entries.extend(store.query(&extra)?);
                        }
                        Ok(entries)
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(entries) => this.apply_entries(entries, cx),
                    Err(err) => tracing::error!("記事を読めません: {err:#}"),
                }
                this.loaded = true;
                let still_listed = this
                    .selected
                    .is_some_and(|id| this.entries.iter().any(|entry| entry.id == id));
                if !still_listed {
                    // 見ていた記事が一覧から消えたら先頭を出す。既読にはしない。
                    match this.first_entry() {
                        Some(first) => this.open(first, false, cx),
                        None => {
                            this.selected = None;
                            this.article.update(cx, |article, cx| article.clear(cx));
                        }
                    }
                } else {
                    // 開いている記事に追記が入ったかもしれないので、本文も読み直す。
                    this.article.update(cx, |article, cx| article.refresh(cx));
                }
                if std::mem::take(&mut this.reveal_after_load)
                    && let Some(position) = this.selected.and_then(|id| this.row_of(id))
                {
                    this.list_state.scroll_to_reveal_item(position);
                }
                cx.notify();
            });
        });
    }

    fn first_entry(&self) -> Option<i64> {
        self.rows.iter().find_map(|row| match row {
            Row::Entry(index) => Some(self.entries[*index].id),
            Row::Header(_) => None,
        })
    }

    /// 読み込んだ記事で一覧を作り直す。先頭に見えていた行の位置を保ち、それより上に増えた記事を数える。
    fn apply_entries(&mut self, entries: Vec<EntrySummary>, cx: &App) {
        let top = self.list_state.logical_scroll_top();
        let anchor = self.rows.get(top.item_ix).map(|row| match row {
            Row::Header(priority) => Anchor::Header(*priority),
            Row::Entry(index) => Anchor::Entry(self.entries[*index].id),
        });
        let at_top = top.item_ix == 0 && top.offset_in_item <= px(0.);
        let known: HashSet<i64> = self.entries.iter().map(|entry| entry.id).collect();
        let had_entries = self.loaded && !self.entries.is_empty();

        self.entries = entries;
        self.rebuild_rows(cx);

        let anchor_ix = anchor.and_then(|anchor| {
            self.rows.iter().position(|row| match (row, anchor) {
                (Row::Header(priority), Anchor::Header(wanted)) => *priority == wanted,
                (Row::Entry(index), Anchor::Entry(id)) => self.entries[*index].id == id,
                _ => false,
            })
        });
        self.list_state
            .reset_with_uniform_height(self.rows.len(), px(ROW_HEIGHT_HINT));
        match anchor_ix {
            Some(ix) if !at_top => {
                let added_above = self.rows[..ix]
                    .iter()
                    .filter(|row| match row {
                        Row::Entry(index) => !known.contains(&self.entries[*index].id),
                        Row::Header(_) => false,
                    })
                    .count();
                if had_entries {
                    self.new_above += added_above;
                }
                self.list_state.scroll_to(ListOffset {
                    item_ix: ix,
                    offset_in_item: top.offset_in_item,
                });
            }
            _ => {
                self.new_above = 0;
                self.list_state.scroll_to(ListOffset::default());
            }
        }
    }

    /// 並びを決め直す。今日・明日の予定は、予定の表と今の時刻から決める (メンテナンスは今日・明日に
    /// 重なる時間帯、イベントなどの期間は始まりと終わりの節目)。予定で絞っているときは、一覧が
    /// もう予定のある記事だけなので、まとまりに分けない (見出しが絞り込みの言い直しになる)。
    fn rebuild_rows(&mut self, cx: &App) {
        let app = self.app.read(cx);
        let prioritize = app.settings().prioritize_today && self.schedule_filter.is_none();
        let agenda = app.agenda().clone();
        let now = Utc::now();
        let days = AppState::days(now);
        let mut ordered: Vec<(Priority, usize)> = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let priority = if prioritize {
                    agenda.priority(entry.id, now, &days)
                } else {
                    Priority::Normal
                };
                (priority, index)
            })
            .collect();
        // 記事はすでに新しい順なので、安定ソートで優先度だけを前に出す。
        ordered.sort_by_key(|(priority, _)| *priority);
        self.group_counts.clear();
        for (priority, _) in &ordered {
            *self.group_counts.entry(*priority).or_default() += 1;
        }
        let grouped = prioritize
            && ordered
                .iter()
                .any(|(priority, _)| *priority != Priority::Normal);
        let mut rows = Vec::with_capacity(ordered.len() + 3);
        let mut current = None;
        for (priority, index) in ordered {
            if grouped && current != Some(priority) {
                rows.push(Row::Header(priority));
                current = Some(priority);
            }
            rows.push(Row::Entry(index));
        }
        self.rows = rows;
    }

    fn row_of(&self, id: i64) -> Option<usize> {
        self.rows.iter().position(|row| match row {
            Row::Entry(index) => self.entries[*index].id == id,
            Row::Header(_) => false,
        })
    }

    /// 利用者が選んだ記事を開く (見えるところまでスクロールする)。`mark_read` なら、本文を出せたら既読にする。
    fn select(&mut self, id: i64, mark_read: bool, cx: &mut Context<Self>) {
        if let Some(position) = self.row_of(id) {
            self.list_state.scroll_to_reveal_item(position);
        }
        self.open(id, mark_read, cx);
    }

    /// 記事を本文に出す。一覧を読み直して選んでいた記事が消えたときの自動の選択は、これだけを使う
    /// (見ている位置を動かさない)。
    fn open(&mut self, id: i64, mark_read: bool, cx: &mut Context<Self>) {
        self.selected = Some(id);
        self.read_on_open = mark_read.then_some(id);
        self.article.update(cx, |article, cx| article.show(id, cx));
        self.app.update(cx, |app, _| {
            app.remember_view(|view| view.news_selected = Some(id))
        });
        cx.notify();
    }

    /// 記事の既読を変える (すべての画面に知らせ、ストアにも書く)。
    fn set_read(&mut self, id: i64, read: bool, cx: &mut Context<Self>) {
        let changed = self
            .entries
            .iter()
            .find(|entry| entry.id == id)
            .is_none_or(|entry| entry.read != read);
        if changed {
            self.app
                .update(cx, |app, cx| app.set_read(vec![id], read, cx));
        }
    }

    /// 一覧の行の既読だけを変える。
    fn set_read_local(&mut self, id: i64, read: bool, cx: &mut Context<Self>) {
        if let Some(entry) = self
            .entries
            .iter_mut()
            .find(|entry| entry.id == id && entry.read != read)
        {
            entry.read = read;
            cx.notify();
        }
    }

    /// 選べる行の位置 (見出しを除く)。
    fn entry_positions(&self) -> Vec<usize> {
        self.rows
            .iter()
            .enumerate()
            .filter_map(|(position, row)| matches!(row, Row::Entry(_)).then_some(position))
            .collect()
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        let positions = self.entry_positions();
        if positions.is_empty() {
            return;
        }
        let current = self
            .selected
            .and_then(|id| self.row_of(id))
            .and_then(|row| positions.iter().position(|position| *position == row));
        let next = match current {
            Some(current) => {
                (current as isize + delta).clamp(0, positions.len() as isize - 1) as usize
            }
            None => 0,
        };
        let Row::Entry(index) = self.rows[positions[next]] else {
            return;
        };
        let id = self.entries[index].id;
        self.select(id, true, cx);
    }

    fn on_select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(1, cx);
    }

    fn on_select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(-1, cx);
    }

    fn on_open_original(&mut self, _: &OpenOriginal, _: &mut Window, cx: &mut Context<Self>) {
        self.article
            .update(cx, |article, cx| article.open_original(cx));
    }

    fn on_toggle_read(&mut self, _: &ToggleRead, _: &mut Window, cx: &mut Context<Self>) {
        self.article
            .update(cx, |article, cx| article.toggle_read(cx));
    }

    fn on_mark_all_read(&mut self, _: &MarkAllRead, window: &mut Window, cx: &mut Context<Self>) {
        self.mark_all_read(window, cx);
    }

    fn on_focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.showing_article = false;
        self.search
            .update(cx, |search, cx| search.focus(window, cx));
    }

    /// 表示中のゲームを今すぐ取得する。
    fn fetch_filtered(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.filter.clone() {
            self.app
                .update(cx, |app, cx| app.fetch_now(Some(vec![id]), cx));
        }
    }

    /// 一覧の条件 (ゲーム・検索・未読) に合う未読の記事を、読み込んだ件数によらずすべて既読にする。
    /// 「元に戻す」で、既読にした記事だけを未読に戻せる。
    fn mark_all_read(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let query = self.entry_query(cx);
        let app = self.app.clone();
        let services = app.read(cx).services().clone();
        cx.spawn_in(window, async move |_, cx| {
            let store = services.clone();
            let result = cx
                .background_executor()
                .spawn(async move {
                    store.with_store(|store| store.mark_read_matching(&query, Utc::now()))
                })
                .await;
            let _ = cx.update(|window, cx| {
                let ids = match result {
                    Ok(ids) => ids,
                    Err(err) => {
                        tracing::error!("既読にできません: {err:#}");
                        window.push_notification(
                            Notification::error(format!("既読にできません: {err:#}")),
                            cx,
                        );
                        return;
                    }
                };
                app.update(cx, |app, cx| app.read_changed(ids.clone(), true, cx));
                if ids.is_empty() {
                    return;
                }
                let count = ids.len();
                let note = Notification::success(format!("{count} 件を既読にしました")).action(
                    move |_, _, cx| {
                        let ids = ids.clone();
                        let services = services.clone();
                        let app = app.clone();
                        Button::new("undo-mark-read")
                            .xsmall()
                            .outline()
                            .label("元に戻す")
                            .on_click(cx.listener(move |note, _, window, cx| {
                                note.dismiss(window, cx);
                                let ids = ids.clone();
                                let services = services.clone();
                                let app = app.clone();
                                cx.spawn_in(window, async move |_, cx| {
                                    let unread = ids.clone();
                                    let result = cx
                                        .background_executor()
                                        .spawn(async move {
                                            services
                                                .with_store(|store| store.mark_unread_ids(&unread))
                                        })
                                        .await;
                                    let _ = cx.update(|window, cx| match result {
                                        // ストアで戻せたときだけ、画面も未読に戻す。
                                        Ok(_) => app
                                            .update(cx, |app, cx| app.read_changed(ids, false, cx)),
                                        Err(err) => {
                                            tracing::error!("未読に戻せません: {err:#}");
                                            window.push_notification(
                                                Notification::error(format!(
                                                    "未読に戻せません: {err:#}"
                                                )),
                                                cx,
                                            );
                                        }
                                    });
                                })
                                .detach();
                            }))
                    },
                );
                window.push_notification(note, cx);
            });
        })
        .detach();
    }

    fn toggle_prioritize(&mut self, cx: &mut Context<Self>) {
        let mut settings = self.app.read(cx).settings().clone();
        settings.prioritize_today = !settings.prioritize_today;
        let result = self
            .app
            .update(cx, |app, cx| app.update_settings(settings, cx));
        if let Err(err) = result {
            tracing::error!("設定を保存できません: {err:#}");
        }
        self.new_above = 0;
        self.list_state.scroll_to(ListOffset::default());
        // 上限の外にある今日・明日の予定の記事も読み込む (外したときは除く)。
        self.reload(cx);
    }

    fn set_unread_only(&mut self, unread_only: bool, cx: &mut Context<Self>) {
        if self.unread_only != unread_only {
            self.unread_only = unread_only;
            self.new_above = 0;
            self.list_state.scroll_to(ListOffset::default());
            self.reload(cx);
        }
    }

    fn highlights(&self, text: &str, cx: &App) -> Vec<(Range<usize>, HighlightStyle)> {
        let style = HighlightStyle {
            background_color: Some(cx.theme().warning.opacity(0.35)),
            ..HighlightStyle::default()
        };
        find_matches(text, &self.query)
            .into_iter()
            .map(|range| (range, style))
            .collect()
    }

    /// 一覧の条件に合う未読の数 (「この一覧を既読にする」の件数)。
    fn unread_in_view(&self, cx: &App) -> usize {
        let app = self.app.read(cx);
        if self.query.trim().is_empty() && self.schedule_filter.is_none() {
            match &self.filter {
                Some(id) => app.unread(Channel::News, id),
                None => app.unread_total(Channel::News),
            }
        } else {
            self.entries.iter().filter(|entry| !entry.read).count()
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let app = self.app.read(cx);
        let source = self.filter.as_deref().and_then(|id| app.source(id));
        let title: SharedString = match source {
            Some(source) => source.name.clone().into(),
            None => "すべてのお知らせ".into(),
        };
        let unread = match &self.filter {
            Some(id) => app.unread(Channel::News, id),
            None => app.unread_total(Channel::News),
        };
        let running = match &self.filter {
            Some(id) => app.is_running(id),
            None => app.running_count() > 0,
        };
        let in_view = self.unread_in_view(cx);
        let filtered = self.filter.is_some();
        let prioritize = app.settings().prioritize_today;
        let schedule = self.schedule_filter;
        // 予定で絞っているときは、絞った記事の数を出す。
        let subtitle = match schedule {
            Some(filter) => format!(
                "{} · {} 件 (未読 {} 件)",
                schedule_labels(filter).1,
                self.entries.len(),
                self.entries.iter().filter(|entry| !entry.read).count()
            ),
            None if unread > 0 => format!("未読 {unread} 件"),
            None => "未読はありません".to_string(),
        };
        let page = cx.entity().downgrade();
        let schedule_page = page.clone();

        v_flex()
            .flex_none()
            .gap_2()
            .px_4()
            .pt_3()
            .pb_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                h_flex()
                    .gap_2()
                    .when_some(source, |this, source| {
                        this.child(
                            SourceAvatar::new(source.name.clone(), source.icon.clone()).small(),
                        )
                    })
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_base().font_semibold().truncate().child(title))
                            .child(
                                div()
                                    .text_xs()
                                    .truncate()
                                    .text_color(theme.muted_foreground)
                                    .child(subtitle),
                            ),
                    )
                    .when(filtered, |this| {
                        this.child(
                            Button::new("news-fetch-filtered")
                                .small()
                                .ghost()
                                .icon(IconName::RefreshCw)
                                .loading(running)
                                .tooltip("このゲームを今すぐ取得")
                                .on_click(cx.listener(|this, _, _, cx| this.fetch_filtered(cx))),
                        )
                    })
                    .child(
                        Button::new("news-more")
                            .small()
                            .ghost()
                            .icon(IconName::Ellipsis)
                            .tooltip("そのほかの操作")
                            .dropdown_menu(move |menu, _, _| {
                                let page = page.clone();
                                menu.item(
                                    PopupMenuItem::new(format!(
                                        "この一覧を既読にする ({in_view} 件)"
                                    ))
                                    .icon(IconName::CheckCheck)
                                    .disabled(in_view == 0)
                                    .on_click(
                                        move |_, window, cx| {
                                            let _ = page.update(cx, |page, cx| {
                                                page.mark_all_read(window, cx)
                                            });
                                        },
                                    ),
                                )
                            }),
                    ),
            )
            .child(
                Input::new(&self.search)
                    .small()
                    .cleanable(true)
                    .prefix(Icon::new(IconName::Search).small()),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(segmented(
                        "news-read-filter",
                        &["すべて", "未読"],
                        usize::from(self.unread_only),
                        cx.listener(|this, index: &usize, _, cx| {
                            this.set_unread_only(*index == 1, cx)
                        }),
                    ))
                    .child(div().flex_1())
                    .child(
                        // 予定で絞る・予定を先に並べる。絞っているときはボタンを選んだ見た目にし、条件を出す。
                        Button::new("news-schedule")
                            .small()
                            .ghost()
                            .icon(IconName::CalendarClock)
                            .label(match schedule {
                                Some(filter) => schedule_labels(filter).1,
                                None => "予定",
                            })
                            .selected(schedule.is_some())
                            .dropdown_caret(true)
                            .tooltip("予定で絞る・並べる")
                            .dropdown_menu(move |menu, _, _| {
                                let pick = |filter: Option<ScheduleFilter>| {
                                    let page = schedule_page.clone();
                                    move |_: &gpui_kit::ClickEvent, _: &mut Window, cx: &mut App| {
                                        let _ = page.update(cx, |page, cx| {
                                            page.set_schedule_filter(filter, cx)
                                        });
                                    }
                                };
                                let mut menu = menu.min_w(px(240.)).label("予定で絞る").item(
                                    PopupMenuItem::new("絞らない (すべての記事)")
                                        .checked(schedule.is_none())
                                        .on_click(pick(None)),
                                );
                                for (index, (filter, text, _)) in
                                    SCHEDULE_FILTERS.iter().enumerate()
                                {
                                    // 「関係する」と「始まる・終わる」の間に区切りを入れる。
                                    if index == 2 {
                                        menu = menu.separator();
                                    }
                                    menu = menu.item(
                                        PopupMenuItem::new(*text)
                                            .checked(schedule == Some(*filter))
                                            .on_click(pick(Some(*filter))),
                                    );
                                }
                                let page = schedule_page.clone();
                                menu.separator().item(
                                    PopupMenuItem::new("今日・明日の予定がある記事を上にまとめる")
                                        .checked(prioritize)
                                        .on_click(move |_, _, cx| {
                                            let _ = page
                                                .update(cx, |page, cx| page.toggle_prioritize(cx));
                                        }),
                                )
                            }),
                    ),
            )
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        if self.loaded
            && self.rows.is_empty()
            && let Some(filter) = self.schedule_filter
        {
            let (menu, _) = schedule_labels(filter);
            return div()
                .size_full()
                .child(
                    EmptyState::new(IconName::CalendarX, format!("{menu}の記事はありません"))
                        .description(
                            "読み取ったメンテナンスと、イベント・ガチャ・キャンペーンの予定で絞っています。",
                        )
                        .action(
                            Button::new("news-clear-schedule")
                                .small()
                                .outline()
                                .label("予定の絞り込みを外す")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.set_schedule_filter(None, cx)
                                })),
                        ),
                )
                .into_any_element();
        }
        if self.loaded && self.rows.is_empty() {
            let (title, description) = if !self.query.trim().is_empty() {
                ("一致する記事がありません", "検索語を変えてみてください。")
            } else if self.unread_only {
                (
                    "未読の記事はありません",
                    "「すべて」に切り替えると既読の記事も出ます。",
                )
            } else {
                (
                    "記事がまだありません",
                    "取得が終わるとここに並びます。ソース画面で取得するゲームを選べます。",
                )
            };
            return div()
                .size_full()
                .child(EmptyState::new(IconName::Inbox, title).description(description))
                .into_any_element();
        }

        div()
            .id("news-list-frame")
            .relative()
            .size_full()
            .child(
                list(
                    self.list_state.clone(),
                    cx.processor(|this, position: usize, _window, cx| {
                        this.render_row(position, cx)
                    }),
                )
                .size_full(),
            )
            .vertical_scrollbar(&self.list_state)
            .into_any_element()
    }

    fn render_row(&self, position: usize, cx: &mut Context<Self>) -> AnyElement {
        match self.rows.get(position).copied() {
            Some(Row::Header(priority)) => self.render_group_header(priority, cx),
            Some(Row::Entry(index)) => self.render_entry(index, cx),
            None => div().into_any_element(),
        }
    }

    fn render_group_header(&self, priority: Priority, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let count = self.group_counts.get(&priority).copied().unwrap_or(0);
        let (icon, text) = match priority {
            Priority::Today => (
                IconName::CalendarClock,
                format!("今日の予定があるお知らせ  {count} 件"),
            ),
            Priority::Tomorrow => (
                IconName::CalendarDays,
                format!("明日の予定があるお知らせ  {count} 件"),
            ),
            Priority::Normal => (IconName::Newspaper, "そのほかのお知らせ".to_string()),
        };
        h_flex()
            .w_full()
            .h(px(32.))
            .px_4()
            .gap_2()
            .bg(theme.muted.opacity(0.6))
            .border_b_1()
            .border_color(Palette::of(cx).divider)
            .text_xs()
            .font_semibold()
            .text_color(theme.muted_foreground)
            .child(Icon::new(icon).xsmall())
            .child(text)
            .into_any_element()
    }

    fn render_entry(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let entry = &self.entries[index];
        let theme = cx.theme().clone();
        let palette = Palette::of(cx);
        let app = self.app.read(cx);
        let source = app.source(&entry.source_id);
        let name = source.map_or(entry.source_id.clone(), |source| source.name.clone());
        let icon = source.and_then(|source| source.icon.clone());
        let selected = self.selected == Some(entry.id);
        let unread = !entry.read;
        let id = entry.id;
        let title = entry.title.clone();
        let highlights = self.highlights(&title, cx);
        let now = Utc::now();
        let mut badges = app
            .agenda()
            .badges(id, now, &AppState::days(now), &Local)
            .into_iter();
        let first = badges.next();
        let rest: Vec<String> = badges.map(|badge| badge.text).collect();
        let time = format::list_time(entry.published, entry.first_seen_at, now);
        let full_time = SharedString::from(format::full_time(entry.published, entry.first_seen_at));
        let show_source = self.filter.is_none();

        v_flex()
            .id(ElementId::NamedInteger("entry".into(), id as u64))
            .relative()
            .w_full()
            .px_4()
            .py_2p5()
            .gap_1()
            .cursor_pointer()
            .border_b_1()
            .border_color(palette.divider)
            .when(selected, |this| this.bg(theme.list_active))
            .when(!selected, |this| {
                this.hover(|style| style.bg(theme.list_hover))
            })
            .when(selected, |this| {
                this.child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(3.))
                        .bg(theme.primary),
                )
            })
            .child(
                h_flex()
                    .h(px(20.))
                    .gap_1p5()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .when(show_source, |this| {
                        this.child(SourceAvatar::new(name.clone(), icon).xsmall())
                            .child(div().min_w_0().truncate().child(name))
                    })
                    .child(div().flex_1())
                    .child(
                        div()
                            .id(ElementId::NamedInteger("entry-time".into(), id as u64))
                            .flex_none()
                            .child(time)
                            .tooltip(move |window, cx| {
                                Tooltip::new(full_time.clone()).build(window, cx)
                            }),
                    )
                    .child(
                        div()
                            .flex_none()
                            .size(px(8.))
                            .rounded_full()
                            .when(unread, |this| this.bg(theme.primary)),
                    ),
            )
            .child(
                div()
                    .text_sm()
                    .line_height(px(20.))
                    .line_clamp(2)
                    .when(unread, |this| this.font_semibold())
                    .child(StyledText::new(title).with_highlights(highlights)),
            )
            .when_some(first, |this, badge| {
                this.child(
                    h_flex()
                        .pt_0p5()
                        .gap_1()
                        .min_w_0()
                        .child(ScheduleChip::new(
                            ElementId::NamedInteger("entry-chip".into(), id as u64),
                            badge,
                        ))
                        .when(!rest.is_empty(), |this| {
                            this.child(MoreChip::new(
                                ElementId::NamedInteger("entry-more".into(), id as u64),
                                rest,
                            ))
                        }),
                )
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                window.focus(&this.list_focus, cx);
                this.select(id, true, cx);
                this.showing_article = true;
            }))
            .into_any_element()
    }

    fn render_new_above(&self, cx: &mut Context<Self>) -> Option<impl IntoElement> {
        (self.new_above > 0).then(|| {
            let theme = cx.theme();
            h_flex()
                .flex_none()
                .gap_2()
                .px_4()
                .py_1p5()
                .border_b_1()
                .border_color(theme.border)
                .bg(Palette::of(cx).selected)
                .text_sm()
                .text_color(Palette::of(cx).selected_foreground)
                .child(Icon::new(IconName::ArrowUp).small())
                .child(
                    div()
                        .flex_1()
                        .child(format!("新しいお知らせが {} 件あります", self.new_above)),
                )
                .child(
                    Button::new("news-scroll-top")
                        .xsmall()
                        .ghost()
                        .label("先頭へ")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.new_above = 0;
                            this.list_state.scroll_to(ListOffset::default());
                            cx.notify();
                        })),
                )
        })
    }

    fn render_list_pane(&self, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .child(self.render_header(cx))
            .children(self.render_new_above(cx))
            .child(
                div()
                    .id("news-list")
                    .key_context("NewsList")
                    .track_focus(&self.list_focus)
                    .on_action(cx.listener(Self::on_select_next))
                    .on_action(cx.listener(Self::on_select_prev))
                    .on_action(cx.listener(Self::on_open_original))
                    .on_action(cx.listener(Self::on_toggle_read))
                    .flex_1()
                    .min_h_0()
                    .child(self.render_list(cx)),
            )
    }
}

/// 検索欄の案内。何を探すのかが分かるようにする。
fn search_placeholder(filter: Option<&str>) -> &'static str {
    if filter.is_some() {
        "このゲームのお知らせを検索"
    } else {
        "すべてのお知らせを検索"
    }
}

impl Focusable for NewsPage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.list_focus.clone()
    }
}

impl Render for NewsPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = Layout::of(window);
        let narrow = !layout.side_by_side();
        self.article.update(cx, |article, cx| {
            article.set_back(narrow.then_some("お知らせに戻る"), cx)
        });
        let root = v_flex()
            .size_full()
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(Self::on_mark_all_read));
        if narrow {
            // 狭いときは一覧か本文のどちらか。本文でもキー操作 (J/K・O・M) が効くようにする。
            return if self.showing_article && self.selected.is_some() {
                root.child(
                    div()
                        .id("news-article-only")
                        .key_context("NewsList")
                        .track_focus(&self.list_focus)
                        .on_action(cx.listener(Self::on_select_next))
                        .on_action(cx.listener(Self::on_select_prev))
                        .on_action(cx.listener(Self::on_open_original))
                        .on_action(cx.listener(Self::on_toggle_read))
                        .size_full()
                        .child(self.article.clone()),
                )
                .into_any_element()
            } else {
                root.child(self.render_list_pane(cx)).into_any_element()
            };
        }
        let (initial, min) = match layout {
            Layout::Wide => (px(400.), px(360.)),
            _ => (px(360.), px(340.)),
        };
        root.child(
            h_resizable("news-split")
                .child(
                    resizable_panel()
                        .size(initial)
                        .size_range(min..px(720.))
                        .child(self.render_list_pane(cx)),
                )
                .child(resizable_panel().child(self.article.clone())),
        )
        .into_any_element()
    }
}
