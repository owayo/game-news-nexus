//! テスト取得の結果。

use gnn_core::RunStatus;
use gpui_kit::assets::IconName;
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{App, IntoElement, ParentElement, RenderOnce, Styled, Window, div, px};

use crate::ui::components::StatusIcon;
use crate::ui::theme::Palette;

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
        let palette = Palette::of(cx);
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
                .line_height(px(18.))
                .child(
                    Icon::new(if step.ok {
                        IconName::Check
                    } else {
                        IconName::X
                    })
                    .xsmall()
                    .flex_none()
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
                .pt_3()
                .border_t_1()
                .border_color(palette.divider)
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
                            .line_height(px(18.))
                            .line_clamp(3)
                            .child(preview.replace('\n', " ")),
                    )
                })
        });
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
                    .text_sm()
                    .font_semibold()
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
