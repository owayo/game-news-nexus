//! ウィンドウの最上位。タイトルバー・サイドバー・各画面・ステータスバーを並べる。

use std::time::Duration;

use chrono::Utc;
use gnn_core::source::Channel;
use gpui_kit::assets::IconName;
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::sidebar::{
    Sidebar, SidebarGroup, SidebarHeader, SidebarMenu, SidebarMenuItem,
};
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Subscription, Task, Window, div, image_cache, px,
};

use crate::format;
use crate::http_client::{ImageLoader, WatchedImages};
use crate::state::AppState;
use crate::ui::actions::{
    CloseWindow, FocusSearch, Refresh, ShowNews, ShowReleases, ShowSettings, ShowSources,
};
use crate::ui::components::{CountPill, MaintenanceMark};
use crate::ui::news::NewsPage;
use crate::ui::releases::ReleasesPage;
use crate::ui::settings::SettingsPage;
use crate::ui::sources::SourcesPage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Page {
    News,
    Releases,
    Sources,
    Settings,
}

pub struct Workspace {
    app: Entity<AppState>,
    page: Page,
    news: Entity<NewsPage>,
    releases: Entity<ReleasesPage>,
    sources: Entity<SourcesPage>,
    settings: Entity<SettingsPage>,
    /// 画像の読み込みを見て、gpui が読めなかったキャッシュの画像を取り直させる (`image_cache` で根に置く)。
    watched_images: Option<Entity<WatchedImages>>,
    focus_handle: FocusHandle,
    _ticker: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl Workspace {
    pub fn new(app: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let news = cx.new(|cx| NewsPage::new(app.clone(), window, cx));
        let releases = cx.new(|cx| ReleasesPage::new(app.clone(), cx));
        let sources = cx.new(|cx| SourcesPage::new(app.clone(), window, cx));
        let settings = cx.new(|cx| SettingsPage::new(app.clone(), window, cx));
        let watched_images = cx
            .try_global::<ImageLoader>()
            .map(|loader| loader.0.clone())
            .map(|loader| cx.new(|_| WatchedImages(loader)));

        let subscriptions = vec![
            cx.observe(&app, |_, _, cx| cx.notify()),
            cx.observe_window_appearance(window, |this, window, cx| {
                if this.app.read(cx).settings().theme == gnn_core::ThemePreference::System {
                    gpui_kit::component::Theme::sync_system_appearance(Some(window), cx);
                }
            }),
        ];

        // 「3 分前」などの表示を進める。
        let ticker = cx.spawn(async move |this, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_secs(30))
                    .await;
                if this.update(cx, |_, cx| cx.notify()).is_err() {
                    break;
                }
            }
        });

        Self {
            app,
            page: Page::News,
            news,
            releases,
            sources,
            settings,
            watched_images,
            focus_handle: cx.focus_handle(),
            _ticker: ticker,
            _subscriptions: subscriptions,
        }
    }

    /// ソース画面を出し、`id` のソースを選ぶ。
    pub fn open_source(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.show_page(Page::Sources, cx);
        self.sources
            .update(cx, |sources, cx| sources.select(id, window, cx));
    }

    pub fn show_page(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        if page == Page::Releases {
            self.releases
                .update(cx, |releases, cx| releases.on_shown(cx));
        }
        cx.notify();
    }

    fn show_news(&mut self, filter: Option<String>, cx: &mut Context<Self>) {
        self.news.update(cx, |news, cx| news.set_filter(filter, cx));
        self.show_page(Page::News, cx);
    }

    /// 通知から記事を開く。
    pub fn reveal_entry(
        &mut self,
        entry_id: i64,
        channel: Channel,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match channel {
            Channel::News => {
                self.news
                    .update(cx, |news, cx| news.reveal(entry_id, window, cx));
                self.show_page(Page::News, cx);
            }
            Channel::Releases => self.show_page(Page::Releases, cx),
        }
    }

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let app = self.app.read(cx);
        let running = app.running_count();
        TitleBar::new()
            .child(
                h_flex()
                    .gap_2()
                    .child(
                        Icon::new(IconName::Newspaper)
                            .small()
                            .text_color(cx.theme().primary),
                    )
                    .child(div().text_sm().font_semibold().child("Game News Nexus")),
            )
            .child(
                h_flex()
                    .px_2()
                    .gap_1()
                    .when(running > 0, |this| {
                        this.child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("取得中 {running}")),
                        )
                    })
                    .child(
                        Button::new("title-refresh")
                            .small()
                            .ghost()
                            .icon(IconName::RefreshCw)
                            .loading(running > 0)
                            .tooltip("すべて取得")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.app.update(cx, |app, cx| app.fetch_now(None, cx))
                            })),
                    ),
            )
    }

    fn render_sidebar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let app = self.app.read(cx);
        let filter = self.news.read(cx).filter().map(str::to_string);
        let on_news = self.page == Page::News;

        let mut source_items = vec![
            SidebarMenuItem::new("すべて")
                .active(on_news && filter.is_none())
                .suffix({
                    let count = app.unread_total(Channel::News);
                    move |_, _| CountPill::new(count)
                })
                .on_click(cx.listener(|this, _, _, cx| this.show_news(None, cx))),
        ];
        for source in app
            .enabled_sources()
            .filter(|source| source.channel == Channel::News)
        {
            let id = source.id.clone();
            let count = app.unread(Channel::News, &source.id);
            // 今メンテナンスの予定の時間内か、今日メンテナンスがあれば印を出す (ストアの予定の表から)。
            let mark = app.maintenance_mark(&source.id).cloned();
            let mark_id = SharedString::from(format!("mark-{}", source.id));
            source_items.push(
                SidebarMenuItem::new(source.name.clone())
                    .active(on_news && filter.as_deref() == Some(source.id.as_str()))
                    .suffix(move |_, _| {
                        h_flex()
                            .gap_1()
                            .when_some(mark.clone(), |this, mark| {
                                this.child(MaintenanceMark::new(mark_id.clone(), mark))
                            })
                            .child(CountPill::new(count))
                    })
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.show_news(Some(id.clone()), cx)),
                    ),
            );
        }

        let release_count = app.unread_total(Channel::Releases);
        Sidebar::new("main-sidebar")
            .w(px(240.))
            .header(
                SidebarHeader::new().child(
                    h_flex()
                        .gap_2()
                        .child(Icon::new(IconName::Gamepad2).text_color(cx.theme().primary))
                        .child(div().text_sm().font_semibold().child("ゲームのお知らせ")),
                ),
            )
            .child(
                SidebarGroup::new("閲覧").child(
                    SidebarMenu::new()
                        .child(
                            SidebarMenuItem::new("ニュース")
                                .icon(IconName::Newspaper)
                                .default_open(true)
                                .click_to_toggle(false)
                                .active(on_news && filter.is_none())
                                .on_click(cx.listener(|this, _, _, cx| this.show_news(None, cx)))
                                .children(source_items),
                        )
                        .child(
                            SidebarMenuItem::new("新作・近日リリース")
                                .icon(IconName::Sparkles)
                                .active(self.page == Page::Releases)
                                .suffix(move |_, _| CountPill::new(release_count).emphasized(true))
                                .on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.show_page(Page::Releases, cx)
                                    }),
                                ),
                        ),
                ),
            )
            .child(
                SidebarGroup::new("管理").child(
                    SidebarMenu::new()
                        .child(
                            SidebarMenuItem::new("ソース")
                                .icon(IconName::Rss)
                                .active(self.page == Page::Sources)
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.show_page(Page::Sources, cx)),
                                ),
                        )
                        .child(
                            SidebarMenuItem::new("設定")
                                .icon(IconName::Settings)
                                .active(self.page == Page::Settings)
                                .on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.show_page(Page::Settings, cx)
                                    }),
                                ),
                        ),
                ),
            )
    }

    fn render_status_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let app = self.app.read(cx);
        let theme = cx.theme();
        let now = Utc::now();
        let running = app.running_count();
        let left = if running > 0 {
            h_flex()
                .gap_1()
                .child(Icon::new(IconName::LoaderCircle).xsmall())
                .child(format!("{running} 件のソースを取得中"))
        } else {
            let last = app.last_cycle_at().or_else(|| {
                app.enabled_sources()
                    .filter_map(|source| {
                        app.state_of(&source.id)
                            .and_then(|state| state.last_attempt_at)
                    })
                    .max()
            });
            h_flex().child(match last {
                Some(at) => format!("最終取得 {}", format::relative_time(at, now)),
                None => "まだ取得していません".to_string(),
            })
        };
        let enabled = app.enabled_sources().count();
        let failing = app.failing_count();
        let next = app
            .next_due()
            .map(|at| format!("次回 {}", format::until(at, now)));

        StatusBar::new()
            .left(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(left),
            )
            .right(
                h_flex()
                    .gap_3()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .when_some(next, |this, next| this.child(next))
                    .child(format!("ソース {enabled}/{}", app.sources().len()))
                    .when(failing > 0, |this| {
                        this.child(
                            div()
                                .text_color(theme.danger)
                                .child(format!("失敗 {failing}")),
                        )
                    }),
            )
    }

    fn page_view(&self) -> AnyElement {
        match self.page {
            Page::News => self.news.clone().into_any_element(),
            Page::Releases => self.releases.clone().into_any_element(),
            Page::Sources => self.sources.clone().into_any_element(),
            Page::Settings => self.settings.clone().into_any_element(),
        }
    }
}

impl Focusable for Workspace {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Render for Workspace {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = v_flex()
            .size_full()
            .bg(cx.theme().background)
            .text_color(cx.theme().foreground)
            .track_focus(&self.focus_handle)
            .key_context("Workspace")
            .on_action(cx.listener(|this, _: &Refresh, _, cx| {
                this.app.update(cx, |app, cx| app.fetch_now(None, cx))
            }))
            .on_action(cx.listener(|this, _: &FocusSearch, window, cx| {
                this.show_page(Page::News, cx);
                let handle = this.news.focus_handle(cx);
                window.focus(&handle, cx);
                window.dispatch_action(Box::new(FocusSearch), cx);
            }))
            .on_action(cx.listener(|this, _: &ShowNews, _, cx| this.show_page(Page::News, cx)))
            .on_action(
                cx.listener(|this, _: &ShowReleases, _, cx| this.show_page(Page::Releases, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ShowSources, _, cx| this.show_page(Page::Sources, cx)),
            )
            .on_action(
                cx.listener(|this, _: &ShowSettings, _, cx| this.show_page(Page::Settings, cx)),
            )
            .on_action(cx.listener(|_, _: &CloseWindow, window, cx| {
                crate::app::close_main_window(window, cx)
            }))
            .child(self.render_title_bar(cx))
            .child(
                h_flex()
                    .flex_1()
                    .min_h_0()
                    .items_stretch()
                    .child(self.render_sidebar(cx))
                    .child(div().flex_1().min_w_0().h_full().child(self.page_view())),
            )
            .child(self.render_status_bar(cx));
        // 下の `img` (本文・サムネイル・アイコン) の読み込みを、gpui の既定のまま見張る。
        match &self.watched_images {
            Some(watched) => image_cache(watched.clone())
                .size_full()
                .child(content)
                .into_any_element(),
            None => content.into_any_element(),
        }
    }
}
