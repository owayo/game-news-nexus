//! 「すべてのお知らせ」のピックアップ。

use std::collections::HashMap;
use std::hash::{BuildHasher as _, RandomState};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use gnn_core::agenda::{Agenda, Days};
use gnn_core::config::{PickupSettings, PickupTopic};
use gnn_core::{Channel, EntryQuery, EntrySummary};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Sizable as _, StyledExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    Context, Entity, EventEmitter, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, StyledImage as _,
    Subscription, Task, Window, div, ease_in_out, img, px, relative,
};

use crate::http_client::display_uri;
use crate::services::image_sources;
use crate::state::{AppEvent, AppState};
use crate::ui::components::SourceAvatar;
use crate::ui::theme::Palette;

pub(super) fn topic_label(topic: PickupTopic) -> &'static str {
    match topic {
        PickupTopic::StartsToday => "今日始まる",
        PickupTopic::EndsToday => "今日終わる",
        PickupTopic::StartsTomorrow => "明日始まる",
        PickupTopic::EndsTomorrow => "明日終わる",
    }
}

fn matches(
    agenda: &Agenda,
    topics: &[PickupTopic],
    now: DateTime<Utc>,
    days: &Days,
) -> HashMap<i64, Vec<PickupTopic>> {
    let mut matches = HashMap::<_, Vec<_>>::new();
    for topic in PickupTopic::ALL
        .into_iter()
        .filter(|topic| topics.contains(topic))
    {
        for id in agenda.filter_entries(topic.filter(), now, days) {
            matches.entry(id).or_default().push(topic);
        }
    }
    matches
}

struct Slide {
    entry: EntrySummary,
    topics: Vec<PickupTopic>,
    image: Option<String>,
}

const SLIDE_DURATION: Duration = Duration::from_millis(320);

/// 選択先を 0 としたときの、動き始めの表示位置。
#[derive(Clone, Copy)]
struct SlideMotion {
    from: f32,
    started_at: Instant,
}

impl SlideMotion {
    fn offset(self, now: Instant) -> f32 {
        let elapsed = now.duration_since(self.started_at);
        let progress = (elapsed.as_secs_f32() / SLIDE_DURATION.as_secs_f32()).min(1.0);
        self.from * (1.0 - ease_in_out(progress))
    }

    fn retarget(current: Option<Self>, step: isize, now: Instant) -> Self {
        // 移動中にもう一度押したら、いま画面に出ている位置を新しい選択先から測り直す。
        Self {
            from: current.map_or(0.0, |motion| motion.offset(now)) - step as f32,
            started_at: now,
        }
    }
}

fn hero_image(uri: &str) -> Option<String> {
    let uri = uri.trim();
    let url = url::Url::parse(uri).ok()?;
    matches!(url.scheme(), "http" | "https").then(|| display_uri(uri).into_owned())
}

pub(super) struct OpenPickup(pub i64);

pub(super) struct NewsHero {
    app: Entity<AppState>,
    settings: PickupSettings,
    visible: bool,
    paused: bool,
    order: RandomState,
    slides: Vec<Slide>,
    index: usize,
    motion: Option<SlideMotion>,
    load_task: Task<()>,
    rotation: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<OpenPickup> for NewsHero {}

impl NewsHero {
    pub(super) fn new(app: Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let settings = app.read(cx).settings().pickup.clone();
        let subscriptions = vec![
            cx.subscribe(&app, |this, _, event, cx| {
                if this.visible
                    && matches!(event, AppEvent::AgendaChanged | AppEvent::EntriesChanged)
                {
                    this.reload(cx);
                }
            }),
            cx.observe(&app, |this, app, cx| {
                let settings = app.read(cx).settings().pickup.clone();
                let topics_changed = this.settings.topics != settings.topics;
                let interval_changed = this.settings.interval() != settings.interval();
                this.settings = settings;
                if this.visible && topics_changed {
                    this.reload(cx);
                }
                if this.visible && interval_changed {
                    this.reset_timer(cx);
                }
                // ソースアイコンの変更も反映する。
                if this.visible {
                    cx.notify();
                }
            }),
        ];
        Self {
            app,
            settings,
            visible: false,
            paused: false,
            order: RandomState::new(),
            slides: Vec::new(),
            index: 0,
            motion: None,
            load_task: Task::ready(()),
            rotation: Task::ready(()),
            _subscriptions: subscriptions,
        }
    }

    pub(super) fn set_visible(&mut self, visible: bool, cx: &mut Context<Self>) {
        if self.visible == visible {
            return;
        }
        self.visible = visible;
        if visible {
            self.order = RandomState::new();
            self.reload(cx);
        } else {
            self.load_task = Task::ready(());
            self.rotation = Task::ready(());
            self.slides.clear();
            self.index = 0;
            self.motion = None;
            cx.notify();
        }
    }

    fn reload(&mut self, cx: &mut Context<Self>) {
        let app = self.app.read(cx);
        let now = Utc::now();
        let mut matches = matches(
            app.agenda(),
            &self.settings.topics,
            now,
            &AppState::days(now),
        );
        let enabled = app.enabled_ids(Channel::News);
        let services = app.services().clone();

        // 対象から外れた記事を先に除く。ソースを無効にした場合も、読み込みを待たずに隠す。
        let previous = self.current_id();
        let previous_len = self.slides.len();
        self.slides.retain_mut(|slide| {
            let Some(topics) = matches.get(&slide.entry.id) else {
                return false;
            };
            slide.topics.clone_from(topics);
            enabled.contains(&slide.entry.source_id)
        });
        if self.slides.len() != previous_len {
            self.motion = None;
        }
        self.restore_position(previous, previous_len > 1, cx);

        if matches.is_empty() || enabled.is_empty() {
            self.load_task = Task::ready(());
            return;
        }
        let query = EntryQuery {
            channel: Channel::News,
            source_ids: Some(enabled),
            limit: matches.len(),
            ids: Some(matches.keys().copied().collect()),
            ..EntryQuery::channel(Channel::News)
        };
        self.load_task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    services.with_store(|store| -> anyhow::Result<Vec<Slide>> {
                        store
                            .query(&query)?
                            .into_iter()
                            .map(|entry| {
                                let mut image = entry.image_url.as_deref().and_then(hero_image);
                                if image.is_none()
                                    && let Some(full) = store.entry(entry.id)?
                                {
                                    image = image_sources(&full.body_html)
                                        .iter()
                                        .find_map(|uri| hero_image(uri));
                                }
                                let topics = matches.remove(&entry.id).unwrap_or_default();
                                Ok(Slide {
                                    entry,
                                    topics,
                                    image,
                                })
                            })
                            .collect()
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| match result {
                Ok(mut slides) => {
                    let previous = this.current_id();
                    let rotating = this.slides.len() > 1;
                    slides.sort_by_cached_key(|slide| this.order.hash_one(slide.entry.id));
                    if this.motion.is_some()
                        && !this
                            .slides
                            .iter()
                            .map(|slide| slide.entry.id)
                            .eq(slides.iter().map(|slide| slide.entry.id))
                    {
                        this.motion = None;
                    }
                    this.slides = slides;
                    this.restore_position(previous, rotating, cx);
                }
                Err(err) => tracing::error!("ピックアップの記事を読めません: {err:#}"),
            });
        });
    }

    fn current_id(&self) -> Option<i64> {
        self.slides.get(self.index).map(|slide| slide.entry.id)
    }

    fn restore_position(&mut self, previous: Option<i64>, rotating: bool, cx: &mut Context<Self>) {
        self.index = previous
            .and_then(|id| self.slides.iter().position(|slide| slide.entry.id == id))
            .unwrap_or_default();
        if previous != self.current_id() || rotating != (self.slides.len() > 1) {
            self.reset_timer(cx);
        }
        cx.notify();
    }

    fn adjacent(&self, offset: isize) -> usize {
        (self.index as isize + offset).rem_euclid(self.slides.len() as isize) as usize
    }

    fn reset_timer(&mut self, cx: &mut Context<Self>) {
        if !self.visible || self.paused || self.slides.len() < 2 {
            self.rotation = Task::ready(());
            return;
        }
        let interval = self.settings.interval();
        self.rotation = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor().timer(interval).await;
                if this.update(cx, |this, cx| this.advance(1, cx)).is_err() {
                    break;
                }
            }
        });
    }

    fn advance(&mut self, step: isize, cx: &mut Context<Self>) {
        if self.slides.len() < 2 {
            return;
        }
        self.index = self.adjacent(step);
        self.motion =
            (!cx.reduce_motion()).then(|| SlideMotion::retarget(self.motion, step, Instant::now()));
        cx.notify();
    }

    fn navigate(&mut self, step: isize, cx: &mut Context<Self>) {
        self.advance(step, cx);
        self.reset_timer(cx);
    }

    fn render_slide(
        &self,
        slide: &Slide,
        compact: bool,
        active: bool,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let palette = Palette::of(cx);
        let app = self.app.read(cx);
        let source = app.source(&slide.entry.source_id);
        let name = source.map_or(slide.entry.source_id.as_str(), |source| {
            source.name.as_str()
        });
        let icon = source.and_then(|source| source.icon.clone());
        let id = slide.entry.id;

        h_flex()
            .size_full()
            .min_w_0()
            .items_stretch()
            .gap_2()
            .rounded_xl()
            .border_1()
            .border_color(palette.divider)
            .bg(palette.selected)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .justify_center()
                    .gap_2()
                    .pl_4()
                    .pr_2()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(SourceAvatar::new(name.to_owned(), icon).small())
                            .child(div().min_w_0().truncate().text_xs().child(name.to_owned())),
                    )
                    .child(
                        div()
                            .text_size(px(if compact { 17. } else { 22. }))
                            .line_height(px(if compact { 21. } else { 27. }))
                            .font_semibold()
                            .line_clamp(2)
                            .child(slide.entry.title.clone()),
                    )
                    .child(
                        h_flex()
                            .min_w_0()
                            .gap_2()
                            .child(
                                Button::new("pickup-open")
                                    .small()
                                    .ghost()
                                    .label("記事を読む")
                                    .text_color(palette.selected_foreground)
                                    .tab_stop(active)
                                    .when(active, |button| {
                                        button.on_click(cx.listener(move |_, _, _, cx| {
                                            cx.emit(OpenPickup(id));
                                        }))
                                    }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .text_color(theme.muted_foreground)
                                    .child(
                                        slide
                                            .topics
                                            .iter()
                                            .copied()
                                            .map(topic_label)
                                            .collect::<Vec<_>>()
                                            .join("・"),
                                    ),
                            ),
                    ),
            )
            .when_some(slide.image.as_ref(), |this, url| {
                this.child(
                    div()
                        .w(relative(if compact { 0.29 } else { 0.39 }))
                        .flex_none()
                        .p_2()
                        .child(
                            img(url.clone())
                                .size_full()
                                .rounded_lg()
                                .object_fit(ObjectFit::Contain),
                        ),
                )
            })
    }

    fn render_carousel(
        &self,
        compact: bool,
        offset: f32,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let multiple = self.slides.len() > 1;
        let width = if !multiple {
            1.0
        } else if compact {
            0.82
        } else {
            0.76
        };
        let edge = (1.0 - width) / 2.0;
        let stride = width + 0.014;
        let slots = if multiple {
            (offset.floor() as isize - 1)..=(offset.ceil() as isize + 1)
        } else {
            0..=0
        };
        let mut carousel = div()
            .relative()
            .w_full()
            .h(px(if compact { 128. } else { 170. }))
            .overflow_hidden()
            .children(slots.filter_map(|slot| {
                let distance = slot as f32 - offset;
                let left = edge + distance * stride;
                if left >= 1.0 || left + width <= 0.0 {
                    return None;
                }
                let slide = &self.slides[self.adjacent(slot)];
                let title_opacity = (-distance * 3.0 - 2.0).clamp(0.0, 1.0);
                Some(
                    div()
                        .id(("pickup-slide", slot as u64))
                        .absolute()
                        .left(relative(left))
                        .top_0()
                        .w(relative(width))
                        .h_full()
                        .opacity(0.38 + 0.62 * (1.0 - distance.abs()).max(0.0))
                        .child(self.render_slide(slide, compact, slot == 0, cx))
                        // 画像のないカードは左端に出ると題が隠れるので、見えている側にも題を出す。
                        .when(title_opacity > 0.0 && slide.image.is_none(), |card| {
                            card.child(
                                div()
                                    .absolute()
                                    .top_4()
                                    .right_0()
                                    .w(relative(edge / width))
                                    .px_2()
                                    .text_xs()
                                    .font_medium()
                                    .line_clamp(3)
                                    .opacity(title_opacity)
                                    .child(slide.entry.title.clone()),
                            )
                        })
                        .when(slot != 0 && offset == 0.0, |card| {
                            card.cursor_pointer().on_click(
                                cx.listener(move |this, _, _, cx| this.navigate(slot, cx)),
                            )
                        }),
                )
            }));
        if multiple {
            for (step, id, icon, label) in [
                (-1, "pickup-prev", IconName::ChevronLeft, "前の記事"),
                (1, "pickup-next", IconName::ChevronRight, "次の記事"),
            ] {
                let control = h_flex()
                    .absolute()
                    .top_0()
                    .h_full()
                    .w(relative(edge))
                    .justify_center()
                    .child(
                        // 矢印を押したときは、重なっているプレビューに同じクリックを通さない。
                        div().flex().flex_none().block_mouse_except_scroll().child(
                            Button::new(id)
                                .secondary()
                                .outline()
                                .rounded_full()
                                .icon(icon)
                                .tooltip(label)
                                .accessibility_label(label)
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.navigate(step, cx)),
                                ),
                        ),
                    );
                carousel = carousel.child(if step < 0 {
                    control.left_0()
                } else {
                    control.right_0()
                });
            }
        }
        carousel
    }
}

impl Render for NewsHero {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.slides.is_empty() {
            return div().flex_none().into_any_element();
        }
        let offset = self
            .motion
            .filter(|_| !cx.reduce_motion())
            .map_or(0.0, |motion| motion.offset(Instant::now()));
        if offset == 0.0 {
            self.motion = None;
        } else {
            window.request_animation_frame();
        }
        let theme = cx.theme().clone();
        let size = window.viewport_size();
        let compact = size.width < px(760.) || size.height < px(590.);
        let multiple = self.slides.len() > 1;
        let pause_label = if self.paused {
            "自動切り替えを再開"
        } else {
            "自動切り替えを一時停止"
        };

        v_flex()
            .flex_none()
            .w_full()
            .gap_2()
            .px_4()
            .pt_2()
            .pb_3()
            .child(
                h_flex()
                    .justify_between()
                    .child(div().text_sm().font_semibold().child("ピックアップ"))
                    .when(multiple, |this| {
                        this.child(
                            h_flex()
                                .gap_1()
                                .child(
                                    div()
                                        .mr_1()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(format!(
                                            "{} / {}",
                                            self.index + 1,
                                            self.slides.len()
                                        )),
                                )
                                .child(
                                    Button::new("pickup-pause")
                                        .xsmall()
                                        .ghost()
                                        .icon(if self.paused {
                                            IconName::Play
                                        } else {
                                            IconName::Pause
                                        })
                                        .tooltip(pause_label)
                                        .accessibility_label(pause_label)
                                        .on_click(cx.listener(|this, _, _, cx| {
                                            this.paused = !this.paused;
                                            this.reset_timer(cx);
                                            cx.notify();
                                        })),
                                ),
                        )
                    }),
            )
            .child(self.render_carousel(compact, offset, cx))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, TimeZone as _};
    use gnn_core::agenda::{AgendaItem, AgendaPeriod};
    use gnn_core::schedule::{Certainty, Period, PeriodKind, Slot, TimeValue, When, Zone};

    #[test]
    fn advancing_and_reversing_mid_slide_preserve_the_visible_position() {
        let start = Instant::now();
        let forward = SlideMotion::retarget(None, 1, start);
        let halfway = start + SLIDE_DURATION / 2;
        assert_eq!(forward.offset(start), -1.0);
        assert_eq!(forward.offset(halfway), -0.5);
        assert_eq!(forward.offset(start + SLIDE_DURATION), 0.0);

        let next = SlideMotion::retarget(Some(forward), 1, halfway);
        assert_eq!(next.offset(halfway), -1.5);
        let previous = SlideMotion::retarget(Some(forward), -1, halfway);
        assert_eq!(previous.offset(halfway), 0.5);
        assert_eq!(previous.offset(halfway + SLIDE_DURATION / 2), 0.25);
        assert_eq!(previous.offset(halfway + SLIDE_DURATION), 0.0);
    }

    #[test]
    fn pickup_combines_topics_and_deduplicates_articles() {
        let now = Utc.with_ymd_and_hms(2026, 11, 19, 3, 0, 0).unwrap();
        let time = |hours| {
            Slot::Value(TimeValue {
                when: When::Instant {
                    at: now + Duration::hours(hours),
                    zone: Zone::Offset(9 * 3600),
                },
                certainty: Certainty::Explicit,
                evidence: Vec::new(),
            })
        };
        let event = |id, start, end| AgendaItem {
            entry_id: id,
            source_id: "game".into(),
            detail: AgendaPeriod {
                index: 0,
                period: Period {
                    kind: PeriodKind::Event,
                    label: None,
                    window: gnn_core::schedule::Window {
                        start: time(start),
                        end: time(end),
                    },
                    changes: Vec::new(),
                    evidence: Vec::new(),
                },
            }
            .into(),
        };
        let agenda = Agenda::new(vec![event(1, 1, 2), event(2, 24, 26), event(3, 48, 50)]);
        let days = Days::at(now, &chrono_tz::Asia::Tokyo);
        let topics = [
            PickupTopic::StartsToday,
            PickupTopic::EndsToday,
            PickupTopic::StartsTomorrow,
            PickupTopic::StartsToday,
        ];
        let matching = matches(&agenda, &topics, now, &days);
        assert_eq!(matching.len(), 2);
        assert_eq!(
            matching[&1],
            [PickupTopic::StartsToday, PickupTopic::EndsToday]
        );
        assert_eq!(matching[&2], [PickupTopic::StartsTomorrow]);
        assert!(matches(&agenda, &[], now, &days).is_empty());
    }
}
