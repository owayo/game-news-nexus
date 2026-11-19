//! 今日・明日の予定。
//!
//! 予定の表 (同じゲームの記事どうしを突き合わせたあと) を 1 件ずつ並べ、選ぶと根拠の記事を右に出す
//! (狭いときはどちらか一方)。予告・延長・完了が別の記事に書かれていても、同じメンテナンスとして見られる。
//! 「メンテ中・要確認」「時刻の決まった予定 (時刻の順)」「時刻の書かれていない予定」の順に並べ、終わった・
//! 中止・延期したメンテナンスは末尾にまとめて畳む。記事の「未読のみ」は引き継がない (読んだ記事の予定も
//! 見失わないように)。並べ方の規則は `gnn_core::agenda::Agenda::day_entries`。

use std::collections::HashMap;

use chrono::{Local, Utc};
use gnn_core::EntryQuery;
use gnn_core::agenda::{Day, DayEntry, DayGroup};
use gnn_core::source::Channel;
use gpui_kit::assets::IconName;
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, ElementId, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, Render, SharedString, StatefulInteractiveElement as _, Styled as _,
    Subscription, Task, Window, div, px,
};

use crate::format;
use crate::state::{AppEvent, AppState};
use crate::ui::Layout;
use crate::ui::article::{ArticleEvent, ArticleView};
use crate::ui::components::{EmptyState, ScheduleChip, SourceAvatar, segmented};
use crate::ui::theme::Palette;

pub struct AgendaPage {
    app: Entity<AppState>,
    article: Entity<ArticleView>,
    day: Day,
    /// 予定の根拠の記事の題名。
    titles: HashMap<i64, String>,
    /// 選んだ予定 (記事の ID と予定の名前)。
    selected: Option<(i64, String)>,
    /// 終わった予定のまとまりを開いているか。
    show_finished: bool,
    /// 狭いときに本文を出しているか。
    showing_article: bool,
    /// 本文を読み込んだら既読にする記事。
    read_on_open: Option<i64>,
    titles_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl AgendaPage {
    pub fn new(app: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let article = cx.new(|cx| {
            ArticleView::new(app.clone(), cx).with_empty_hint(
                "予定を選んでください",
                "予定を選ぶと、それが書かれた記事をここに出します。",
            )
        });
        let subscriptions = vec![
            cx.subscribe(&app, |this, _, event, cx| match event {
                AppEvent::EntriesChanged | AppEvent::AgendaChanged => {
                    this.load_titles(cx);
                    this.article.update(cx, |article, cx| article.refresh(cx));
                }
                AppEvent::ReadChanged { .. } | AppEvent::NewEntries(_) => {}
            }),
            cx.observe(&app, |_, _, cx| cx.notify()),
            cx.subscribe(&article, |this, _, event, cx| match event {
                ArticleEvent::Opened(id) => {
                    if this.read_on_open == Some(*id) {
                        this.read_on_open = None;
                        this.mark_read(*id, cx);
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
            day: Day::Today,
            titles: HashMap::new(),
            selected: None,
            show_finished: false,
            showing_article: false,
            read_on_open: None,
            titles_task: Task::ready(()),
            _subscriptions: subscriptions,
        };
        page.load_titles(cx);
        page
    }

    fn entries(&self, cx: &gpui_kit::App) -> Vec<DayEntry> {
        let now = Utc::now();
        self.app
            .read(cx)
            .agenda()
            .day_entries(self.day, now, &AppState::days(now), &Local)
    }

    /// 今日と明日の予定の根拠の記事の題名を読む。
    fn load_titles(&mut self, cx: &mut Context<Self>) {
        let now = Utc::now();
        let days = AppState::days(now);
        let app = self.app.read(cx);
        let mut ids: Vec<i64> = [Day::Today, Day::Tomorrow]
            .into_iter()
            .flat_map(|day| app.agenda().day_entries(day, now, &days, &Local))
            .map(|entry| entry.entry_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        if ids.is_empty() {
            return;
        }
        let services = app.services().clone();
        let query = EntryQuery {
            limit: ids.len(),
            ids: Some(ids),
            ..EntryQuery::channel(Channel::News)
        };
        self.titles_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move { services.with_store(|store| store.query(&query)) })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(entries) => {
                        this.titles = entries
                            .into_iter()
                            .map(|entry| (entry.id, entry.title))
                            .collect();
                    }
                    Err(err) => tracing::error!("記事を読めません: {err:#}"),
                }
                cx.notify();
            });
        });
    }

    fn mark_read(&mut self, id: i64, cx: &mut Context<Self>) {
        if self.article.read(cx).is_read() != Some(true) {
            self.app
                .update(cx, |app, cx| app.set_read(vec![id], true, cx));
        }
    }

    fn select(&mut self, entry: &DayEntry, cx: &mut Context<Self>) {
        self.selected = Some((entry.entry_id, entry.title.clone()));
        self.read_on_open = Some(entry.entry_id);
        self.showing_article = true;
        let id = entry.entry_id;
        self.article.update(cx, |article, cx| article.show(id, cx));
        cx.notify();
    }

    fn set_day(&mut self, day: Day, cx: &mut Context<Self>) {
        if self.day != day {
            self.day = day;
            self.show_finished = false;
            cx.notify();
        }
    }

    fn render_header(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let now = Utc::now();
        let today = now.with_timezone(&Local).date_naive();
        let date = match self.day {
            Day::Today => today,
            Day::Tomorrow => today.succ_opt().unwrap_or(today),
        };
        let offset = now.with_timezone(&Local).format("%:z").to_string();
        h_flex()
            .flex_none()
            .gap_4()
            .px_6()
            .py_4()
            .border_b_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_0p5()
                    .child(
                        h_flex()
                            .gap_3()
                            .items_baseline()
                            .child(div().text_xl().font_semibold().child(match self.day {
                                Day::Today => "今日の予定",
                                Day::Tomorrow => "明日の予定",
                            }))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(format::day_heading(date)),
                            ),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!(
                                "読み取ったメンテナンスと、イベント・ガチャ・キャンペーンの始まりと終わりです。時刻はこの PC の時刻 (UTC{offset})。"
                            )),
                    ),
            )
            .child(segmented(
                "agenda-day",
                &["今日", "明日"],
                usize::from(self.day == Day::Tomorrow),
                cx.listener(|this, index: &usize, _, cx| {
                    this.set_day(if *index == 1 { Day::Tomorrow } else { Day::Today }, cx)
                }),
            ))
    }

    fn render_group_header(
        &self,
        group: DayGroup,
        count: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme();
        let (icon, text) = match group {
            DayGroup::Now => (IconName::Wrench, "メンテ中・要確認".to_string()),
            DayGroup::Timed => (
                IconName::CalendarClock,
                match self.day {
                    Day::Today => "今日の予定",
                    Day::Tomorrow => "明日の予定",
                }
                .to_string(),
            ),
            DayGroup::DateOnly => (IconName::Calendar, "時刻の書かれていない予定".to_string()),
            DayGroup::Finished => (IconName::CircleCheck, "終わった・中止した予定".to_string()),
        };
        let finished = group == DayGroup::Finished;
        h_flex()
            .id(ElementId::NamedInteger("agenda-group".into(), group as u64))
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
            .child(div().flex_1().child(format!("{text}  {count} 件")))
            .when(finished, |this| {
                this.cursor_pointer()
                    .child(
                        Icon::new(if self.show_finished {
                            IconName::ChevronUp
                        } else {
                            IconName::ChevronDown
                        })
                        .xsmall(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.show_finished = !this.show_finished;
                        cx.notify();
                    }))
            })
            .into_any_element()
    }

    fn render_entry(&self, position: usize, entry: DayEntry, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let palette = Palette::of(cx);
        let app = self.app.read(cx);
        let source = app.source(&entry.source_id);
        let name = source.map_or(entry.source_id.clone(), |source| source.name.clone());
        let icon = source.and_then(|source| source.icon.clone());
        let selected = self.selected.as_ref() == Some(&(entry.entry_id, entry.title.clone()));
        let time = match (entry.group, entry.at) {
            (DayGroup::DateOnly, _) | (_, None) => "—".to_string(),
            (_, Some(at)) => at.with_timezone(&Local).format("%H:%M").to_string(),
        };
        let article_title = self.titles.get(&entry.entry_id).cloned();
        let finished = entry.group == DayGroup::Finished;
        let clicked = entry.clone();
        h_flex()
            .id(ElementId::NamedInteger(
                "agenda-entry".into(),
                position as u64,
            ))
            .relative()
            .w_full()
            .items_start()
            .gap_3()
            .px_4()
            .py_3()
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
                div()
                    .flex_none()
                    .w(px(44.))
                    .pt_0p5()
                    .text_sm()
                    .font_semibold()
                    .when(finished, |this| this.text_color(theme.muted_foreground))
                    .child(time),
            )
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_1p5()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(SourceAvatar::new(name.clone(), icon).xsmall())
                            .child(div().min_w_0().truncate().child(name)),
                    )
                    .child(
                        div()
                            .text_sm()
                            .line_height(px(20.))
                            .font_semibold()
                            .line_clamp(2)
                            .when(finished, |this| this.text_color(theme.muted_foreground))
                            .child(entry.title.clone()),
                    )
                    .child(h_flex().gap_2().min_w_0().child(ScheduleChip::new(
                        ElementId::NamedInteger("agenda-chip".into(), position as u64),
                        entry.badge.clone(),
                    )))
                    .when_some(entry.when.clone(), |this, when| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .child(when),
                        )
                    })
                    .when_some(article_title, |this, title| {
                        this.child(
                            div()
                                .text_xs()
                                .truncate()
                                .text_color(theme.muted_foreground)
                                .child(SharedString::from(format!("「{title}」より"))),
                        )
                    }),
            )
            .on_click(cx.listener(move |this, _, _, cx| this.select(&clicked, cx)))
            .into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let entries = self.entries(cx);
        if entries.is_empty() {
            let title = match self.day {
                Day::Today => "今日の予定はありません",
                Day::Tomorrow => "明日の予定はありません",
            };
            return div()
                .size_full()
                .child(EmptyState::new(IconName::CalendarCheck, title).description(
                    "記事から読み取ったメンテナンスと、イベント・ガチャ・キャンペーンの始まりと終わりがここに並びます。",
                ))
                .into_any_element();
        }
        let mut counts: HashMap<DayGroup, usize> = HashMap::new();
        for entry in &entries {
            *counts.entry(entry.group).or_default() += 1;
        }
        let mut rows: Vec<AnyElement> = Vec::new();
        let mut current = None;
        for (position, entry) in entries.into_iter().enumerate() {
            if current != Some(entry.group) {
                current = Some(entry.group);
                rows.push(self.render_group_header(
                    entry.group,
                    counts.get(&entry.group).copied().unwrap_or(0),
                    cx,
                ));
            }
            if entry.group == DayGroup::Finished && !self.show_finished {
                continue;
            }
            rows.push(self.render_entry(position, entry, cx));
        }
        div()
            .id("agenda-list")
            .size_full()
            .overflow_y_scrollbar()
            .child(v_flex().w_full().children(rows))
            .into_any_element()
    }
}

impl Render for AgendaPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = Layout::of(window);
        let narrow = !layout.side_by_side();
        self.article.update(cx, |article, cx| {
            article.set_back(narrow.then_some("予定に戻る"), cx)
        });
        let root = v_flex().size_full().child(self.render_header(cx));
        if narrow {
            return if self.showing_article && self.selected.is_some() {
                root.child(div().flex_1().min_h_0().child(self.article.clone()))
            } else {
                root.child(div().flex_1().min_h_0().child(self.render_list(cx)))
            };
        }
        root.child(
            div().flex_1().min_h_0().child(
                h_resizable("agenda-split")
                    .child(
                        resizable_panel()
                            .size(px(420.))
                            .size_range(px(340.)..px(720.))
                            .child(self.render_list(cx)),
                    )
                    .child(resizable_panel().child(self.article.clone())),
            ),
        )
    }
}
