//! サイドバーのナビの項目。

use std::rc::Rc;

use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{ActiveTheme as _, StyledExt as _, h_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ClickEvent, ElementId, Hsla, InteractiveElement as _, IntoElement,
    ParentElement, RenderOnce, Role, SharedString, StatefulInteractiveElement as _, Styled, Window,
    div, px,
};

use crate::ui::theme::Palette;

/// 押したときの処理。
type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// サイドバーの 1 行 (高さ 36px)。左にアイコンかゲームのアイコン、名前、右に印と件数。
/// 選んだ項目は淡いローズの背景と左の線、太字で示す (色を見分けなくても分かるように)。
/// `collapsed` (細いナビ) ではアイコンだけを出し、名前はツールチップに出す。
#[derive(IntoElement)]
pub struct NavItem {
    id: ElementId,
    label: SharedString,
    leading: Option<AnyElement>,
    trailing: Vec<AnyElement>,
    active: bool,
    collapsed: bool,
    /// 細いナビで、件数の代わりにアイコンの右上に出す点の色。
    dot: Option<Hsla>,
    on_click: Option<ClickHandler>,
}

impl NavItem {
    pub fn new(id: impl Into<ElementId>, label: impl Into<SharedString>) -> Self {
        Self {
            id: id.into(),
            label: label.into(),
            leading: None,
            trailing: Vec::new(),
            active: false,
            collapsed: false,
            dot: None,
            on_click: None,
        }
    }

    /// 細いナビで出す点 (未読がある・失敗しているなど)。
    pub fn dot(mut self, dot: Option<Hsla>) -> Self {
        self.dot = dot;
        self
    }

    pub fn leading(mut self, leading: impl IntoElement) -> Self {
        self.leading = Some(leading.into_any_element());
        self
    }

    pub fn trailing(mut self, trailing: impl IntoElement) -> Self {
        self.trailing.push(trailing.into_any_element());
        self
    }

    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    pub fn collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        self
    }

    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for NavItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let palette = Palette::of(cx);
        let active = self.active;
        // 名前が入りきらないかもしれないとき (細いナビ・長い名前) だけ、全文をツールチップに出す。
        let tooltip =
            (self.collapsed || display_width(&self.label) > 22).then(|| self.label.clone());
        h_flex()
            .id(self.id)
            .role(Role::Tab)
            .aria_label(self.label.clone())
            .aria_selected(active)
            .relative()
            .flex_none()
            .h(px(36.))
            .gap_2()
            .rounded(px(8.))
            .cursor_pointer()
            .text_sm()
            .map(|this| {
                if self.collapsed {
                    this.w(px(40.)).justify_center()
                } else {
                    this.w_full().pl_3().pr_2()
                }
            })
            .map(|this| {
                if active {
                    this.bg(palette.selected)
                        .text_color(palette.selected_foreground)
                        .font_semibold()
                } else {
                    this.text_color(theme.sidebar_foreground)
                        .hover(|style| style.bg(theme.secondary_hover))
                }
            })
            .when(active, |this| {
                this.child(
                    div()
                        .absolute()
                        .left_0()
                        .top(px(9.))
                        .w(px(3.))
                        .h(px(18.))
                        .rounded_full()
                        .bg(theme.primary),
                )
            })
            .children(self.leading)
            .when_some(self.dot.filter(|_| self.collapsed), |this, color| {
                this.child(
                    div()
                        .absolute()
                        .top(px(7.))
                        .right(px(8.))
                        .size(px(7.))
                        .rounded_full()
                        .bg(color),
                )
            })
            .when(!self.collapsed, |this| {
                this.child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(self.label.clone()),
                )
                .children(self.trailing)
            })
            .when_some(tooltip, |this, tooltip| {
                this.tooltip(move |window, cx| Tooltip::new(tooltip.clone()).build(window, cx))
            })
            .when_some(self.on_click, |this, handler| {
                this.on_click(move |event, window, cx| handler(event, window, cx))
            })
    }
}

/// 表示の幅のめやす (全角を 2、半角を 1 と数える)。
fn display_width(text: &str) -> usize {
    text.chars()
        .map(|ch| if ch.is_ascii() { 1 } else { 2 })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_full_width_characters_as_two() {
        assert_eq!(display_width("原神"), 4);
        assert_eq!(display_width("Twinkle☆Star Knights"), 21);
    }
}
