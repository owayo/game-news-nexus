//! 画面の骨組み: 見出し・カード・設定の行・入力の行・空の案内・コード・情報の枠・切り替え。

use gpui_kit::assets::IconName;
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, ElementId, IntoElement, ParentElement, RenderOnce, SharedString, Styled,
    Window, div, px,
};

use super::AppLogo;
use crate::ui::theme::Palette;

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
                    .child(div().text_xl().font_semibold().child(self.title))
                    .when_some(self.description, |this, description| {
                        this.child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child(description),
                        )
                    }),
            )
            .child(h_flex().flex_none().gap_2().children(self.actions))
    }
}

/// カード。地の上に置く面で、見出し (題・説明・操作) と中身を持つ。`divided` にすると、
/// 中身の 1 つずつを区切り線で分けた行にする (設定の行など)。
#[derive(IntoElement)]
pub struct Card {
    title: Option<SharedString>,
    description: Option<SharedString>,
    actions: Vec<AnyElement>,
    children: Vec<AnyElement>,
    divided: bool,
}

impl Card {
    pub fn new() -> Self {
        Self {
            title: None,
            description: None,
            actions: Vec::new(),
            children: Vec::new(),
            divided: false,
        }
    }

    pub fn title(mut self, title: impl Into<SharedString>) -> Self {
        self.title = Some(title.into());
        self
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.actions.push(action.into_any_element());
        self
    }

    pub fn divided(mut self) -> Self {
        self.divided = true;
        self
    }
}

impl ParentElement for Card {
    fn extend(&mut self, elements: impl IntoIterator<Item = AnyElement>) {
        self.children.extend(elements);
    }
}

impl RenderOnce for Card {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let palette = Palette::of(cx);
        let has_header = self.title.is_some() || !self.actions.is_empty();
        let header = has_header.then(|| {
            h_flex()
                .w_full()
                .gap_3()
                .items_start()
                .px_5()
                .pt_4()
                .when(!self.divided, |this| this.pb_1())
                .child(
                    v_flex()
                        .flex_1()
                        .min_w_0()
                        .gap_0p5()
                        .when_some(self.title, |this, title| {
                            this.child(
                                div()
                                    .text_base()
                                    .line_height(px(24.))
                                    .font_semibold()
                                    .child(title),
                            )
                        })
                        .when_some(self.description, |this, description| {
                            this.child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child(description),
                            )
                        }),
                )
                .when(!self.actions.is_empty(), |this| {
                    this.child(h_flex().flex_none().gap_2().children(self.actions))
                })
        });
        let body = if self.divided {
            v_flex()
                .w_full()
                .when(has_header, |this| this.pt_1())
                .children(self.children.into_iter().enumerate().map(|(index, child)| {
                    div()
                        .w_full()
                        .px_5()
                        .when(index > 0, |this| {
                            this.border_t_1().border_color(palette.divider)
                        })
                        .child(child)
                }))
        } else {
            v_flex()
                .w_full()
                .gap_3()
                .px_5()
                .pb_5()
                .when(!has_header, |this| this.pt_5())
                .when(has_header, |this| this.pt_2())
                .children(self.children)
        };
        v_flex()
            .w_full()
            .bg(palette.surface)
            .border_1()
            .border_color(theme.border)
            .rounded(px(12.))
            .children(header)
            .child(body)
    }
}

/// 設定の 1 行。左にラベルと説明、右に操作。狭いとき (`stacked`) は操作をラベルの下に置く。
#[derive(IntoElement)]
pub struct SettingRow {
    label: SharedString,
    description: Option<SharedString>,
    error: Option<SharedString>,
    control: AnyElement,
    stacked: bool,
}

impl SettingRow {
    pub fn new(label: impl Into<SharedString>, control: impl IntoElement) -> Self {
        Self {
            label: label.into(),
            description: None,
            error: None,
            control: control.into_any_element(),
            stacked: false,
        }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn error(mut self, error: Option<impl Into<SharedString>>) -> Self {
        self.error = error.map(Into::into);
        self
    }

    pub fn stacked(mut self, stacked: bool) -> Self {
        self.stacked = stacked;
        self
    }
}

impl RenderOnce for SettingRow {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let text = v_flex()
            .flex_1()
            .min_w_0()
            .gap_0p5()
            .child(div().text_sm().font_medium().child(self.label))
            .when_some(self.description, |this, description| {
                this.child(
                    div()
                        .text_xs()
                        .line_height(px(18.))
                        .text_color(theme.muted_foreground)
                        .child(description),
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
            });
        if self.stacked {
            v_flex()
                .w_full()
                .gap_2()
                .py_3p5()
                .child(text)
                .child(self.control)
                .into_any_element()
        } else {
            h_flex()
                .w_full()
                .min_h(px(64.))
                .gap_6()
                .py_3()
                .child(text)
                .child(
                    h_flex()
                        .flex_none()
                        .justify_end()
                        .max_w(px(320.))
                        .child(self.control),
                )
                .into_any_element()
        }
    }
}

/// 入力の 1 行 (取得の定義の編集)。ラベル・入力欄・説明・誤りを縦に並べる。
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
            .gap_1p5()
            .child(div().text_sm().font_medium().child(self.label))
            .child(self.control)
            .when_some(self.help, |this, help| {
                this.child(
                    div()
                        .text_xs()
                        .line_height(px(18.))
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

/// 中身がないときの案内。
#[derive(IntoElement)]
pub struct EmptyState {
    emblem: Emblem,
    title: SharedString,
    description: Option<SharedString>,
    action: Option<AnyElement>,
}

/// 案内の上に置く絵。
enum Emblem {
    /// 丸の中のアイコン。中身がないわけ (検索・絞り込み・未取得など) を表す。
    Icon(IconName),
    /// アプリのロゴ。何も開いていない本文の欄のように、画面の顔になる大きな空きに置く。
    Logo,
}

impl EmptyState {
    pub fn new(icon: IconName, title: impl Into<SharedString>) -> Self {
        Self::with_emblem(Emblem::Icon(icon), title)
    }

    /// アイコンの代わりにアプリのロゴを出す案内。
    pub fn with_logo(title: impl Into<SharedString>) -> Self {
        Self::with_emblem(Emblem::Logo, title)
    }

    fn with_emblem(emblem: Emblem, title: impl Into<SharedString>) -> Self {
        Self {
            emblem,
            title: title.into(),
            description: None,
            action: None,
        }
    }

    pub fn description(mut self, description: impl Into<SharedString>) -> Self {
        self.description = Some(description.into());
        self
    }

    pub fn action(mut self, action: impl IntoElement) -> Self {
        self.action = Some(action.into_any_element());
        self
    }
}

impl RenderOnce for EmptyState {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let emblem = match self.emblem {
            Emblem::Icon(icon) => div()
                .size(px(48.))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(theme.muted)
                .text_color(theme.muted_foreground)
                .child(Icon::new(icon).size(px(22.)))
                .into_any_element(),
            // 明るいテーマではタイルの淡い地が背景になじむので、タイルの角丸 (一辺の約 21%) に合わせた
            // 影で浮かせる。
            Emblem::Logo => div()
                .mb_1()
                .rounded(px(15.))
                .shadow_md()
                .child(AppLogo::new(px(72.)))
                .into_any_element(),
        };
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .gap_3()
            .p_8()
            .child(emblem)
            .child(
                div()
                    .text_base()
                    .font_semibold()
                    .text_center()
                    .child(self.title),
            )
            .when_some(self.description, |this, description| {
                this.child(
                    div()
                        .text_sm()
                        .text_center()
                        .max_w(px(380.))
                        .text_color(theme.muted_foreground)
                        .child(description),
                )
            })
            .children(self.action)
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
            .p_4()
            .rounded(px(8.))
            .border_1()
            .border_color(theme.border)
            .bg(theme.muted.opacity(0.5))
            .font_family(theme.mono_font_family.clone())
            .text_size(theme.mono_font_size)
            .child(self.text)
    }
}

/// 情報の小さな枠 (「最終成功」「今日 08:12 · 30 分前」のような項目名と値)。
#[derive(IntoElement)]
pub struct StatItem {
    label: SharedString,
    value: AnyElement,
    note: Option<SharedString>,
}

impl StatItem {
    pub fn new(label: impl Into<SharedString>, value: impl IntoElement) -> Self {
        Self {
            label: label.into(),
            value: value.into_any_element(),
            note: None,
        }
    }

    pub fn note(mut self, note: impl Into<SharedString>) -> Self {
        self.note = Some(note.into());
        self
    }
}

impl RenderOnce for StatItem {
    fn render(self, _: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let palette = Palette::of(cx);
        v_flex()
            .flex_1()
            .min_w(px(150.))
            .gap_1()
            .px_4()
            .py_3()
            .rounded(px(10.))
            .border_1()
            .border_color(theme.border)
            .bg(palette.surface)
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(self.label),
            )
            .child(div().text_sm().font_medium().child(self.value))
            .when_some(self.note, |this, note| {
                this.child(
                    div()
                        .text_xs()
                        .line_height(px(18.))
                        .text_color(theme.muted_foreground)
                        .child(note),
                )
            })
    }
}

/// 切り替え (「すべて／未読」「システム／ライト／ダーク」など)。
pub fn segmented(
    id: impl Into<ElementId>,
    labels: &[&'static str],
    selected: usize,
    on_change: impl Fn(&usize, &mut Window, &mut App) + 'static,
) -> TabBar {
    TabBar::new(id)
        .segmented()
        .small()
        .selected_index(selected)
        .children(labels.iter().map(|label| Tab::new().label(*label)))
        .on_click(on_change)
}
