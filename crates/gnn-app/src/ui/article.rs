//! 記事の本文。ニュースと「今日の予定」の画面で同じものを使う。
//!
//! 上に操作の帯 (戻る・既読の切り替え・元の記事を開く) を固定し、その下を 1 本のスクロールにして、
//! 最大 704px の段に、ゲームと日時・題名・予定のチップ・読み取った予定のカード・本文を並べる。
//! 本文 (TextView) は外側のスクロールの中で自然な高さにし、中で別にスクロールさせない。
//!
//! 開いている記事の本文が取得で変わったときは、読んでいる位置を動かさないよう、先頭にいなければ
//! 差し替えずに「記事が更新されました」を出す (予定だけの変化はすぐ反映する)。

use chrono::{Local, Utc};
use gnn_core::text::{find_matches, mark_html};
use gnn_core::{Entry, EntrySchedule};
use gpui_kit::assets::IconName;
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::text::TextView;
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Icon, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    App, Context, ElementId, EventEmitter, FontWeight, HighlightStyle, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, ScrollHandle, SharedString,
    StatefulInteractiveElement as _, Styled as _, StyledText, Subscription, Task, Window, div,
    point, px,
};

use crate::format;
use crate::http_client::{ImageLoader, display_html, display_uri};
use crate::services::image_sources;
use crate::state::{AppEvent, AppState};
use crate::ui::components::{EmptyState, ScheduleCard, ScheduleChip, SourceAvatar, open_web_url};

/// 本文の段の最大幅 (日本語でおよそ 44 文字)。
const COLUMN_MAX_WIDTH: f32 = 704.;

/// 本文の画面から、それを置いた画面への知らせ。既読・未読の変化は [`AppEvent::ReadChanged`] で
/// すべての画面に届く。
#[derive(Debug, Clone)]
pub enum ArticleEvent {
    /// 記事を開いて本文を読み込んだ。選んだ記事を既読にするのはこのあと。
    Opened(i64),
    /// 狭いときの「お知らせに戻る」を押した。
    Back,
}

/// 開いている記事の中身。
struct Loaded {
    entry: Entry,
    /// 画面に渡す形にした本文 (検索語の印を付ける前)。
    html: String,
    schedule: Option<EntrySchedule>,
}

pub struct ArticleView {
    app: gpui_kit::Entity<AppState>,
    id: Option<i64>,
    /// 開いたが、まだ本文を読み込めていない記事。読み直し ([`ArticleView::refresh`]) で読み込みが
    /// 置き換わっても、最初に読み込めたときに `Opened` を知らせる。
    opening: Option<i64>,
    loaded: Option<Loaded>,
    /// 検索語の印を付けた本文。本文と検索語が同じなら作り直さない。
    marked: SharedString,
    query: String,
    /// 読んでいる間に取得で変わった本文 (「更新を見る」で差し替える)。
    pending: Option<Loaded>,
    /// 「お知らせに戻る」を出すか (狭いとき)。
    back_label: Option<SharedString>,
    /// 何も開いていないときの案内 (題と説明)。
    empty_hint: (&'static str, &'static str),
    /// 開こうとした記事が、使わない成人向けのソースの記事だった。
    hidden: bool,
    scroll: ScrollHandle,
    task: Task<()>,
    _subscription: Subscription,
}

impl EventEmitter<ArticleEvent> for ArticleView {}

impl ArticleView {
    pub fn new(app: gpui_kit::Entity<AppState>, cx: &mut Context<Self>) -> Self {
        let subscription = cx.subscribe(&app, |this, app, event, cx| match event {
            // ほかの画面 (一覧の一括の既読など) で変えた既読を合わせる。
            AppEvent::ReadChanged { ids, read } => {
                if let Some(id) = this.id.filter(|id| ids.contains(id)) {
                    this.set_read_local(id, *read, cx);
                }
            }
            // 使うソースが変わった。成人向けのソースを使うのをやめたなら、開いている記事を閉じる。
            AppEvent::EntriesChanged => {
                let hidden = this
                    .loaded
                    .as_ref()
                    .is_some_and(|loaded| app.read(cx).hides(&loaded.entry.summary.source_id));
                if hidden {
                    this.hide(cx);
                }
            }
            _ => {}
        });
        Self {
            _subscription: subscription,
            app,
            id: None,
            opening: None,
            loaded: None,
            marked: SharedString::default(),
            query: String::new(),
            pending: None,
            back_label: None,
            empty_hint: (
                "記事を選んでください",
                "一覧で記事を選ぶと、ここに本文が出ます。",
            ),
            hidden: false,
            scroll: ScrollHandle::new(),
            task: Task::ready(()),
        }
    }

    /// 何も開いていないときの案内。
    pub fn with_empty_hint(mut self, title: &'static str, description: &'static str) -> Self {
        self.empty_hint = (title, description);
        self
    }

    /// 開いている記事の既読。
    pub fn is_read(&self) -> Option<bool> {
        self.loaded.as_ref().map(|loaded| loaded.entry.summary.read)
    }

    /// 狭いときに出す「戻る」の文言。`None` なら出さない。
    pub fn set_back(&mut self, label: Option<&'static str>, cx: &mut Context<Self>) {
        let label = label.map(SharedString::from);
        if self.back_label != label {
            self.back_label = label;
            cx.notify();
        }
    }

    /// 検索語 (題名と本文で目立たせる)。
    pub fn set_query(&mut self, query: &str, cx: &mut Context<Self>) {
        if self.query != query {
            self.query = query.to_string();
            self.remark();
            cx.notify();
        }
    }

    /// 記事を開く。前に取れなかった本文の画像を読み直させる (記事を開き直せば直る)。
    pub fn show(&mut self, id: i64, cx: &mut Context<Self>) {
        if self.id != Some(id) {
            self.id = Some(id);
            self.loaded = None;
            self.pending = None;
            self.marked = SharedString::default();
            self.scroll.set_offset(point(px(0.), px(0.)));
        }
        self.opening = Some(id);
        self.load(id, cx);
    }

    /// 何も開いていない状態にする。
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.id = None;
        self.opening = None;
        self.loaded = None;
        self.pending = None;
        self.hidden = false;
        self.marked = SharedString::default();
        self.task = Task::ready(());
        cx.notify();
    }

    /// 開いている記事を、使わない成人向けのソースの記事として閉じる (本文も画像も出さない)。
    fn hide(&mut self, cx: &mut Context<Self>) {
        self.opening = None;
        self.loaded = None;
        self.pending = None;
        self.hidden = true;
        self.marked = SharedString::default();
        cx.notify();
    }

    /// 開いている記事を読み直す (記事や予定が変わったとき)。
    pub fn refresh(&mut self, cx: &mut Context<Self>) {
        if let Some(id) = self.id {
            self.load(id, cx);
        }
    }

    /// ほかの画面で既読を変えたときに合わせる (差し替えを待っている本文にも)。
    pub fn set_read_local(&mut self, id: i64, read: bool, cx: &mut Context<Self>) {
        let mut changed = false;
        for loaded in [self.loaded.as_mut(), self.pending.as_mut()]
            .into_iter()
            .flatten()
            .filter(|loaded| loaded.entry.summary.id == id)
        {
            changed |= loaded.entry.summary.read != read;
            loaded.entry.summary.read = read;
        }
        if changed {
            cx.notify();
        }
    }

    fn remark(&mut self) {
        self.marked = self
            .loaded
            .as_ref()
            .map(|loaded| mark_html(&loaded.html, &self.query))
            .unwrap_or_default()
            .into();
    }

    /// 記事を読む。開いたばかり (`opening`) なら、画像を読み直させ、読み込めたら `Opened` を知らせる。
    fn load(&mut self, id: i64, cx: &mut Context<Self>) {
        let services = self.app.read(cx).services().clone();
        self.task = cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    services.with_store(|store| -> anyhow::Result<_> {
                        Ok((store.entry(id)?, store.entry_schedule(id)?))
                    })
                })
                .await;
            let _ = this.update(cx, |this, cx| {
                if this.id != Some(id) {
                    return;
                }
                let (entry, schedule) = match result {
                    Ok(found) => found,
                    Err(err) => {
                        tracing::error!("記事を読めません: {err:#}");
                        return;
                    }
                };
                let Some(entry) = entry else {
                    this.loaded = None;
                    this.opening = None;
                    cx.notify();
                    return;
                };
                // 使わない成人向けのソースの記事は、通知や前に開いていた記事からでも出さない。
                if this.app.read(cx).hides(&entry.summary.source_id) {
                    this.hide(cx);
                    return;
                }
                this.hidden = false;
                let opened = this.opening == Some(id);
                if opened && let Some(loader) = cx.try_global::<ImageLoader>() {
                    let loader = loader.0.clone();
                    let uris: Vec<String> = image_sources(&entry.body_html)
                        .iter()
                        .map(|uri| display_uri(uri).into_owned())
                        .collect();
                    loader.retry_failed(&uris, cx);
                }
                let html = display_html(&entry.body_html).into_owned();
                let next = Loaded {
                    entry,
                    html,
                    schedule,
                };
                let body_changed = this
                    .loaded
                    .as_ref()
                    .is_some_and(|loaded| loaded.html != next.html);
                let reading = this.scroll.offset().y < px(0.);
                if !opened && body_changed && reading {
                    // 読んでいる位置を動かさない。予定と既読はすぐ反映する。
                    if let Some(loaded) = this.loaded.as_mut() {
                        loaded.schedule = next.schedule.clone();
                        loaded.entry.summary.read = next.entry.summary.read;
                    }
                    this.pending = Some(next);
                } else {
                    let same_body = !body_changed && this.loaded.is_some();
                    this.loaded = Some(next);
                    this.pending = None;
                    // 本文が同じなら作り直さない (予定だけが変わったときに表示を崩さないため)。
                    if !same_body {
                        this.remark();
                    }
                }
                if opened {
                    this.opening = None;
                    cx.emit(ArticleEvent::Opened(id));
                }
                cx.notify();
            });
        });
    }

    fn apply_pending(&mut self, cx: &mut Context<Self>) {
        if let Some(mut next) = self.pending.take() {
            // 帯を出したあとに変えた既読を引き継ぐ。
            if let Some(loaded) = &self.loaded {
                next.entry.summary.read = loaded.entry.summary.read;
            }
            self.loaded = Some(next);
            self.remark();
            self.scroll.set_offset(point(px(0.), px(0.)));
            cx.notify();
        }
    }

    /// 既読と未読を切り替える (`M` キーと操作の帯のボタン)。
    pub fn toggle_read(&mut self, cx: &mut Context<Self>) {
        let Some((id, read)) = self
            .loaded
            .as_ref()
            .map(|loaded| (loaded.entry.summary.id, !loaded.entry.summary.read))
        else {
            return;
        };
        if let Some(loaded) = self.loaded.as_mut() {
            loaded.entry.summary.read = read;
        }
        self.app
            .update(cx, |app, cx| app.set_read(vec![id], read, cx));
        cx.notify();
    }

    /// 元の記事をブラウザで開く (`O`・`Enter` と操作の帯のボタン)。
    pub fn open_original(&self, cx: &mut App) {
        if let Some(url) = self
            .loaded
            .as_ref()
            .and_then(|loaded| loaded.entry.summary.url.clone())
        {
            open_web_url(&url, cx);
        }
    }

    fn highlights(&self, text: &str, cx: &App) -> Vec<(std::ops::Range<usize>, HighlightStyle)> {
        let style = HighlightStyle {
            background_color: Some(cx.theme().warning.opacity(0.35)),
            ..HighlightStyle::default()
        };
        find_matches(text, &self.query)
            .into_iter()
            .map(|range| (range, style))
            .collect()
    }

    fn render_action_bar(&self, cx: &mut Context<Self>) -> gpui_kit::AnyElement {
        let theme = cx.theme();
        let summary = self.loaded.as_ref().map(|loaded| &loaded.entry.summary);
        let read = summary.is_some_and(|summary| summary.read);
        let has_url = summary.is_some_and(|summary| summary.url.is_some());
        h_flex()
            .flex_none()
            .h(px(44.))
            .px_4()
            .gap_2()
            .border_b_1()
            .border_color(theme.border)
            .when_some(self.back_label.clone(), |this, label| {
                this.child(
                    Button::new("article-back")
                        .small()
                        .ghost()
                        .icon(IconName::ArrowLeft)
                        .label(label)
                        .on_click(cx.listener(|_, _, _, cx| cx.emit(ArticleEvent::Back))),
                )
            })
            .child(div().flex_1())
            .when(summary.is_some(), |this| {
                this.child(
                    Button::new("article-toggle-read")
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
                        .tooltip("既読・未読を切り替える (M)")
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_read(cx))),
                )
                .child(
                    Button::new("article-open-original")
                        .small()
                        .outline()
                        .icon(IconName::ExternalLink)
                        .label("元の記事を開く")
                        .disabled(!has_url)
                        .tooltip(if has_url {
                            "ブラウザで開く (O)"
                        } else {
                            "元の記事の URL がありません"
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.open_original(cx))),
                )
            })
            .into_any_element()
    }
}

impl Render for ArticleView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let action_bar = self.render_action_bar(cx);
        let Some(loaded) = &self.loaded else {
            let empty = if self.hidden {
                div().size_full().child(
                    EmptyState::new(IconName::EyeOff, "このお知らせは表示しません").description(
                        "成人向けのソースのお知らせです。設定の「成人向けのソースを使う」を入れているときだけ表示します。",
                    ),
                )
            } else if self.id.is_some() {
                div().size_full()
            } else {
                // 何も開いていない本文の欄は画面でいちばん大きな空きなので、アプリのロゴを置く。
                div()
                    .size_full()
                    .child(EmptyState::with_logo(self.empty_hint.0).description(self.empty_hint.1))
            };
            return v_flex()
                .size_full()
                .when(self.back_label.is_some(), |this| this.child(action_bar))
                .child(empty)
                .into_any_element();
        };
        let summary = &loaded.entry.summary;
        let app = self.app.read(cx);
        let source = app.source(&summary.source_id);
        let name = source.map_or(summary.source_id.clone(), |source| source.name.clone());
        let icon = source.and_then(|source| source.icon.clone());
        let now = Utc::now();
        let badges = app
            .agenda()
            .badges(summary.id, now, &AppState::days(now), &Local);
        let card = loaded
            .schedule
            .clone()
            .filter(ScheduleCard::has_content)
            .map(|schedule| {
                ScheduleCard::new(
                    summary.id,
                    summary.source_id.clone(),
                    schedule,
                    loaded.entry.body_checked_at,
                )
            });
        let title_highlights = self.highlights(&summary.title, cx);
        let body = if loaded.entry.body_html.trim().is_empty() {
            EmptyState::new(IconName::FileText, "本文はありません")
                .description("元の記事を開いて読んでください。")
                .into_any_element()
        } else {
            TextView::html(
                ElementId::NamedInteger("article".into(), summary.id as u64),
                self.marked.clone(),
            )
            .scrollable(false)
            .selectable(true)
            .on_link_click(|url, _, _, cx| {
                if url.starts_with("mailto:") {
                    cx.open_url(url);
                } else {
                    open_web_url(url, cx);
                }
            })
            .w_full()
            .into_any_element()
        };

        let column = v_flex()
            .w_full()
            .max_w(px(COLUMN_MAX_WIDTH))
            .px_6()
            .pt_6()
            .pb_12()
            .gap_5()
            .child(
                v_flex()
                    .gap_3()
                    .child(
                        h_flex()
                            .gap_2()
                            .text_size(px(13.))
                            .line_height(px(20.))
                            .text_color(theme.muted_foreground)
                            .child(SourceAvatar::new(name.clone(), icon).small())
                            .child(div().font_medium().text_color(theme.foreground).child(name))
                            .child("·")
                            .child(format::full_time(summary.published, summary.first_seen_at)),
                    )
                    .child(
                        div()
                            .text_size(px(24.))
                            .line_height(px(34.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(
                                StyledText::new(summary.title.clone())
                                    .with_highlights(title_highlights),
                            ),
                    )
                    .when(!badges.is_empty(), |this| {
                        this.child(h_flex().flex_wrap().gap_1p5().children(
                            badges.into_iter().enumerate().map(|(index, badge)| {
                                ScheduleChip::new(
                                    ElementId::NamedInteger("article-badge".into(), index as u64),
                                    badge,
                                )
                            }),
                        ))
                    }),
            )
            .children(card)
            .child(
                div()
                    .w_full()
                    .text_size(px(16.))
                    .line_height(px(28.))
                    .child(body),
            );

        v_flex()
            .size_full()
            .child(action_bar)
            .when(self.pending.is_some(), |this| {
                this.child(
                    h_flex()
                        .flex_none()
                        .gap_2()
                        .px_4()
                        .py_2()
                        .border_b_1()
                        .border_color(theme.border)
                        .bg(theme.muted)
                        .text_sm()
                        .child(Icon::new(IconName::RefreshCw).small())
                        .child(div().flex_1().child("記事が更新されました"))
                        .child(
                            Button::new("article-apply-update")
                                .xsmall()
                                .outline()
                                .label("更新を見る")
                                .on_click(cx.listener(|this, _, _, cx| this.apply_pending(cx))),
                        ),
                )
            })
            .child(
                div()
                    .id("article-frame")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(
                        div()
                            .id("article-scroll")
                            .size_full()
                            .track_scroll(&self.scroll)
                            .overflow_y_scroll()
                            .child(h_flex().w_full().justify_center().child(column)),
                    )
                    .vertical_scrollbar(&self.scroll),
            )
            .into_any_element()
    }
}
