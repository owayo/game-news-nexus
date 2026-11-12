//! 複数の画面で使う共通の部品。見た目を揃えるため、同じ表示はここの部品で作る。

use gnn_core::RunStatus;
use gnn_core::agenda::{self, SourceMark, Tone};
use gpui_kit::assets::IconName;
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ElementId, Hsla, ImageSource, InteractiveElement as _, IntoElement, ObjectFit,
    ParentElement, Pixels, RenderOnce, SharedString, StatefulInteractiveElement as _, Styled,
    StyledImage as _, Window, div, hsla, img, px, rgb,
};

/// ソースのアイコン。角丸の正方形で、画像がなければ名前の頭文字を出す。
#[derive(IntoElement)]
pub struct SourceAvatar {
    name: SharedString,
    icon: Option<SharedString>,
    size: Pixels,
}

impl SourceAvatar {
    pub fn new(name: impl Into<SharedString>, icon: Option<impl Into<SharedString>>) -> Self {
        Self {
            name: name.into(),
            icon: icon
                .map(Into::into)
                .filter(|icon: &SharedString| !icon.trim().is_empty()),
            size: px(28.),
        }
    }

    /// 一覧の行に置く大きさ。
    pub fn small(mut self) -> Self {
        self.size = px(20.);
        self
    }

    /// 設定画面で確かめる大きさ。
    pub fn large(mut self) -> Self {
        self.size = px(44.);
        self
    }
}

impl RenderOnce for SourceAvatar {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let tile = div()
            .size(self.size)
            .flex_shrink_0()
            .rounded(self.size * 0.24)
            .overflow_hidden();
        match self.icon {
            // gpui は親の角丸で子を切り抜かないので、画像そのものを角丸で描く。
            Some(icon) => tile.bg(cx.theme().muted).child(
                img(icon_source(icon.trim()))
                    .size_full()
                    .rounded(self.size * 0.24)
                    .object_fit(ObjectFit::Cover),
            ),
            None => tile
                .bg(name_color(&self.name))
                .flex()
                .items_center()
                .justify_center()
                .text_color(gpui_kit::white())
                .text_size(self.size * 0.5)
                .font_semibold()
                .child(initial(&self.name)),
        }
    }
}

/// 名前の頭文字。英字は大文字にする。
fn initial(name: &str) -> String {
    name.trim()
        .chars()
        .next()
        .map(|ch| ch.to_uppercase().collect())
        .unwrap_or_default()
}

/// 名前から決まる背景色。同じ名前なら同じ色になる。
fn name_color(name: &str) -> Hsla {
    let hash = name.bytes().fold(0u32, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte.into())
    });
    hsla((hash % 360) as f32 / 360.0, 0.45, 0.5, 1.0)
}

/// ブラウザで開く。http と https 以外は開かない
/// (取得した記事の URL で `ms-settings:` などの手元のアプリを起動されないように)。
pub fn open_web_url(url: &str, cx: &mut App) {
    match url::Url::parse(url) {
        Ok(parsed) if matches!(parsed.scheme(), "http" | "https") => cx.open_url(parsed.as_str()),
        _ => tracing::warn!("http / https 以外の URL は開きません: {url}"),
    }
}

/// アイコンの指定を画像の読み込み元にする。URL はそのまま、それ以外は手元のファイル。
fn icon_source(icon: &str) -> ImageSource {
    if icon.starts_with("http://") || icon.starts_with("https://") {
        ImageSource::from(crate::http_client::display_uri(icon).into_owned())
    } else {
        let path = icon.strip_prefix("file:///").unwrap_or(icon);
        ImageSource::from(std::path::PathBuf::from(path))
    }
}

/// 未読数などの小さな件数表示。0 のときは何も出さない。
#[derive(IntoElement)]
pub struct CountPill {
    count: usize,
}

impl CountPill {
    pub fn new(count: usize) -> Self {
        Self { count }
    }
}

impl RenderOnce for CountPill {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let text = if self.count > 999 {
            "999+".to_string()
        } else {
            self.count.to_string()
        };
        div().when(self.count > 0, |this| {
            this.px_1p5()
                .min_w(px(20.))
                .h(px(18.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .text_xs()
                .bg(theme.muted)
                .text_color(theme.muted_foreground)
                .child(text)
        })
    }
}

/// ソースの取得状況のアイコン。
#[derive(IntoElement)]
pub struct StatusIcon {
    status: Option<RunStatus>,
    running: bool,
    enabled: bool,
}

impl StatusIcon {
    pub fn new(status: Option<RunStatus>, running: bool, enabled: bool) -> Self {
        Self {
            status,
            running,
            enabled,
        }
    }

    pub fn label(status: Option<RunStatus>, running: bool, enabled: bool) -> &'static str {
        if running {
            "取得中"
        } else if !enabled {
            "使っていない"
        } else {
            match status {
                Some(RunStatus::Ok) => "正常",
                Some(RunStatus::Partial) => "一部失敗",
                Some(RunStatus::Failed) => "失敗",
                None => "未取得",
            }
        }
    }
}

impl RenderOnce for StatusIcon {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (icon, color) = if self.running {
            (IconName::LoaderCircle, theme.primary)
        } else if !self.enabled {
            (IconName::CircleDashed, theme.muted_foreground)
        } else {
            match self.status {
                Some(RunStatus::Ok) => (IconName::CircleCheck, theme.success),
                Some(RunStatus::Partial) => (IconName::TriangleAlert, theme.warning),
                Some(RunStatus::Failed) => (IconName::CircleAlert, theme.danger),
                None => (IconName::Circle, theme.muted_foreground),
            }
        };
        Icon::new(icon).small().text_color(color)
    }
}

/// 画面の見出し。左に題と説明、右に操作。
#[derive(IntoElement)]
pub struct PageHeader {
    title: SharedString,
    description: Option<SharedString>,
    actions: Vec<AnyElement>,
}

impl PageHeader {
    pub fn new(title: impl Into<SharedString>) -> Self {
        Self {
            title: title.into(),
            description: None,
            actions: Vec::new(),
        }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }
}

impl RenderOnce for PageHeader {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        h_flex()
            .w_full()
            .gap_3()
            .px_4()
            .py_3()
            .border_b_1()
            .border_color(theme.border)
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(div().text_lg().font_semibold().child(self.title))
                    .when_some(self.description, |this, description| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(description),
                        )
                    }),
            )
            .child(h_flex().gap_2().children(self.actions))
    }
}

/// 設定フォームの 1 行。ラベルと説明、入力欄、誤りを縦に並べる。
#[derive(IntoElement)]
pub struct FormRow {
    label: SharedString,
    help: Option<SharedString>,
    error: Option<SharedString>,
    control: AnyElement,
}

impl FormRow {
    pub fn new(label: impl Into<SharedString>, control: impl IntoElement) -> Self {
        Self {
            label: label.into(),
            help: None,
            error: None,
            control: control.into_any_element(),
        }
    }

    pub fn help(mut self, help: impl Into<SharedString>) -> Self {
        self.help = Some(help.into());
        self
    }

    pub fn error(mut self, error: Option<impl Into<SharedString>>) -> Self {
        self.error = error.map(Into::into);
        self
    }
}

impl RenderOnce for FormRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .w_full()
            .gap_1()
            .child(div().text_sm().font_medium().child(self.label))
            .child(self.control)
            .when_some(self.help, |this, help| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(help),
                )
            })
            .when_some(self.error, |this, error| {
                this.child(
                    h_flex()
                        .gap_1()
                        .text_xs()
                        .text_color(theme.danger)
                        .child(Icon::new(IconName::CircleAlert).xsmall())
                        .child(error),
                )
            })
    }
}

/// 区切りの見出し。
#[derive(IntoElement)]
pub struct SectionLabel {
    text: SharedString,
}

impl SectionLabel {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self { text: text.into() }
    }
}

impl RenderOnce for SectionLabel {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        div()
            .pt_2()
            .text_xs()
            .font_semibold()
            .text_color(cx.theme().muted_foreground)
            .child(self.text)
    }
}

/// 中身がないときの案内。
#[derive(IntoElement)]
pub struct EmptyState {
    icon: IconName,
    title: SharedString,
    description: Option<SharedString>,
}

impl EmptyState {
    pub fn new(icon: IconName, title: impl Into<SharedString>) -> Self {
        Self {
            icon,
            title: title.into(),
            description: None,
        }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }
}

impl RenderOnce for EmptyState {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_2()
            .p_6()
            .text_color(theme.muted_foreground)
            .child(Icon::new(self.icon).large())
            .child(div().text_sm().font_medium().child(self.title))
            .when_some(self.description, |this, description| {
                this.child(
                    div()
                        .text_xs()
                        .text_center()
                        .max_w(px(360.))
                        .child(description),
                )
            })
    }
}

/// 小さな札。ソースの出どころ (上書き・手元) や、必要なアプリの版を示す。
#[derive(IntoElement)]
pub struct Badge {
    text: SharedString,
    tone: BadgeTone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BadgeTone {
    Muted,
    Accent,
    Warning,
}

impl Badge {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            tone: BadgeTone::Muted,
        }
    }

    pub fn tone(mut self, tone: BadgeTone) -> Self {
        self.tone = tone;
        self
    }
}

impl RenderOnce for Badge {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let (fg, bg) = match self.tone {
            BadgeTone::Muted => (theme.muted_foreground, theme.muted),
            BadgeTone::Accent => (theme.primary, theme.primary.opacity(0.12)),
            BadgeTone::Warning => (theme.warning, theme.warning.opacity(0.14)),
        };
        div()
            .flex_none()
            .px_1p5()
            .rounded(px(4.))
            .text_xs()
            .text_color(fg)
            .bg(bg)
            .child(self.text)
    }
}

/// 予定の色合いの文字色と背景色。
fn tone_colors(tone: Tone, cx: &App) -> (Hsla, Hsla) {
    let theme = cx.theme();
    let palette = |foreground, background| (rgb(foreground).into(), rgb(background).into());
    match tone {
        Tone::Active if theme.is_dark() => palette(0xFECACA, 0x7F1D1D),
        Tone::Active => palette(0x991B1B, 0xFEE2E2),
        Tone::Scheduled | Tone::Starting if theme.is_dark() => palette(0xBFDBFE, 0x1E3A8A),
        Tone::Scheduled | Tone::Starting => palette(0x1E40AF, 0xDBEAFE),
        Tone::Attention | Tone::Ending if theme.is_dark() => palette(0xFEF3C7, 0x78350F),
        Tone::Attention | Tone::Ending => palette(0x78350F, 0xFEF3C7),
        Tone::Done if theme.is_dark() => palette(0xD1FAE5, 0x064E3B),
        Tone::Done => palette(0x065F46, 0xD1FAE5),
        Tone::Muted => (theme.foreground, theme.muted),
    }
}

fn tone_icon(tone: Tone) -> IconName {
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

/// 予定のバッジ (「メンテ中 (17:00 まで予定)」「メンテ 11/12(木) 14:00–17:00」など)。
/// 一覧の行と記事の見出しで同じものを出す。文言と色合いは `gnn_core::agenda` が予定の表と
/// 今の時刻から決め、詳しい説明はツールチップに出す。
#[derive(IntoElement)]
pub struct ScheduleBadge {
    id: ElementId,
    badge: agenda::Badge,
}

impl ScheduleBadge {
    pub fn new(id: impl Into<ElementId>, badge: agenda::Badge) -> Self {
        Self {
            id: id.into(),
            badge,
        }
    }
}

impl RenderOnce for ScheduleBadge {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let (fg, bg) = tone_colors(self.badge.tone, cx);
        let detail = SharedString::from(self.badge.detail);
        h_flex()
            .id(self.id)
            .flex_none()
            .gap_1()
            .px_1p5()
            .h(px(20.))
            .rounded(px(4.))
            .border_1()
            .border_color(fg.opacity(0.5))
            .text_xs()
            .font_semibold()
            .text_color(fg)
            .bg(bg)
            .child(Icon::new(tone_icon(self.badge.tone)).xsmall())
            .child(self.badge.text)
            .tooltip(move |window, cx| Tooltip::new(detail.clone()).build(window, cx))
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
        let (fg, _) = tone_colors(self.mark.badge.tone, cx);
        let text = SharedString::from(match self.mark.level {
            agenda::MarkLevel::Now => {
                format!("{}\n{}", self.mark.badge.text, self.mark.badge.detail)
            }
            agenda::MarkLevel::Today => format!(
                "今日メンテナンスがあります: {}\n{}",
                self.mark.badge.text, self.mark.badge.detail
            ),
        });
        div()
            .id(self.id)
            .flex_none()
            .child(Icon::new(IconName::Wrench).xsmall().text_color(fg))
            .tooltip(move |window, cx| Tooltip::new(text.clone()).build(window, cx))
    }
}

/// 記事の詳細の「読み取った予定」の欄。メンテナンスと期間、その根拠 (記事の記述) を出す。
#[derive(IntoElement)]
pub struct SchedulePanel {
    source_id: String,
    schedule: gnn_core::EntrySchedule,
    /// 本文を最後に取得できた時刻。
    body_checked_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl SchedulePanel {
    pub fn new(
        source_id: impl Into<String>,
        schedule: gnn_core::EntrySchedule,
        body_checked_at: Option<chrono::DateTime<chrono::Utc>>,
    ) -> Self {
        Self {
            source_id: source_id.into(),
            schedule,
            body_checked_at,
        }
    }

    /// 欄を出すか。予定がなかった記事には出さない (読み取りに失敗した記事には、その旨を出す)。
    pub fn has_content(schedule: &gnn_core::EntrySchedule) -> bool {
        schedule.status != gnn_core::AnalysisStatus::Nothing
    }
}

impl RenderOnce for SchedulePanel {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        use gnn_core::agenda::{AgendaDetail, SourceSchedule};
        use gnn_core::schedule::{Evidence, Field};

        let theme = cx.theme().clone();
        let now = chrono::Utc::now();
        let tz = chrono::Local;
        let schedule = self.schedule;
        let muted = |text: String| {
            div()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child(text)
        };
        let evidence = |items: &[Evidence]| {
            items
                .iter()
                .take(3)
                .map(|evidence| {
                    let place = match evidence.field {
                        Field::Title => "題名",
                        Field::Body => "本文",
                    };
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .line_clamp(2)
                        .child(format!("{place}: 「{}」", evidence.text.trim()))
                })
                .collect::<Vec<_>>()
        };

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

        let mut rows: Vec<AnyElement> = Vec::new();
        for (position, item) in items.iter().enumerate() {
            match &item.detail {
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
                    // 延長は「延長 11/12(木) 17:00 → 19:00」、日程変更は変更ごとに前と後。
                    lines.extend(
                        maintenance
                            .changes
                            .iter()
                            .map(|change| agenda::change_text(change, now, &tz)),
                    );
                    let mut result =
                        format!("結果 {}", agenda::disposition_text(maintenance.disposition));
                    if let Some(by) = maintenance.resolved_by {
                        match schedule.resolvers.get(&by) {
                            Some(title) => result.push_str(&format!(" (「{title}」の報告による)")),
                            None => result.push_str(" (別の記事の報告による)"),
                        }
                    }
                    lines.push(result);
                    let title = match agenda::scope_text(&maintenance.scope) {
                        Some(scope) => format!("メンテナンス ({scope})"),
                        None => "メンテナンス".to_string(),
                    };
                    rows.push(
                        v_flex()
                            .gap_0p5()
                            .child(
                                h_flex()
                                    .gap_2()
                                    .child(div().text_sm().font_medium().child(title))
                                    .child(ScheduleBadge::new(
                                        ElementId::NamedInteger(
                                            "panel-badge".into(),
                                            position as u64,
                                        ),
                                        agenda::maintenance_badge(maintenance, now, &tz),
                                    )),
                            )
                            .children(lines.into_iter().map(|line| div().text_xs().child(line)))
                            .children(
                                written
                                    .map(|written| evidence(&written.evidence))
                                    .unwrap_or_default(),
                            )
                            .children(
                                ongoing
                                    .map(|ongoing| evidence(&ongoing.evidence))
                                    .unwrap_or_default(),
                            )
                            .into_any_element(),
                    );
                }
                AgendaDetail::Period(period) => {
                    let kind = agenda::period_kind_text(period.period.kind);
                    let title = match &period.period.label {
                        Some(label) => format!("{kind} ({label})"),
                        None => kind.to_string(),
                    };
                    // 期間は延長・日程変更の後のもの。変更は前と後を並べる。
                    let window = agenda::window_text(&period.period.window, now, &tz)
                        .unwrap_or_else(|| "日時の記載なし".to_string());
                    let changes: Vec<String> = period
                        .period
                        .changes
                        .iter()
                        .map(|change| agenda::change_text(change, now, &tz))
                        .collect();
                    rows.push(
                        v_flex()
                            .gap_0p5()
                            .child(div().text_sm().font_medium().child(title))
                            .child(div().text_xs().child(format!("期間 {window}")))
                            .children(changes.into_iter().map(|line| div().text_xs().child(line)))
                            .children(evidence(&period.period.evidence))
                            .into_any_element(),
                    );
                }
            }
        }

        let mut header = h_flex().gap_3().child(
            div()
                .text_xs()
                .font_semibold()
                .text_color(theme.muted_foreground)
                .child("読み取った予定"),
        );
        if !schedule.current {
            header = header.child(muted(
                "本文が変わったため読み直しています (前の本文から読んだ予定です)".into(),
            ));
        }
        if let Some(checked) = self.body_checked_at {
            header = header.child(muted(format!(
                "本文の確認 {}",
                crate::format::relative_time(checked, now)
            )));
        }

        div()
            .id("schedule-panel")
            .max_h(px(220.))
            .overflow_y_scroll()
            .px_5()
            .py_2()
            .border_b_1()
            .border_color(theme.border)
            .bg(theme.muted.opacity(0.25))
            .child(
                v_flex()
                    .gap_2()
                    .child(header)
                    .when(
                        schedule.status == gnn_core::AnalysisStatus::Failed,
                        |this| {
                            this.child(muted(format!(
                                "予定を読み取れませんでした: {}",
                                schedule.error.clone().unwrap_or_default()
                            )))
                        },
                    )
                    .children(rows)
                    .when(!read.notes.is_empty(), |this| {
                        this.child(muted(format!(
                            "決められなかったこと: {}",
                            read.notes
                                .iter()
                                .take(3)
                                .map(|note| note.message.as_str())
                                .collect::<Vec<_>>()
                                .join(" / ")
                        )))
                    }),
            )
    }
}

/// テスト取得の結果。段ごとの記録と、読めた記事の先頭を出す。
#[derive(IntoElement)]
pub struct RunResult {
    output: gnn_fetch::RunOutput,
}

impl RunResult {
    pub fn new(output: gnn_fetch::RunOutput) -> Self {
        Self { output }
    }
}

impl RenderOnce for RunResult {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme().clone();
        let output = self.output;
        let headline = match output.status {
            RunStatus::Failed => format!("失敗: {}", output.message.clone().unwrap_or_default()),
            _ => format!(
                "一覧で {} 件を見つけ、先頭 {} 件を読みました{}",
                output.listed,
                output.drafts.len(),
                output
                    .message
                    .as_deref()
                    .map(|message| format!(" ({message})"))
                    .unwrap_or_default()
            ),
        };
        let trace = output.trace.iter().map(|step| {
            h_flex()
                .gap_2()
                .items_start()
                .text_xs()
                .child(
                    Icon::new(if step.ok {
                        IconName::Check
                    } else {
                        IconName::X
                    })
                    .xsmall()
                    .text_color(if step.ok {
                        theme.success
                    } else {
                        theme.danger
                    }),
                )
                .child(
                    div()
                        .flex_none()
                        .w(px(150.))
                        .text_color(theme.muted_foreground)
                        .child(format!("{} · {}", step.stage.label(), step.title)),
                )
                .child(div().flex_1().min_w_0().child(step.detail.clone()))
        });
        let now = chrono::Utc::now();
        let drafts = output.drafts.iter().map(|draft| {
            let body = draft
                .body_html
                .as_deref()
                .map(gnn_core::text::html_to_text)
                .unwrap_or_default();
            let preview: String = body.chars().take(140).collect();
            let meta = format!(
                "{}{}{}",
                draft
                    .published
                    .map(|published| crate::format::full_time(Some(published), now))
                    .unwrap_or_else(|| "日時なし".into()),
                if draft.image_url.is_some() {
                    " · 画像あり"
                } else {
                    ""
                },
                draft
                    .note
                    .as_deref()
                    .map(|note| format!(" · {note}"))
                    .unwrap_or_default(),
            );
            v_flex()
                .gap_0p5()
                .pt_2()
                .border_t_1()
                .border_color(theme.border)
                .child(div().text_sm().font_medium().child(draft.title.clone()))
                .child(
                    div()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(meta),
                )
                .when(!preview.is_empty(), |this| {
                    this.child(
                        div()
                            .text_xs()
                            .line_clamp(3)
                            .child(preview.replace('\n', " ")),
                    )
                })
        });
        v_flex()
            .gap_2()
            .p_3()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted.opacity(0.4))
            .child(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .font_medium()
                    .child(StatusIcon::new(Some(output.status), false, true))
                    .child(headline),
            )
            .when(!output.trace.is_empty(), |this| {
                this.child(v_flex().gap_1().children(trace))
            })
            .children(output.warnings.iter().take(5).map(|warning| {
                div()
                    .text_xs()
                    .text_color(theme.warning)
                    .child(warning.clone())
            }))
            .children(drafts)
    }
}

/// 読むだけのコード (YAML など) の表示。
#[derive(IntoElement)]
pub struct CodeBlock {
    text: SharedString,
}

impl CodeBlock {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self { text: text.into() }
    }
}

impl RenderOnce for CodeBlock {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        div()
            .w_full()
            .p_3()
            .rounded(theme.radius)
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted.opacity(0.3))
            .font_family(theme.mono_font_family.clone())
            .text_size(theme.mono_font_size)
            .child(self.text)
    }
}
