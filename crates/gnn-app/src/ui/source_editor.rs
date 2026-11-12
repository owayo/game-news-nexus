//! 手元の取得定義の編集。
//!
//! 定義は JSON の値 (`serde_json::Value`) として持ち、フォームの各項目をその 1 か所
//! (`lists.0.request.url` のようなパス) に結び付ける。CSS セレクタや JSON Pointer は
//! そのまま書き、属性の指定などは 1 行の YAML (`{ selector: a, attr: href }`) で書ける。
//! フォームにない項目 (`prepare` など) は「YAML」タブで全文を直接書く。
//!
//! YAML タブで書いたコメントは、フォームで値を変えない限りそのまま保存する。

use std::collections::HashMap;

use gnn_core::config::source_file_text;
use gnn_core::source::{ConfigIssue, ListKind, ListSpec, SourceConfig};
use gnn_fetch::RunOutput;
use gpui_kit::assets::IconName;
use gpui_kit::component::StyledExt as _;
use gpui_kit::component::button::{Button, ButtonVariant, ButtonVariants as _};
use gpui_kit::component::input::{Editor, EditorState, Input, InputEvent, InputState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::scroll::ScrollableElement as _;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Icon, Selectable as _, Sizable as _, WindowExt as _,
    h_flex, v_flex,
};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::{
    AnyElement, AppContext as _, Context, Entity, EventEmitter, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, SharedString, Styled as _, Subscription, Task, Window,
    div, px, relative,
};
use serde_json::Value;

use crate::state::AppState;
use crate::ui::components::{
    Badge, BadgeTone, FormRow, RunResult, SectionLabel, SourceAvatar, StatusIcon,
};
use crate::ui::icon_cropper::pick_and_crop;

/// 編集画面から一覧への知らせ。
#[derive(Debug, Clone)]
pub enum EditorEvent {
    Saved(String),
    /// 定義を消した。カタログの上書きなら、カタログの定義に戻った。
    Deleted(String),
}

/// 何を編集しているか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorMode {
    /// 新しく作る手元のソース。
    New,
    /// 保存済みの手元だけのソース。
    Local,
    /// カタログのソースを手元で上書きしたもの。
    Override,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FieldKind {
    /// 文字列。
    Text,
    /// 文字列か、`{` `[` で始まる 1 行の YAML。
    Value,
    Number,
    Toggle {
        default: bool,
    },
    Choice(&'static [(&'static str, &'static str)]),
}

/// フォームの項目。`rel` は区切り (一覧なら `lists.N`) から見たパス。
#[derive(Debug)]
struct FieldDef {
    rel: &'static str,
    label: &'static str,
    help: &'static str,
    kind: FieldKind,
}

const fn field(
    rel: &'static str,
    label: &'static str,
    help: &'static str,
    kind: FieldKind,
) -> FieldDef {
    FieldDef {
        rel,
        label,
        help,
        kind,
    }
}

/// 配置した項目。
struct Field {
    path: String,
    def: &'static FieldDef,
}

struct Section {
    title: String,
    fields: Vec<Field>,
    /// 一覧の区切りなら、その番号。
    list: Option<usize>,
}

const KINDS: &[(&str, &str)] = &[
    ("feed", "RSS / Atom"),
    ("json", "JSON API"),
    ("html", "HTML の一覧"),
    ("html_json", "HTML 埋め込み JSON"),
];
const DETAIL_KINDS: &[(&str, &str)] = &[
    ("none", "取得しない"),
    ("html", "HTML"),
    ("json", "JSON"),
    ("html_json", "HTML 埋め込み JSON"),
];
const FEED_CONTENT: &[(&str, &str)] = &[("auto", "自動"), ("content", "本文"), ("summary", "概要")];
const BODY_FORMATS: &[(&str, &str)] = &[("html", "HTML"), ("text", "テキスト")];

const ID_FIELD: FieldDef = field(
    "id",
    "ID",
    "英小文字・数字・_・- で。保存後は変えられません。",
    FieldKind::Text,
);

const BASIC: &[FieldDef] = &[field("name", "名前", "", FieldKind::Text)];

const LIST_HEAD: &[FieldDef] = &[
    field(
        "kind",
        "取得方式",
        "変えると、この一覧の読み方の項目が初期値に戻る。",
        FieldKind::Choice(KINDS),
    ),
    field(
        "name",
        "一覧の名前",
        "失敗したときの表示に使う。空欄でもよい。",
        FieldKind::Text,
    ),
    field(
        "request.url",
        "URL",
        "{{vars.名前}} で事前の取得 (YAML タブの prepare) の値を入れられる。",
        FieldKind::Text,
    ),
];

const PAGES: FieldDef = field(
    "pages",
    "ページ送り",
    "{ kind: page, param: page, max_pages: 3 } や { kind: next_link, selector: 'a.next', max_pages: 5 }。空欄なら 1 ページだけ。",
    FieldKind::Value,
);

const FEED: &[FieldDef] = &[field(
    "content",
    "本文に使う要素",
    "",
    FieldKind::Choice(FEED_CONTENT),
)];

const JSON_HELP: &str = "記事から見た JSON Pointer (/title など)。テンプレート ('https://example.com/{{/id}}') も書ける。";
const HTML_HELP: &str =
    "記事要素から見た CSS セレクタ。属性は { selector: a, attr: href } のように書く。";

const JSON_FIELDS: &[FieldDef] = &[
    field("fields.id", "ID", JSON_HELP, FieldKind::Value),
    field("fields.title", "タイトル", JSON_HELP, FieldKind::Value),
    field("fields.url", "URL", JSON_HELP, FieldKind::Value),
    field(
        "fields.published",
        "投稿日時",
        "日時の文字列か UNIX 時刻。",
        FieldKind::Value,
    ),
    field(
        "fields.body",
        "本文",
        "HTML の文字列。{ blocks: /bodies, text: /text, image: /imageUrl } でブロックの配列も読める。",
        FieldKind::Value,
    ),
    field("fields.image", "画像", JSON_HELP, FieldKind::Value),
    field("fields.note", "補足", JSON_HELP, FieldKind::Value),
];

const JSON: &[FieldDef] = &[
    field(
        "items",
        "記事の配列",
        "JSON Pointer。/data/items のように書く。空欄はルート。",
        FieldKind::Text,
    ),
    field(
        "filter",
        "条件",
        "[{ pointer: /visible, eq: true }] のように書く。空欄なら全件。",
        FieldKind::Value,
    ),
];

const HTML: &[FieldDef] = &[
    field(
        "items",
        "記事の要素",
        "記事 1 件に当たる要素の CSS セレクタ。",
        FieldKind::Text,
    ),
    field(
        "require",
        "必須の要素",
        "これを含む記事だけを残す (CSS セレクタ)。",
        FieldKind::Text,
    ),
    field("fields.id", "ID", HTML_HELP, FieldKind::Value),
    field("fields.title", "タイトル", HTML_HELP, FieldKind::Value),
    field(
        "fields.url",
        "URL",
        "空欄なら記事要素か、その中の最初のリンク。",
        FieldKind::Value,
    ),
    field("fields.published", "投稿日時", HTML_HELP, FieldKind::Value),
    field("fields.body", "本文", HTML_HELP, FieldKind::Value),
    field(
        "fields.image",
        "画像",
        "空欄なら取らない。{ selector: img, attr: src } など。",
        FieldKind::Value,
    ),
    field("fields.note", "補足", HTML_HELP, FieldKind::Value),
];

const HTML_JSON: &[FieldDef] = &[
    field(
        "selector",
        "JSON を持つ要素",
        "CSS セレクタ。",
        FieldKind::Text,
    ),
    field(
        "attr",
        "JSON を持つ属性",
        "空欄なら要素の中身を JSON として読む。",
        FieldKind::Text,
    ),
    field(
        "items",
        "記事の配列",
        "JSON Pointer。空欄はルート。",
        FieldKind::Text,
    ),
    field(
        "filter",
        "条件",
        "[{ pointer: /visible, eq: true }] のように書く。",
        FieldKind::Value,
    ),
];

const DETAIL_KIND: FieldDef = field(
    "detail.kind",
    "詳細ページ",
    "一覧で本文が取れないとき、記事ごとに詳細ページを取る。",
    FieldKind::Choice(DETAIL_KINDS),
);

const DETAIL_URL: FieldDef = field(
    "detail.request.url",
    "詳細の URL",
    "{{item.id}} {{item.url}} {{item.title}} と、JSON の一覧なら {{/pointer}} が使える。空欄なら記事の URL。",
    FieldKind::Text,
);

const DETAIL_HTML: &[FieldDef] = &[
    field(
        "detail.fields.title",
        "タイトル",
        "詳細ページから取ると一覧の値より優先する。",
        FieldKind::Value,
    ),
    field("detail.fields.published", "投稿日時", "", FieldKind::Value),
    field(
        "detail.fields.body",
        "本文",
        "空欄なら <body> 全体。{ selector: '#main', remove: [h2] } で一部を除ける。",
        FieldKind::Value,
    ),
    field("detail.fields.image", "画像", "", FieldKind::Value),
];

const DETAIL_JSON: &[FieldDef] = &[
    field(
        "detail.fields.title",
        "タイトル",
        "応答から見た JSON Pointer。",
        FieldKind::Value,
    ),
    field("detail.fields.published", "投稿日時", "", FieldKind::Value),
    field("detail.fields.body", "本文", "", FieldKind::Value),
    field("detail.fields.image", "画像", "", FieldKind::Value),
];

const DETAIL_HTML_JSON: &[FieldDef] = &[
    field(
        "detail.selector",
        "JSON を持つ要素",
        "CSS セレクタ。",
        FieldKind::Text,
    ),
    field(
        "detail.attr",
        "JSON を持つ属性",
        "空欄なら要素の中身。",
        FieldKind::Text,
    ),
    field(
        "detail.item",
        "記事の値",
        "JSON Pointer。空欄なら JSON 全体。",
        FieldKind::Text,
    ),
];

const BODY: &[FieldDef] = &[
    field(
        "body.format",
        "本文の形式",
        "テキストなら HTML の特殊文字を逃がし、改行を改行として出す。",
        FieldKind::Choice(BODY_FORMATS),
    ),
    field(
        "body.newlines_to_br",
        "改行を改行として出す",
        "",
        FieldKind::Toggle { default: false },
    ),
    field(
        "body.image_in_body",
        "画像を本文の先頭に入れる",
        "",
        FieldKind::Toggle { default: false },
    ),
    field(
        "body.tidy",
        "空の要素や余分な改行を除く",
        "",
        FieldKind::Toggle { default: true },
    ),
    field(
        "body.remove",
        "取り除く要素",
        "['.ad', '.share'] のように CSS セレクタを並べる。",
        FieldKind::Value,
    ),
    field(
        "body.base_url",
        "相対 URL の基準",
        "空欄なら本文を取得した URL。",
        FieldKind::Text,
    ),
];

const SCHEDULE: &[FieldDef] = &[
    field(
        "interval_minutes",
        "取得間隔 (分)",
        "空欄ならアプリ設定の既定値。",
        FieldKind::Number,
    ),
    field(
        "max_age_days",
        "取り込む期間 (日)",
        "これより古い記事は読まない。0 で無制限。",
        FieldKind::Number,
    ),
    field(
        "max_items",
        "1 回に読む記事数の上限",
        "詳細ページを取る数を抑える。空欄なら 100。",
        FieldKind::Number,
    ),
    field(
        "timezone",
        "タイムゾーン",
        "時差のない日時をどの地域の時刻として読むか。空欄なら Asia/Tokyo。",
        FieldKind::Text,
    ),
];

const SCHEDULE_READING: &[FieldDef] = &[
    field(
        "schedule.timezone",
        "本文の日時の地域時刻",
        "本文のメンテナンスなどの日時に (JST) などの明記がないとき、どの地域の時刻として読むか。空欄なら上のタイムゾーン。",
        FieldKind::Text,
    ),
    field(
        "schedule.server_timezone",
        "「サーバー時間」の地域時刻",
        "本文に「サーバー時間」と書かれた日時の地域時刻 (Asia/Shanghai など)。空欄なら決めない。",
        FieldKind::Text,
    ),
];

const DISPLAY: &[FieldDef] = &[
    field(
        "icon",
        "アイコンの URL",
        "配信元の画像の URL。手元の画像は下の「画像から作る」で設定する。",
        FieldKind::Text,
    ),
    field("homepage", "公式サイト", "", FieldKind::Text),
    field("note", "説明", "", FieldKind::Text),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EditorTab {
    Form,
    Yaml,
}

enum TestState {
    Idle,
    Running,
    Done(Box<RunOutput>),
}

pub struct SourceEditor {
    app: Entity<AppState>,
    mode: EditorMode,
    /// 保存済みの定義と、その YAML。新規なら `None`。
    original: Option<(SourceConfig, String)>,
    draft: Value,
    /// 保存するときに使う YAML。フォームで値を変えるまでは、読み込んだ全文 (コメントを含む)。
    yaml_text: String,
    /// フォームで値を変えたか。変えたら YAML は下書きから作り直す。
    form_touched: bool,
    inputs: HashMap<String, Entity<InputState>>,
    input_errors: HashMap<String, String>,
    issues: Vec<ConfigIssue>,
    parse_error: Option<String>,
    tab: EditorTab,
    yaml_editor: Entity<EditorState>,
    yaml_error: Option<String>,
    test: TestState,
    dirty: bool,
    test_task: Task<()>,
    _subscriptions: Vec<Subscription>,
    input_subscriptions: Vec<Subscription>,
}

impl EventEmitter<EditorEvent> for SourceEditor {}

impl SourceEditor {
    /// `text` は保存済みの YAML (新規なら `None`)。
    pub fn new(
        app: Entity<AppState>,
        source: SourceConfig,
        text: Option<String>,
        mode: EditorMode,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let draft = serde_json::to_value(&source).unwrap_or_default();
        let yaml_text = match &text {
            Some(text) => text.clone(),
            None => source_file_text(&source).unwrap_or_default(),
        };
        let yaml_editor = cx.new(|cx| {
            EditorState::new(window, cx).placeholder("取得定義の YAML (schema: 2 から書く)")
        });
        // YAML タブで書き換えたら保存できるようにする。読み込みと検証は保存時とフォームに戻るとき。
        let yaml_subscription = cx.subscribe(&yaml_editor, |this, _, event: &InputEvent, cx| {
            if matches!(event, InputEvent::Change) && this.tab == EditorTab::Yaml {
                this.dirty = true;
                this.yaml_error = None;
                cx.notify();
            }
        });
        let mut editor = Self {
            app,
            mode,
            original: text.map(|text| (source, text)),
            draft,
            yaml_text,
            form_touched: false,
            inputs: HashMap::new(),
            input_errors: HashMap::new(),
            issues: Vec::new(),
            parse_error: None,
            tab: EditorTab::Form,
            yaml_editor,
            yaml_error: None,
            test: TestState::Idle,
            dirty: mode == EditorMode::New,
            test_task: Task::ready(()),
            _subscriptions: vec![yaml_subscription],
            input_subscriptions: Vec::new(),
        };
        editor.rebuild_inputs(window, cx);
        editor.validate(cx);
        if cfg!(debug_assertions) && std::env::var("GNN_DEV_TAB").as_deref() == Ok("yaml") {
            // 開発用: YAML タブを開いた状態にする (画面を操作せずに確かめるため)。
            editor.switch_tab(EditorTab::Yaml, window, cx);
        }
        editor
    }

    pub fn source_id(&self) -> Option<String> {
        get(&self.draft, "id")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn is_new(&self) -> bool {
        self.mode == EditorMode::New
    }

    fn list_count(&self) -> usize {
        get(&self.draft, "lists")
            .and_then(Value::as_array)
            .map_or(0, Vec::len)
    }

    /// フォームの並び。
    fn layout(&self) -> Vec<Section> {
        let place = |prefix: &str, defs: &'static [FieldDef]| -> Vec<Field> {
            defs.iter()
                .map(|def| Field {
                    path: if prefix.is_empty() {
                        def.rel.to_string()
                    } else {
                        format!("{prefix}.{}", def.rel)
                    },
                    def,
                })
                .collect()
        };
        let mut sections = Vec::new();

        let mut basic = place("", BASIC);
        if self.is_new() {
            basic.insert(
                1,
                Field {
                    path: "id".into(),
                    def: &ID_FIELD,
                },
            );
        }
        sections.push(Section {
            title: "基本".into(),
            fields: basic,
            list: None,
        });

        for index in 0..self.list_count() {
            let prefix = format!("lists.{index}");
            let kind = get(&self.draft, &format!("{prefix}.kind"))
                .and_then(Value::as_str)
                .unwrap_or("feed");
            let mut fields = place(&prefix, LIST_HEAD);
            match kind {
                "json" => {
                    fields.extend(place(&prefix, JSON));
                    fields.extend(place(&prefix, JSON_FIELDS));
                }
                "html" => fields.extend(place(&prefix, HTML)),
                "html_json" => {
                    fields.extend(place(&prefix, HTML_JSON));
                    fields.extend(place(&prefix, JSON_FIELDS));
                }
                _ => fields.extend(place(&prefix, FEED)),
            }
            if kind != "feed" {
                fields.push(Field {
                    path: format!("{prefix}.{}", PAGES.rel),
                    def: &PAGES,
                });
            }
            let name = get(&self.draft, &format!("{prefix}.name"))
                .and_then(Value::as_str)
                .map(|name| format!("「{name}」"))
                .unwrap_or_default();
            sections.push(Section {
                title: format!("一覧 {}{name}", index + 1),
                fields,
                list: Some(index),
            });
        }

        let mut detail = vec![Field {
            path: DETAIL_KIND.rel.into(),
            def: &DETAIL_KIND,
        }];
        let detail_kind = get(&self.draft, "detail.kind").and_then(Value::as_str);
        if detail_kind.is_some() {
            detail.push(Field {
                path: DETAIL_URL.rel.into(),
                def: &DETAIL_URL,
            });
        }
        match detail_kind {
            Some("html") => detail.extend(place("", DETAIL_HTML)),
            Some("json") => detail.extend(place("", DETAIL_JSON)),
            Some("html_json") => {
                detail.extend(place("", DETAIL_HTML_JSON));
                detail.extend(place("", DETAIL_JSON));
            }
            _ => {}
        }
        sections.push(Section {
            title: "記事の詳細".into(),
            fields: detail,
            list: None,
        });
        for (title, defs) in [
            ("本文の整形", BODY),
            ("取得の設定", SCHEDULE),
            ("予定の読み取り", SCHEDULE_READING),
            ("表示", DISPLAY),
        ] {
            sections.push(Section {
                title: title.into(),
                fields: place("", defs),
                list: None,
            });
        }
        sections
    }

    /// 今の配置に合わせて入力欄を作り直す。
    fn rebuild_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.inputs.clear();
        self.input_errors.clear();
        self.input_subscriptions.clear();
        for section in self.layout() {
            for field in section.fields {
                if !matches!(
                    field.def.kind,
                    FieldKind::Text | FieldKind::Value | FieldKind::Number
                ) {
                    continue;
                }
                let value = get(&self.draft, &field.path)
                    .map(display_value)
                    .unwrap_or_default();
                let state = cx.new(|cx| InputState::new(window, cx).default_value(value));
                let path = field.path.clone();
                let kind = field.def.kind;
                self.input_subscriptions.push(cx.subscribe_in(
                    &state,
                    window,
                    move |this, state, event, _, cx| {
                        if matches!(event, InputEvent::Change) {
                            let text = state.read(cx).value().to_string();
                            this.apply_text(&path, kind, &text, cx);
                        }
                    },
                ));
                self.inputs.insert(field.path, state);
            }
        }
    }

    fn touch(&mut self, cx: &mut Context<Self>) {
        self.dirty = true;
        self.form_touched = true;
        self.validate(cx);
    }

    fn apply_text(&mut self, path: &str, kind: FieldKind, text: &str, cx: &mut Context<Self>) {
        match parse_input(text, kind) {
            Ok(value) => {
                self.input_errors.remove(path);
                set(&mut self.draft, path, value);
            }
            Err(message) => {
                self.input_errors.insert(path.to_string(), message);
            }
        }
        self.touch(cx);
    }

    fn set_choice(&mut self, path: &str, value: &str, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(index) = list_kind_path(path) {
            let Some(kind) = ListKind::parse(value) else {
                return;
            };
            let prefix = format!("lists.{index}");
            let url = get(&self.draft, &format!("{prefix}.request.url"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let mut list = serde_json::to_value(ListSpec::empty(kind, url)).unwrap_or_default();
            if let Some(name) = get(&self.draft, &format!("{prefix}.name")).cloned() {
                set(&mut list, "name", Some(name));
            }
            set(&mut self.draft, &prefix, Some(list));
            self.rebuild_inputs(window, cx);
        } else if path == "detail.kind" {
            if value == "none" {
                set(&mut self.draft, "detail", None);
            } else {
                let request = get(&self.draft, "detail.request").cloned();
                let mut detail = serde_json::json!({ "kind": value });
                if value == "html_json" {
                    set(&mut detail, "selector", Some(Value::String(String::new())));
                }
                if let Some(request) = request {
                    set(&mut detail, "request", Some(request));
                }
                set(&mut self.draft, "detail", Some(detail));
            }
            self.rebuild_inputs(window, cx);
        } else {
            set(&mut self.draft, path, Some(Value::String(value.into())));
        }
        self.touch(cx);
    }

    fn set_toggle(&mut self, path: &str, value: bool, default: bool, cx: &mut Context<Self>) {
        set(
            &mut self.draft,
            path,
            (value != default).then_some(Value::Bool(value)),
        );
        self.touch(cx);
    }

    fn add_list(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let list = serde_json::to_value(ListSpec::empty(ListKind::Feed, "")).unwrap_or_default();
        if let Some(Value::Array(lists)) = self.draft.get_mut("lists") {
            lists.push(list);
        }
        self.rebuild_inputs(window, cx);
        self.touch(cx);
    }

    fn remove_list(&mut self, index: usize, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(Value::Array(lists)) = self.draft.get_mut("lists")
            && lists.len() > 1
            && index < lists.len()
        {
            lists.remove(index);
        }
        self.rebuild_inputs(window, cx);
        self.touch(cx);
    }

    fn validate(&mut self, cx: &mut Context<Self>) {
        match parse_draft(&self.draft) {
            Ok(config) => {
                self.parse_error = None;
                self.issues = gnn_fetch::validate(&config);
                if self.is_new() {
                    let app = self.app.read(cx);
                    if app.catalog().get(&config.id).is_some()
                        || app.local_file(&config.id).is_some()
                    {
                        self.issues.push(ConfigIssue {
                            field: "id".into(),
                            message: "同じ ID のソースがあります".into(),
                        });
                    }
                }
            }
            Err(err) => {
                self.parse_error = Some(err);
                self.issues.clear();
            }
        }
        cx.notify();
    }

    fn issue_for(&self, path: &str) -> Option<String> {
        if let Some(error) = self.input_errors.get(path) {
            return Some(error.clone());
        }
        let path = issue_path(path);
        self.issues
            .iter()
            .find(|issue| {
                issue.field == path
                    || issue.field.starts_with(&format!("{path}."))
                    || issue.field.starts_with(&format!("{path}["))
            })
            .map(|issue| issue.message.clone())
    }

    /// フォームのどこにも出ない問題 (YAML タブで書く項目など)。
    fn other_issues(&self) -> Vec<String> {
        let shown: Vec<String> = self
            .layout()
            .into_iter()
            .flat_map(|section| {
                section
                    .fields
                    .into_iter()
                    .map(|field| issue_path(&field.path))
            })
            .collect();
        self.issues
            .iter()
            .filter(|issue| {
                !shown.iter().any(|path| {
                    issue.field == *path
                        || issue.field.starts_with(&format!("{path}."))
                        || issue.field.starts_with(&format!("{path}["))
                })
            })
            .map(ToString::to_string)
            .collect()
    }

    fn has_errors(&self) -> bool {
        self.parse_error.is_some() || !self.issues.is_empty() || !self.input_errors.is_empty()
    }

    fn switch_tab(&mut self, tab: EditorTab, window: &mut Window, cx: &mut Context<Self>) {
        if self.tab == tab {
            return;
        }
        match tab {
            EditorTab::Yaml => {
                if self.form_touched
                    && let Ok(config) = parse_draft(&self.draft)
                    && let Ok(text) = source_file_text(&config)
                {
                    self.yaml_text = text;
                    self.form_touched = false;
                }
                let text = self.yaml_text.clone();
                self.yaml_editor
                    .update(cx, |editor, cx| editor.set_value(text, window, cx));
                self.yaml_error = None;
            }
            EditorTab::Form => {
                if !self.apply_yaml(window, cx) {
                    return;
                }
            }
        }
        self.tab = tab;
        cx.notify();
    }

    /// YAML タブの内容を下書きにする。読めなければ `false`。
    fn apply_yaml(&mut self, window: &mut Window, cx: &mut Context<Self>) -> bool {
        let text = self.yaml_editor.read(cx).value().to_string();
        match SourceConfig::from_yaml(&text) {
            Ok(config) => {
                if !self.is_new() && Some(config.id.clone()) != self.source_id() {
                    self.yaml_error = Some("保存済みのソースの ID は変えられません".into());
                    cx.notify();
                    return false;
                }
                let draft = serde_json::to_value(&config).unwrap_or_default();
                if draft != self.draft || text != self.yaml_text {
                    self.dirty = true;
                }
                self.draft = draft;
                self.yaml_text = text;
                self.form_touched = false;
                self.yaml_error = None;
                self.rebuild_inputs(window, cx);
                self.validate(cx);
                true
            }
            Err(err) => {
                self.yaml_error = Some(format!("YAML を読めません: {err}"));
                cx.notify();
                false
            }
        }
    }

    /// 保存する定義と、その YAML の全文。
    fn current(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<(SourceConfig, String)> {
        if self.tab == EditorTab::Yaml && !self.apply_yaml(window, cx) {
            return None;
        }
        let config = parse_draft(&self.draft).ok()?;
        let text = if self.form_touched {
            source_file_text(&config).ok()?
        } else {
            self.yaml_text.clone()
        };
        Some((config, text))
    }

    fn save(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((config, text)) = self.current(window, cx) else {
            return;
        };
        if self.has_errors() {
            window.push_notification(
                Notification::error("設定に誤りがあるため保存できません"),
                cx,
            );
            return;
        }
        let id = config.id.clone();
        let is_new = self.is_new();
        let result = self
            .app
            .update(cx, |app, cx| app.save_local_source(&id, &text, is_new, cx));
        match result {
            Ok(()) => {
                if is_new {
                    self.mode = EditorMode::Local;
                }
                self.original = Some((config, text.clone()));
                self.yaml_text = text;
                self.form_touched = false;
                self.dirty = false;
                if is_new {
                    self.rebuild_inputs(window, cx);
                }
                window.push_notification(Notification::success("保存しました"), cx);
                cx.emit(EditorEvent::Saved(id));
                cx.notify();
            }
            Err(err) => {
                window
                    .push_notification(Notification::error(format!("保存できません: {err:#}")), cx);
            }
        }
    }

    fn revert(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((original, text)) = self.original.clone() else {
            return;
        };
        self.draft = serde_json::to_value(&original).unwrap_or_default();
        self.yaml_text = text.clone();
        self.form_touched = false;
        self.dirty = false;
        if self.tab == EditorTab::Yaml {
            self.yaml_editor
                .update(cx, |editor, cx| editor.set_value(text, window, cx));
        }
        self.rebuild_inputs(window, cx);
        self.validate(cx);
    }

    fn run_test(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((config, _)) = self.current(window, cx) else {
            return;
        };
        if self.has_errors() {
            window.push_notification(Notification::error("設定に誤りがあるため試せません"), cx);
            return;
        }
        self.test = TestState::Running;
        let receiver = self.app.read(cx).services().test_fetch(config, false, 5);
        self.test_task = cx.spawn(async move |this, cx| {
            let output = receiver.await;
            let _ = this.update(cx, |this, cx| {
                this.test = match output {
                    Ok(output) => TestState::Done(Box::new(output)),
                    Err(_) => TestState::Idle,
                };
                cx.notify();
            });
        });
        cx.notify();
    }

    fn fetch_now(&mut self, cx: &mut Context<Self>) {
        if let Some((original, _)) = &self.original {
            let id = original.id.clone();
            self.app
                .update(cx, |app, cx| app.fetch_now(Some(vec![id]), cx));
        }
    }

    /// 今の出どころ。編集画面を開いたあとでカタログが変わることがあるので、そのつど決める。
    fn current_mode(&self, cx: &gpui_kit::App) -> EditorMode {
        match (&self.original, self.mode) {
            (None, _) | (_, EditorMode::New) => EditorMode::New,
            (Some((source, _)), _) => {
                if self.app.read(cx).catalog().get(&source.id).is_some() {
                    EditorMode::Override
                } else {
                    EditorMode::Local
                }
            }
        }
    }

    fn confirm_delete(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some((original, _)) = self.original.clone() else {
            return;
        };
        let editor = cx.entity().downgrade();
        let name = original.name.clone();
        // 確かめた内容と、実際に消す範囲を合わせる。
        let delete_entries = self.current_mode(cx) == EditorMode::Local;
        let (title, description, ok) = if delete_entries {
            (
                format!("「{name}」を削除しますか？"),
                "このソースから取得した記事もすべて消えます。",
                "削除",
            )
        } else {
            (
                format!("「{name}」をカタログの定義に戻しますか？"),
                "手元で書き換えた定義を消します。取得した記事は残ります。",
                "カタログに戻す",
            )
        };
        window.open_alert_dialog(cx, move |alert, _, _| {
            let editor = editor.clone();
            alert
                .title(title.clone())
                .description(description)
                .confirm()
                .ok_text(ok)
                .ok_variant(ButtonVariant::Danger)
                .on_ok(move |_, window, cx| {
                    let _ =
                        editor.update(cx, |editor, cx| editor.delete(delete_entries, window, cx));
                    true
                })
        });
    }

    fn delete(&mut self, delete_entries: bool, window: &mut Window, cx: &mut Context<Self>) {
        let Some((original, _)) = self.original.clone() else {
            return;
        };
        let result = self.app.update(cx, |app, cx| {
            app.delete_local_source(&original.id, delete_entries, cx)
        });
        match result {
            Ok(()) => cx.emit(EditorEvent::Deleted(original.id)),
            Err(err) => window
                .push_notification(Notification::error(format!("削除できません: {err:#}")), cx),
        }
    }

    /// 画像を選んで切り抜き、このアプリでのアイコンにする (すぐ保存する)。
    fn pick_icon(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let Some(id) = self.original.as_ref().map(|(source, _)| source.id.clone()) else {
            return;
        };
        let name = get(&self.draft, "name")
            .and_then(Value::as_str)
            .unwrap_or(&id)
            .to_string();
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

    fn clear_icon(&mut self, cx: &mut Context<Self>) {
        if let Some((source, _)) = &self.original {
            let id = source.id.clone();
            if let Err(err) = self.app.update(cx, |app, cx| app.set_icon(&id, None, cx)) {
                tracing::error!("アイコンを外せません: {err:#}");
            }
        }
    }

    /// このアプリでのアイコン (購読に保存する手元の画像)。
    fn render_local_icon(&self, cx: &mut Context<Self>) -> AnyElement {
        let saved = self.original.is_some();
        let id = self.source_id().unwrap_or_default();
        let app = self.app.read(cx);
        let local_icon = app
            .subscriptions()
            .get(&id)
            .and_then(|item| item.icon.clone());
        let name = get(&self.draft, "name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let icon = local_icon.clone().or_else(|| {
            get(&self.draft, "icon")
                .and_then(Value::as_str)
                .map(str::to_string)
        });
        let control = h_flex()
            .gap_2()
            .child(SourceAvatar::new(name, icon))
            .child(
                Button::new("editor-pick-icon")
                    .small()
                    .outline()
                    .icon(IconName::ImagePlus)
                    .label("画像から作る")
                    .disabled(!saved)
                    .on_click(cx.listener(|this, _, window, cx| this.pick_icon(window, cx))),
            )
            .when(local_icon.is_some(), |this| {
                this.child(
                    Button::new("editor-clear-icon")
                        .small()
                        .ghost()
                        .label("外す")
                        .on_click(cx.listener(|this, _, _, cx| this.clear_icon(cx))),
                )
            });
        FormRow::new("このアプリでのアイコン", control)
            .help(if saved {
                "切り抜いた画像は、このパソコンだけに保存します。"
            } else {
                "保存してから設定できます。"
            })
            .into_any_element()
    }

    fn render_field(&self, field: &Field, cx: &mut Context<Self>) -> AnyElement {
        let error = self.issue_for(&field.path);
        let spec = field.def;
        let control: AnyElement = match spec.kind {
            FieldKind::Text | FieldKind::Value | FieldKind::Number => {
                match self.inputs.get(&field.path) {
                    Some(state) => Input::new(state).small().into_any_element(),
                    None => div().into_any_element(),
                }
            }
            FieldKind::Toggle { default } => {
                let checked = get(&self.draft, &field.path)
                    .and_then(Value::as_bool)
                    .unwrap_or(default);
                let path = field.path.clone();
                return Switch::new(SharedString::from(format!("toggle-{}", field.path)))
                    .small()
                    .label(spec.label)
                    .checked(checked)
                    .on_click(cx.listener(move |this, checked: &bool, _, cx| {
                        this.set_toggle(&path, *checked, default, cx)
                    }))
                    .into_any_element();
            }
            FieldKind::Choice(options) => {
                let current = match get(&self.draft, &field.path).and_then(Value::as_str) {
                    Some(value) => value.to_string(),
                    None if field.path == "detail.kind" => "none".to_string(),
                    None => options[0].0.to_string(),
                };
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .children(options.iter().map(|(value, label)| {
                        let value = *value;
                        let path = field.path.clone();
                        Button::new(SharedString::from(format!("{}-{value}", field.path)))
                            .small()
                            .outline()
                            .selected(current == value)
                            .label(*label)
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.set_choice(&path, value, window, cx)
                            }))
                    }))
                    .into_any_element()
            }
        };
        let mut row = FormRow::new(spec.label, control).error(error);
        if !spec.help.is_empty() {
            row = row.help(spec.help);
        }
        row.into_any_element()
    }

    fn render_form(&self, cx: &mut Context<Self>) -> AnyElement {
        let theme = cx.theme().clone();
        let list_count = self.list_count();
        let has_comments = self.form_touched && has_comments(&self.yaml_text_for_comments());
        let other_issues = self.other_issues();
        v_flex()
            .gap_3()
            .when(has_comments, |this| {
                this.child(
                    div()
                        .text_xs()
                        .text_color(theme.warning)
                        .child("フォームで値を変えたため、保存すると YAML の中のコメントは消えます。コメントを残すなら YAML タブで直してください。"),
                )
            })
            .children(self.layout().into_iter().map(|section| {
                let header = h_flex()
                    .gap_2()
                    .justify_between()
                    .child(SectionLabel::new(section.title.clone()))
                    .when_some(section.list.filter(|_| list_count > 1), |this, index| {
                        this.child(
                            Button::new(SharedString::from(format!("remove-list-{index}")))
                                .xsmall()
                                .ghost()
                                .icon(IconName::Trash)
                                .label("この一覧を消す")
                                .on_click(cx.listener(move |this, _, window, cx| {
                                    this.remove_list(index, window, cx)
                                })),
                        )
                    });
                let is_last_list = section.list == Some(list_count.saturating_sub(1));
                v_flex()
                    .gap_3()
                    .child(header)
                    .children(
                        section
                            .fields
                            .iter()
                            .map(|field| self.render_field(field, cx))
                            .collect::<Vec<_>>(),
                    )
                    .when(section.title == "表示", |this| {
                        this.child(self.render_local_icon(cx))
                    })
                    .when(is_last_list, |this| {
                        this.child(
                            h_flex().child(
                                Button::new("add-list")
                                    .small()
                                    .outline()
                                    .icon(IconName::Plus)
                                    .label("一覧を足す")
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.add_list(window, cx)
                                    })),
                            ),
                        )
                    })
            }))
            .when(!other_issues.is_empty(), |this| {
                this.child(
                    v_flex()
                        .gap_1()
                        .text_xs()
                        .text_color(theme.danger)
                        .children(other_issues.into_iter().map(|issue| div().child(issue))),
                )
            })
            .child(
                div()
                    .text_xs()
                    .text_color(theme.muted_foreground)
                    .child("事前の取得 (prepare)・ヘッダ・問い合わせ・置換 (body.replace)・画像の同一性 (url_identity) は「YAML」タブで書けます。書き方は game-news-nexus-config の CONTRIBUTING.md を参照してください。"),
            )
            .into_any_element()
    }

    /// コメントの有無を見る YAML。フォームで変える前の全文。
    fn yaml_text_for_comments(&self) -> String {
        self.original
            .as_ref()
            .map(|(_, text)| text.clone())
            .unwrap_or_default()
    }

    fn render_yaml(&self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .gap_2()
            .h(px(560.))
            .when_some(self.yaml_error.clone(), |this, error| {
                this.child(div().text_xs().text_color(cx.theme().danger).child(error))
            })
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(cx.theme().radius)
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_size(cx.theme().mono_font_size)
                    .child(
                        Editor::new(&self.yaml_editor)
                            .h(relative(1.))
                            .appearance(false),
                    ),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child("文字列は単一引用符で囲みます。フォームに戻ると内容を読み込みます。"),
            )
            .into_any_element()
    }

    fn render_test(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        match &self.test {
            TestState::Idle => None,
            TestState::Running => Some(
                h_flex()
                    .gap_2()
                    .text_sm()
                    .child(Icon::new(IconName::LoaderCircle).small())
                    .child("取得しています…")
                    .into_any_element(),
            ),
            TestState::Done(output) => {
                let _ = cx;
                Some(RunResult::new((**output).clone()).into_any_element())
            }
        }
    }
}

impl Render for SourceEditor {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let app = self.app.read(cx);
        let name = get(&self.draft, "name")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        let kind_label = parse_draft(&self.draft)
            .ok()
            .map(|config| config.kind_label());
        let id = self.source_id().unwrap_or_default();
        let resolved = app.resolved(&id).cloned();
        let state = app.state_of(&id).cloned();
        let running = app.is_running(&id);
        let enabled = resolved.as_ref().is_none_or(|source| source.enabled);
        let allow_private = resolved
            .as_ref()
            .is_some_and(|source| source.allow_private_network);
        let is_new = self.is_new();
        let can_save = self.dirty && (self.tab == EditorTab::Yaml || !self.has_errors());
        let testing = matches!(self.test, TestState::Running);
        let mode = self.current_mode(cx);

        let status_line = match &state {
            Some(state) => format!(
                "{} · 最終取得 {}{}",
                StatusIcon::label(state.last_status, running, enabled),
                crate::services::describe_last_attempt(state.last_attempt_at),
                state
                    .last_message
                    .as_deref()
                    .map(|message| format!(" · {message}"))
                    .unwrap_or_default()
            ),
            None if is_new => "未保存".to_string(),
            None => StatusIcon::label(None, running, enabled).to_string(),
        };

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
                            .gap_2()
                            .child(div().text_lg().font_semibold().truncate().child(
                                if name.is_empty() {
                                    "名前のないソース".to_string()
                                } else {
                                    name
                                },
                            ))
                            .child(match mode {
                                EditorMode::Override => {
                                    Badge::new("カタログを手元で上書き").tone(BadgeTone::Accent)
                                }
                                _ => Badge::new("手元のソース"),
                            })
                            .when_some(kind_label, |this, label| {
                                this.child(
                                    div()
                                        .text_xs()
                                        .text_color(theme.muted_foreground)
                                        .child(label),
                                )
                            }),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .text_xs()
                            .text_color(theme.muted_foreground)
                            .child(StatusIcon::new(
                                state.as_ref().and_then(|state| state.last_status),
                                running,
                                enabled,
                            ))
                            .child(div().truncate().child(status_line)),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .flex_wrap()
                            .child(
                                Button::new("editor-save")
                                    .small()
                                    .primary()
                                    .icon(IconName::Save)
                                    .label(if is_new { "追加" } else { "保存" })
                                    .disabled(!can_save)
                                    .on_click(
                                        cx.listener(|this, _, window, cx| this.save(window, cx)),
                                    ),
                            )
                            .child(
                                Button::new("editor-test")
                                    .small()
                                    .outline()
                                    .icon(IconName::FlaskConical)
                                    .label("テスト取得")
                                    .loading(testing)
                                    .on_click(
                                        cx.listener(|this, _, window, cx| {
                                            this.run_test(window, cx)
                                        }),
                                    ),
                            )
                            .when(!is_new && enabled, |this| {
                                this.child(
                                    Button::new("editor-fetch")
                                        .small()
                                        .ghost()
                                        .icon(IconName::RefreshCw)
                                        .label("今すぐ取得")
                                        .loading(running)
                                        .on_click(cx.listener(|this, _, _, cx| this.fetch_now(cx))),
                                )
                            })
                            .when(!is_new, |this| {
                                this.child(
                                    Button::new("editor-revert")
                                        .small()
                                        .ghost()
                                        .icon(IconName::Undo2)
                                        .label("元に戻す")
                                        .disabled(!self.dirty)
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.revert(window, cx)
                                        })),
                                )
                                .child(
                                    Button::new("editor-delete")
                                        .small()
                                        .ghost()
                                        .icon(if mode == EditorMode::Override {
                                            IconName::Undo2
                                        } else {
                                            IconName::Trash
                                        })
                                        .label(if mode == EditorMode::Override {
                                            "カタログの定義に戻す"
                                        } else {
                                            "削除"
                                        })
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.confirm_delete(window, cx)
                                        })),
                                )
                            }),
                    )
                    .when(!is_new, |this| {
                        let id = id.clone();
                        this.child(
                            Switch::new("editor-allow-private")
                                .small()
                                .label("手元のネットワーク (localhost・LAN) への取得を許す")
                                .checked(allow_private)
                                .on_click(cx.listener(move |this, checked: &bool, window, cx| {
                                    let result = this.app.update(cx, |app, cx| {
                                        app.set_allow_private_network(&id, *checked, cx)
                                    });
                                    if let Err(err) = result {
                                        window.push_notification(
                                            Notification::error(format!("{err:#}")),
                                            cx,
                                        );
                                    }
                                })),
                        )
                    })
                    .when_some(self.parse_error.clone(), |this, error| {
                        this.child(div().text_xs().text_color(theme.danger).child(error))
                    }),
            )
            .child(
                TabBar::new("editor-tabs")
                    .small()
                    .selected_index(match self.tab {
                        EditorTab::Form => 0,
                        EditorTab::Yaml => 1,
                    })
                    .child(Tab::new().label("フォーム"))
                    .child(Tab::new().label("YAML"))
                    .on_click(cx.listener(|this, index: &usize, window, cx| {
                        let tab = if *index == 0 {
                            EditorTab::Form
                        } else {
                            EditorTab::Yaml
                        };
                        this.switch_tab(tab, window, cx);
                    })),
            )
            .child(
                div()
                    .id("editor-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(v_flex().gap_4().p_4().children(self.render_test(cx)).child(
                        match self.tab {
                            EditorTab::Form => self.render_form(cx),
                            EditorTab::Yaml => self.render_yaml(cx),
                        },
                    )),
            )
    }
}

/// `lists.N.kind` なら N。
fn list_kind_path(path: &str) -> Option<usize> {
    path.strip_prefix("lists.")?
        .strip_suffix(".kind")?
        .parse()
        .ok()
}

/// フォームのパス (`lists.0.request.url`) を、検査の項目名 (`lists[0].request.url`) にする。
fn issue_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len() + 2);
    for (index, part) in path.split('.').enumerate() {
        if part.bytes().all(|b| b.is_ascii_digit()) && !part.is_empty() {
            out.push('[');
            out.push_str(part);
            out.push(']');
        } else {
            if index > 0 {
                out.push('.');
            }
            out.push_str(part);
        }
    }
    out
}

/// 先頭のコメント (名前とエディタの指定) 以外にコメントがあるか。
fn has_comments(text: &str) -> bool {
    text.lines()
        .skip_while(|line| line.trim_start().starts_with('#'))
        .any(|line| {
            let line = line.trim_start();
            line.starts_with('#') || line.contains(" #")
        })
}

fn parse_draft(draft: &Value) -> Result<SourceConfig, String> {
    serde_json::from_value::<SourceConfig>(draft.clone())
        .map_err(|err| format!("設定を読めません: {err}"))
}

/// `a.b.0.c` の値。数字の部分は配列の位置。
fn get<'a>(value: &'a Value, path: &str) -> Option<&'a Value> {
    let mut current = value;
    for part in path.split('.') {
        current = match current {
            Value::Object(map) => map.get(part)?,
            Value::Array(items) => items.get(part.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(current)
}

/// `a.b.0.c` に値を置く。`None` なら消す。途中の表はなければ作る (配列は作らない)。
fn set(value: &mut Value, path: &str, new: Option<Value>) {
    let parts: Vec<&str> = path.split('.').collect();
    let (last, parents) = parts.split_last().expect("空でないパス");
    let mut current = value;
    for part in parents {
        current = match current {
            Value::Array(items) => match part
                .parse::<usize>()
                .ok()
                .and_then(|index| items.get_mut(index))
            {
                Some(item) => item,
                None => return,
            },
            other => {
                if !other.is_object() {
                    *other = Value::Object(serde_json::Map::new());
                }
                other
                    .as_object_mut()
                    .expect("表")
                    .entry(part.to_string())
                    .or_insert_with(|| Value::Object(serde_json::Map::new()))
            }
        };
    }
    match current {
        Value::Array(items) => {
            if let Some(index) = last
                .parse::<usize>()
                .ok()
                .filter(|index| *index < items.len())
            {
                match new {
                    Some(new) => items[index] = new,
                    None => {
                        items.remove(index);
                    }
                }
            }
        }
        other => {
            if !other.is_object() {
                *other = Value::Object(serde_json::Map::new());
            }
            let map = other.as_object_mut().expect("表");
            match new {
                Some(new) => {
                    map.insert(last.to_string(), new);
                }
                None => {
                    map.remove(*last);
                }
            }
        }
    }
}

/// 入力欄に出す文字列。文字列はそのまま、それ以外は 1 行の YAML。
fn display_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => gnn_core::yaml::to_flow_string(other),
    }
}

/// 入力欄の文字列を値にする。空欄は `None`。
fn parse_input(text: &str, kind: FieldKind) -> Result<Option<Value>, String> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    match kind {
        FieldKind::Number => trimmed
            .parse::<u64>()
            .map(|number| Some(Value::from(number)))
            .map_err(|_| "0 以上の整数を入れてください".to_string()),
        FieldKind::Value if trimmed.starts_with('{') || trimmed.starts_with('[') => {
            gnn_core::yaml::from_str::<Value>(trimmed)
                .map(Some)
                .map_err(|err| format!("YAML の書式が誤っています: {err}"))
        }
        _ => Ok(Some(Value::String(trimmed.to_string()))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn sample() -> SourceConfig {
        gnn_core::Catalog::bundled()
            .get("twinkle_star_knights")
            .unwrap()
            .source
            .clone()
    }

    #[test]
    fn edits_paths_and_inline_values() {
        let mut draft = serde_json::to_value(sample()).unwrap();
        assert_eq!(
            display_value(get(&draft, "lists.0.fields.url").unwrap()),
            "{ selector: 'div[data-detail_url]', attr: data-detail_url }"
        );
        assert_eq!(
            display_value(get(&draft, "lists.0.fields.published").unwrap()),
            ".list_date"
        );

        let value = parse_input("{ selector: a, attr: href }", FieldKind::Value).unwrap();
        set(&mut draft, "lists.0.fields.url", value);
        set(&mut draft, "detail.fields.title", None);
        set(
            &mut draft,
            "interval_minutes",
            parse_input("15", FieldKind::Number).unwrap(),
        );
        set(
            &mut draft,
            "lists.0.pages",
            parse_input(
                "{ kind: next_link, selector: 'a.next', max_pages: 3 }",
                FieldKind::Value,
            )
            .unwrap(),
        );
        let config = parse_draft(&draft).unwrap();
        assert_eq!(config.interval_minutes, Some(15));
        assert_eq!(
            config.lists[0].pages().map(|pages| pages.max_pages()),
            Some(3)
        );
        let Some(gnn_core::source::DetailSpec::Html(detail)) = config.detail else {
            panic!("html detail")
        };
        assert_eq!(detail.fields.title, None);

        assert!(parse_input("abc", FieldKind::Number).is_err());
        assert!(parse_input("{ broken", FieldKind::Value).is_err());
        assert_eq!(parse_input("  ", FieldKind::Text).unwrap(), None);
        // `#` で始まるセレクタは文字列のまま。
        assert_eq!(
            parse_input("#main", FieldKind::Value).unwrap(),
            Some(Value::String("#main".into()))
        );
    }

    #[test]
    fn maps_form_paths_to_issue_fields() {
        assert_eq!(issue_path("lists.0.request.url"), "lists[0].request.url");
        assert_eq!(issue_path("detail.fields.body"), "detail.fields.body");
        assert_eq!(list_kind_path("lists.2.kind"), Some(2));
        assert_eq!(list_kind_path("detail.kind"), None);
    }

    #[test]
    fn finds_comments_after_the_header() {
        assert!(!has_comments(
            "# 名前\n# yaml-language-server: x\nschema: 2\nid: a\n"
        ));
        assert!(has_comments("# 名前\nschema: 2\n# 説明\nid: a\n"));
        assert!(has_comments("# 名前\nschema: 2\nid: a  # ID\n"));
    }
}
