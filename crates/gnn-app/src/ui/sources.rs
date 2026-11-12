//! ソースの一覧。カタログのソースを on/off し、手元の定義を作って直す。
//!
//! 左にカタログのグループごとのソースと手元のソース、右に選んだソースの内容を出す。
//! カタログのソースは定義 (YAML) を読むだけで、直すときは「自分用にコピー」して
//! 手元の定義 (上書き) にする。新しいゲームはカタログへの PR で足してもらう。

use gnn_core::catalog::{
    CATALOG_REPOSITORY, CatalogEntry, ResolvedSource, SourceOrigin, UnavailableEntry,
};
use gnn_core::source::ListKind;
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

use crate::services::describe_last_attempt;
use crate::state::AppState;
use crate::ui::components::{
    Badge, BadgeTone, CodeBlock, CountPill, EmptyState, PageHeader, RunResult, SectionLabel,
    SourceAvatar, StatusIcon, open_web_url,
};
use crate::ui::icon_cropper::pick_and_crop;
use crate::ui::source_editor::{EditorEvent, EditorMode, SourceEditor};

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

enum TestState {
    Running,
    Done(Box<RunOutput>),
}

pub struct SourcesPage {
    app: Entity<AppState>,
    selected: Option<Selection>,
    editor: Option<Entity<SourceEditor>>,
    search: Entity<InputState>,
    query: String,
    /// カタログのソースのテスト取得。ソース ID と結果。
    test: Option<(String, TestState)>,
    test_task: Task<()>,
    _subscriptions: Vec<Subscription>,
    editor_subscription: Option<Subscription>,
}

impl SourcesPage {
    pub fn new(app: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let search =
            cx.new(|cx| InputState::new(window, cx).placeholder("名前・ID・グループで絞り込む"));
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
            test: None,
            test_task: Task::ready(()),
            _subscriptions: subscriptions,
            editor_subscription: None,
        }
    }

    /// ソースを選ぶ。手元の定義があれば編集画面、カタログだけなら定義の表示を出す。
    pub fn select(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let app = self.app.read(cx);
        let local = app
            .local_file(id)
            .and_then(|file| Some((file.source.as_ref().ok()?.clone(), file.text.clone())));
        let in_catalog = app.catalog().get(id).is_some();
        self.selected = Some(Selection::Source(id.to_string()));
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
                self.run_catalog_test(&entry, cx);
            }
        }
        cx.notify();
    }

    fn select_other(&mut self, selection: Selection, cx: &mut Context<Self>) {
        self.selected = Some(selection);
        self.editor = None;
        self.editor_subscription = None;
        cx.notify();
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
                        // カタログの上書きを消したなら、カタログの定義を出す。
                        if this.app.read(cx).resolved(&id).is_some() {
                            this.select(&id, window, cx);
                        } else {
                            this.selected = None;
                            this.editor = None;
                            cx.notify();
                        }
                    }
                }),
            );
        self.editor = Some(editor);
        cx.notify();
    }

    fn add_blank(&mut self, kind: ListKind, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.app.read(cx).unique_source_id("my_source");
        self.selected = Some(Selection::New);
        self.open_editor(
            gnn_core::templates::blank(kind, id),
            None,
            EditorMode::New,
            window,
            cx,
        );
    }

    fn open_add_dialog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let page = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            dialog
                .title("手元のソースを作る")
                .w(px(560.))
                .child(add_dialog_content(page.clone(), cx))
        });
    }

    /// カタログのソースを手元に写し、編集画面を開く。
    fn copy_to_local(&mut self, id: &str, window: &mut Window, cx: &mut Context<Self>) {
        let result = self.app.update(cx, |app, cx| app.copy_to_local(id, cx));
        match result {
            Ok(()) => {
                self.select(id, window, cx);
                window.push_notification(
                    Notification::success(
                        "手元に写しました。保存した定義がカタログの定義の代わりに使われます。",
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
                if let Err(err) = result {
                    window.push_notification(
                        Notification::error(format!("アイコンを保存できません: {err:#}")),
                        cx,
                    );
                }
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

    fn render_row(&self, source: &ResolvedSource, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
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
        let detail = if source.enabled {
            format!(
                "{} · {}",
                config.kind_label(),
                describe_last_attempt(state.and_then(|state| state.last_attempt_at))
            )
        } else {
            config.kind_label()
        };

        h_flex()
            .id(ElementId::Name(SharedString::from(format!(
                "source-{}",
                config.id
            ))))
            .w_full()
            .gap_2()
            .px_3()
            .py_2()
            .cursor_pointer()
            .rounded(theme.radius)
            .when(selected, |this| this.bg(theme.list_active))
            .when(!selected, |this| {
                this.hover(|style| style.bg(theme.list_hover))
            })
            .child(SourceAvatar::new(config.name.clone(), config.icon.clone()))
            .child(
                v_flex()
                    .flex_1()
                    .min_w_0()
                    .child(
                        h_flex()
                            .gap_1()
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
                            .when(source.origin == SourceOrigin::Override, |this| {
                                this.child(Badge::new("上書き").tone(BadgeTone::Accent))
                            })
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
                            .child(div().truncate().child(detail)),
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
        let theme = cx.theme();
        let selected = self.selected == Some(Selection::Unavailable(entry.id.clone()));
        let id = entry.id.clone();
        h_flex()
            .id(ElementId::Name(SharedString::from(format!(
                "unavailable-{}",
                entry.id
            ))))
            .w_full()
            .gap_2()
            .px_3()
            .py_2()
            .cursor_pointer()
            .rounded(theme.radius)
            .when(selected, |this| this.bg(theme.list_active))
            .when(!selected, |this| {
                this.hover(|style| style.bg(theme.list_hover))
            })
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
            .on_click(cx.listener(move |this, _, _, cx| {
                this.select_other(Selection::Unavailable(id.clone()), cx)
            }))
            .into_any_element()
    }

    fn render_broken_row(&self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme();
        let selected = self.selected == Some(Selection::Broken(id.to_string()));
        let select_id = id.to_string();
        h_flex()
            .id(ElementId::Name(SharedString::from(format!("broken-{id}"))))
            .w_full()
            .gap_2()
            .px_3()
            .py_2()
            .cursor_pointer()
            .rounded(theme.radius)
            .when(selected, |this| this.bg(theme.list_active))
            .when(!selected, |this| {
                this.hover(|style| style.bg(theme.list_hover))
            })
            .child(
                Icon::new(IconName::FileExclamationPoint)
                    .small()
                    .text_color(theme.danger),
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
            .on_click(cx.listener(move |this, _, _, cx| {
                this.select_other(Selection::Broken(select_id.clone()), cx)
            }))
            .into_any_element()
    }

    fn render_list(&self, cx: &mut Context<Self>) -> AnyElement {
        let app = self.app.read(cx);
        let catalog = app.catalog().clone();
        let sources: Vec<ResolvedSource> = app.sources().to_vec();
        let broken: Vec<String> = app
            .local_files()
            .iter()
            .filter(|file| file.source.is_err())
            .map(|file| file.id.clone())
            .collect();

        let mut list = v_flex().gap_1().p_2();
        for group_id in catalog.group_ids() {
            let group_name = catalog.group_name(&group_id).to_string();
            let rows: Vec<AnyElement> = sources
                .iter()
                .filter(|source| source.group.as_deref() == Some(group_id.as_str()))
                .filter(|source| {
                    self.matches(&[&source.config.name, &source.config.id, &group_name])
                })
                .map(|source| self.render_row(source, cx))
                .collect();
            let unavailable: Vec<AnyElement> = catalog
                .unavailable()
                .iter()
                .filter(|entry| entry.group == group_id)
                .filter(|entry| self.matches(&[&entry.name, &entry.id, &group_name]))
                .map(|entry| self.render_unavailable_row(entry, cx))
                .collect();
            if rows.is_empty() && unavailable.is_empty() {
                continue;
            }
            let on = sources
                .iter()
                .filter(|source| {
                    source.group.as_deref() == Some(group_id.as_str()) && source.enabled
                })
                .count();
            let total = sources
                .iter()
                .filter(|source| source.group.as_deref() == Some(group_id.as_str()))
                .count();
            list = list
                .child(
                    div()
                        .px_1()
                        .child(SectionLabel::new(format!("{group_name} ({on}/{total})"))),
                )
                .children(rows)
                .children(unavailable);
        }

        let local: Vec<AnyElement> = sources
            .iter()
            .filter(|source| source.origin == SourceOrigin::Local)
            .filter(|source| self.matches(&[&source.config.name, &source.config.id, "手元"]))
            .map(|source| self.render_row(source, cx))
            .collect();
        let broken: Vec<AnyElement> = broken
            .iter()
            .filter(|id| self.matches(&[id.as_str(), "手元"]))
            .map(|id| self.render_broken_row(id, cx))
            .collect();
        if !local.is_empty() || !broken.is_empty() {
            list = list
                .child(div().px_1().child(SectionLabel::new("手元のソース")))
                .children(local)
                .children(broken);
        }
        list.into_any_element()
    }

    /// カタログのソースの内容。定義は読むだけで、直すときは手元に写す。
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
        let local_icon = app
            .subscriptions()
            .get(&config.id)
            .and_then(|item| item.icon.clone());
        let status_line = match &state {
            Some(state) => format!(
                "{} · 最終取得 {}{}",
                StatusIcon::label(state.last_status, running, source.enabled),
                describe_last_attempt(state.last_attempt_at),
                state
                    .last_message
                    .as_deref()
                    .map(|message| format!(" · {message}"))
                    .unwrap_or_default()
            ),
            None => StatusIcon::label(None, running, source.enabled).to_string(),
        };
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
        let name = config.name.clone();
        let entry_for_test = entry.clone();
        let homepage = config.homepage.clone();
        let repository_url = format!("{CATALOG_REPOSITORY}/blob/main/{}", entry.path);

        v_flex()
            .size_full()
            .child(
                v_flex()
                    .gap_2()
                    .px_4()
                    .py_3()
                    .border_b_1()
                    .border_color(theme.border)
                    .child(
                        h_flex()
                            .gap_3()
                            .child(SourceAvatar::new(config.name.clone(), config.icon.clone()).large())
                            .child(
                                v_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .gap_1()
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(div().text_lg().font_semibold().truncate().child(config.name.clone()))
                                            .child(Badge::new(group_name))
                                            .child(
                                                div()
                                                    .text_xs()
                                                    .text_color(theme.muted_foreground)
                                                    .child(format!("{} · {}", config.kind_label(), config.channel.label())),
                                            ),
                                    )
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child(StatusIcon::new(
                                                state.as_ref().and_then(|state| state.last_status),
                                                running,
                                                source.enabled,
                                            ))
                                            .child(div().truncate().child(status_line)),
                                    ),
                            )
                            .child({
                                let id = id.clone();
                                Switch::new("catalog-enabled")
                                    .checked(source.enabled)
                                    .label(if source.enabled { "使う" } else { "使わない" })
                                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                                        this.set_enabled(&id, *checked, cx)
                                    }))
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
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
                            .when(source.enabled, |this| {
                                let id = id.clone();
                                this.child(
                                    Button::new("catalog-fetch")
                                        .small()
                                        .ghost()
                                        .icon(IconName::RefreshCw)
                                        .label("今すぐ取得")
                                        .loading(running)
                                        .on_click(cx.listener(move |this, _, _, cx| {
                                            this.app.update(cx, |app, cx| {
                                                app.fetch_now(Some(vec![id.clone()]), cx)
                                            })
                                        })),
                                )
                            })
                            .child({
                                let id = id.clone();
                                Button::new("catalog-copy")
                                    .small()
                                    .ghost()
                                    .icon(IconName::Copy)
                                    .label("自分用にコピーして直す")
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        this.copy_to_local(&id, window, cx)
                                    }))
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
                    ),
            )
            .child(
                div()
                    .id("catalog-view-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(
                        v_flex()
                            .gap_4()
                            .p_4()
                            .children(test)
                            .when_some(config.note.clone(), |this, note| {
                                this.child(div().text_sm().child(note))
                            })
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(SectionLabel::new("このアプリでのアイコン"))
                                    .child(
                                        h_flex()
                                            .gap_2()
                                            .child(SourceAvatar::new(config.name.clone(), config.icon.clone()))
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
                            .child(
                                v_flex()
                                    .gap_2()
                                    .child(SectionLabel::new(format!("取得定義 ({})", entry.path)))
                                    .child(
                                        div()
                                            .text_xs()
                                            .text_color(theme.muted_foreground)
                                            .child("カタログの定義は、ここでは直せません。直すときは「自分用にコピーして直す」を押すか、カタログに PR を送ってください。"),
                                    )
                                    .child(CodeBlock::new(entry.text.clone())),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn render_unavailable(&self, id: &str, cx: &mut Context<Self>) -> AnyElement {
        let app = self.app.read(cx);
        let Some(entry) = app
            .catalog()
            .unavailable()
            .iter()
            .find(|entry| entry.id == id)
        else {
            return EmptyState::new(IconName::Rss, "ソースを選んでください").into_any_element();
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
            return EmptyState::new(IconName::Rss, "ソースを選んでください").into_any_element();
        };
        let error = file.source.as_ref().err().cloned().unwrap_or_default();
        let path = file.path.clone();
        v_flex()
            .gap_3()
            .p_4()
            .child(
                div()
                    .text_lg()
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
            .child(CodeBlock::new(file.text.clone()))
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
                    _ => EmptyState::new(IconName::Rss, "ソースを選んでください").into_any_element(),
                }
            }
            Some(Selection::Unavailable(id)) => self.render_unavailable(&id, cx),
            Some(Selection::Broken(id)) => self.render_broken(&id, cx),
            Some(Selection::New) | None => EmptyState::new(IconName::Rss, "ソースを選んでください")
                .description("左の一覧で on にしたゲームのお知らせを取得します。ゲームを選ぶと、取得の定義と取得の結果を確かめられます。")
                .into_any_element(),
        }
    }
}

/// 追加ダイアログの中身。取得方式を並べる。
fn add_dialog_content(page: WeakEntity<SourcesPage>, cx: &mut App) -> AnyElement {
    let theme = cx.theme().clone();
    let kinds = ListKind::ALL.into_iter().map(|kind| {
        let page = page.clone();
        h_flex()
            .gap_3()
            .py_1()
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
    v_flex()
        .gap_3()
        .child(
            div()
                .text_sm()
                .child("カタログにないゲームは、手元で取得の定義を作れます。定義はこのパソコンだけに保存します。"),
        )
        .child(v_flex().gap_1().children(kinds))
        .child(
            h_flex()
                .gap_2()
                .text_xs()
                .text_color(theme.muted_foreground)
                .child("ほかの人にも使ってほしい定義は、カタログ (game-news-nexus-config) に PR を送ってください。")
                .child(
                    Button::new("open-catalog-repository")
                        .xsmall()
                        .ghost()
                        .label("カタログを開く")
                        .on_click(|_, _, cx| open_web_url(CATALOG_REPOSITORY, cx)),
                ),
        )
        .into_any_element()
}

impl Render for SourcesPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let app = self.app.read(cx);
        let any_running = app.running_count() > 0;
        let list = self.render_list(cx);
        let detail = self.render_detail(cx);

        v_flex()
            .size_full()
            .child(
                PageHeader::new("ソース")
                    .description("使うゲームを on にします。取得の定義はカタログ (game-news-nexus-config) から取り込み、カタログにないものは手元で作れます。")
                    .action(
                        Button::new("fetch-all")
                            .small()
                            .outline()
                            .icon(IconName::RefreshCw)
                            .label("すべて取得")
                            .loading(any_running)
                            .on_click(cx.listener(|this, _, _, cx| this.app.update(cx, |app, cx| app.fetch_now(None, cx)))),
                    )
                    .action(
                        Button::new("add-source")
                            .small()
                            .primary()
                            .icon(IconName::Plus)
                            .label("手元で作る")
                            .on_click(cx.listener(|this, _, window, cx| this.open_add_dialog(window, cx))),
                    ),
            )
            .child(
                div().flex_1().min_h_0().child(
                    h_resizable("sources-split")
                        .child(
                            resizable_panel()
                                .size(px(360.))
                                .size_range(px(260.)..px(560.))
                                .child(
                                    v_flex()
                                        .size_full()
                                        .child(div().px_3().pt_2().child(Input::new(&self.search).small()))
                                        .child(
                                            div()
                                                .id("sources-list")
                                                .flex_1()
                                                .min_h_0()
                                                .overflow_y_scrollbar()
                                                .child(list),
                                        ),
                                ),
                        )
                        .child(resizable_panel().child(detail)),
                ),
            )
    }
}
