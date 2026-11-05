//! 新作・近日リリースの一覧。ソースごとにカードを並べる。

use chrono::Utc;
use gnn_core::source::Channel;
use gnn_core::{EntryQuery, EntrySummary};
use gpui_kit::assets::IconName;
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::tag::Tag;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, ElementId, Entity, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, StyledImage as _,
    Subscription, Task, Window, div, img, px,
};

use crate::state::{AppEvent, AppState};
use crate::ui::components::{EmptyState, PageHeader, SectionLabel, open_web_url};

const CARD_WIDTH: f32 = 236.;

pub struct ReleasesPage {
    app: Entity<AppState>,
    entries: Vec<EntrySummary>,
    loaded: bool,
    load_task: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl ReleasesPage {
    pub fn new(app: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscriptions = vec![
            cx.subscribe(&app, |this, _, event, cx| {
                if matches!(event, AppEvent::EntriesChanged) {
                    this.reload(cx);
                }
            }),
            cx.observe(&app, |_, _, cx| cx.notify()),
        ];
        let mut page = Self {
            app,
            entries: Vec::new(),
            loaded: false,
            load_task: Task::ready(()),
            _subscriptions: subscriptions,
        };
        page.reload(cx);
        page
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let services = self.app.read(cx).services().clone();
        self.load_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    services
                        .with_store(|store| store.query(&EntryQuery::channel(Channel::Releases)))
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(entries) => this.entries = entries,
                    Err(err) => tracing::error!("リリース情報を読めません: {err:#}"),
                }
                this.loaded = true;
                cx.notify();
            });
        });
    }

    /// 開いたら既読にする。新着の印を消すため。
    fn mark_seen(&mut self, cx: &mut Context<Self>) {
        let app = self.app.clone();
        let services = app.read(cx).services().clone();
        cx.spawn(async move |_, cx| {
            let _ = cx
                .background_executor()
                .spawn(async move {
                    services.with_store(|store| {
                        store.mark_all_read(Channel::Releases, None, Utc::now())
                    })
                })
                .await;
            app.update(cx, |app, cx| app.refresh_counts(cx));
        })
        .detach();
    }

    pub fn on_shown(&mut self, cx: &mut Context<Self>) {
        self.mark_seen(cx);
    }

    fn render_card(&self, entry: &EntrySummary, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let url = entry.url.clone();
        let is_new = !entry.read;
        let tag = self.app.read(cx).source(&entry.source_id).map(|source| {
            if source.tags.iter().any(|tag| tag == "fanza") {
                "FANZA"
            } else {
                "DMM"
            }
        });

        v_flex()
            .id(ElementId::NamedInteger("release".into(), entry.id as u64))
            .w(px(CARD_WIDTH))
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.border)
            .bg(theme.background)
            .overflow_hidden()
            .cursor_pointer()
            .hover(|style| style.border_color(theme.primary))
            .child(
                div()
                    .w_full()
                    .h(px(132.))
                    .bg(theme.muted)
                    .flex()
                    .items_center()
                    .justify_center()
                    .map(|this| match &entry.image_url {
                        Some(image) => this.child(
                            img(crate::http_client::display_uri(image).into_owned())
                                .size_full()
                                .object_fit(ObjectFit::Contain),
                        ),
                        None => this.child(
                            Icon::new(IconName::Image)
                                .large()
                                .text_color(theme.muted_foreground),
                        ),
                    }),
            )
            .child(
                v_flex()
                    .gap_1()
                    .p_3()
                    .child(
                        h_flex()
                            .gap_1()
                            .when_some(tag, |this, tag| {
                                this.child(Tag::secondary().xsmall().child(tag))
                            })
                            .when(is_new, |this| {
                                this.child(Tag::primary().xsmall().child("NEW"))
                            }),
                    )
                    .child(
                        div()
                            .text_sm()
                            .font_semibold()
                            .line_clamp(2)
                            .child(entry.title.clone()),
                    )
                    .when_some(entry.note.clone(), |this, note| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(theme.muted_foreground)
                                .line_clamp(2)
                                .child(note),
                        )
                    }),
            )
            .on_click(move |_, _, cx| {
                if let Some(url) = &url {
                    open_web_url(url, cx);
                }
            })
    }
}

impl Render for ReleasesPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let app = self.app.read(cx);
        let mut sections: Vec<(String, Vec<EntrySummary>)> = Vec::new();
        for source in app
            .enabled_sources()
            .filter(|source| source.channel == Channel::Releases)
        {
            let entries: Vec<EntrySummary> = self
                .entries
                .iter()
                .filter(|entry| entry.source_id == source.id)
                .cloned()
                .collect();
            if !entries.is_empty() {
                sections.push((source.name.clone(), entries));
            }
        }

        let body = if self.loaded && sections.is_empty() {
            EmptyState::new(IconName::Sparkles, "リリース情報はまだありません")
                .description("ソース画面で「新作・近日リリース」の取得元を有効にすると、ここにカードが並びます。")
                .into_any_element()
        } else {
            v_flex()
                .gap_4()
                .p_4()
                .children(sections.into_iter().map(|(name, entries)| {
                    v_flex().gap_2().child(SectionLabel::new(name)).child(
                        div().flex().flex_wrap().gap_3().children(
                            entries
                                .iter()
                                .map(|entry| self.render_card(entry, cx).into_any_element())
                                .collect::<Vec<_>>(),
                        ),
                    )
                }))
                .into_any_element()
        };

        v_flex()
            .size_full()
            .child(
                PageHeader::new("新作・近日リリース")
                    .description("DMM GAMES / FANZA GAMES などの新作と配信予定。カードを押すと紹介ページを開きます。"),
            )
            .child(div().id("releases-scroll").flex_1().min_h_0().overflow_y_scrollbar().child(body))
    }
}
