//! ソース。お知らせを取得するゲームを選び、手元の定義を作って直す。
//!
//! 左にゲームの一覧 (カタログのグループごとと手元のソース。「すべて／利用中／要確認」で絞る)、右に選んだ
//! ソースを出す。カタログのソースは「概要」(取得の状況・記事の数・アイコン) を主にし、取得の定義 (YAML) は
//! 「取得の定義」のタブで読むだけにする。直すときは「自分用に編集」で手元の定義 (上書き) にする。新しい
//! ゲームはカタログへの PR で足してもらう。編集中の変更を保存せずにほかのソースへ移るときは確かめる。

use std::cell::RefCell;
use std::rc::Rc;

use chrono::{Duration, Local, Utc};
use gnn_core::RunStatus;
use gnn_core::catalog::{
    CATALOG_REPOSITORY, CatalogEntry, ResolvedSource, SourceOrigin, UnavailableEntry,
};
use gnn_core::source::{Channel, ListKind};
use gnn_fetch::RunOutput;
use gpui_kit::assets::IconName;
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{h_resizable, resizable_panel};
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{ActiveTheme as _, Icon, Sizable as _, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, ElementId, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, StatefulInteractiveElement as _,
    Styled as _, Subscription, Task, WeakEntity, Window, div, px,
};

use crate::format;
use crate::state::AppState;
use crate::ui::Layout;
use crate::ui::components::{
    Badge, BadgeTone, Card, CodeBlock, CountPill, EmptyState, PageHeader, RunResult, SourceAvatar,
    StatItem, StatusIcon, open_web_url, segmented,
};
use crate::ui::icon_cropper::pick_and_crop;
use crate::ui::source_editor::{EditorEvent, EditorMode, SourceEditor};
use crate::ui::theme::Palette;

/// 選んでいるもの。
#[derive(Debug, Clone, PartialEq, Eq)]
enum Selection {
    /// 使えるソース (カタログ・上書き・手元)。
    Source(String),
    /// このアプリでは使えないカタログのソース。
    Unavailable(String),
    /// 読めない手元の定義のファイル。
    Broken(String),
    /// 新しく作っている手元のソース。
    New,
}

/// 一覧の絞り込み。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ListFilter {
    All,
    Enabled,
    /// 取得に失敗した・一部失敗した on のソースと、読めない手元の定義。
    Attention,
}

impl ListFilter {
    const LABELS: [&'static str; 3] = ["すべて", "利用中", "要確認"];

    fn index(self) -> usize {
        match self {
            ListFilter::All => 0,
            ListFilter::Enabled => 1,
            ListFilter::Attention => 2,
        }
    }

    fn from_index(index: usize) -> Self {
        match index {
            1 => ListFilter::Enabled,
            2 => ListFilter::Attention,
            _ => ListFilter::All,
        }
    }
}

/// カタログのソースの詳細のタブ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DetailTab {
    Overview,
    Definition,
}

enum TestState {
    Running,
    Done(Box<RunOutput>),
}

/// 移る前に確かめる操作 (保存していない変更があるとき)。
type Next = Box<dyn FnOnce(&mut SourcesPage, &mut Window, &mut Context<SourcesPage>)>;

pub struct SourcesPage {
    app: Entity<AppState>,
    selected: Option<Selection>,
    editor: Option<Entity<SourceEditor>>,
    search: Entity<InputState>,
    query: String,
    filter: ListFilter,
    tab: DetailTab,
    /// 狭いときに詳細を出しているか。
    showing_detail: bool,
    /// カタログのソースのテスト取得。ソース ID と結果。
    test: Option<(String, TestState)>,
    test_task: Task<()>,
    _subscriptions: Vec<Subscription>,
    editor_subscription: Option<Subscription>,
}

impl SourcesPage {
    pub fn new(app: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search = cx.new(|cx| InputState::new(window, cx).placeholder("ゲーム名で検索"));
        let subscriptions = vec![
            cx.observe(&app, |_, _, cx| cx.notify()),
            cx.subscribe(&search, |this, state, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    this.query = state.read(cx).value().trim().to_lowercase();
                    cx.notify();
                }
            }),
        ];
        Self {
            app,
            selected: None,
            editor: None,
            search,
            query: String::new(),
            filter: ListFilter::All,
            tab: DetailTab::Overview,
            showing_detail: false,
            test: None,
            test_task: Task::ready(()),
            _subscriptions: subscriptions,
            editor_subscription: None,
        }
    }

    /// 取得に失敗しているソースを出す (ステータスバーの「要確認」)。
    pub fn show_failing(&mut self, cx: &mut Context<Self>) {
        self.filter = ListFilter::Attention;
        self.showing_detail = false;
        cx.notify();
    }

    /// 保存していない変更があれば確かめてから `next` を行う。
    fn guard(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
        next: impl FnOnce(&mut Self, &mut Window, &mut Context<Self>) + 'static,
    ) {
        let Some(editor) = self
            .editor
            .clone()
            .filter(|editor| editor.read(cx).is_dirty())
        else {
            next(self, window, cx);
            return;
        };
        let name = editor.read(cx).display_name();
        let page = cx.entity().downgrade();
        let next: Rc<RefCell<Option<Next>>> = Rc::new(RefCell::new(Some(Box::new(next))));
        window.open_dialog(cx, move |dialog, _, cx| {
            let theme = cx.theme();
            let run = {
                let page = page.clone();
                let next = next.clone();
                move |save: bool, window: &mut Window, cx: &mut App| {
                    window.close_dialog(cx);
                    let Some(next) = next.borrow_mut().take() else {
                        return;
                    };
                    let _ = page.update(cx, |page, cx| {
                        if save
                            && let Some(editor) = page.editor.clone()
                            && !editor.update(cx, |editor, cx| editor.save(window, cx))
                        {
                            // 保存できなければ移らない (誤りは編集画面に出る)。
                            return;
                        }
                        next(page, window, cx);
                    });
                }
            };
            let discard = run.clone();
            dialog
                .title("保存していない変更があります")
                .w(px(440.))
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child(format!(
                            "「{name}」の変更を保存しますか？ 保存しないと変更は失われます。"
                        )),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("unsaved-cancel")
                                .small()
                                .ghost()
                                .label("キャンセル")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("unsaved-discard")
                                .small()
                                .outline()
                                .label("破棄して移る")
                                .on_click(move |_, window, cx| discard(false, window, cx)),
                        )
                        .child(
                            Button::new("unsaved-save")
                                .small()
                                .primary()
                                .label("保存して移る")
                                .on_click(move |_, window, cx| run(true, window, cx)),
                        ),
                )
        });
    }

    /// ソースを選ぶ。手元の定義があれば編集画面、カタログだけなら概要を出す。
    pub fn select(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let id = id.to_string();
        self.guard(window, cx, move |this, window, cx| {
            this.open(&id, window, cx)
        });
    }

    fn open(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let app = self.app.read(cx);
        let local = app
            .local_file(id)
            .and_then(|file| Some((file.source.as_ref().ok()?.clone(), file.text.clone())));
        let in_catalog = app.catalog().get(id).is_some();
        if self.selected.as_ref() != Some(&Selection::Source(id.to_string())) {
            self.tab = DetailTab::Overview;
        }
        self.selected = Some(Selection::Source(id.to_string()));
        self.showing_detail = true;
        self.editor = None;
        self.editor_subscription = None;
        if let Some((source, text)) = local {
            let mode = if in_catalog {
                EditorMode::Override
            } else {
                EditorMode::Local
            };
            self.open_editor(source, Some(text), mode, window, cx);
        } else if cfg!(debug_assertions) && std::env::var_os("GNN_DEV_TEST").is_some() {
            // 開発用: 選んだカタログのソースのテスト取得をすぐ始める (画面を操作せずに確かめるため)。
            if let Some(entry) = self.app.read(cx).catalog().get(id).cloned() {
                self.tab = DetailTab::Definition;
                self.run_catalog_test(&entry, cx);
            }
        }
        cx.notify();
    }

    fn select_other(&mut self, selection: Selection, window: &mut Window, cx: &mut Context<Self>) {
        self.guard(window, cx, move |this, _, cx| {
            this.selected = Some(selection);
            this.showing_detail = true;
            this.editor = None;
            this.editor_subscription = None;
            cx.notify();
        });
    }

    fn open_editor(
        &mut self,
        source: gnn_core::SourceConfig,
        text: Option<String>,
        mode: EditorMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let app = self.app.clone();
        let editor = cx.new(|cx| SourceEditor::new(app, source, text, mode, window, cx));
        self.editor_subscription =
            Some(
                cx.subscribe_in(&editor, window, |this, _, event, window, cx| match event {
                    EditorEvent::Saved(id) => {
                        this.selected = Some(Selection::Source(id.clone()));
                        cx.notify();
                    }
                    EditorEvent::Deleted(id) => {
                        let id = id.clone();
                        this.editor = None;
                        this.editor_subscription = None;
                        // カタログの上書きを消したなら、カタログの定義を出す。
                        if this.app.read(cx).resolved(&id).is_some() {
                            this.open(&id, window, cx);
                        } else {
                            this.selected = None;
                            this.showing_detail = false;
                            cx.notify();
                        }
                    }
                }),
            );
        self.editor = Some(editor);
        cx.notify();
    }

    fn add_blank(&mut self, kind: ListKind, window: &mut Window, cx: &mut Context<Self>) {
        self.guard(window, cx, move |this, window, cx| {
            let id = this.app.read(cx).unique_source_id("my_source");
            this.selected = Some(Selection::New);
            this.showing_detail = true;
            this.open_editor(
                gnn_core::templates::blank(kind, id),
                None,
                EditorMode::New,
                window,
                cx,
            );
        });
    }

    fn open_add_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let page = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .title("ソースを追加")
                .w(px(600.))
                .child(add_dialog_content(page.clone(), cx))
        });
    }

    /// カタログのソースを手元に写し、編集画面を開く。
    fn copy_to_local(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let result = self.app.update(cx, |app, cx| app.copy_to_local(id, cx));
        match result {
            Ok(()) => {
                self.open(id, window, cx);
                window.push_notification(
                    Notification::success(
                        "自分用に写しました。保存した定義がカタログの定義の代わりに使われます。",
                    ),
                    cx,
                );
            }
            Err(err) => {
                window.push_notification(Notification::error(format!("写せません: {err:#}")), cx)
            }
        }
    }

    fn set_enabled(&mut self, id: &str, enabled: bool, cx: &mut Context<Self>) {
        let result = self
            .app
            .update(cx, |app, cx| app.set_enabled(id, enabled, cx));
        if let Err(err) = result {
            tracing::error!("設定を保存できません: {err:#}");
        }
    }

    fn run_catalog_test(&mut self, entry: &CatalogEntry, cx: &mut Context<Self>) {
        let id = entry.source.id.clone();
        self.test = Some((id.clone(), TestState::Running));
        let receiver = self
            .app
            .read(cx)
            .services()
            .test_fetch(entry.source.clone(), true, 5);
        self.test_task = cx.spawn(async move |this, cx| {
            let output = receiver.await;
            let _ = this.update(cx, |this, cx| {
                this.test = match output {
                    Ok(output) => Some((id, TestState::Done(Box::new(output)))),
                    Err(_) => None,
                };
                cx.notify();
            });
        });
        cx.notify();
    }

    fn pick_icon(&mut self, id: String, name: String, window: &mut Window, cx: &mut Context<Self>) {
        let icons_dir = self.app.read(cx).services().paths().icons_dir();
        let app = self.app.clone();
        pick_and_crop(
            id.clone(),
            name,
            icons_dir,
            window,
            cx,
            move |path, window, cx| {
                let icon = path.display().to_string();
                let result = app.update(cx, |app, cx| app.set_icon(&id, Some(icon), cx));
                let note = match result {
                    Ok(()) => Notification::success("アイコンを設定しました"),
                    Err(err) => Notification::error(format!("アイコンを保存できません: {err:#}")),
                };
                window.push_notification(note, cx);
            },
        );
    }

    /// 検索に合うか。
    fn matches(&self, texts: &[&str]) -> bool {
        self.query.is_empty()
            || texts
                .iter()
                .any(|text| text.to_lowercase().contains(&self.query))
    }

    /// 絞り込みに合うか。
    fn passes(&self, source: &ResolvedSource, cx: &App) -> bool {
        match self.filter {
            ListFilter::All => true,
            ListFilter::Enabled => source.enabled,
            ListFilter::Attention => {
                source.enabled
                    && self
                        .app
                        .read(cx)
                        .state_of(source.id())
                        .and_then(|state| state.last_status)
                        .is_some_and(|status| status != RunStatus::Ok)
            }
        }
    }

    /// 一覧の行の 2 行目 (取得の状況)。
    fn status_text(&self, source: &ResolvedSource, cx: &App) -> String {
        let app = self.app.read(cx);
        let id = source.id();
        let running = app.is_running(id);
        let state = app.state_of(id);
        let label = StatusIcon::label(
            state.and_then(|state| state.last_status),
            running,
            source.enabled,
        );
        if !source.enabled || running {
            return label.to_string();
        }
        match state.and_then(|state| state.last_attempt_at) {
            Some(at) => format!("{label} · {}", format::relative_time(at, Utc::now())),
            None => label.to_string(),
        }
    }

    fn row_frame(
        &self,
        id: ElementId,
        selected: bool,
        cx: &App,
    ) -> gpui_kit::Stateful<gpui_kit::Div> {
        let theme = cx.theme();
        let palette = Palette::of(cx);
        h_flex()
            .id(id)
            .relative()
            .w_full()
            .min_h(px(60.))
            .gap_3()
            .px_4()
            .py_2p5()
            .cursor_pointer()
            .border_b_1()
            .border_color(palette.divider)
            .when(selected, |this| {
                this.bg(theme.list_active).child(
                    div()
                        .absolute()
                        .left_0()
                        .top_0()
                        .bottom_0()
                        .w(px(3.))
                        .bg(theme.primary),
                )
            })
            .when(!selected, |this| {
                this.hover(|style| style.bg(theme.list_hover))
            })
    }

    fn render_row(&self, source: &ResolvedSource, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let app = self.app.read(cx);
        let config = &source.config;
        let state = app.state_of(&config.id);
        let running = app.is_running(&config.id);
        let selected = self.selected == Some(Selection::Source(config.id.clone()));
        let unread = if source.enabled {
            app.unread(config.channel, &config.id)
        } else {
            0
        };
        let id = config.id.clone();
        let toggle_id = config.id.clone();
        let status = self.status_text(source, cx);

        self.row_frame(
            ElementId::Name(SharedString::from(format!("source-{}", config.id))),
            selected,
            cx,
        )
        .child(SourceAvatar::new(config.name.clone(), config.icon.clone()))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(
                    h_flex()
                        .gap_1p5()
                        .child(
                            div()
                                .text_sm()
                                .font_medium()
                                .truncate()
                                .when(!source.enabled, |this| {
                                    this.text_color(theme.muted_foreground)
                                })
                                .child(config.name.clone()),
                        )
                        .map(|this| match source.origin {
                            SourceOrigin::Override => {
                                this.child(Badge::new("自分用に変更").tone(BadgeTone::Accent))
                            }
                            SourceOrigin::Local => this.child(Badge::new("自作")),
                            SourceOrigin::Catalog => this,
                        })
                        // 成人向けの版 (設定で同意したときだけ出る)。
                        .when(config.adult, |this| this.child(Badge::new("成人向け")))
                        .child(CountPill::new(unread)),
                )
                .child(
                    h_flex()
                        .gap_1()
                        .text_xs()
                        .text_color(theme.muted_foreground)
                        .child(StatusIcon::new(
                            state.and_then(|state| state.last_status),
                            running,
                            source.enabled,
                        ))
                        .child(div().truncate().child(status)),
                ),
        )
        .child(
            Switch::new(ElementId::Name(SharedString::from(format!(
                "enabled-{}",
                config.id
            ))))
            .small()
            .checked(source.enabled)
            .tooltip(if source.enabled {
                "使っている (off にすると取得も表示もしない)"
            } else {
                "使う"
            })
            .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                this.set_enabled(&toggle_id, *checked, cx)
            })),
        )
        .on_click(cx.listener(move |this, _, window, cx| this.select(&id, window, cx)))
        .into_any_element()
    }

    fn render_unavailable_row(
        &self,
        entry: &UnavailableEntry,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let selected = self.selected == Some(Selection::Unavailable(entry.id.clone()));
        let id = entry.id.clone();
        self.row_frame(
            ElementId::Name(SharedString::from(format!("unavailable-{}", entry.id))),
            selected,
            cx,
        )
        .child(SourceAvatar::new(entry.name.clone(), None::<String>))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .truncate()
                .text_color(theme.muted_foreground)
                .child(entry.name.clone()),
        )
        .child(
            Badge::new(match &entry.required_app_version {
                Some(version) => format!("アプリ {version} 以降"),
                None => "新しいアプリが必要".into(),
            })
            .tone(BadgeTone::Warning),
        )
        .on_click(cx.listener(move |this, _, window, cx| {
            this.select_other(Selection::Unavailable(id.clone()), window, cx)
        }))
        .into_any_element()
    }

    fn render_broken_row(&self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let selected = self.selected == Some(Selection::Broken(id.to_string()));
        let select_id = id.to_string();
        self.row_frame(
            ElementId::Name(SharedString::from(format!("broken-{id}"))),
            selected,
            cx,
        )
        .child(
            div()
                .size(px(28.))
                .flex()
                .items_center()
                .justify_center()
                .child(
                    Icon::new(IconName::FileExclamationPoint)
                        .small()
                        .text_color(theme.danger),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_sm()
                .truncate()
                .child(format!("{id}.yaml")),
        )
        .child(Badge::new("読めません").tone(BadgeTone::Warning))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.select_other(Selection::Broken(select_id.clone()), window, cx)
        }))
        .into_any_element()
    }

    fn group_heading(&self, text: String, cx: &App) -> AnyElement {
        let theme = cx.theme();
        div()
            .px_4()
            .pt_4()
            .pb_1p5()
            .text_xs()
            .font_semibold()
            .text_color(theme.muted_foreground)
            .child(text)
            .into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let app = self.app.read(cx);
        let catalog = app.catalog().clone();
        let sources: Vec<ResolvedSource> = app.sources().to_vec();
        // 成人向けの版を使わないなら、まだ取り込めない成人向けのソースの名前も出さない。
        let adult = app.settings().adult_content();
        let broken: Vec<String> = app
            .local_files()
            .iter()
            .filter(|file| file.source.is_err())
            .map(|file| file.id.clone())
            .collect();

        let mut items: Vec<AnyElement> = Vec::new();
        for group_id in catalog.group_ids() {
            let group_name = catalog.group_name(&group_id).to_string();
            let in_group: Vec<&ResolvedSource> = sources
                .iter()
                .filter(|source| source.group.as_deref() == Some(group_id.as_str()))
                .collect();
            let visible: Vec<&ResolvedSource> = in_group
                .iter()
                .copied()
                .filter(|source| self.passes(source, cx))
                .filter(|source| {
                    self.matches(&[&source.config.name, &source.config.id, &group_name])
                })
                .collect();
            let rows: Vec<AnyElement> = visible
                .into_iter()
                .map(|source| self.render_row(source, cx))
                .collect();
            let unavailable: Vec<AnyElement> = if self.filter == ListFilter::All {
                catalog
                    .unavailable()
                    .iter()
                    .filter(|entry| entry.group == group_id && (adult || !entry.adult))
                    .filter(|entry| self.matches(&[&entry.name, &entry.id, &group_name]))
                    .map(|entry| self.render_unavailable_row(entry, cx))
                    .collect()
            } else {
                Vec::new()
            };
            if rows.is_empty() && unavailable.is_empty() {
                continue;
            }
            let on = in_group.iter().filter(|source| source.enabled).count();
            items.push(self.group_heading(format!("{group_name}  {on}/{}", in_group.len()), cx));
            items.extend(rows);
            items.extend(unavailable);
        }

        let local: Vec<&ResolvedSource> = sources
            .iter()
            .filter(|source| source.origin == SourceOrigin::Local)
            .filter(|source| self.passes(source, cx))
            .filter(|source| self.matches(&[&source.config.name, &source.config.id, "手元"]))
            .collect();
        let local: Vec<AnyElement> = local
            .into_iter()
            .map(|source| self.render_row(source, cx))
            .collect();
        let broken: Vec<AnyElement> = if self.filter == ListFilter::Enabled {
            Vec::new()
        } else {
            broken
                .iter()
                .filter(|id| self.matches(&[id.as_str(), "手元"]))
                .map(|id| self.render_broken_row(id, cx))
                .collect()
        };
        if !local.is_empty() || !broken.is_empty() {
            items.push(self.group_heading("手元のソース".to_string(), cx));
            items.extend(local);
            items.extend(broken);
        }

        if items.is_empty() {
            let (title, description) = match self.filter {
                ListFilter::Attention => (
                    "確かめることはありません",
                    "取得に失敗しているソースはありません。",
                ),
                ListFilter::Enabled => (
                    "使っているゲームはありません",
                    "「すべて」でゲームを on にすると、お知らせを取得します。",
                ),
                ListFilter::All => ("一致するゲームがありません", "検索語を変えてみてください。"),
            };
            let filtered = self.filter != ListFilter::All;
            return div()
                .size_full()
                .child(
                    EmptyState::new(IconName::Rss, title)
                        .description(description)
                        .when(filtered, |this| {
                            this.action(
                                Button::new("sources-show-all")
                                    .small()
                                    .outline()
                                    .label("すべてのゲームを見る")
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.filter = ListFilter::All;
                                        cx.notify();
                                    })),
                            )
                        }),
                )
                .into_any_element();
        }
        div()
            .id("sources-list")
            .size_full()
            .overflow_y_scrollbar()
            .child(v_flex().w_full().pb_4().children(items))
            .into_any_element()
    }

    fn render_list_pane(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        v_flex()
            .size_full()
            .child(
                v_flex()
                    .flex_none()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        Input::new(&self.search)
                            .small()
                            .cleanable(true)
                            .prefix(Icon::new(IconName::Search).small()),
                    )
                    .child(h_flex().child(segmented(
                        "sources-filter",
                        &ListFilter::LABELS,
                        self.filter.index(),
                        cx.listener(|this, index: &usize, _, cx| {
                            this.filter = ListFilter::from_index(*index);
                            cx.notify();
                        }),
                    ))),
            )
            .child(div().flex_1().min_h_0().child(self.render_list(cx)))
    }

    /// 概要の情報の枠 (最終成功・前回の結果・保存済みの記事・次回取得)。
    fn render_stats(&self, source: &ResolvedSource, cx: &mut Context<Self>) -> impl IntoElement {
        let app = self.app.read(cx);
        let id = source.id();
        let now = Utc::now();
        let state = app.state_of(id);
        let last_success = state.and_then(|state| state.last_success_at);
        let success = match last_success {
            Some(at) => StatItem::new("最終成功", format::relative_time(at, now)).note(
                at.with_timezone(&Local)
                    .format("%Y年%m月%d日 %H:%M")
                    .to_string(),
            ),
            None => StatItem::new("最終成功", "まだありません"),
        };
        let result = match app.last_run(id) {
            Some(run) => {
                let mut text = StatusIcon::label(Some(run.status), false, true).to_string();
                if run.added > 0 {
                    text.push_str(&format!(" · 新規 {} 件", run.added));
                }
                if run.updated > 0 {
                    text.push_str(&format!(" · 更新 {} 件", run.updated));
                }
                let item = StatItem::new("前回の結果", text);
                match &run.message {
                    Some(message) => item.note(message.clone()),
                    None => item.note(format!("一覧で {} 件", run.listed)),
                }
            }
            None => StatItem::new("前回の結果", "まだ取得していません"),
        };
        let stored = StatItem::new("保存済みの記事", format!("{} 件", app.entry_count(id)))
            .note(format!("未読 {} 件", app.unread(Channel::News, id)));
        let next = if !source.enabled {
            StatItem::new("次回取得", "使っていません")
        } else if app.is_running(id) {
            StatItem::new("次回取得", "取得中")
        } else {
            let interval = Duration::minutes(app.settings().interval_for(&source.config).into());
            match state.and_then(|state| state.last_attempt_at) {
                Some(last) => StatItem::new("次回取得", format::until(last + interval, now))
                    .note(format::interval(interval.num_minutes())),
                None => StatItem::new("次回取得", "まもなく"),
            }
        };
        h_flex()
            .w_full()
            .flex_wrap()
            .gap_3()
            .child(success)
            .child(result)
            .child(stored)
            .child(next)
    }

    /// カタログのソースの詳細。概要と取得の定義をタブで切り替える。
    fn render_catalog_view(
        &self,
        entry: &CatalogEntry,
        source: &ResolvedSource,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let theme = cx.theme().clone();
        let app = self.app.read(cx);
        let config = &source.config;
        let state = app.state_of(&config.id).cloned();
        let running = app.is_running(&config.id);
        let group_name = app.catalog().group_name(&entry.group).to_string();
        let id = config.id.clone();

        let header = h_flex()
            .flex_none()
            .gap_4()
            .px_6()
            .pt_5()
            .pb_4()
            .child(SourceAvatar::new(config.name.clone(), config.icon.clone()).large())
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .gap_1()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_xl()
                                    .font_semibold()
                                    .truncate()
                                    .child(config.name.clone()),
                            )
                            .child(Badge::new("カタログ").tone(BadgeTone::Accent))
                            .child(Badge::new(group_name)),
                    )
                    .child(
                        h_flex()
                            .gap_1()
                            .text_sm()
                            .text_color(theme.muted_foreground)
                            .child(StatusIcon::new(
                                state.as_ref().and_then(|state| state.last_status),
                                running,
                                source.enabled,
                            ))
                            .child(self.status_text(source, cx)),
                    ),
            )
            .child({
                let id = id.clone();
                Switch::new("catalog-enabled")
                    .checked(source.enabled)
                    .label(if source.enabled {
                        "使う"
                    } else {
                        "使わない"
                    })
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        this.set_enabled(&id, *checked, cx)
                    }))
            });

        let tabs = h_flex()
            .flex_none()
            .px_6()
            .pb_3()
            .border_b_1()
            .border_color(theme.border)
            .child(segmented(
                "catalog-tabs",
                &["概要", "取得の定義"],
                usize::from(self.tab == DetailTab::Definition),
                cx.listener(|this, index: &usize, _, cx| {
                    this.tab = if *index == 1 {
                        DetailTab::Definition
                    } else {
                        DetailTab::Overview
                    };
                    cx.notify();
                }),
            ));

        let body: AnyElement = match self.tab {
            DetailTab::Overview => self.render_overview(entry, source, cx).into_any_element(),
            DetailTab::Definition => self.render_definition(entry, source, cx).into_any_element(),
        };

        v_flex()
            .size_full()
            .child(header)
            .child(tabs)
            .child(
                div()
                    .id("catalog-view-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(px(880.))
                            .gap_5()
                            .px_6()
                            .py_5()
                            .child(body),
                    ),
            )
            .into_any_element()
    }

    fn render_overview(
        &self,
        entry: &CatalogEntry,
        source: &ResolvedSource,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let app = self.app.read(cx);
        let config = &source.config;
        let state = app.state_of(&config.id).cloned();
        let running = app.is_running(&config.id);
        let local_icon = app
            .subscriptions()
            .get(&config.id)
            .and_then(|item| item.icon.clone());
        let failed = source.enabled
            && state
                .as_ref()
                .and_then(|state| state.last_status)
                .is_some_and(|status| status == RunStatus::Failed);
        let id = config.id.clone();
        let name = config.name.clone();
        let homepage = config.homepage.clone();
        let repository_url = format!("{CATALOG_REPOSITORY}/blob/main/{}", entry.path);

        v_flex()
            .w_full()
            .gap_5()
            .when(failed, |this| {
                let message = state
                    .as_ref()
                    .and_then(|state| state.last_message.clone())
                    .unwrap_or_else(|| "理由は記録されていません".to_string());
                let id = id.clone();
                this.child(
                    h_flex()
                        .w_full()
                        .gap_3()
                        .items_start()
                        .p_4()
                        .rounded(px(12.))
                        .border_1()
                        .border_color(theme.danger.opacity(0.4))
                        .bg(theme.danger.opacity(0.06))
                        .child(Icon::new(IconName::CircleAlert).text_color(theme.danger))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .gap_1()
                                .child(div().text_sm().font_semibold().child("前回の取得に失敗しました"))
                                .child(div().text_sm().text_color(theme.muted_foreground).child(message)),
                        )
                        .child(
                            Button::new("catalog-retry")
                                .small()
                                .outline()
                                .icon(IconName::RefreshCw)
                                .label("取得し直す")
                                .loading(running)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.app.update(cx, |app, cx| {
                                        app.fetch_now(Some(vec![id.clone()]), cx)
                                    })
                                })),
                        ),
                )
            })
            .child(self.render_stats(source, cx))
            .child(
                h_flex()
                    .gap_2()
                    .flex_wrap()
                    .when(source.enabled, |this| {
                        let id = id.clone();
                        this.child(
                            Button::new("catalog-fetch")
                                .small()
                                .outline()
                                .icon(IconName::RefreshCw)
                                .label("このゲームを取得")
                                .loading(running)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.app.update(cx, |app, cx| {
                                        app.fetch_now(Some(vec![id.clone()]), cx)
                                    })
                                })),
                        )
                    })
                    .when_some(homepage, |this, homepage| {
                        this.child(
                            Button::new("catalog-homepage")
                                .small()
                                .ghost()
                                .icon(IconName::ExternalLink)
                                .label("公式サイト")
                                .on_click(move |_, _, cx| open_web_url(&homepage, cx)),
                        )
                    })
                    .child(
                        Button::new("catalog-repository")
                            .small()
                            .ghost()
                            .icon(IconName::Github)
                            .label("カタログで見る")
                            .on_click(move |_, _, cx| open_web_url(&repository_url, cx)),
                    ),
            )
            .child(
                Card::new()
                    .title("このアプリでのアイコン")
                    .description("画像を選ぶと正方形に切り抜く画面が開きます。切り抜いた画像は、このパソコンだけに保存します。")
                    .child(
                        h_flex()
                            .gap_3()
                            .child(SourceAvatar::new(config.name.clone(), config.icon.clone()).large())
                            .child({
                                let id = id.clone();
                                let name = name.clone();
                                Button::new("catalog-pick-icon")
                                    .small()
                                    .outline()
                                    .icon(IconName::ImagePlus)
                                    .label("画像から作る")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.pick_icon(id.clone(), name.clone(), window, cx)
                                    }))
                            })
                            .when(local_icon.is_some(), |this| {
                                let id = id.clone();
                                this.child(
                                    Button::new("catalog-clear-icon")
                                        .small()
                                        .ghost()
                                        .label("外す")
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            if let Err(err) = this.app.update(cx, |app, cx| app.set_icon(&id, None, cx)) {
                                                tracing::error!("アイコンを外せません: {err:#}");
                                            }
                                        })),
                                )
                            }),
                    ),
            )
            .when_some(config.note.clone(), |this, note| {
                this.child(Card::new().title("説明").child(div().text_sm().child(note)))
            })
    }

    fn render_definition(
        &self,
        entry: &CatalogEntry,
        source: &ResolvedSource,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let theme = cx.theme().clone();
        let config = &source.config;
        let test = match &self.test {
            Some((id, TestState::Running)) if *id == config.id => Some(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .child(Icon::new(IconName::LoaderCircle).small())
                    .child("取得しています…")
                    .into_any_element(),
            ),
            Some((id, TestState::Done(output))) if *id == config.id => {
                Some(RunResult::new((**output).clone()).into_any_element())
            }
            _ => None,
        };
        let testing = matches!(&self.test, Some((id, TestState::Running)) if *id == config.id);
        let id = config.id.clone();
        let entry_for_test = entry.clone();
        v_flex()
            .w_full()
            .gap_4()
            .child(
                h_flex()
                    .w_full()
                    .gap_4()
                    .items_start()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(div().text_sm().font_medium().child(format!("{} · {}", config.kind_label(), entry.path)))
                            .child(
                                div()
                                    .text_sm()
                                    .text_color(theme.muted_foreground)
                                    .child("カタログの定義は、ここでは直せません。直すときは「自分用に編集」を押すか、カタログに PR を送ってください。"),
                            ),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_2()
                            .child(
                                Button::new("catalog-test")
                                    .small()
                                    .outline()
                                    .icon(IconName::FlaskConical)
                                    .label("テスト取得")
                                    .loading(testing)
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.run_catalog_test(&entry_for_test, cx)
                                    })),
                            )
                            .child(
                                Button::new("catalog-copy")
                                    .small()
                                    .primary()
                                    .icon(IconName::SquarePen)
                                    .label("自分用に編集")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.copy_to_local(&id, window, cx)
                                    })),
                            ),
                    ),
            )
            .children(test)
            .child(CodeBlock::new(entry.text.clone()))
    }

    fn render_unavailable(&self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let app = self.app.read(cx);
        // 成人向けのソースを使わないなら、まだ取り込めない成人向けのソースも出さない (一覧と同じ)。
        let adult = app.settings().adult_content();
        let Some(entry) = app
            .catalog()
            .unavailable()
            .iter()
            .find(|entry| entry.id == id && (adult || !entry.adult))
        else {
            return self.render_placeholder();
        };
        let description = match &entry.required_app_version {
            Some(version) => format!(
                "このソースはアプリ {version} 以降で使えます (このアプリは {})。アプリを新しくすると使えるようになります。",
                gnn_core::source::APP_VERSION
            ),
            None => "このソースは新しい書式で書かれているため、このアプリでは読めません。アプリを新しくすると使えるようになります。".to_string(),
        };
        EmptyState::new(IconName::CircleArrowUp, entry.name.clone())
            .description(description)
            .into_any_element()
    }

    fn render_broken(&self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let app = self.app.read(cx);
        let Some(file) = app.local_file(id).cloned() else {
            return self.render_placeholder();
        };
        let error = file.source.as_ref().err().cloned().unwrap_or_default();
        let path = file.path.clone();
        div()
            .id("broken-scroll")
            .size_full()
            .overflow_y_scrollbar()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(880.))
                    .gap_4()
                    .px_6()
                    .py_5()
                    .child(
                        div()
                            .text_xl()
                            .font_semibold()
                            .child(format!("{id}.yaml を読めません")),
                    )
                    .child(div().text_sm().text_color(theme.danger).child(error))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(format!("{}", path.display())),
                    )
                    .child(
                        h_flex().child(
                            Button::new("broken-reveal")
                                .small()
                                .outline()
                                .icon(IconName::FolderOpen)
                                .label("ファイルの場所を開く")
                                .on_click(move |_, _, cx: &mut App| cx.reveal_path(&path)),
                        ),
                    )
                    .child(CodeBlock::new(file.text.clone())),
            )
            .into_any_element()
    }

    fn render_placeholder(&self) -> AnyElement {
        EmptyState::new(IconName::Rss, "ゲームを選んでください")
            .description("左の一覧で on にしたゲームのお知らせを取得します。ゲームを選ぶと、取得の状況とアイコンを確かめられます。")
            .into_any_element()
    }

    fn render_detail(&self, cx: &mut Context<Self>) -> AnyElement {
        if let Some(editor) = &self.editor {
            return editor.clone().into_any_element();
        }
        match self.selected.clone() {
            Some(Selection::Source(id)) => {
                let app = self.app.read(cx);
                let source = app.resolved(&id).cloned();
                let entry = app.catalog().get(&id).cloned();
                match (source, entry) {
                    (Some(source), Some(entry)) => self.render_catalog_view(&entry, &source, cx),
                    _ => self.render_placeholder(),
                }
            }
            Some(Selection::Unavailable(id)) => self.render_unavailable(&id, cx),
            Some(Selection::Broken(id)) => self.render_broken(&id, cx),
            Some(Selection::New) | None => self.render_placeholder(),
        }
    }
}

/// 追加ダイアログの中身。カタログのゲームは一覧で on にすることを案内し、自分で作る取得方式を並べる。
fn add_dialog_content(page: WeakEntity<SourcesPage>, cx: &mut App) -> AnyElement {
    let theme = cx.theme().clone();
    let palette = Palette::of(cx);
    let kinds = ListKind::ALL.into_iter().map(|kind| {
        let page = page.clone();
        h_flex()
            .gap_3()
            .py_2p5()
            .border_t_1()
            .border_color(palette.divider)
            .child(
                v_flex()
                    .flex_1()
                    .child(div().text_sm().font_medium().child(kind.label()))
                    .child(
                        div()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(kind.description()),
                    ),
            )
            .child(
                Button::new(SharedString::from(format!("add-kind-{kind:?}")))
                    .small()
                    .outline()
                    .label("作る")
                    .on_click(move |_, window, cx| {
                        window.close_dialog(cx);
                        let _ = page.update(cx, |page, cx| page.add_blank(kind, window, cx));
                    }),
            )
    });
    let show_all = {
        let page = page.clone();
        move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
            window.close_dialog(cx);
            let _ = page.update(cx, |page, cx| {
                page.filter = ListFilter::All;
                cx.notify();
            });
        }
    };
    v_flex()
        .gap_5()
        .child(
            v_flex()
                .gap_2()
                .child(div().text_sm().font_semibold().child("カタログのゲームを使う"))
                .child(
                    div()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("カタログにあるゲームは、一覧の「すべて」でスイッチを on にするだけで取得を始めます。ほかの人にも使ってほしいゲームは、カタログ (game-news-nexus-config) に PR を送ってください。"),
                )
                .child(
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("add-show-catalog")
                                .small()
                                .outline()
                                .label("一覧ですべてのゲームを見る")
                                .on_click(show_all),
                        )
                        .child(
                            Button::new("open-catalog-repository")
                                .small()
                                .ghost()
                                .icon(IconName::Github)
                                .label("カタログを開く")
                                .on_click(|_, _, cx| open_web_url(CATALOG_REPOSITORY, cx)),
                        ),
                ),
        )
        .child(
            v_flex()
                .gap_1()
                .child(div().text_sm().font_semibold().child("自分で取得の定義を作る"))
                .child(
                    div()
                        .pb_1()
                        .text_sm()
                        .text_color(theme.muted_foreground)
                        .child("カタログにないゲームは、手元で取得の定義を作れます。定義はこのパソコンだけに保存します。"),
                )
                .children(kinds),
        )
        .into_any_element()
}

impl Render for SourcesPage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let layout = Layout::of(window);
        let header = PageHeader::new("ソース")
            .description("お知らせを取得するゲームを選びます。")
            .action(
                Button::new("add-source")
                    .small()
                    .primary()
                    .icon(IconName::Plus)
                    .label("ソースを追加")
                    .on_click(cx.listener(|this, _, window, cx| this.open_add_dialog(window, cx))),
            );
        let root = v_flex().size_full().child(header);
        if !layout.side_by_side() {
            // 狭いときは一覧か詳細のどちらか。
            return if self.showing_detail && self.selected.is_some() {
                let back = h_flex()
                    .flex_none()
                    .h(px(44.))
                    .px_4()
                    .border_b_1()
                    .border_color(cx.theme().border)
                    .child(
                        Button::new("sources-back")
                            .small()
                            .ghost()
                            .icon(IconName::ArrowLeft)
                            .label("ソースの一覧に戻る")
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.guard(window, cx, |this, _, cx| {
                                    this.showing_detail = false;
                                    cx.notify();
                                })
                            })),
                    );
                root.child(back)
                    .child(div().flex_1().min_h_0().child(self.render_detail(cx)))
            } else {
                root.child(div().flex_1().min_h_0().child(self.render_list_pane(cx)))
            };
        }
        root.child(
            div().flex_1().min_h_0().child(
                h_resizable("sources-split")
                    .child(
                        resizable_panel()
                            .size(px(320.))
                            .size_range(px(260.)..px(520.))
                            .child(self.render_list_pane(cx)),
                    )
                    .child(resizable_panel().child(self.render_detail(cx))),
            ),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn list_filters_round_trip_through_the_tabs() {
        for filter in [ListFilter::All, ListFilter::Enabled, ListFilter::Attention] {
            assert_eq!(ListFilter::from_index(filter.index()), filter);
        }
        assert_eq!(ListFilter::LABELS[ListFilter::Attention.index()], "要確認");
    }
}
