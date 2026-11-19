//! 小さな印: 件数・取得状況・札。

use gnn_core::RunStatus;
use gpui_kit::assets::IconName;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, IntoElement, ParentElement, RenderOnce, SharedString, Styled, Window, div, px,
};

use crate::ui::theme::Palette;

/// 未読数・予定の件数。0 のときは何も出さない。ふだんは中立の色で、選んだ項目の中では
/// ローズの淡い色にする (全部をローズにすると、どれも同じ強さになる)。
#[derive(IntoElement)]
pub struct CountPill {
    count: usize,
    active: bool,
}

impl CountPill {
    pub fn new(count: usize) -> Self {
        Self {
            count,
            active: false,
        }
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }
}

impl RenderOnce for CountPill {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let palette = Palette::of(cx);
        let text = if self.count > 999 {
            "999+".to_string()
        } else {
            self.count.to_string()
        };
        div().flex_none().when(self.count > 0, |this| {
            this.px_1p5()
                .min_w(px(24.))
                .h(px(20.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .text_xs()
                .map(|this| {
                    if self.active {
                        this.bg(palette.surface)
                            .text_color(palette.selected_foreground)
                    } else {
                        this.bg(theme.muted).text_color(theme.muted_foreground)
                    }
                })
                .child(text)
        })
    }
}

/// ソースの取得状況のアイコン。色だけでなく形 (チェック・三角・丸の感嘆符) でも区別する。
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
                Some(RunStatus::Failed) => "取得に失敗",
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

/// 小さな札。ソースの出どころ (カタログ・自分用に変更・自作) や、必要なアプリの版を示す。
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
        let palette = Palette::of(cx);
        let (fg, bg) = match self.tone {
            BadgeTone::Muted => (theme.muted_foreground, theme.muted),
            BadgeTone::Accent => (palette.selected_foreground, palette.selected),
            BadgeTone::Warning => palette.tone(gnn_core::agenda::Tone::Attention),
        };
        div()
            .flex_none()
            .h(px(20.))
            .px_1p5()
            .flex()
            .items_center()
            .rounded(px(6.))
            .text_xs()
            .text_color(fg)
            .bg(bg)
            .child(self.text)
    }
}
