//! お知らせの一覧と本文。
//!
//! 左に記事の一覧 (新しい順。今日・明日の予定を含む記事を上に出せる)、右に本文を出す。
//! 検索語は一覧のタイトルと本文の両方で目立たせる。
//!
//! 予定 (メンテナンスの時間帯と結果、イベントなどの期間) は `AppState` の予定の表から引き、
//! 今の時刻で状態 (メンテ中・終了予定を過ぎた など) を決めてバッジにする。

use std::collections::HashSet;
use std::ops::Range;
use std::time::Duration;

use chrono::{Local, Utc};
use gnn_core::agenda::Priority;
use gnn_core::source::Channel;
use gnn_core::text::{find_matches, mark_html};
use gnn_core::{Entry, EntryQuery, EntrySchedule, EntrySummary};
use gpui_kit::assets::IconName;
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::switch::Switch;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, AppContext as _, Context, ElementId, Entity, FocusHandle, Focusable, FontWeight,
    HighlightStyle, InteractiveElement as _, IntoElement, ParentElement as _, Render,
    ScrollStrategy, SharedString, StatefulInteractiveElement as _, Styled as _, StyledText,
    Subscription, Task, UniformListScrollHandle, Window, div, px, uniform_list,
};

use crate::format;
use crate::http_client::{ImageLoader, display_html, display_uri};
use crate::services::image_sources;
use crate::state::{AppEvent, AppState};
use crate::ui::actions::{
    FocusSearch, MarkAllRead, OpenOriginal, Refresh, SelectNext, SelectPrev, ToggleRead,
};
use crate::ui::components::{EmptyState, ScheduleBadge, SchedulePanel, SourceAvatar, open_web_url};

const ROW_HEIGHT: f32 = 36.;
const SEARCH_DELAY: Duration = Duration::from_millis(250);
/// 一覧に読み込む記事の上限 (新しい順)。今日・明日の予定がある記事は、この外にあっても足す。
const LIST_LIMIT: usize = 5000;

#[derive(Debug, Clone, Copy)]
struct Row {
    index: usize,
    priority: Priority,
    /// 優先する記事のまとまりの最後。下に区切り線を引く。
    group_end: bool,
}

pub struct NewsPage {
    app: Entity<AppState>,
    filter: Option<String>,
    search: Entity<InputState>,
    query: String,
    unread_only: bool,
    entries: Vec<EntrySummary>,
    rows: Vec<Row>,
    selected: Option<i64>,
    detail: Option<Entry>,
    detail_html: SharedString,
    /// 開いている記事から読み取った予定。まだ読み取っていなければ `None`。
    detail_schedule: Option<EntrySchedule>,
    loaded: bool,
    list_focus: FocusHandle,
    list_scroll: UniformListScrollHandle,
    load_task: Task<()>,
    detail_task: Task<()>,
    search_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl NewsPage {
    pub fn new(app: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("タイトルと本文を検索"));
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
                AppEvent::NewEntries(_) => {}
            }),
            cx.observe(&app, |_, _, cx| cx.notify()),
        ];
        let mut page = Self {
            app,
            filter: None,
            search,
            query: String::new(),
            unread_only: false,
            entries: Vec::new(),
            rows: Vec::new(),
            selected: None,
            detail: None,
            detail_html: SharedString::default(),
            detail_schedule: None,
            loaded: false,
            list_focus: cx.focus_handle(),
            list_scroll: UniformListScrollHandle::new(),
            load_task: Task::ready(()),
            detail_task: Task::ready(()),
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
    pub fn set_filter(&mut self, filter: Option<String>, cx: &mut Context<Self>) {
        if self.filter != filter {
            self.filter = filter;
            self.reload(cx);
        }
    }

    /// 記事を開く。通知から呼ばれる。
    /// 記事を開く。通知から呼ばれるので、絞り込みと検索を外してから選ぶ。
    pub fn reveal(&mut self, entry_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        self.filter = None;
        self.unread_only = false;
        self.query.clear();
        self.search_task = Task::ready(());
        self.search
            .update(cx, |search, cx| search.set_value("", window, cx));
        self.selected = Some(entry_id);
        self.reload(cx);
        self.select(entry_id, true, cx);
    }

    fn schedule_search(&mut self, query: String, cx: &mut Context<Self>) {
        self.search_task = cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SEARCH_DELAY).await;
            let _ = this.update(cx, |this, cx| {
                if this.query != query {
                    this.query = query;
                    this.reload(cx);
                }
            });
        });
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let app = self.app.read(cx);
        let services = app.services().clone();
        let source_ids = match &self.filter {
            Some(id) => vec![id.clone()],
            None => app.enabled_ids(Channel::News),
        };
        // 今日・明日の予定がある記事は、新しい順の上限の外にあっても一覧に足す
        // (1 か月前の告知に書かれた明日のメンテナンスなど)。
        let prioritized = if app.settings().prioritize_today {
            let now = Utc::now();
            app.agenda().prioritized(now, &AppState::days(now))
        } else {
            Vec::new()
        };
        let query = EntryQuery {
            channel: Channel::News,
            source_ids: Some(source_ids),
            search: (!self.query.trim().is_empty()).then(|| self.query.clone()),
            unread_only: self.unread_only,
            ids: None,
            limit: LIST_LIMIT,
        };
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
                    Ok(entries) => this.entries = entries,
                    Err(err) => tracing::error!("記事を読めません: {err:#}"),
                }
                this.loaded = true;
                this.rebuild_rows(cx);
                let still_listed = this
                    .selected
                    .is_some_and(|id| this.entries.iter().any(|entry| entry.id == id));
                if !still_listed {
                    // 見ていた記事が一覧から消えたら先頭を出す。既読にはしない。
                    match this.rows.first().map(|row| this.entries[row.index].id) {
                        Some(first) => this.select(first, false, cx),
                        None => {
                            this.selected = None;
                            this.detail = None;
                            this.detail_schedule = None;
                        }
                    }
                } else if let Some(id) = this.selected {
                    // 開いている記事に追記が入ったかもしれないので、本文も読み直す。
                    this.load_detail(id, false, cx);
                }
                cx.notify();
            });
        });
    }

    /// 並びを決め直す。今日・明日の予定は、予定の表と今の時刻から決める (メンテナンスは今日・明日に
    /// 重なる時間帯、イベントなどの期間は始まりと終わりの節目)。
    fn rebuild_rows(&mut self, cx: &App) {
        let app = self.app.read(cx);
        let prioritize = app.settings().prioritize_today;
        let agenda = app.agenda().clone();
        let now = Utc::now();
        let days = AppState::days(now);
        let mut rows: Vec<Row> = self
            .entries
            .iter()
            .enumerate()
            .map(|(index, entry)| {
                let priority = if prioritize {
                    agenda.priority(entry.id, now, &days)
                } else {
                    Priority::Normal
                };
                Row {
                    index,
                    priority,
                    group_end: false,
                }
            })
            .collect();
        // 記事はすでに新しい順なので、安定ソートで優先度だけを前に出す。
        rows.sort_by_key(|row| row.priority);
        for index in 0..rows.len() {
            let next_is_normal = rows
                .get(index + 1)
                .is_some_and(|next| next.priority == Priority::Normal);
            rows[index].group_end = rows[index].priority != Priority::Normal && next_is_normal;
        }
        self.rows = rows;
    }

    fn select(&mut self, id: i64, mark_read: bool, cx: &mut Context<Self>) {
        self.selected = Some(id);
        if let Some(position) = self
            .rows
            .iter()
            .position(|row| self.entries[row.index].id == id)
        {
            self.list_scroll
                .scroll_to_item(position, ScrollStrategy::Nearest);
        }
        if mark_read {
            self.set_read(id, true, cx);
        }
        self.load_detail(id, true, cx);
        cx.notify();
    }

    /// 記事の本文と、読み取った予定を読む。選んだときと、記事や予定が更新されたときに呼ぶ。
    /// `opened` (選んだとき) なら、前に取れなかった本文の画像を読み直させる (記事を開き直せば直る)。
    fn load_detail(&mut self, id: i64, opened: bool, cx: &mut Context<Self>) {
        let services = self.app.read(cx).services().clone();
        self.detail_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    services.with_store(|store| -> anyhow::Result<_> {
                        Ok((store.entry(id)?, store.entry_schedule(id)?))
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.selected != Some(id) {
                    return;
                }
                match result {
                    Ok((entry, schedule)) => {
                        if opened
                            && let Some(entry) = &entry
                            && let Some(loader) = cx.try_global::<ImageLoader>()
                        {
                            let loader = loader.0.clone();
                            let uris: Vec<String> = image_sources(&entry.body_html)
                                .iter()
                                .map(|uri| display_uri(uri).into_owned())
                                .collect();
                            loader.retry_failed(&uris, cx);
                        }
                        let html: SharedString = entry
                            .as_ref()
                            .map(|entry| mark_html(&display_html(&entry.body_html), &this.query))
                            .unwrap_or_default()
                            .into();
                        // 本文が同じなら作り直さない (予定だけが変わったときに表示を崩さないため)。
                        if html != this.detail_html {
                            this.detail_html = html;
                        }
                        this.detail = entry;
                        this.detail_schedule = schedule;
                    }
                    Err(err) => tracing::error!("記事を読めません: {err:#}"),
                }
                cx.notify();
            });
        });
    }

    fn set_read(&mut self, id: i64, read: bool, cx: &mut Context<Self>) {
        let Some(entry) = self.entries.iter_mut().find(|entry| entry.id == id) else {
            return;
        };
        if entry.read == read {
            return;
        }
        entry.read = read;
        if let Some(detail) = self
            .detail
            .as_mut()
            .filter(|detail| detail.summary.id == id)
        {
            detail.summary.read = read;
        }
        let app = self.app.clone();
        let services = app.read(cx).services().clone();
        cx.spawn(async move |_, cx| {
            let result =
                cx.background_executor()
                    .spawn(async move {
                        services.with_store(|store| store.set_read(id, read, Utc::now()))
                    })
                    .await;
            if let Err(err) = result {
                tracing::error!("既読にできません: {err:#}");
            }
            app.update(cx, |app, cx| app.refresh_counts(cx));
        })
        .detach();
        cx.notify();
    }

    fn selected_position(&self) -> Option<usize> {
        let id = self.selected?;
        self.rows
            .iter()
            .position(|row| self.entries[row.index].id == id)
    }

    fn move_selection(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.rows.is_empty() {
            return;
        }
        let next = match self.selected_position() {
            Some(position) => {
                (position as isize + delta).clamp(0, self.rows.len() as isize - 1) as usize
            }
            None => 0,
        };
        let id = self.entries[self.rows[next].index].id;
        self.select(id, true, cx);
    }

    fn on_select_next(&mut self, _: &SelectNext, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(1, cx);
    }

    fn on_select_prev(&mut self, _: &SelectPrev, _: &mut Window, cx: &mut Context<Self>) {
        self.move_selection(-1, cx);
    }

    fn on_open_original(&mut self, _: &OpenOriginal, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(url) = self
            .detail
            .as_ref()
            .and_then(|entry| entry.summary.url.clone())
        {
            open_web_url(&url, cx);
        }
    }

    fn on_toggle_read(&mut self, _: &ToggleRead, _: &mut Window, cx: &mut Context<Self>) {
        if let Some(entry) = self.detail.as_ref().map(|entry| entry.summary.clone()) {
            self.set_read(entry.id, !entry.read, cx);
        }
    }

    fn on_mark_all_read(&mut self, _: &MarkAllRead, _: &mut Window, cx: &mut Context<Self>) {
        self.mark_all_read(cx);
    }

    fn on_focus_search(&mut self, _: &FocusSearch, window: &mut Window, cx: &mut Context<Self>) {
        self.search
            .update(cx, |search, cx| search.focus(window, cx));
    }

    fn on_refresh(&mut self, _: &Refresh, _: &mut Window, cx: &mut Context<Self>) {
        self.refresh(cx);
    }

    fn refresh(&mut self, cx: &mut Context<Self>) {
        let ids = self.filter.clone().map(|id| vec![id]);
        self.app.update(cx, |app, cx| app.fetch_now(ids, cx));
    }

    /// 表示中の記事 (絞り込みと検索の結果) だけを既読にする。
    fn mark_all_read(&mut self, cx: &mut Context<Self>) {
        let ids: Vec<i64> = self
            .entries
            .iter()
            .filter(|entry| !entry.read)
            .map(|entry| entry.id)
            .collect();
        if ids.is_empty() {
            return;
        }
        for entry in &mut self.entries {
            entry.read = true;
        }
        if let Some(detail) = self.detail.as_mut() {
            detail.summary.read = true;
        }
        let app = self.app.clone();
        let services = app.read(cx).services().clone();
        cx.spawn(async move |_, cx| {
            let result =
                cx.background_executor()
                    .spawn(async move {
                        services.with_store(|store| store.mark_read_ids(&ids, Utc::now()))
                    })
                    .await;
            if let Err(err) = result {
                tracing::error!("既読にできません: {err:#}");
            }
            app.update(cx, |app, cx| app.refresh_counts(cx));
        })
        .detach();
        cx.notify();
    }

    fn toggle_prioritize(&mut self, value: bool, cx: &mut Context<Self>) {
        let mut settings = self.app.read(cx).settings().clone();
        settings.prioritize_today = value;
        let result = self
            .app
            .update(cx, |app, cx| app.update_settings(settings, cx));
        if let Err(err) = result {
            tracing::error!("設定を保存できません: {err:#}");
        }
        self.rebuild_rows(cx);
        cx.notify();
        // 上限の外にある今日・明日の予定の記事も読み込む (外したときは除く)。
        self.reload(cx);
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

    fn render_toolbar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let app = self.app.read(cx);
        let prioritize = app.settings().prioritize_today;
        let running = match &self.filter {
            Some(id) => app.is_running(id),
            None => app.running_count() > 0,
        };
        h_flex()
            .w_full()
            .gap_3()
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(cx.theme().border)
            .child(
                div().flex_1().min_w(px(180.)).child(
                    Input::new(&self.search)
                        .small()
                        .cleanable(true)
                        .prefix(Icon::new(IconName::Search).small()),
                ),
            )
            .child(
                Switch::new("prioritize-today")
                    .small()
                    .label("今日・明日の予定を上に")
                    .checked(prioritize)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.toggle_prioritize(*checked, cx)
                    })),
            )
            .child(
                Switch::new("unread-only")
                    .small()
                    .label("未読のみ")
                    .checked(self.unread_only)
                    .on_click(cx.listener(|this, checked: &bool, _, cx| {
                        this.unread_only = *checked;
                        this.reload(cx);
                    })),
            )
            .child(
                Button::new("mark-all-read")
                    .small()
                    .ghost()
                    .icon(IconName::CheckCheck)
                    .tooltip("表示中の記事をすべて既読にする")
                    .on_click(cx.listener(|this, _, _, cx| this.mark_all_read(cx))),
            )
            .child(
                Button::new("refresh")
                    .small()
                    .ghost()
                    .icon(IconName::RefreshCw)
                    .loading(running)
                    .tooltip(crate::ui::actions::REFRESH_TOOLTIP)
                    .on_click(cx.listener(|this, _, _, cx| this.refresh(cx))),
            )
    }

    fn render_list(&self, cx: &mut Context<Self>) -> impl IntoElement {
        if self.loaded && self.rows.is_empty() {
            let (title, description) = if !self.query.trim().is_empty() {
                ("一致する記事がありません", "検索語を変えてみてください。")
            } else if self.unread_only {
                (
                    "未読の記事はありません",
                    "「未読のみ」を外すと既読の記事も出ます。",
                )
            } else {
                (
                    "記事がまだありません",
                    "取得が終わるとここに並びます。ソース画面で取得元を追加できます。",
                )
            };
            return div()
                .size_full()
                .child(EmptyState::new(IconName::Inbox, title).description(description))
                .into_any_element();
        }

        uniform_list(
            "news-rows",
            self.rows.len(),
            cx.processor(|this, range: Range<usize>, _window, cx| {
                range
                    .map(|position| this.render_row(position, cx).into_any_element())
                    .collect::<Vec<_>>()
            }),
        )
        .track_scroll(&self.list_scroll)
        .size_full()
        .into_any_element()
    }

    fn render_row(&self, position: usize, cx: &mut Context<Self>) -> impl IntoElement {
        let row = self.rows[position];
        let entry = &self.entries[row.index];
        let theme = cx.theme();
        let app = self.app.read(cx);
        let source = app.source(&entry.source_id);
        let name = source.map_or(entry.source_id.clone(), |source| source.name.clone());
        let icon = source.and_then(|source| source.icon.clone());
        let selected = self.selected == Some(entry.id);
        let id = entry.id;
        let title = entry.title.clone();
        let highlights = self.highlights(&title, cx);
        // 行には、今に近い状態の予定を 1 つだけ出す (すべては記事の見出しに出す)。
        let now = Utc::now();
        let badge = app
            .agenda()
            .badges(id, now, &AppState::days(now), &Local)
            .into_iter()
            .next();

        h_flex()
            .id(ElementId::NamedInteger("entry".into(), id as u64))
            .h(px(ROW_HEIGHT))
            .w_full()
            .px_2()
            .gap_2()
            .cursor_pointer()
            .when(selected, |this| this.bg(theme.list_active))
            .when(!selected, |this| {
                this.hover(|style| style.bg(theme.list_hover))
            })
            .when(row.group_end, |this| {
                this.border_b_1().border_color(theme.primary.opacity(0.5))
            })
            .child(
                div()
                    .size(px(6.))
                    .flex_shrink_0()
                    .rounded_full()
                    .when(!entry.read, |this| this.bg(theme.primary)),
            )
            .child(SourceAvatar::new(name.clone(), icon).small())
            .child(
                div()
                    .w(px(80.))
                    .flex_shrink_0()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(format::list_time(entry.published, entry.sort_at)),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_sm()
                    .when(row.priority == Priority::Today, |this| {
                        this.font_weight(FontWeight::BOLD)
                    })
                    .when(entry.read && !selected, |this| {
                        this.text_color(theme.muted_foreground)
                    })
                    .child(StyledText::new(title).with_highlights(highlights)),
            )
            .when_some(badge, |this, badge| {
                this.child(ScheduleBadge::new("schedule", badge))
            })
            .on_click(cx.listener(move |this, _, window, cx| {
                window.focus(&this.list_focus, cx);
                this.select(id, true, cx);
            }))
    }

    fn render_detail(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(entry) = &self.detail else {
            return div()
                .size_full()
                .child(EmptyState::new(IconName::Newspaper, "記事を選んでください"))
                .into_any_element();
        };
        let theme = cx.theme();
        let app = self.app.read(cx);
        let summary = &entry.summary;
        let source = app.source(&summary.source_id);
        let name = source.map_or(summary.source_id.clone(), |source| source.name.clone());
        let icon = source.and_then(|source| source.icon.clone());
        let url = summary.url.clone();
        let read = summary.read;
        let title_highlights = self.highlights(&summary.title, cx);
        let now = Utc::now();
        let badges = app
            .agenda()
            .badges(summary.id, now, &AppState::days(now), &Local);
        let panel = self
            .detail_schedule
            .clone()
            .filter(SchedulePanel::has_content)
            .map(|schedule| {
                SchedulePanel::new(summary.source_id.clone(), schedule, entry.body_checked_at)
            });

        v_flex()
            .size_full()
            .child(
                v_flex()
                    .gap_2()
                    .px_5()
                    .pt_4()
                    .pb_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .gap_2()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(SourceAvatar::new(name.clone(), icon).small())
                            .child(name)
                            .child("·")
                            .child(format::full_time(summary.published, summary.first_seen_at)),
                    )
                    .child(div().text_xl().font_semibold().child(
                        StyledText::new(summary.title.clone()).with_highlights(title_highlights),
                    ))
                    .when(!badges.is_empty(), |this| {
                        this.child(h_flex().flex_wrap().gap_1().children(
                            badges.into_iter().enumerate().map(|(index, badge)| {
                                ScheduleBadge::new(
                                    ElementId::NamedInteger("detail-badge".into(), index as u64),
                                    badge,
                                )
                            }),
                        ))
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("open-original")
                                    .small()
                                    .outline()
                                    .icon(IconName::ExternalLink)
                                    .label("元の記事を開く")
                                    .disabled(url.is_none())
                                    .on_click(move |_, _, cx| {
                                        if let Some(url) = &url {
                                            open_web_url(url, cx);
                                        }
                                    }),
                            )
                            .child(
                                Button::new("toggle-read")
                                    .small()
                                    .ghost()
                                    .icon(if read {
                                        IconName::Mail
                                    } else {
                                        IconName::MailOpen
                                    })
                                    .label(if read {
                                        "未読に戻す"
                                    } else {
                                        "既読にする"
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        if let Some(id) = this.selected {
                                            this.set_read(id, !read, cx);
                                        }
                                    })),
                            ),
                    ),
            )
            .children(panel)
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(if entry.body_html.trim().is_empty() {
                        EmptyState::new(IconName::FileText, "本文はありません")
                            .description("元の記事を開いて読んでください。")
                            .into_any_element()
                    } else {
                        TextView::html(
                            ElementId::NamedInteger("article".into(), summary.id as u64),
                            self.detail_html.clone(),
                        )
                        .scrollable(true)
                        .selectable(true)
                        .on_link_click(|url, _, _, cx| {
                            if url.starts_with("mailto:") {
                                cx.open_url(url);
                            } else {
                                open_web_url(url, cx);
                            }
                        })
                        .px_5()
                        .py_4()
                        .size_full()
                        .into_any_element()
                    }),
            )
            .into_any_element()
    }
}

impl Focusable for NewsPage {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.list_focus.clone()
    }
}

impl Render for NewsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .on_action(cx.listener(Self::on_focus_search))
            .on_action(cx.listener(Self::on_refresh))
            .on_action(cx.listener(Self::on_mark_all_read))
            .child(self.render_toolbar(cx))
            .child(
                div().flex_1().min_h_0().child(
                    h_resizable("news-split")
                        .child(
                            resizable_panel()
                                .size(px(460.))
                                .size_range(px(280.)..px(900.))
                                .child(
                                    div()
                                        .id("news-list")
                                        .key_context("NewsList")
                                        .track_focus(&self.list_focus)
                                        .on_action(cx.listener(Self::on_select_next))
                                        .on_action(cx.listener(Self::on_select_prev))
                                        .on_action(cx.listener(Self::on_open_original))
                                        .on_action(cx.listener(Self::on_toggle_read))
                                        .size_full()
                                        .child(self.render_list(cx)),
                                ),
                        )
                        .child(resizable_panel().child(self.render_detail(cx))),
                ),
            )
    }
}
