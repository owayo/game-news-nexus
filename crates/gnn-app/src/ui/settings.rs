//! アプリの設定。表示・自動取得・常駐と通知・記事の保存・カタログ・詳細をカードに分け、各行は
//! 左に説明、右に操作で統一する (狭いときは操作を説明の下に置く)。変更はすぐに保存する。
//!
//! ゲームのアイコンはソース画面で変える (設定にゲームの一覧をもう 1 つ置かない)。

use std::path::PathBuf;

use chrono::{Local, Utc};
use gnn_core::catalog::{CATALOG_REPOSITORY, ProblemKind};
use gnn_core::{AppSettings, ThemePreference};
use gpui_kit::assets::IconName;
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::checkbox::Checkbox;
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, App, AppContext as _, Context, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, PathPromptOptions, Render, SharedString, Styled as _, Subscription, Window,
    div, px,
};

use crate::state::AppState;
use crate::ui::components::{AppLogo, Card, PageHeader, SettingRow, open_web_url, segmented};

/// 取得の間隔の選択肢 (分)。ほかの値は「カスタム」で分を入れる。
const INTERVALS: [(u32, &str); 6] = [
    (30, "30 分"),
    (60, "1 時間"),
    (180, "3 時間"),
    (360, "6 時間"),
    (720, "12 時間"),
    (1440, "24 時間"),
];

/// 選択肢の名前。選択肢になければ `None`。
fn interval_label(minutes: u32) -> Option<&'static str> {
    INTERVALS
        .iter()
        .find(|(value, _)| *value == minutes)
        .map(|(_, label)| *label)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    /// 入力欄の後ろに出す単位。
    fn unit(self) -> &'static str {
        match self {
            NumberField::Interval => "分",
            NumberField::MaxAge | NumberField::Retention => "日",
            NumberField::Concurrency => "件",
        }
    }
}

/// 成人向けの版を入れる確認で、何を出すようになるかの説明。
const ADULT_CONSENT_TEXT: &str = "FANZA GAMES の成人向けの版のお知らせを取得して表示します。お知らせの文章や画像には、性的な表現が含まれることがあります。";

pub struct SettingsPage {
    app: Entity<AppState>,
    /// 自動起動の状態。レジストリ (macOS は LaunchAgent) が正本なので、開いたときと切り替えたときに読む。
    autostart: bool,
    numbers: Vec<(NumberField, Entity<InputState>)>,
    export_dir: Entity<InputState>,
    errors: Vec<(NumberField, &'static str)>,
    /// 取得の間隔を分で入れているか (選択肢にない値か、「カスタム」を選んだ)。
    custom_interval: bool,
    /// 成人向けの版を入れる確認で、「18 歳以上です」に印を付けたか。確認を開くたびに外す。
    adult_age_confirmed: bool,
    _subscriptions: Vec<Subscription>,
}

impl SettingsPage {
    pub fn new(app: Entity<AppState>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = app.read(cx).settings().clone();
        let mut subscriptions = vec![cx.observe(&app, |_, _, cx| cx.notify())];
        let mut numbers = Vec::new();
        for field in NumberField::ALL {
            let value = field.read(&settings).to_string();
            let state = cx.new(|cx| InputState::new(window, cx).default_value(value));
            subscriptions.push(cx.subscribe_in(
                &state,
                window,
                move |this, state, event, _, cx| {
                    if matches!(event, InputEvent::Change) {
                        let text = state.read(cx).value().to_string();
                        this.apply_number(field, &text, cx);
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
        Self {
            app,
            autostart: crate::autostart::is_enabled(),
            numbers,
            export_dir,
            errors: Vec::new(),
            custom_interval: interval_label(settings.interval_minutes).is_none(),
            adult_age_confirmed: false,
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
        crate::ui::theme::apply(settings.theme, cx);
        cx.notify();
    }

    /// 数の入力欄。入れ途中の空欄や誤った値は保存せず、その行に誤りを出す。
    fn apply_number(&mut self, field: NumberField, text: &str, cx: &mut Context<Self>) {
        self.errors.retain(|(f, _)| *f != field);
        match text.trim().parse::<u64>() {
            Ok(value) => {
                let mut error = None;
                self.update(cx, |settings| {
                    if let Err(message) = field.write(settings, value) {
                        error = Some(message);
                    }
                });
                if let Some(message) = error {
                    self.errors.push((field, message));
                }
            }
            Err(_) => self.errors.push((field, "数を入れてください")),
        }
        cx.notify();
    }

    fn set_interval(&mut self, minutes: u32, window: &mut Window, cx: &mut Context<Self>) {
        self.custom_interval = false;
        self.errors
            .retain(|(field, _)| *field != NumberField::Interval);
        if let Some((_, state)) = self
            .numbers
            .iter()
            .find(|(field, _)| *field == NumberField::Interval)
        {
            // 入力欄の値を変えると、その知らせで保存する。
            state.update(cx, |state, cx| {
                state.set_value(minutes.to_string(), window, cx)
            });
        }
        self.update(cx, |settings| settings.interval_minutes = minutes);
    }

    fn number_input(&self, field: NumberField) -> impl IntoElement {
        let state = self
            .numbers
            .iter()
            .find(|(f, _)| *f == field)
            .map(|(_, state)| state.clone())
            .expect("数の入力欄");
        div().w(px(128.)).child(
            Input::new(&state)
                .small()
                .suffix(div().text_xs().child(field.unit())),
        )
    }

    fn error_of(&self, field: NumberField) -> Option<&'static str> {
        self.errors
            .iter()
            .find(|(f, _)| *f == field)
            .map(|(_, message)| *message)
    }

    /// 取得の間隔: 選択肢のメニューと、カスタムのときの分の入力欄。
    fn render_interval(&self, cx: &mut Context<Self>) -> impl IntoElement {
        let minutes = self.app.read(cx).settings().interval_minutes;
        let custom = self.custom_interval || interval_label(minutes).is_none();
        let label: SharedString = if custom {
            "カスタム".into()
        } else {
            interval_label(minutes).unwrap_or_default().into()
        };
        let page = cx.entity().downgrade();
        h_flex()
            .gap_2()
            .when(custom, |this| {
                this.child(self.number_input(NumberField::Interval))
            })
            .child(
                Button::new("interval-choice")
                    .small()
                    .outline()
                    .label(label)
                    .dropdown_caret(true)
                    .dropdown_menu(move |mut menu, _, _| {
                        for (value, text) in INTERVALS {
                            let page = page.clone();
                            menu = menu.item(
                                PopupMenuItem::new(text)
                                    .checked(!custom && value == minutes)
                                    .on_click(move |_, window, cx| {
                                        let _ = page.update(cx, |page, cx| {
                                            page.set_interval(value, window, cx)
                                        });
                                    }),
                            );
                        }
                        let page = page.clone();
                        menu.separator().item(
                            PopupMenuItem::new("カスタム (分で指定)")
                                .checked(custom)
                                .on_click(move |_, _, cx| {
                                    let _ = page.update(cx, |page, cx| {
                                        page.custom_interval = true;
                                        cx.notify();
                                    });
                                }),
                        )
                    }),
            )
    }

    /// カタログの取り込みの状況と操作。
    fn render_catalog(&self, stacked: bool, cx: &mut Context<Self>) -> AnyElement {
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
            None => facts.push("同梱の版を使っています".to_string()),
        }
        if let Some(revision) = &status.revision {
            facts.push(format!("版 {}", &revision[..revision.len().min(7)]));
        }
        let problems = catalog.problems().to_vec();
        Card::new()
            .title("カタログ")
            .description("ゲームごとの取得の定義を、起動時と 1 日 1 回、自動で取り込みます。")
            .divided()
            .child(
                SettingRow::new(
                    "取得の定義のカタログ",
                    h_flex()
                        .gap_2()
                        .child(
                            Button::new("open-catalog")
                                .small()
                                .ghost()
                                .icon(IconName::ExternalLink)
                                .label("カタログを開く")
                                .on_click(|_, _, cx| open_web_url(CATALOG_REPOSITORY, cx)),
                        )
                        .child(
                            Button::new("apply-catalog")
                                .small()
                                .outline()
                                .icon(IconName::RefreshCw)
                                .label("今すぐ確認")
                                .loading(status.checking)
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.app.update(cx, |app, cx| app.refresh_catalog(cx));
                                })),
                        ),
                )
                .description(facts.join(" · "))
                .error(
                    status
                        .error
                        .clone()
                        .map(|error| format!("取り込めませんでした: {error}")),
                )
                .stacked(stacked),
            )
            .when(!problems.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap_1()
                        .py_3()
                        .child(
                            div()
                                .text_xs()
                                .font_semibold()
                                .text_color(theme.muted_foreground)
                                .child(format!("読めないファイル {} 件", problems.len())),
                        )
                        .children(problems.iter().take(5).map(|problem| {
                            div()
                                .text_xs()
                                .text_color(match problem.kind {
                                    ProblemKind::NeedsNewerApp => theme.muted_foreground,
                                    ProblemKind::Invalid => theme.warning,
                                })
                                .child(problem.to_string())
                        })),
                )
            })
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

    fn switch(
        &self,
        id: &'static str,
        checked: bool,
        cx: &mut Context<Self>,
        edit: impl Fn(&mut AppSettings, bool) + 'static,
    ) -> impl IntoElement {
        Switch::new(id)
            .checked(checked)
            .on_click(cx.listener(move |this, value: &bool, _, cx| {
                let value = *value;
                this.update(cx, |settings| edit(settings, value));
            }))
    }

    /// 成人向けの版を使うかを切り替える。入れるときは毎回、18 歳以上であることを明示させ、
    /// 同意を得てから入れる (同意した時刻を設定に残す)。切るときは確かめず、同意も消す。
    fn set_adult_content(&mut self, enabled: bool, window: &mut Window, cx: &mut Context<Self>) {
        if !enabled {
            self.apply_adult_consent(None, window, cx);
            return;
        }
        self.adult_age_confirmed = false;
        let page = cx.entity().downgrade();
        window.open_dialog(cx, move |dialog, _, cx| {
            let theme = cx.theme();
            let confirmed = page
                .upgrade()
                .is_some_and(|page| page.read(cx).adult_age_confirmed);
            let check = page.clone();
            let accept = page.clone();
            dialog
                .title("18 歳以上であることの確認")
                .w(px(480.))
                .child(
                    v_flex()
                        .gap_3()
                        .child(div().text_sm().child(ADULT_CONSENT_TEXT))
                        .child(
                            div()
                                .text_sm()
                                .text_color(theme.muted_foreground)
                                .child("18 歳未満の方は有効にしないでください。有効にしたあとも、この設定でいつでも切れます。"),
                        )
                        .child(
                            Checkbox::new("adult-age")
                                .label("私は 18 歳以上です")
                                .checked(confirmed)
                                .on_click(move |checked: &bool, window, cx| {
                                    let checked = *checked;
                                    let _ = check.update(cx, |page, cx| {
                                        page.adult_age_confirmed = checked;
                                        cx.notify();
                                    });
                                    window.refresh();
                                }),
                        ),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .justify_end()
                        .gap_2()
                        .child(
                            Button::new("adult-cancel")
                                .small()
                                .ghost()
                                .label("キャンセル")
                                .on_click(|_, window, cx| window.close_dialog(cx)),
                        )
                        .child(
                            Button::new("adult-accept")
                                .small()
                                .primary()
                                .label("同意して有効にする")
                                .disabled(!confirmed)
                                .on_click(move |_, window, cx| {
                                    window.close_dialog(cx);
                                    let _ = accept.update(cx, |page, cx| {
                                        if std::mem::take(&mut page.adult_age_confirmed) {
                                            page.apply_adult_consent(Some(Utc::now()), window, cx);
                                        }
                                    });
                                }),
                        ),
                )
        });
    }

    /// 成人向けのソースの同意を設定に入れる・消す。保存できなければ知らせる (切るときは、保存に
    /// 失敗しても先に使うのをやめている)。
    fn apply_adult_consent(
        &mut self,
        consented_at: Option<chrono::DateTime<Utc>>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Err(err) = self
            .app
            .update(cx, |app, cx| app.set_adult_consent(consented_at, cx))
        {
            tracing::error!("設定を保存できません: {err:#}");
            window.push_notification(
                Notification::error(format!("設定を保存できません: {err:#}")),
                cx,
            );
        }
        cx.notify();
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let settings = self.app.read(cx).settings().clone();
        let paths = self.app.read(cx).services().paths().clone();
        let theme = settings.theme;
        // カードの中が狭いときは、操作を説明の下に置く。
        let stacked = window.viewport_size().width < px(880.);

        let display = Card::new().title("表示").divided().child(
            SettingRow::new(
                "テーマ",
                segmented(
                    "theme",
                    &["システム", "ライト", "ダーク"],
                    match theme {
                        ThemePreference::System => 0,
                        ThemePreference::Light => 1,
                        ThemePreference::Dark => 2,
                    },
                    cx.listener(|this, index: &usize, _, cx| {
                        let value = match index {
                            1 => ThemePreference::Light,
                            2 => ThemePreference::Dark,
                            _ => ThemePreference::System,
                        };
                        this.update(cx, |settings| settings.theme = value)
                    }),
                ),
            )
            .description("「システム」は OS のライト・ダークに合わせます。")
            .stacked(stacked),
        );

        let fetching = Card::new()
            .title("自動取得")
            .divided()
            .child(
                SettingRow::new("取得の間隔", self.render_interval(cx))
                    .description("ソースごとの指定がなければこの間隔で取得します。失敗が続くソースは間隔を延ばします。")
                    .error(self.error_of(NumberField::Interval))
                    .stacked(stacked),
            )
            .child(
                SettingRow::new(
                    "起動したらすぐ取得する",
                    self.switch("fetch-on-startup", settings.fetch_on_startup, cx, |settings, value| {
                        settings.fetch_on_startup = value
                    }),
                )
                .stacked(stacked),
            );

        let resident = Card::new()
            .title("常駐と通知")
            .divided()
            .child(
                SettingRow::new(
                    crate::autostart::SETTING_LABEL,
                    Switch::new("autostart")
                        .checked(self.autostart)
                        .on_click(cx.listener(|this, value: &bool, window, cx| {
                            this.set_autostart(*value, window, cx)
                        })),
                )
                .stacked(stacked),
            )
            .child(
                SettingRow::new(
                    crate::tray::CLOSE_TO_TRAY_LABEL,
                    self.switch(
                        "close-to-tray",
                        settings.close_to_tray,
                        cx,
                        |settings, value| settings.close_to_tray = value,
                    ),
                )
                .description("切ると、ウィンドウを閉じたときにアプリを終了します。")
                .stacked(stacked),
            )
            .child(
                SettingRow::new(
                    crate::tray::START_IN_TRAY_LABEL,
                    self.switch(
                        "start-in-tray",
                        settings.start_in_tray,
                        cx,
                        |settings, value| settings.start_in_tray = value,
                    ),
                )
                .stacked(stacked),
            )
            .child(
                SettingRow::new(
                    crate::shortcut::NOTIFY_SETTING_LABEL,
                    self.switch(
                        "notify",
                        settings.notify_new_entries,
                        cx,
                        |settings, value| {
                            settings.notify_new_entries = value;
                            if value {
                                crate::shortcut::ensure_in_background();
                            }
                        },
                    ),
                )
                .stacked(stacked),
            );

        let storage = Card::new()
            .title("記事の保存")
            .divided()
            .child(
                SettingRow::new("新しく取得する記事", self.number_input(NumberField::MaxAge))
                    .description("この日数より古い記事は取り込みません。0 で期間を制限しません。")
                    .error(self.error_of(NumberField::MaxAge))
                    .stacked(stacked),
            )
            .child(
                SettingRow::new(
                    "保存済みの記事を残す期間",
                    self.number_input(NumberField::Retention),
                )
                .description("これより古いお知らせを消します。0 で自動では消しません。")
                .error(self.error_of(NumberField::Retention))
                .stacked(stacked),
            );

        let adult = Card::new().title("成人向けの内容").divided().child(
            SettingRow::new(
                "成人向けのソースを使う (FANZA GAMES など)",
                Switch::new("adult-content")
                    .checked(settings.adult_content())
                    .on_click(cx.listener(|this, value: &bool, window, cx| {
                        this.set_adult_content(*value, window, cx)
                    })),
            )
            .description(match settings.adult_consented_at {
                Some(at) => format!(
                    "{} に 18 歳以上であることを確かめて有効にしました。FANZA GAMES の版があるゲームは、その版のお知らせを取得します。切ると成人向けのソースを使うのをやめ、そのお知らせを隠します (保存した記事は消しません)。",
                    at.with_timezone(&Local).format("%Y年%m月%d日 %H:%M")
                ),
                None => "DMM GAMES のゲームのうち、FANZA GAMES の成人向けの版があるものはその版のお知らせを取得し、FANZA GAMES だけのゲームもソース画面で選べるようにします。18 歳以上の方だけが使えます (入れるときに確かめます)。".to_string(),
            })
            .stacked(stacked),
        );

        let advanced = Card::new()
            .title("詳細")
            .divided()
            .child(
                SettingRow::new("同時に取得するソースの数", self.number_input(NumberField::Concurrency))
                    .description("同じサイトへの同時接続は、これとは別に 2 本までに抑えています。")
                    .error(self.error_of(NumberField::Concurrency))
                    .stacked(stacked),
            )
            .child(
                SettingRow::new(
                    "game_info.jsonl の書き出し先",
                    h_flex()
                        .gap_2()
                        .child(
                            div()
                                .w(px(220.))
                                .child(Input::new(&self.export_dir).small().cleanable(true)),
                        )
                        .child(
                            Button::new("choose-export")
                                .small()
                                .outline()
                                .label("選ぶ")
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.choose_export_dir(window, cx)
                                })),
                        ),
                )
                .description("指定すると、取得のたびに game_info.jsonl 形式で記事を書き出します。")
                .stacked(true),
            )
            .child(
                SettingRow::new(
                    "記事の取り込み",
                    Button::new("import-legacy")
                        .small()
                        .outline()
                        .icon(IconName::Download)
                        .label("game_info.jsonl を取り込む")
                        .on_click(cx.listener(|this, _, window, cx| this.import_legacy(window, cx))),
                )
                .description("game_info.jsonl 形式の記事を取り込みます。同じタイトルの記事を取得すると引き継ぎます。")
                .stacked(stacked),
            )
            .child(
                SettingRow::new(
                    "データの場所",
                    h_flex()
                        .gap_2()
                        .flex_wrap()
                        .child(folder_button("open-config", "設定", paths.settings_file()))
                        .child(folder_button("open-local-sources", "手元の定義", paths.local_sources_dir()))
                        .child(folder_button("open-data", "データベース", paths.database_file()))
                        .child(folder_button("open-logs", "ログ", paths.logs_dir()))
                        .child(folder_button("open-cache", "画像キャッシュ", paths.images_dir())),
                )
                .description("エクスプローラー (macOS は Finder) で開きます。")
                .stacked(true),
            );

        v_flex()
            .size_full()
            .child(PageHeader::new("設定").description("変更は自動で保存されます。"))
            .child(
                div()
                    .id("settings-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(
                        v_flex()
                            .w_full()
                            .max_w(px(880.))
                            .gap_5()
                            .px_8()
                            .py_6()
                            .child(display)
                            .child(fetching)
                            .child(resident)
                            .child(storage)
                            .child(adult)
                            .child(self.render_catalog(stacked, cx))
                            .child(advanced)
                            .child(
                                h_flex().gap_3().pt_2().child(AppLogo::new(px(40.))).child(
                                    v_flex()
                                        .gap_0p5()
                                        .child(
                                            div()
                                                .text_sm()
                                                .font_semibold()
                                                .child("Game News Nexus"),
                                        )
                                        .child(
                                            div()
                                                .text_xs()
                                                .text_color(cx.theme().muted_foreground)
                                                .child(format!(
                                                    "バージョン {}",
                                                    env!("CARGO_PKG_VERSION")
                                                )),
                                        ),
                                ),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interval_choices_cover_the_common_values() {
        assert_eq!(interval_label(60), Some("1 時間"));
        assert_eq!(interval_label(1440), Some("24 時間"));
        assert_eq!(interval_label(45), None);
        let mut settings = AppSettings::default();
        assert!(NumberField::Interval.write(&mut settings, 0).is_err());
        assert!(NumberField::Interval.write(&mut settings, 5000).is_ok());
        assert_eq!(settings.interval_minutes, 24 * 60);
    }
}
