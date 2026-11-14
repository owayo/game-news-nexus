//! 予定の表示: チップ・サイドバーの印・記事の「読み取った予定」のカード。
//!
//! 文言と色合いは `gnn_core::agenda` が予定の表と今の時刻から決める。ここは見た目だけを持つ。

use chrono::{DateTime, Local, Utc};
use gnn_core::agenda::{self, AgendaDetail, SourceMark, SourceSchedule, Tone};
use gnn_core::schedule::{Evidence, Field};
use gnn_core::{AnalysisStatus, EntrySchedule};
use gpui_kit::assets::IconName;
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ElementId, InteractiveElement as _, IntoElement, ParentElement, RenderOnce,
    SharedString, StatefulInteractiveElement as _, Styled, Window, div, px,
};

use crate::ui::theme::Palette;

/// 予定の状態のアイコン。色だけに頼らず、形でも状態を示す。
pub fn tone_icon(tone: Tone) -> IconName {
    match tone {
        Tone::Active => IconName::Wrench,
        Tone::Scheduled => IconName::CalendarClock,
        Tone::Starting => IconName::Play,
        Tone::Ending => IconName::Flag,
        Tone::Attention => IconName::TriangleAlert,
        Tone::Done => IconName::CircleCheck,
        Tone::Muted => IconName::Calendar,
    }
}

/// 予定のチップ (「メンテ中 (17:00 まで予定)」「ガチャ 今日 23:59 まで」)。枠のない淡い色の札で、
/// 詳しい説明はツールチップに出す。幅が足りなければ文言を省略する。
#[derive(IntoElement)]
pub struct ScheduleChip {
    id: ElementId,
    badge: agenda::Badge,
}

impl ScheduleChip {
    pub fn new(id: impl Into<ElementId>, badge: agenda::Badge) -> Self {
        Self {
            id: id.into(),
            badge,
        }
    }
}

impl RenderOnce for ScheduleChip {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let (fg, bg) = Palette::of(cx).tone(self.badge.tone);
        let detail = SharedString::from(self.badge.detail);
        h_flex()
            .id(self.id)
            .min_w_0()
            .h(px(22.))
            .gap_1()
            .px(px(6.))
            .rounded(px(6.))
            .text_xs()
            .font_medium()
            .text_color(fg)
            .bg(bg)
            .child(Icon::new(tone_icon(self.badge.tone)).xsmall().flex_none())
            .child(div().min_w_0().truncate().child(self.badge.text))
            .tooltip(move |window, cx| Tooltip::new(detail.clone()).build(window, cx))
    }
}

/// 一覧の行に出しきれなかった予定の数 (「+2」)。ツールチップに文言を並べる。
#[derive(IntoElement)]
pub struct MoreChip {
    id: ElementId,
    texts: Vec<String>,
}

impl MoreChip {
    pub fn new(id: impl Into<ElementId>, texts: Vec<String>) -> Self {
        Self {
            id: id.into(),
            texts,
        }
    }
}

impl RenderOnce for MoreChip {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let tooltip = SharedString::from(self.texts.join("\n"));
        h_flex()
            .id(self.id)
            .flex_none()
            .h(px(22.))
            .px(px(6.))
            .rounded(px(6.))
            .text_xs()
            .text_color(theme.muted_foreground)
            .bg(theme.muted)
            .child(format!("+{}", self.texts.len()))
            .tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
    }
}

/// サイドバーのゲームの横の印。今メンテナンスの予定の時間内か、今日メンテナンスがあることを示す。
#[derive(IntoElement)]
pub struct MaintenanceMark {
    id: ElementId,
    mark: SourceMark,
}

impl MaintenanceMark {
    pub fn new(id: impl Into<ElementId>, mark: SourceMark) -> Self {
        Self {
            id: id.into(),
            mark,
        }
    }
}

impl RenderOnce for MaintenanceMark {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let (fg, bg) = Palette::of(cx).tone(self.mark.badge.tone);
        let now = self.mark.level == agenda::MarkLevel::Now;
        let text = SharedString::from(if now {
            format!("{}\n{}", self.mark.badge.text, self.mark.badge.detail)
        } else {
            format!(
                "今日メンテナンスがあります: {}\n{}",
                self.mark.badge.text, self.mark.badge.detail
            )
        });
        div()
            .id(self.id)
            .flex_none()
            .size(px(20.))
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(6.))
            .when(now, |this| this.bg(bg))
            .child(Icon::new(IconName::Wrench).xsmall().text_color(fg))
            .tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
    }
}

/// 記事の「読み取った予定」のカード。予定ごとに名前・状態のチップ・時間帯・直近の変更を出し、
/// 根拠の文・決められなかったこと・すべての変更は「根拠を表示」で開く。予定が 4 件以上なら先頭 3 件を
/// 出して、残りは「ほか N 件の予定を表示」で開く。開け閉めは記事ごとに覚える。
#[derive(IntoElement)]
pub struct ScheduleCard {
    entry_id: i64,
    source_id: String,
    schedule: EntrySchedule,
    /// 本文を最後に取得できた時刻。
    body_checked_at: Option<DateTime<Utc>>,
}

/// 最初に出す予定の数。
const FIRST_ITEMS: usize = 3;

impl ScheduleCard {
    pub fn new(
        entry_id: i64,
        source_id: impl Into<String>,
        schedule: EntrySchedule,
        body_checked_at: Option<DateTime<Utc>>,
    ) -> Self {
        Self {
            entry_id,
            source_id: source_id.into(),
            schedule,
            body_checked_at,
        }
    }

    /// カードを出すか。予定がなかった記事には出さない (読み取りに失敗した記事には、その旨を出す)。
    pub fn has_content(schedule: &EntrySchedule) -> bool {
        schedule.status != AnalysisStatus::Nothing
    }
}

/// 予定 1 件の表示の中身。
struct ItemView {
    icon: IconName,
    title: String,
    badge: Option<agenda::Badge>,
    /// いつも出す行 (時間帯・実施中の報告・直近の変更・結果)。
    lines: Vec<String>,
    /// 「根拠を表示」で出す行 (すべての変更)。
    history: Vec<String>,
    evidence: Vec<String>,
}

fn evidence_lines(items: &[Evidence]) -> Vec<String> {
    items
        .iter()
        .take(3)
        .map(|evidence| {
            let place = match evidence.field {
                Field::Title => "題名",
                Field::Body => "本文",
            };
            format!("{place}: 「{}」", evidence.text.trim())
        })
        .collect()
}

impl ScheduleCard {
    fn item_views(&self, now: DateTime<Utc>) -> Vec<ItemView> {
        let tz = Local;
        let schedule = &self.schedule;
        // 予定の表の行 (同じソースで突き合わせたあと)。表にまだなければ、記事の記述のまま出す。
        let read = schedule.schedule.clone().unwrap_or_default();
        let items = if schedule.items.is_empty() {
            agenda::build_items(
                &self.source_id,
                &[SourceSchedule {
                    entry_id: 0,
                    published: None,
                    first_seen_at: now,
                    schedule: read.clone(),
                }],
                &[],
            )
        } else {
            schedule.items.clone()
        };
        let days = agenda::Days::at(now, &tz);
        items
            .iter()
            .map(|item| match &item.detail {
                AgendaDetail::Maintenance(maintenance) => {
                    let written = read.maintenance.get(maintenance.index);
                    let mut lines = Vec::new();
                    if let Some(planned) = agenda::window_text(&maintenance.planned, now, &tz) {
                        lines.push(format!("予定 {planned}"));
                    }
                    if let Some(actual) = agenda::window_text(&maintenance.actual, now, &tz) {
                        lines.push(format!("実際 {actual}"));
                    }
                    // 「11/6(金) 15:00 の時点で実施中」(別の記事の報告のこともある)。
                    let ongoing = maintenance.ongoing_report();
                    if let Some(ongoing) = ongoing {
                        lines.push(agenda::ongoing_text(ongoing, now, &tz));
                    }
                    let changes: Vec<String> = maintenance
                        .changes
                        .iter()
                        .map(|change| agenda::change_text(change, now, &tz))
                        .collect();
                    // 直近の変更 (延長・日程変更) はいつも出し、それより前の変更は根拠と一緒に出す。
                    if let Some(last) = changes.last() {
                        lines.push(last.clone());
                    }
                    let mut result =
                        format!("結果 {}", agenda::disposition_text(maintenance.disposition));
                    if let Some(by) = maintenance.resolved_by {
                        match schedule.resolvers.get(&by) {
                            Some(title) => result.push_str(&format!(" (「{title}」の報告による)")),
                            None => result.push_str(" (別の記事の報告による)"),
                        }
                    }
                    lines.push(result);
                    let mut evidence = written
                        .map(|written| evidence_lines(&written.evidence))
                        .unwrap_or_default();
                    evidence.extend(
                        ongoing
                            .map(|ongoing| evidence_lines(&ongoing.evidence))
                            .unwrap_or_default(),
                    );
                    ItemView {
                        icon: IconName::Wrench,
                        title: match agenda::scope_text(&maintenance.scope) {
                            Some(scope) => format!("メンテナンス ({scope})"),
                            None => "メンテナンス".to_string(),
                        },
                        badge: Some(agenda::maintenance_badge(maintenance, now, &tz)),
                        history: changes
                            .iter()
                            .take(changes.len().saturating_sub(1))
                            .cloned()
                            .collect(),
                        lines,
                        evidence,
                    }
                }
                AgendaDetail::Period(period) => {
                    let kind = agenda::period_kind_text(period.period.kind);
                    // 期間は延長・日程変更の後のもの。変更は前と後を並べる。
                    let window = agenda::window_text(&period.period.window, now, &tz)
                        .unwrap_or_else(|| "日時の記載なし".to_string());
                    let changes: Vec<String> = period
                        .period
                        .changes
                        .iter()
                        .map(|change| agenda::change_text(change, now, &tz))
                        .collect();
                    let mut lines = vec![format!("期間 {window}")];
                    if let Some(last) = changes.last() {
                        lines.push(last.clone());
                    }
                    ItemView {
                        icon: IconName::CalendarDays,
                        title: match &period.period.label {
                            Some(label) => format!("{kind} ({label})"),
                            None => kind.to_string(),
                        },
                        badge: agenda::period_badge(&period.period, now, &days, &tz),
                        history: changes
                            .iter()
                            .take(changes.len().saturating_sub(1))
                            .cloned()
                            .collect(),
                        lines,
                        evidence: evidence_lines(&period.period.evidence),
                    }
                }
            })
            .collect()
    }
}

impl RenderOnce for ScheduleCard {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let palette = Palette::of(cx);
        let now = Utc::now();
        let key = self.entry_id as u64;
        let show_all = window.use_keyed_state(
            ElementId::NamedInteger("schedule-show-all".into(), key),
            cx,
            |_, _| false,
        );
        let show_details = window.use_keyed_state(
            ElementId::NamedInteger("schedule-show-details".into(), key),
            cx,
            |_, _| false,
        );
        let all = *show_all.read(cx);
        let details = *show_details.read(cx);
        let views = self.item_views(now);
        let hidden = views.len().saturating_sub(FIRST_ITEMS);
        let notes: Vec<String> = self
            .schedule
            .schedule
            .as_ref()
            .map(|read| {
                read.notes
                    .iter()
                    .take(3)
                    .map(|note| note.message.clone())
                    .collect()
            })
            .unwrap_or_default();
        let has_details = !notes.is_empty()
            || views
                .iter()
                .any(|view| !view.evidence.is_empty() || !view.history.is_empty());

        let muted = |text: String| {
            div()
                .text_xs()
                .line_height(px(18.))
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let status = if !self.schedule.current {
            Some("予定を更新しています (前の本文から読んだ予定です)".to_string())
        } else {
            self.body_checked_at
                .map(|checked| format!("本文の確認 {}", crate::format::relative_time(checked, now)))
        };
        let rows: Vec<AnyElement> = views
            .into_iter()
            .take(if all { usize::MAX } else { FIRST_ITEMS })
            .enumerate()
            .map(|(position, view)| {
                v_flex()
                    .gap_1()
                    .when(position > 0, |this| {
                        this.pt_3().border_t_1().border_color(palette.divider)
                    })
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Icon::new(view.icon)
                                    .small()
                                    .flex_none()
                                    .text_color(theme.muted_foreground),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_sm()
                                    .font_semibold()
                                    .child(view.title),
                            )
                            .when_some(view.badge, |this, badge| {
                                this.child(ScheduleChip::new(
                                    ElementId::NamedInteger("card-chip".into(), position as u64),
                                    badge,
                                ))
                            }),
                    )
                    .child(
                        v_flex()
                            .pl_6()
                            .gap_0p5()
                            .children(
                                view.lines
                                    .into_iter()
                                    .map(|line| div().text_sm().line_height(px(20.)).child(line)),
                            )
                            .when(details, |this| {
                                this.children(view.history.into_iter().map(&muted))
                                    .children(view.evidence.into_iter().map(&muted))
                            }),
                    )
                    .into_any_element()
            })
            .collect();

        v_flex()
            .w_full()
            .gap_3()
            .p_4()
            .rounded(px(12.))
            .border_1()
            .border_color(theme.border)
            .bg(palette.surface)
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Icon::new(IconName::CalendarClock)
                            .small()
                            .text_color(theme.primary),
                    )
                    .child(div().text_sm().font_semibold().child("読み取った予定"))
                    .child(div().flex_1())
                    .when_some(status, |this, status| this.child(muted(status))),
            )
            .when(self.schedule.status == AnalysisStatus::Failed, |this| {
                this.child(muted(format!(
                    "予定を読み取れませんでした: {}",
                    self.schedule.error.clone().unwrap_or_default()
                )))
            })
            .children(rows)
            .when(details && !notes.is_empty(), |this| {
                this.child(muted(format!(
                    "決められなかったこと: {}",
                    notes.join(" / ")
                )))
            })
            .when(hidden > 0 || has_details, |this| {
                this.child(
                    h_flex()
                        .gap_1()
                        .when(hidden > 0 && !all, |this| {
                            let show_all = show_all.clone();
                            this.child(
                                Button::new("schedule-more")
                                    .xsmall()
                                    .ghost()
                                    .icon(IconName::ChevronDown)
                                    .label(format!("ほか {hidden} 件の予定を表示"))
                                    .on_click(move |_, _, cx| {
                                        show_all.update(cx, |value, cx| {
                                            *value = true;
                                            cx.notify();
                                        })
                                    }),
                            )
                        })
                        .child(div().flex_1())
                        .when(has_details, |this| {
                            let show_details = show_details.clone();
                            this.child(
                                Button::new("schedule-details")
                                    .xsmall()
                                    .ghost()
                                    .icon(if details {
                                        IconName::ChevronUp
                                    } else {
                                        IconName::ChevronDown
                                    })
                                    .label(if details {
                                        "根拠を閉じる"
                                    } else {
                                        "根拠を表示"
                                    })
                                    .on_click(move |_, _, cx| {
                                        show_details.update(cx, |value, cx| {
                                            *value = !*value;
                                            cx.notify();
                                        })
                                    }),
                            )
                        }),
                )
            })
    }
}
