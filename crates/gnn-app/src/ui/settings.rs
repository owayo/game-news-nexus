//! アプリの設定。取得間隔・カタログ・常駐・通知・表示・旧形式との連携。

use std::path::PathBuf;

use gnn_core::catalog::{CATALOG_REPOSITORY, DEFAULT_CATALOG_URL, ProblemKind};
use gnn_core::{AppSettings, ThemePreference};
use gpui_kit::assets::IconName;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Selectable as _, Sizable as _, WindowExt as _, h_flex,
    v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _, Subscription, Window,
    div, px,
};

use crate::state::AppState;
use crate::ui::components::{FormRow, PageHeader, SectionLabel, SourceAvatar, open_web_url};
use crate::ui::icon_cropper::pick_and_crop;

#[derive(Debug, Clone, Copy)]
enum NumberField {
    Interval,
    MaxAge,
    Retention,
    Concurrency,
}

impl NumberField {
    const ALL: [NumberField; 4] = [
        NumberField::Interval,
        NumberField::MaxAge,
        NumberField::Retention,
        NumberField::Concurrency,
    ];

    fn read(self, settings: &AppSettings) -> u64 {
        match self {
            NumberField::Interval => settings.interval_minutes.into(),
            NumberField::MaxAge => settings.max_age_days.into(),
            NumberField::Retention => settings.retention_days.into(),
            NumberField::Concurrency => settings.concurrency as u64,
        }
    }

    fn write(self, settings: &mut AppSettings, value: u64) -> Result<(), &'static str> {
        match self {
            NumberField::Interval if value >= 1 => {
                settings.interval_minutes = value.min(24 * 60) as u32
            }
            NumberField::MaxAge => settings.max_age_days = value.min(3650) as u32,
            NumberField::Retention => settings.retention_days = value.min(36500) as u32,
            NumberField::Concurrency if (1..=16).contains(&value) => {
                settings.concurrency = value as usize
            }
            NumberField::Interval => return Err("1 分以上にしてください"),
            NumberField::Concurrency => return Err("1〜16 にしてください"),
        }
        Ok(())
    }

    fn label(self) -> &'static str {
        match self {
            NumberField::Interval => "取得間隔 (分)",
            NumberField::MaxAge => "取り込む期間 (日)",
            NumberField::Retention => "記事を残す日数",
            NumberField::Concurrency => "同時に取得するソースの数",
        }
    }

    fn help(self) -> &'static str {
        match self {
            NumberField::Interval => {
                "ソースごとの設定がなければこの間隔で取得します。失敗が続くソースは間隔を延ばします。"
            }
            NumberField::MaxAge => "これより古い記事は取り込みません。0 で無制限。",
            NumberField::Retention => "これより古いお知らせを消します。0 なら消しません。",
            NumberField::Concurrency => "同じサイトへの同時接続は別に 2 本までに抑えています。",
        }
    }
}

pub struct SettingsPage {
    app: Entity<AppState>,
    /// 自動起動の状態。レジストリが正本なので、開いたときと切り替えたときに読む。
    autostart: bool,
    numbers: Vec<(NumberField, Entity<InputState>)>,
    export_dir: Entity<InputState>,
    /// カタログの場所。「今すぐ取り込む」を押すまで設定には入れない。
    catalog: Entity<InputState>,
    errors: Vec<(usize, &'static str)>,
    _subscriptions: Vec<Subscription>,
}

impl SettingsPage {
    pub fn new(app: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = app.read(cx).settings().clone();
        let mut subscriptions = vec![cx.observe(&app, |_, _, cx| cx.notify())];
        let mut numbers = Vec::new();
        for (index, field) in NumberField::ALL.into_iter().enumerate() {
            let value = field.read(&settings).to_string();
            let state = cx.new(|cx| InputState::new(window, cx).default_value(value));
            subscriptions.push(cx.subscribe_in(
                &state,
                window,
                move |this, state, event, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        let text = state.read(cx).value().to_string();
                        this.apply_number(index, field, &text, cx);
                    }
                },
            ));
            numbers.push((field, state));
        }
        let export = settings
            .legacy_export_dir
            .as_ref()
            .map(|dir| dir.display().to_string())
            .unwrap_or_default();
        let export_dir = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("例: D:\\data (空欄なら書き出さない)")
                .default_value(export)
        });
        subscriptions.push(
            cx.subscribe_in(&export_dir, window, |this, state, event, _, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = state.read(cx).value().trim().to_string();
                    this.update(cx, move |settings| {
                        settings.legacy_export_dir =
                            (!text.is_empty()).then(|| PathBuf::from(text));
                    });
                }
            }),
        );
        let catalog = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("tar.gz の URL か、リポジトリを clone したフォルダ")
                .default_value(settings.catalog.clone())
        });
        Self {
            app,
            autostart: crate::autostart::is_enabled(),
            numbers,
            export_dir,
            catalog,
            errors: Vec::new(),
            _subscriptions: subscriptions,
        }
    }

    fn update(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut AppSettings)) {
        let mut settings = self.app.read(cx).settings().clone();
        edit(&mut settings);
        if let Err(err) = self
            .app
            .update(cx, |app, cx| app.update_settings(settings.clone(), cx))
        {
            tracing::error!("設定を保存できません: {err:#}");
        }
        crate::app::apply_theme(settings.theme, cx);
        cx.notify();
    }

    fn apply_number(
        &mut self,
        index: usize,
        field: NumberField,
        text: &str,
        cx: &mut Context<Self>,
    ) {
        self.errors.retain(|(i, _)| *i != index);
        match text.trim().parse::<u64>() {
            Ok(value) => {
                let mut error = None;
                self.update(cx, |settings| {
                    if let Err(message) = field.write(settings, value) {
                        error = Some(message);
                    }
                });
                if let Some(message) = error {
                    self.errors.push((index, message));
                }
            }
            Err(_) => self.errors.push((index, "数値を入れてください")),
        }
        cx.notify();
    }

    /// 入力したカタログの場所を設定に入れて、取り込む。
    fn apply_catalog(&mut self, cx: &mut Context<Self>) {
        let location = self.catalog.read(cx).value().trim().to_string();
        let location = if location.is_empty() {
            DEFAULT_CATALOG_URL.to_string()
        } else {
            location
        };
        let changed = self.app.read(cx).settings().catalog != location;
        if changed {
            // 場所を変えると、サービスが取り込み直す。
            self.update(cx, move |settings| settings.catalog = location);
        } else {
            self.app.update(cx, |app, cx| app.refresh_catalog(cx));
        }
    }

    fn reset_catalog(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.catalog.update(cx, |input, cx| {
            input.set_value(DEFAULT_CATALOG_URL.to_string(), window, cx)
        });
        self.apply_catalog(cx);
    }

    fn choose_catalog_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("game-news-nexus-config を clone したフォルダを選ぶ".into()),
        });
        let input = self.catalog.clone();
        let page = cx.entity().downgrade();
        cx.spawn_in(window, async move |_, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = cx.update(|window, cx| {
                    input.update(cx, |input, cx| {
                        input.set_value(path.display().to_string(), window, cx)
                    });
                    let _ = page.update(cx, |page, cx| page.apply_catalog(cx));
                });
            }
        })
        .detach();
    }

    /// カタログの取り込みの状況。
    fn render_catalog(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let app = self.app.read(cx);
        let status = app.catalog_status().clone();
        let catalog = app.catalog().clone();
        let now = chrono::Utc::now();
        let mut facts = vec![format!("ソース {} 件", catalog.entries().len())];
        match status.checked_at {
            Some(at) => facts.push(format!(
                "最終確認 {}",
                crate::format::relative_time(at, now)
            )),
            None => facts.push("同梱の版を使用中".to_string()),
        }
        if let Some(revision) = &status.revision {
            facts.push(format!("版 {}", &revision[..revision.len().min(7)]));
        }
        let problems = catalog.problems().to_vec();
        if !problems.is_empty() {
            facts.push(format!("読めないファイル {} 件", problems.len()));
        }
        let controls = h_flex()
            .gap_2()
            .child(div().flex_1().child(Input::new(&self.catalog).small()))
            .child(
                Button::new("apply-catalog")
                    .small()
                    .primary()
                    .icon(IconName::RefreshCw)
                    .label("今すぐ取り込む")
                    .loading(status.checking)
                    .on_click(cx.listener(|this, _, _, cx| this.apply_catalog(cx))),
            )
            .child(
                Button::new("choose-catalog")
                    .small()
                    .outline()
                    .label("フォルダを選ぶ")
                    .on_click(
                        cx.listener(|this, _, window, cx| this.choose_catalog_dir(window, cx)),
                    ),
            )
            .child(
                Button::new("reset-catalog")
                    .small()
                    .ghost()
                    .label("既定に戻す")
                    .disabled(status.location == DEFAULT_CATALOG_URL)
                    .on_click(cx.listener(|this, _, window, cx| this.reset_catalog(window, cx))),
            );
        v_flex()
            .gap_2()
            .child(
                FormRow::new("取得定義のカタログ", controls).help(
                    "ゲームごとの取得の定義を、起動時と 1 日 1 回ここから取り込みます。取得の定義を書いて試すときは、リポジトリを clone したフォルダを指定できます。",
                ),
            )
            .child(
                h_flex()
                    .gap_2()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child(facts.join(" · "))
                    .child(
                        Button::new("open-catalog")
                            .xsmall()
                            .ghost()
                            .icon(IconName::ExternalLink)
                            .label("カタログを開く")
                            .on_click(|_, _, cx| open_web_url(CATALOG_REPOSITORY, cx)),
                    ),
            )
            .when_some(status.error.clone(), |this, error| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(theme.danger)
                        .child(format!("取り込めませんでした: {error}")),
                )
            })
            .children(problems.iter().take(5).map(|problem| {
                div()
                    .text_xs()
                    .text_color(match problem.kind {
                        ProblemKind::NeedsNewerApp => theme.muted_foreground,
                        ProblemKind::Invalid => theme.warning,
                    })
                    .child(problem.to_string())
            }))
            .into_any_element()
    }

    fn choose_export_dir(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("書き出し先を選ぶ".into()),
        });
        let input = self.export_dir.clone();
        cx.spawn_in(window, async move |_, cx| {
            if let Ok(Ok(Some(paths))) = receiver.await
                && let Some(path) = paths.into_iter().next()
            {
                let _ = cx.update(|window, cx| {
                    input.update(cx, |input, cx| {
                        input.set_value(path.display().to_string(), window, cx)
                    });
                });
            }
        })
        .detach();
    }

    fn import_legacy(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(PathPromptOptions {
            files: true,
            directories: false,
            multiple: false,
            prompt: Some("game_info.jsonl を選ぶ".into()),
        });
        let app = self.app.clone();
        let services = app.read(cx).services().clone();
        cx.spawn_in(window, async move |_, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            let result = cx
                .background_executor()
                .spawn(async move { services.import_legacy(&path) })
                .await;
            let _ = cx.update(|window, cx| {
                let note = match result {
                    Ok(summary) => Notification::success(format!(
                        "{} 件のうち {} 件を取り込みました (画像 {} 枚){}",
                        summary.total,
                        summary.added,
                        summary.images,
                        if summary.skipped > 0 {
                            format!("、読めない行 {} 件", summary.skipped)
                        } else {
                            String::new()
                        }
                    )),
                    Err(err) => Notification::error(format!("取り込めません: {err:#}")),
                };
                window.push_notification(note, cx);
                app.update(cx, |app, cx| app.entries_changed(cx));
            });
        })
        .detach();
    }

    fn toggle(
        &self,
        id: &'static str,
        label: &'static str,
        checked: bool,
        cx: &mut Context<Self>,
        edit: impl Fn(&mut AppSettings, bool) + 'static,
    ) -> impl IntoElement {
        Switch::new(id)
            .small()
            .label(label)
            .checked(checked)
            .on_click(cx.listener(move |this, value: &bool, _, cx| {
                let value = *value;
                this.update(cx, |settings| edit(settings, value));
            }))
    }
}

impl SettingsPage {
    /// ソースごとのアイコンと、画像を選ぶ・外すボタン。
    fn render_icons(&self, cx: &mut Context<Self>) -> AnyElement {
        let app = self.app.read(cx);
        let icons_dir = app.services().paths().icons_dir();
        let subscriptions = app.subscriptions().clone();
        let rows: Vec<AnyElement> = app
            .enabled_sources()
            .map(|source| {
                let has_icon = subscriptions
                    .get(&source.id)
                    .is_some_and(|item| item.icon.is_some());
                let pick = {
                    let app = self.app.clone();
                    let id = source.id.clone();
                    let name = source.name.clone();
                    let icons_dir = icons_dir.clone();
                    move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                        let app = app.clone();
                        let source_id = id.clone();
                        pick_and_crop(
                            id.clone(),
                            name.clone(),
                            icons_dir.clone(),
                            window,
                            cx,
                            move |path, window, cx| {
                                let icon = path.display().to_string();
                                let result = app
                                    .update(cx, |app, cx| app.set_icon(&source_id, Some(icon), cx));
                                let note = match result {
                                    Ok(()) => Notification::success("アイコンを設定しました"),
                                    Err(err) => {
                                        Notification::error(format!("保存できません: {err:#}"))
                                    }
                                };
                                window.push_notification(note, cx);
                            },
                        );
                    }
                };
                let clear = {
                    let app = self.app.clone();
                    let id = source.id.clone();
                    move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut App| {
                        if let Err(err) = app.update(cx, |app, cx| app.set_icon(&id, None, cx)) {
                            window.push_notification(
                                Notification::error(format!("保存できません: {err:#}")),
                                cx,
                            );
                        }
                    }
                };
                h_flex()
                    .gap_3()
                    .py_1()
                    .child(SourceAvatar::new(source.name.clone(), source.icon.clone()).large())
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_sm()
                            .child(source.name.clone()),
                    )
                    .child(
                        Button::new(SharedString::from(format!("pick-icon-{}", source.id)))
                            .small()
                            .outline()
                            .icon(IconName::ImagePlus)
                            .label("画像を選ぶ")
                            .on_click(pick),
                    )
                    .child(
                        Button::new(SharedString::from(format!("clear-icon-{}", source.id)))
                            .small()
                            .ghost()
                            .label("外す")
                            .disabled(!has_icon)
                            .on_click(clear),
                    )
                    .into_any_element()
            })
            .collect();
        v_flex().gap_1().children(rows).into_any_element()
    }

    fn set_autostart(&mut self, enabled: bool, window: &mut Window, cx: &mut Context<Self>) {
        if let Err(err) = crate::autostart::set_enabled(enabled) {
            window.push_notification(Notification::error(format!("{err:#}")), cx);
        }
        self.autostart = crate::autostart::is_enabled();
        cx.notify();
    }
}

fn folder_button(id: &'static str, label: &'static str, path: PathBuf) -> impl IntoElement {
    Button::new(id)
        .small()
        .outline()
        .icon(IconName::FolderOpen)
        .label(label)
        .on_click(move |_, _, cx: &mut App| cx.reveal_path(&path))
}

impl Render for SettingsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self.app.read(cx).settings().clone();
        let paths = self.app.read(cx).services().paths().clone();
        let theme = settings.theme;

        let numbers: Vec<_> = self
            .numbers
            .iter()
            .enumerate()
            .map(|(index, (field, state))| {
                let error = self
                    .errors
                    .iter()
                    .find(|(i, _)| *i == index)
                    .map(|(_, message)| *message);
                FormRow::new(
                    field.label(),
                    div().w(px(160.)).child(Input::new(state).small()),
                )
                .help(field.help())
                .error(error)
            })
            .collect();

        let theme_buttons = [
            (ThemePreference::System, "システムに合わせる"),
            (ThemePreference::Light, "ライト"),
            (ThemePreference::Dark, "ダーク"),
        ]
        .into_iter()
        .map(|(value, label)| {
            Button::new(SharedString::from(format!("theme-{value:?}")))
                .small()
                .outline()
                .selected(theme == value)
                .label(label)
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.update(cx, |settings| settings.theme = value)
                }))
        })
        .collect::<Vec<_>>();

        v_flex()
            .size_full()
            .child(PageHeader::new("設定").description("取得の間隔と常駐のしかた、表示を変えます。変更はすぐに保存されます。"))
            .child(
                div().id("settings-scroll").flex_1().min_h_0().overflow_y_scrollbar().child(
                    v_flex()
                        .gap_4()
                        .p_4()
                        .max_w(px(720.))
                        .child(SectionLabel::new("取得"))
                        .children(numbers)
                        .child(self.toggle("fetch-on-startup", "起動したらすぐ取得する", settings.fetch_on_startup, cx, |settings, value| {
                            settings.fetch_on_startup = value
                        }))
                        .child(SectionLabel::new("カタログ"))
                        .child(self.render_catalog(cx))
                        .child(SectionLabel::new("常駐と通知"))
                        .child(
                            Switch::new("autostart")
                                .small()
                                .label(crate::autostart::SETTING_LABEL)
                                .checked(self.autostart)
                                .on_click(cx.listener(|this, value: &bool, window, cx| {
                                    this.set_autostart(*value, window, cx)
                                })),
                        )
                        .child(self.toggle("close-to-tray", crate::tray::CLOSE_TO_TRAY_LABEL, settings.close_to_tray, cx, |settings, value| {
                            settings.close_to_tray = value
                        }))
                        .child(self.toggle("start-in-tray", crate::tray::START_IN_TRAY_LABEL, settings.start_in_tray, cx, |settings, value| {
                            settings.start_in_tray = value
                        }))
                        .child(self.toggle("notify", crate::shortcut::NOTIFY_SETTING_LABEL, settings.notify_new_entries, cx, |settings, value| {
                            settings.notify_new_entries = value;
                            if value {
                                crate::shortcut::ensure_in_background();
                            }
                        }))
                        .child(SectionLabel::new("表示"))
                        .child(FormRow::new("テーマ", h_flex().gap_1().children(theme_buttons)))
                        .child(SectionLabel::new("ゲームのアイコン"))
                        .child(
                            div()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child("使っているゲームのアイコンを変えます。画像を選ぶと正方形に切り抜く画面が開き、切り抜いたアイコンはニュースの一覧と本文に角丸で出ます。"),
                        )
                        .child(self.render_icons(cx))
                        .child(SectionLabel::new("旧形式 (dmm-games-info) との連携"))
                        .child(
                            FormRow::new(
                                "game_info.jsonl の書き出し先",
                                h_flex()
                                    .gap_2()
                                    .child(div().flex_1().child(Input::new(&self.export_dir).small().cleanable(true)))
                                    .child(
                                        Button::new("choose-export")
                                            .small()
                                            .outline()
                                            .label("選ぶ")
                                            .on_click(cx.listener(|this, _, window, cx| this.choose_export_dir(window, cx))),
                                    ),
                            )
                            .help("指定すると、取得のたびに旧形式の game_info.jsonl を書き出します。tauri-dmm-game-info などで読めます。"),
                        )
                        .child(
                            FormRow::new(
                                "旧形式の取り込み",
                                h_flex().child(
                                    Button::new("import-legacy")
                                        .small()
                                        .outline()
                                        .icon(IconName::Download)
                                        .label("game_info.jsonl を取り込む")
                                        .on_click(cx.listener(|this, _, window, cx| this.import_legacy(window, cx))),
                                ),
                            )
                            .help("dmm-games-info が書き出した記事を取り込みます。同じタイトルの記事を取得すると引き継ぎます。"),
                        )
                        .child(SectionLabel::new("データの場所"))
                        .child(
                            h_flex()
                                .gap_2()
                                .flex_wrap()
                                .child(folder_button("open-config", "設定", paths.settings_file()))
                                .child(folder_button("open-local-sources", "手元の定義", paths.local_sources_dir()))
                                .child(folder_button("open-data", "データベース", paths.database_file()))
                                .child(folder_button("open-logs", "ログ", paths.logs_dir()))
                                .child(folder_button("open-cache", "画像キャッシュ", paths.images_dir())),
                        )
                        .child(
                            div()
                                .pt_4()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(format!("Game News Nexus {}", env!("CARGO_PKG_VERSION"))),
                        ),
                ),
            )
    }
}
