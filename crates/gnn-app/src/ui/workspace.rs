//! ウィンドウの最上位。タイトルバー・サイドバー・各画面・ステータスバーを並べる。
//!
//! サイドバーは上に「すべてのお知らせ」「今日の予定」、その下にゲームの一覧 (ここだけスクロール)、
//! 下に「ソース」「設定」を固定する。ウィンドウが狭いとき ([`Layout`]) は 64px の細いナビにし、
//! ゲームはボタンから開く一覧で選ぶ。全体の取得はタイトルバーの「すべて取得」1 か所に置く。

use std::time::Duration;

use chrono::Utc;
use gnn_core::source::Channel;
use gpui_kit::assets::IconName;
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, TitleBar, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, FocusHandle, Focusable,
    InteractiveElement as _, IntoElement, ParentElement as _, Render, SharedString,
    StatefulInteractiveElement as _, Styled as _, Subscription, Task, Window, div, image_cache, px,
};

pub use crate::state::Page;

use crate::format;
use crate::http_client::{ImageLoader, WatchedImages};
use crate::state::AppState;
use crate::ui::Layout;
use crate::ui::actions::{
    CloseWindow, FocusSearch, REFRESH_TOOLTIP, Refresh, ShowAgenda, ShowNews, ShowSettings,
    ShowSources,
};
use crate::ui::agenda::AgendaPage;
use crate::ui::components::{AppLogo, CountPill, MaintenanceMark, NavItem, SourceAvatar};
use crate::ui::news::NewsPage;
use crate::ui::settings::SettingsPage;
use crate::ui::sources::SourcesPage;
use crate::ui::theme::Palette;

/// サイドバーの幅 (広いとき) と細いナビの幅。
const SIDEBAR_WIDTH: f32 = 248.;
const RAIL_WIDTH: f32 = 64.;

pub struct Workspace {
    app: Entity<AppState>,
    page: Page,
    news: Entity<NewsPage>,
    agenda: Entity<AgendaPage>,
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
        let agenda = cx.new(|cx| AgendaPage::new(app.clone(), cx));
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

        // 開き直したウィンドウでは、前に見ていた画面・ゲーム・記事に戻す。
        let view = app.read(cx).view().clone();
        news.update(cx, |news, cx| {
            news.restore(
                view.news_filter.clone(),
                view.news_schedule,
                view.news_selected,
                window,
                cx,
            )
        });

        Self {
            app,
            page: view.page,
            news,
            agenda,
            sources,
            settings,
            watched_images,
            focus_handle: cx.focus_handle(),
            _ticker: ticker,
            _subscriptions: subscriptions,
        }
    }

    /// ソース画面を出し、`id` のソースを選ぶ。開発用の `GNN_DEV_SOURCE` だけが使う
    /// (リリースビルドに入れると使われない関数の警告になる)。
    #[cfg(debug_assertions)]
    pub fn open_source(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        self.show_page(Page::Sources, cx);
        self.sources
            .update(cx, |sources, cx| sources.select(id, window, cx));
    }

    pub fn show_page(&mut self, page: Page, cx: &mut Context<Self>) {
        self.page = page;
        self.app
            .update(cx, |app, _| app.remember_view(|view| view.page = page));
        cx.notify();
    }

    fn show_news(&mut self, filter: Option<String>, window: &mut Window, cx: &mut Context<Self>) {
        // 絞り込みは NewsPage が覚える (通知から記事を開いたときも同じ)。
        self.news
            .update(cx, |news, cx| news.set_filter(filter, window, cx));
        self.show_page(Page::News, cx);
    }

    /// 取得に失敗しているソースを確かめる (ソース画面の「要確認」)。
    fn show_failing(&mut self, cx: &mut Context<Self>) {
        self.sources
            .update(cx, |sources, cx| sources.show_failing(cx));
        self.show_page(Page::Sources, cx);
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
        }
    }

    fn render_title_bar(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let app = self.app.read(cx);
        let running = app.running_count();
        TitleBar::new()
            .child(
                h_flex()
                    .gap_2()
                    .child(AppLogo::new(px(20.)))
                    .child(div().text_sm().font_semibold().child("Game News Nexus")),
            )
            .child(
                h_flex().px_2().child(
                    Button::new("title-refresh")
                        .small()
                        .ghost()
                        .icon(IconName::RefreshCw)
                        .label(if running > 0 {
                            format!("{running} 件を取得中")
                        } else {
                            "すべて取得".to_string()
                        })
                        .loading(running > 0)
                        .tooltip(REFRESH_TOOLTIP)
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.app.update(cx, |app, cx| app.fetch_now(None, cx))
                        })),
                ),
            )
    }

    fn render_sidebar(&self, collapsed: bool, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme().clone();
        let app = self.app.read(cx);
        let filter = self.news.read(cx).filter().map(str::to_string);
        let on_news = self.page == Page::News;
        let unread_total = app.unread_total(Channel::News);
        let today = app.today_count();

        // 細いナビでは件数を出せないので、名前 (ツールチップ) に入れ、アイコンに点を付ける。
        let label = |name: &str, count: usize, unit: &str| -> SharedString {
            if collapsed && count > 0 {
                format!("{name} ({unit} {count} 件)").into()
            } else {
                name.to_string().into()
            }
        };
        let failing = app.failing_count();
        let all_active = on_news && filter.is_none();
        let all = NavItem::new("nav-all", label("すべてのお知らせ", unread_total, "未読"))
            .leading(Icon::new(IconName::Inbox).small())
            .trailing(CountPill::new(unread_total).active(all_active))
            .dot((unread_total > 0).then_some(theme.primary))
            .active(all_active)
            .collapsed(collapsed)
            .on_click(cx.listener(|this, _, window, cx| this.show_news(None, window, cx)));
        let agenda_active = self.page == Page::Agenda;
        let agenda = NavItem::new("nav-agenda", label("今日の予定", today, "予定"))
            .leading(Icon::new(IconName::CalendarClock).small())
            .trailing(CountPill::new(today).active(agenda_active))
            .dot((today > 0).then_some(theme.primary))
            .active(agenda_active)
            .collapsed(collapsed)
            .on_click(cx.listener(|this, _, _, cx| this.show_page(Page::Agenda, cx)));
        let sources = NavItem::new("nav-sources", label("ソース", failing, "要確認"))
            .leading(Icon::new(IconName::Rss).small())
            .dot((failing > 0).then_some(theme.danger))
            .active(self.page == Page::Sources)
            .collapsed(collapsed)
            .when(failing > 0 && !collapsed, |this| {
                this.trailing(
                    Icon::new(IconName::CircleAlert)
                        .small()
                        .text_color(theme.danger),
                )
            })
            .on_click(cx.listener(|this, _, _, cx| this.show_page(Page::Sources, cx)));
        let settings = NavItem::new("nav-settings", "設定")
            .leading(Icon::new(IconName::Settings).small())
            .active(self.page == Page::Settings)
            .collapsed(collapsed)
            .on_click(cx.listener(|this, _, _, cx| this.show_page(Page::Settings, cx)));

        let games: Vec<(String, String, Option<String>, usize)> = app
            .enabled_sources()
            .filter(|source| source.channel == Channel::News)
            .map(|source| {
                (
                    source.id.clone(),
                    source.name.clone(),
                    source.icon.clone(),
                    app.unread(Channel::News, &source.id),
                )
            })
            .collect();

        let frame = v_flex()
            .h_full()
            .flex_none()
            .bg(theme.sidebar)
            .border_r_1()
            .border_color(theme.sidebar_border);

        if collapsed {
            let page = cx.entity().downgrade();
            let game_active = on_news && filter.is_some();
            let picker = Button::new("nav-games")
                .ghost()
                .icon(IconName::Gamepad2)
                .tooltip("ゲームを選ぶ")
                .when(game_active, |this| {
                    this.text_color(Palette::of(cx).selected_foreground)
                })
                .dropdown_menu(move |menu, _, _| {
                    let mut menu = menu.min_w(px(260.)).max_h(px(480.)).scrollable(true);
                    for (id, name, icon, unread) in games.clone() {
                        let page = page.clone();
                        let checked = filter.as_deref() == Some(id.as_str());
                        menu = menu.item(
                            PopupMenuItem::element(move |_, _| {
                                h_flex()
                                    .w_full()
                                    .gap_2()
                                    .child(SourceAvatar::new(name.clone(), icon.clone()).small())
                                    .child(div().flex_1().min_w_0().truncate().child(name.clone()))
                                    .child(CountPill::new(unread))
                            })
                            .checked(checked)
                            .on_click(move |_, window, cx| {
                                let id = id.clone();
                                let _ = page
                                    .update(cx, |this, cx| this.show_news(Some(id), window, cx));
                            }),
                        );
                    }
                    menu
                });
            return frame
                .w(px(RAIL_WIDTH))
                .items_center()
                .py_3()
                .gap_1()
                .child(all)
                .child(agenda)
                .child(picker)
                .child(div().flex_1())
                .child(sources)
                .child(settings)
                .into_any_element();
        }

        let game_items: Vec<AnyElement> = games
            .into_iter()
            .map(|(id, name, icon, unread)| {
                let active = on_news && filter.as_deref() == Some(id.as_str());
                // 今メンテナンスの予定の時間内か、今日メンテナンスがあれば印を出す (ストアの予定の表から)。
                let mark = app.maintenance_mark(&id).cloned();
                let mark_id = SharedString::from(format!("mark-{id}"));
                let target = id.clone();
                NavItem::new(
                    ElementId::Name(format!("nav-game-{id}").into()),
                    name.clone(),
                )
                .leading(SourceAvatar::new(name, icon).small())
                .when_some(mark, |this, mark| {
                    this.trailing(MaintenanceMark::new(mark_id, mark))
                })
                .trailing(CountPill::new(unread).active(active))
                .active(active)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.show_news(Some(target.clone()), window, cx)
                }))
                .into_any_element()
            })
            .collect();

        frame
            .w(px(SIDEBAR_WIDTH))
            .child(v_flex().px_3().pt_3().gap_0p5().child(all).child(agenda))
            .child(
                div()
                    .px_5()
                    .pt_5()
                    .pb_1p5()
                    .text_xs()
                    .font_semibold()
                    .text_color(theme.muted_foreground)
                    .child("ゲーム"),
            )
            .child(
                div()
                    .id("sidebar-games")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(v_flex().px_3().pb_2().gap_0p5().children(game_items)),
            )
            .child(
                v_flex()
                    .px_3()
                    .py_2()
                    .gap_0p5()
                    .border_t_1()
                    .border_color(theme.sidebar_border)
                    .child(sources)
                    .child(settings),
            )
            .into_any_element()
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
            // 成功した時刻と受け取られないよう、「確認」と書く (試した時刻も含む)。
            let last = app.last_cycle_at().or_else(|| {
                app.enabled_sources()
                    .filter_map(|source| {
                        app.state_of(&source.id)
                            .and_then(|state| state.last_attempt_at)
                    })
                    .max()
            });
            h_flex().child(match last {
                Some(at) => format!("最終確認 {}", format::relative_time(at, now)),
                None => "まだ取得していません".to_string(),
            })
        };
        let enabled = app.enabled_sources().count();
        let failing = app.failing_count();
        let next = app
            .next_due()
            .map(|at| format!("次回取得 {}", format::until(at, now)));

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
                    .child(format!("利用中 {enabled} ゲーム"))
                    .when(failing > 0, |this| {
                        this.child(
                            h_flex()
                                .id("status-failing")
                                .gap_1()
                                .cursor_pointer()
                                .text_color(theme.danger)
                                .child(Icon::new(IconName::CircleAlert).xsmall())
                                .child(format!("要確認 {failing}"))
                                .on_click(cx.listener(|this, _, _, cx| this.show_failing(cx))),
                        )
                    }),
            )
    }

    fn page_view(&self) -> AnyElement {
        match self.page {
            Page::News => self.news.clone().into_any_element(),
            Page::Agenda => self.agenda.clone().into_any_element(),
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let collapsed = Layout::of(window) != Layout::Wide;
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
            .on_action(cx.listener(|this, _: &ShowAgenda, _, cx| this.show_page(Page::Agenda, cx)))
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
                    .child(self.render_sidebar(collapsed, cx))
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
