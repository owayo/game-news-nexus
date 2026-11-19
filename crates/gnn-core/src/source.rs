//! 取得定義 (1 ファイル 1 ソースの YAML) のスキーマ。
//!
//! 処理の流れは段で決まっていて、各段で何をするかを宣言する。
//!
//! 1. `prepare`: 事前に取得し、応答から値を取り出して変数にする (任意)。
//! 2. `lists`: 一覧を取得し、記事の候補を作る。複数の一覧を書くと宣言順にまとめる。
//!    一覧ごとに上限付きのページ送りができる。
//! 3. 候補をまとめ、重複・取得済み・古い記事を除く (エンジンが行う)。
//! 4. `detail`: 残った記事だけ詳細ページを取得し、項目を補う (任意)。
//! 5. `body`: 本文を整える。
//!
//! 任意の処理を並べる仕組み (スクリプトや汎用のステップ列) は持たない。第三者の PR で
//! 入る設定をレビューでき、通信の回数と流れが設定から読み取れるようにするため。

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::yaml::{self, YamlError};

/// 今の書式の版。
pub const SCHEMA_VERSION: u32 = 2;
/// 一覧の数の上限。
pub const MAX_LISTS: usize = 10;
/// 事前取得の数の上限。
pub const MAX_PREPARE: usize = 5;
/// ページ送りで読むページ数の上限。設定ではこれより大きくできない。
pub const MAX_PAGES: u32 = 10;
/// このアプリの版。`min_app_version` と比べる。
pub const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
/// 書式 2 を読めるようになったアプリの版。
pub const SCHEMA_2_SINCE: &str = "26.11.6";

/// 書式 2 に後から足した機能と、その機能を使えるようになったアプリの版。
///
/// 機能を足したら、使っているかの判定と版をここに加える。カタログの検査は、各ソースの
/// `min_app_version` が使っている機能の版以上かを確かめる。
type FeatureCheck = fn(&SourceConfig) -> bool;
const LATER_FEATURES: &[(&str, &str, FeatureCheck)] = &[
    (
        "本文の予定の地域時刻 (schedule)",
        "26.11.6",
        |source| source.schedule != ScheduleSpec::default(),
    ),
    ("画像の同一性 (url_identity)", "26.11.6", |source| {
        source.url_identity != UrlIdentity::default()
    }),
    (
        "成人向けの版 (adult・general_edition・inherit_articles)",
        "26.11.6",
        |source| source.adult || source.general_edition.is_some() || source.inherit_articles,
    ),
];

/// 1 つの取得元。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct SourceConfig {
    /// 書式の版。今は `2`。
    pub schema: u32,
    /// このソースに必要なアプリの最小の版 (`26.12.0` など)。古いアプリはこのソースの
    /// 前の版を使い続ける。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_app_version: Option<String>,
    /// ソースの識別子。英小文字・数字・`_`・`-` だけを使う。カタログではファイル名と同じにする。
    pub id: String,
    /// 画面に出す名前。
    pub name: String,
    /// お知らせの出し先。
    #[serde(default, skip_serializing_if = "is_default")]
    pub channel: Channel,
    /// 公式サイトなど、ソースの案内先。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub homepage: Option<String>,
    /// アイコン画像の URL。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub icon: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags: Vec<String>,
    /// 利用者向けの説明。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// 成人向け (FANZA GAMES の版など)。利用者が設定で 18 歳以上であることと、成人向けの内容を
    /// 出すことに同意したときだけ使う。同意していなければ、取得もソース画面への表示もしない。
    #[serde(default, skip_serializing_if = "is_false")]
    pub adult: bool,
    /// 同じゲームの一般向けの版のソースの ID (`adult: true` のときだけ書ける)。成人向けの版を使う
    /// 設定では、一般向けの版の代わりにこのソースを使い、on/off などの購読は一般向けの版のものを使う。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub general_edition: Option<String>,
    /// 一般向けの版のソースがこれまで成人向けの版の取得先から取得していた記事を、このソースに移す
    /// (`general_edition` があるときだけ書ける)。一般向けの版の取得先を、成人向けの版から一般向けの版に
    /// 直すときに書く。アプリは組を初めて見たときに 1 度だけ、一般向けの版の記事を既読・予定ごと移す。
    #[serde(default, skip_serializing_if = "is_false")]
    pub inherit_articles: bool,
    /// タイムゾーンの指定がない日時をどの地域時刻として読むか。既定は `Asia/Tokyo`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// 本文から予定 (メンテナンスやイベントの期間) を読むときの地域時刻。
    #[serde(default, skip_serializing_if = "is_default")]
    pub schedule: ScheduleSpec,
    /// 取得間隔 (分) の目安。省略時はアプリ設定の既定値。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub interval_minutes: Option<u32>,
    /// これより古い記事は取り込まない (日)。省略時はアプリ設定の既定値。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_age_days: Option<u32>,
    /// 1 回の取得で読む記事の上限。詳細ページの取得数を抑える。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_items: Option<usize>,
    /// 1. 事前の取得。応答から取り出した値を `{{vars.名前}}` で後の段に渡す。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub prepare: Vec<PrepareStep>,
    /// 2. 一覧。複数書くと宣言順にまとめ、同じ記事は先の一覧の値を使う。
    pub lists: Vec<ListSpec>,
    /// 4. 記事ごとの詳細ページ。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<DetailSpec>,
    /// 5. 本文の整え方。
    #[serde(default, skip_serializing_if = "is_default")]
    pub body: BodySpec,
    /// URL の同一性。配信元が取得のたびに変える問い合わせ (署名・期限など) があっても、
    /// 同じ画像として扱う決まり。
    #[serde(default, skip_serializing_if = "is_default")]
    pub url_identity: UrlIdentity,
}

impl SourceConfig {
    /// YAML から読む。書式の誤りだけを見る。中身の検査は [`SourceConfig::validate`]。
    pub fn from_yaml(text: &str) -> Result<Self, YamlError> {
        yaml::from_str(text)
    }

    pub fn to_yaml(&self) -> Result<String, YamlError> {
        yaml::to_string(self)
    }

    /// 最初の一覧の取得方式。画面の表示に使う。
    pub fn kind(&self) -> ListKind {
        self.lists.first().map_or(ListKind::Feed, ListSpec::kind)
    }

    /// 取得方式の説明。一覧が複数あれば数も添える。
    pub fn kind_label(&self) -> String {
        match self.lists.len() {
            0 | 1 => self.kind().label().to_string(),
            count => format!("{} ほか {} 件の一覧", self.kind().label(), count - 1),
        }
    }

    /// このアプリの版 (`app_version`) より新しいアプリが必要か。
    pub fn requires_newer_app(&self, app_version: &str) -> bool {
        self.min_app_version
            .as_deref()
            .is_some_and(|required| compare_versions(required, app_version) == Ordering::Greater)
    }

    /// 使っている機能から決まる、必要なアプリの最小の版。
    pub fn required_app_version(&self) -> &'static str {
        LATER_FEATURES
            .iter()
            .filter(|(_, _, uses)| uses(self))
            .map(|(_, version, _)| *version)
            .fold(SCHEMA_2_SINCE, |required, version| {
                if compare_versions(version, required) == Ordering::Greater {
                    version
                } else {
                    required
                }
            })
    }

    /// 使っている機能のうち、`SCHEMA_2_SINCE` より後に足したものの名前。
    pub fn later_features(&self) -> Vec<&'static str> {
        LATER_FEATURES
            .iter()
            .filter(|(_, _, uses)| uses(self))
            .map(|(name, _, _)| *name)
            .collect()
    }

    /// 本文の日時に地域時刻の明記がないときに使う地域時刻。
    /// `schedule.timezone` → `timezone` → `Asia/Tokyo` の順に決める (解釈できない名前は飛ばす)。
    pub fn schedule_timezone(&self) -> chrono_tz::Tz {
        [self.schedule.timezone.as_deref(), self.timezone.as_deref()]
            .into_iter()
            .flatten()
            .find_map(|name| name.parse().ok())
            .unwrap_or(chrono_tz::Asia::Tokyo)
    }

    /// 本文に「サーバー時間」と書かれた日時の地域時刻。書かれていなければ決めない。
    pub fn server_timezone(&self) -> Option<chrono_tz::Tz> {
        self.schedule
            .server_timezone
            .as_deref()
            .and_then(|name| name.parse().ok())
    }

    /// 秘密の値 (`{{secret:名前}}`) を使っているか。
    pub fn uses_secrets(&self) -> bool {
        self.templates().into_iter().any(|(_, template)| {
            template_parts(template).is_ok_and(|parts| {
                parts
                    .iter()
                    .any(|part| matches!(part, TemplatePart::Placeholder(Placeholder::Secret(_))))
            })
        })
    }

    /// 取得先の URL のテンプレート。フィールドの名前と組で返す。
    pub fn request_urls(&self) -> Vec<(String, &str)> {
        let mut urls = Vec::new();
        for (index, step) in self.prepare.iter().enumerate() {
            urls.push((
                format!("prepare[{index}].request.url"),
                step.request.url.as_str(),
            ));
        }
        for (index, list) in self.lists.iter().enumerate() {
            urls.push((
                format!("lists[{index}].request.url"),
                list.request().url.as_str(),
            ));
        }
        if let Some(request) = self.detail.as_ref().and_then(DetailSpec::request) {
            urls.push(("detail.request.url".into(), request.url.as_str()));
        }
        urls
    }

    /// すべてのテンプレート (URL・問い合わせ・ヘッダ・本文)。
    fn templates(&self) -> Vec<(String, &str)> {
        let mut requests: Vec<(String, &RequestSpec)> = Vec::new();
        for (index, step) in self.prepare.iter().enumerate() {
            requests.push((format!("prepare[{index}].request"), &step.request));
        }
        for (index, list) in self.lists.iter().enumerate() {
            requests.push((format!("lists[{index}].request"), list.request()));
        }
        if let Some(request) = self.detail.as_ref().and_then(DetailSpec::request) {
            requests.push(("detail.request".into(), request));
        }
        let mut templates = Vec::new();
        for (prefix, request) in requests {
            request.each_template(&prefix, &mut |field, template| {
                templates.push((field, template))
            });
        }
        templates
    }
}

/// ソースの出し先。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// お知らせ。取得した記事を蓄積する。
    #[default]
    News,
}

impl Channel {
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::News => "news",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "news" => Some(Channel::News),
            _ => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Channel::News => "お知らせ",
        }
    }
}

/// HTTP リクエスト。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RequestSpec {
    /// URL。`{{vars.名前}}` などを差し込める (ホストの部分には差し込めない)。
    pub url: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub method: HttpMethod,
    /// URL に付け足す問い合わせ (`?名前=値`)。値は符号化して付ける。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub query: BTreeMap<String, String>,
    /// 追加のヘッダ。手元のソースでは値に `{{secret:名前}}` と書くと `secrets.toml` の値になる
    /// (カタログのソースでは使えない)。
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub headers: BTreeMap<String, String>,
    /// POST の本文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// 応答の文字コードを固定する (`shift_jis` など)。省略時は応答から判定する。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub encoding: Option<String>,
}

impl RequestSpec {
    pub fn get(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            ..Self::default()
        }
    }

    fn each_template<'a>(&'a self, prefix: &str, f: &mut impl FnMut(String, &'a str)) {
        f(format!("{prefix}.url"), &self.url);
        for (name, value) in &self.query {
            f(format!("{prefix}.query.{name}"), value);
        }
        for (name, value) in &self.headers {
            f(format!("{prefix}.headers.{name}"), value);
        }
        if let Some(body) = &self.body {
            f(format!("{prefix}.body"), body);
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "UPPERCASE")]
pub enum HttpMethod {
    #[default]
    Get,
    Post,
}

/// 事前の取得。応答から値を取り出し、`{{vars.名前}}` として後の段で使う。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PrepareStep {
    /// 応答の読み方。
    #[serde(default, skip_serializing_if = "is_default")]
    pub kind: DocumentKind,
    pub request: RequestSpec,
    /// 取り出す値。名前と取り出し方の組。
    pub bind: BTreeMap<String, BindValue>,
}

/// 応答の読み方。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DocumentKind {
    /// JSON。値は JSON Pointer で取り出す。
    #[default]
    Json,
    /// HTML。値は CSS セレクタで取り出す。
    Html,
    /// テキスト。値は正規表現で取り出す。
    Text,
}

/// 事前の取得で取り出す値。文字列だけを書くと、JSON なら JSON Pointer、HTML なら
/// CSS セレクタ (要素の文字列)、テキストなら正規表現 (最初の括弧の中) として読む。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum BindValue {
    Short(String),
    Spec(BindSpec),
}

impl BindValue {
    pub fn spec(&self, kind: DocumentKind) -> BindSpec {
        match self {
            BindValue::Spec(spec) => spec.clone(),
            BindValue::Short(text) => match kind {
                DocumentKind::Json => BindSpec {
                    pointer: Some(text.clone()),
                    ..BindSpec::default()
                },
                DocumentKind::Html => BindSpec {
                    selector: Some(text.clone()),
                    ..BindSpec::default()
                },
                DocumentKind::Text => BindSpec {
                    regex: Some(text.clone()),
                    ..BindSpec::default()
                },
            },
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BindSpec {
    /// JSON の値を指す JSON Pointer。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
    /// HTML の要素の CSS セレクタ。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    /// 要素の属性を取る。省略時は要素の文字列。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attr: Option<String>,
    /// 取り出した文字列 (テキストなら応答全体) に当てる正規表現。最初の括弧の中を値にする。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub regex: Option<String>,
}

/// 取得方式。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ListKind {
    Feed,
    Json,
    Html,
    HtmlJson,
}

impl ListKind {
    pub const ALL: [ListKind; 4] = [
        ListKind::Feed,
        ListKind::Json,
        ListKind::Html,
        ListKind::HtmlJson,
    ];

    pub fn label(self) -> &'static str {
        match self {
            ListKind::Feed => "RSS / Atom フィード",
            ListKind::Json => "JSON API",
            ListKind::Html => "HTML の一覧",
            ListKind::HtmlJson => "HTML に埋め込まれた JSON",
        }
    }

    pub fn description(self) -> &'static str {
        match self {
            ListKind::Feed => "フィードの URL だけで記事を取り込む",
            ListKind::Json => "JSON の配列を JSON Pointer で読む",
            ListKind::Html => "HTML の一覧を CSS セレクタで読む",
            ListKind::HtmlJson => "HTML 要素の属性や本文に入った JSON を読む",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ListKind::Feed => "feed",
            ListKind::Json => "json",
            ListKind::Html => "html",
            ListKind::HtmlJson => "html_json",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|kind| kind.as_str() == value)
    }

    /// 記事が JSON の値として読めるか。テンプレートの `{{/pointer}}` が使える。
    pub fn has_json_items(self) -> bool {
        matches!(self, ListKind::Json | ListKind::HtmlJson)
    }
}

/// 一覧。`kind` で取得方式を選ぶ。
// 設定値として数個しか作らないので、方式ごとの大きさの差は問題にならない。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ListSpec {
    /// RSS / Atom / JSON Feed。
    Feed(FeedList),
    /// JSON の配列を読む。
    Json(JsonList),
    /// HTML の一覧を CSS セレクタで読む。
    Html(HtmlList),
    /// HTML 要素に埋め込まれた JSON を読む。
    HtmlJson(HtmlJsonList),
}

impl ListSpec {
    pub fn kind(&self) -> ListKind {
        match self {
            ListSpec::Feed(_) => ListKind::Feed,
            ListSpec::Json(_) => ListKind::Json,
            ListSpec::Html(_) => ListKind::Html,
            ListSpec::HtmlJson(_) => ListKind::HtmlJson,
        }
    }

    pub fn name(&self) -> Option<&str> {
        match self {
            ListSpec::Feed(list) => list.name.as_deref(),
            ListSpec::Json(list) => list.name.as_deref(),
            ListSpec::Html(list) => list.name.as_deref(),
            ListSpec::HtmlJson(list) => list.name.as_deref(),
        }
    }

    pub fn request(&self) -> &RequestSpec {
        match self {
            ListSpec::Feed(list) => &list.request,
            ListSpec::Json(list) => &list.request,
            ListSpec::Html(list) => &list.request,
            ListSpec::HtmlJson(list) => &list.request,
        }
    }

    pub fn request_mut(&mut self) -> &mut RequestSpec {
        match self {
            ListSpec::Feed(list) => &mut list.request,
            ListSpec::Json(list) => &mut list.request,
            ListSpec::Html(list) => &mut list.request,
            ListSpec::HtmlJson(list) => &mut list.request,
        }
    }

    pub fn pages(&self) -> Option<&PagesSpec> {
        match self {
            ListSpec::Feed(_) => None,
            ListSpec::Json(list) => list.pages.as_ref(),
            ListSpec::Html(list) => list.pages.as_ref(),
            ListSpec::HtmlJson(list) => list.pages.as_ref(),
        }
    }

    /// 取得方式ごとの空の設定。
    pub fn empty(kind: ListKind, url: impl Into<String>) -> Self {
        let request = RequestSpec::get(url);
        match kind {
            ListKind::Feed => ListSpec::Feed(FeedList {
                request,
                ..FeedList::default()
            }),
            ListKind::Json => ListSpec::Json(JsonList {
                request,
                ..JsonList::default()
            }),
            ListKind::Html => ListSpec::Html(HtmlList {
                request,
                ..HtmlList::default()
            }),
            ListKind::HtmlJson => ListSpec::HtmlJson(HtmlJsonList {
                request,
                ..HtmlJsonList::default()
            }),
        }
    }

    /// 診断に出す一覧の名前。
    pub fn label(&self, index: usize) -> String {
        match self.name() {
            Some(name) => format!("一覧「{name}」"),
            None => format!("{} 番目の一覧", index + 1),
        }
    }
}

/// RSS / Atom / JSON Feed。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FeedList {
    /// 一覧の名前。診断に出す。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub request: RequestSpec,
    /// 本文に使う要素。
    #[serde(default, skip_serializing_if = "is_default")]
    pub content: FeedContent,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum FeedContent {
    /// 本文 (content) があれば本文、なければ概要 (summary)。
    #[default]
    Auto,
    Content,
    Summary,
}

/// JSON の配列を読む。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JsonList {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub request: RequestSpec,
    /// 記事の配列を指す JSON Pointer。空文字はルート。
    pub items: String,
    /// 取り込む記事の条件。すべて満たしたものだけを残す。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filter: Vec<Condition>,
    pub fields: JsonFields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<PagesSpec>,
}

/// HTML の一覧を読む。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HtmlList {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub request: RequestSpec,
    /// 記事 1 件に当たる要素の CSS セレクタ。
    pub items: String,
    /// 指定すると、この CSS セレクタに合う要素を含む記事だけを残す。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require: Option<String>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub fields: HtmlFields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<PagesSpec>,
}

/// HTML 要素に埋め込まれた JSON を読む。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HtmlJsonList {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    pub request: RequestSpec,
    /// JSON を持つ要素の CSS セレクタ。
    pub selector: String,
    /// JSON を持つ属性。省略時は要素の本文 (`<script type="application/json">` など)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attr: Option<String>,
    /// 記事の配列を指す JSON Pointer。省略時 (空文字) は JSON そのもの。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub items: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filter: Vec<Condition>,
    pub fields: JsonFields,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pages: Option<PagesSpec>,
}

/// ページ送り。どの方式も `max_pages` で読むページ数を限る。
///
/// 記事のないページ、同じ URL やカーソルの繰り返しで止める。お知らせでは、ページの記事が
/// すべて取得済みか古ければ、それより先は読まない。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PagesSpec {
    /// ページ番号 (またはオフセット) を増やしながら取る。
    Page(PageNumbers),
    /// 応答の中の「次のページ」の URL をたどる。
    NextLink(NextLink),
    /// 応答の中のカーソルを、次のリクエストの問い合わせに付ける。
    Cursor(CursorPages),
}

impl PagesSpec {
    pub fn max_pages(&self) -> u32 {
        match self {
            PagesSpec::Page(pages) => pages.max_pages,
            PagesSpec::NextLink(pages) => pages.max_pages,
            PagesSpec::Cursor(pages) => pages.max_pages,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct PageNumbers {
    /// 番号を付ける問い合わせの名前。省略時は URL の `{{page}}` に入れる。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub param: Option<String>,
    /// 最初の番号。
    #[serde(default = "default_page_start", skip_serializing_if = "is_page_start")]
    pub start: u32,
    /// 番号の増え方。オフセットなら 1 ページの件数。
    #[serde(default = "default_page_step", skip_serializing_if = "is_page_step")]
    pub step: u32,
    /// 読むページ数の上限 (最大 10)。
    pub max_pages: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NextLink {
    /// JSON の一覧: 次のページの URL を指す JSON Pointer。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer: Option<String>,
    /// HTML の一覧: 次のページへのリンクの CSS セレクタ。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    /// HTML の一覧: URL を持つ属性。既定は `href`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attr: Option<String>,
    /// 読むページ数の上限 (最大 10)。
    pub max_pages: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct CursorPages {
    /// 次のカーソルを指す JSON Pointer。
    pub pointer: String,
    /// カーソルを付ける問い合わせの名前。
    pub param: String,
    /// 読むページ数の上限 (最大 10)。
    pub max_pages: u32,
}

/// JSON の記事から値を取り出す設定。
///
/// 値は `/title` のような JSON Pointer か、`'https://example.com/{{/id}}'` のような
/// テンプレートで書く。テンプレートの `{{/pointer}}` には記事の値が入る。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JsonFields {
    /// 記事を見分ける値。省略時は URL、それもなければタイトル。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// 投稿日時。文字列の日時か UNIX 時刻 (秒・ミリ秒)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<JsonBody>,
    /// バナーやサムネイルの画像 URL。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// 発売時期などの補足。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

/// JSON の本文。HTML 文字列か、画像と文章のブロックの配列。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum JsonBody {
    Value(String),
    Blocks(BlocksSpec),
}

/// 本文を表すブロックの配列。画像のブロックは `<img>`、それ以外は段落にする。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BlocksSpec {
    /// ブロックの配列を指す JSON Pointer。
    pub blocks: String,
    /// ブロックの文章を指す JSON Pointer。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// ブロックの画像 URL を指す JSON Pointer。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<String>,
    /// 画像のブロックを見分ける条件。省略時は画像 URL があるものを画像とみなす。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_when: Option<Condition>,
}

/// HTML の記事から値を取り出す設定。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HtmlFields {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<HtmlValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<HtmlValue>,
    /// 省略時は記事要素 (またはその中の最初の `<a>`) の `href`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<HtmlValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub published: Option<HtmlValue>,
    /// 詳細ページでは、省略時に文書の `<body>` 全体を本文にする。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body: Option<HtmlValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image: Option<HtmlValue>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<HtmlValue>,
}

impl HtmlFields {
    /// 名前と値の組。
    pub fn entries(&self) -> [(&'static str, &Option<HtmlValue>); 7] {
        [
            ("id", &self.id),
            ("title", &self.title),
            ("url", &self.url),
            ("published", &self.published),
            ("body", &self.body),
            ("image", &self.image),
            ("note", &self.note),
        ]
    }
}

/// HTML から値を取り出す設定。CSS セレクタだけを書くと、その要素の文字列を取る。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum HtmlValue {
    Selector(String),
    Spec(HtmlValueSpec),
}

impl HtmlValue {
    pub fn spec(&self) -> HtmlValueSpec {
        match self {
            HtmlValue::Selector(selector) => HtmlValueSpec {
                selector: Some(selector.clone()),
                ..HtmlValueSpec::default()
            },
            HtmlValue::Spec(spec) => spec.clone(),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HtmlValueSpec {
    /// 記事要素から見た CSS セレクタ。省略時は記事要素そのもの。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selector: Option<String>,
    /// 取り出し方。省略時は項目ごとの既定 (本文は HTML、それ以外は文字列)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub extract: Option<Extract>,
    /// 属性の値を取る。`extract` より優先する。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attr: Option<String>,
    /// 取り出す前に取り除く要素の CSS セレクタ。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
    /// 取り出した値を埋め込むテンプレート。`{{value}}` が値に置き換わる。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub template: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Extract {
    /// 要素の文字列。
    Text,
    /// 要素の中身の HTML。
    Html,
    /// 要素自身を含む HTML。
    OuterHtml,
}

/// JSON の値に対する条件。`eq`・`ne`・`exists`・`contains` のどれか 1 つを書く。
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    /// 記事から見た JSON Pointer。
    pub pointer: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub eq: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ne: Option<serde_json::Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exists: Option<bool>,
    /// 文字列に含む。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contains: Option<String>,
}

/// 詳細ページの読み方。
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DetailSpec {
    /// HTML のページ。項目は CSS セレクタで取る。
    Html(HtmlDetail),
    /// JSON。項目は JSON Pointer で取る。
    Json(JsonDetail),
    /// HTML 要素に埋め込まれた JSON。
    HtmlJson(HtmlJsonDetail),
}

impl DetailSpec {
    /// リクエスト。省略時は記事の URL (`{{item.url}}`) を GET する。
    pub fn request(&self) -> Option<&RequestSpec> {
        match self {
            DetailSpec::Html(detail) => detail.request.as_ref(),
            DetailSpec::Json(detail) => detail.request.as_ref(),
            DetailSpec::HtmlJson(detail) => detail.request.as_ref(),
        }
    }

    pub fn kind_label(&self) -> &'static str {
        match self {
            DetailSpec::Html(_) => "HTML",
            DetailSpec::Json(_) => "JSON",
            DetailSpec::HtmlJson(_) => "HTML に埋め込まれた JSON",
        }
    }
}

/// 詳細の URL の既定値。
pub const DEFAULT_DETAIL_URL: &str = "{{item.url}}";

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HtmlDetail {
    /// 詳細ページのリクエスト。URL には `{{item.id}}` `{{item.url}}` `{{item.title}}` と、
    /// JSON の一覧なら `{{/pointer}}` が使える。省略時は記事の URL。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<RequestSpec>,
    /// 詳細ページから取る値。一覧で取れた値より優先する。
    #[serde(default, skip_serializing_if = "is_default")]
    pub fields: HtmlFields,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct JsonDetail {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<RequestSpec>,
    #[serde(default, skip_serializing_if = "is_default")]
    pub fields: JsonFields,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct HtmlJsonDetail {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub request: Option<RequestSpec>,
    /// JSON を持つ要素の CSS セレクタ。
    pub selector: String,
    /// JSON を持つ属性。省略時は要素の本文。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attr: Option<String>,
    /// 記事の値を指す JSON Pointer。空文字はルート。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub item: String,
    #[serde(default, skip_serializing_if = "is_default")]
    pub fields: JsonFields,
}

/// 本文の整え方。
///
/// 順に「テキストなら HTML の特殊文字を逃がす → 正規表現の置換 → 記事の画像を先頭に入れる →
/// 要素の除去 → 安全なタグ・属性・URL だけを残す (常に行う) → 余分な空要素と改行を整理する」。
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BodySpec {
    #[serde(default, skip_serializing_if = "is_default")]
    pub format: BodyFormat,
    /// 文字列中の改行を `<br>` にする。表と `<pre>` の中は除く。
    #[serde(default, skip_serializing_if = "is_false")]
    pub newlines_to_br: bool,
    /// 正規表現の置換。`with` の `$1` でキャプチャを参照できる。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub replace: Vec<ReplaceRule>,
    /// 本文から取り除く要素の CSS セレクタ。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remove: Vec<String>,
    /// 相対 URL の基準。省略時は本文を取得した URL。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    /// 記事の画像 (`image`) を本文の先頭に入れる。
    #[serde(default, skip_serializing_if = "is_false")]
    pub image_in_body: bool,
    /// 空の要素と連続した改行を整理する。安全のためのタグの除去は、この設定によらず行う。
    #[serde(default = "default_true", skip_serializing_if = "is_true")]
    pub tidy: bool,
}

impl Default for BodySpec {
    fn default() -> Self {
        Self {
            format: BodyFormat::default(),
            newlines_to_br: false,
            replace: Vec::new(),
            remove: Vec::new(),
            base_url: None,
            image_in_body: false,
            tidy: true,
        }
    }
}

/// 本文から予定 (メンテナンスやイベントの期間) を読むときの地域時刻。
///
/// 投稿日時を読む `timezone` とは分ける。一覧の日時と本文の日時で地域時刻が違う配信元があるため
/// (一覧は UTC+8 で、本文のメンテナンスは日本時間で書かれている、など)。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ScheduleSpec {
    /// 本文の日時に地域時刻の明記 (`(JST)` など) がないとき、どの地域時刻として読むか。
    /// 省略時はソースの `timezone`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timezone: Option<String>,
    /// 本文に「サーバー時間」と書かれた日時の地域時刻。省略時は決めない (その日時の地域時刻を補わない)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_timezone: Option<String>,
}

/// URL の同一性。
///
/// 署名付きの URL (`?s=署名&t=期限`) のように、同じ画像でも取得のたびに問い合わせが変わる配信元がある。
/// ここで宣言した画像は、記事を比べるとき・画像キャッシュの鍵を決めるときに問い合わせの違いを見ない。
/// 取りに行くときは、配信元の URL (署名付き) をそのまま使う (外すと取れない配信元があるため)。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UrlIdentity {
    /// 画像 (一覧・詳細の `image` と本文の `<img>`) の決まり。記事の URL やリンクには使わない。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub images: Vec<UrlIdentityRule>,
}

/// 同じとみなす URL の決まり。`origin` と `path_prefix` に合う URL は、`ignore_query` の
/// 問い合わせとフラグメントが違っても同じものとして扱う。ほかの問い合わせは見分けに使う。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct UrlIdentityRule {
    /// スキームとホスト (と既定でないポート)。`https://cdn.example.com` のように書く。
    pub origin: String,
    /// パスの先頭 (`/img/` など)。`/` で始める。省略時はオリジンのすべてのパス。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path_prefix: Option<String>,
    /// 違っても同じとみなす問い合わせの名前 (`s`・`t` など)。大文字と小文字は区別する。
    pub ignore_query: Vec<String>,
}

impl UrlIdentityRule {
    /// パスの先頭。省略時は `/`。
    pub fn path_prefix(&self) -> &str {
        self.path_prefix.as_deref().unwrap_or("/")
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BodyFormat {
    /// HTML として扱う。
    #[default]
    Html,
    /// プレーンテキストとして扱う。HTML の特殊文字を逃がし、改行を `<br>` にする。
    Text,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ReplaceRule {
    /// 正規表現。
    pub pattern: String,
    /// 置換後の文字列。`$1` でキャプチャを参照できる。
    #[serde(default)]
    pub with: String,
}

/// 設定の誤り。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigIssue {
    pub field: String,
    pub message: String,
}

impl fmt::Display for ConfigIssue {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

// ---- テンプレート ----

/// テンプレートの差し込み `{{...}}`。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Placeholder {
    /// `{{vars.名前}}`: `prepare` で取り出した値。
    Var(String),
    /// `{{item.id}}` `{{item.url}}` `{{item.title}}`: 一覧で読んだ記事の値。
    Item(ItemField),
    /// `{{/pointer}}`: JSON の記事の値。
    Pointer(String),
    /// `{{page}}`: ページ送りの番号。
    Page,
    /// `{{value}}`: HTML から取り出した値 (`template` の中)。
    Value,
    /// `{{secret:名前}}`: `secrets.toml` の値。手元のソースのヘッダだけで使える。
    Secret(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ItemField {
    Id,
    Url,
    Title,
}

/// テンプレートの部品。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplatePart<'a> {
    Text(&'a str),
    Placeholder(Placeholder),
}

/// テンプレートを部品に分ける。`{{` が閉じていないか、中身を解釈できなければ誤り。
/// `{` や `}` が 1 つだけなら文字としてそのまま残す。
pub fn template_parts(template: &str) -> Result<Vec<TemplatePart<'_>>, String> {
    let mut parts = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("{{") {
        if start > 0 {
            parts.push(TemplatePart::Text(&rest[..start]));
        }
        let after = &rest[start + 2..];
        let end = after
            .find("}}")
            .ok_or_else(|| format!("`{{{{` が閉じていません: {template}"))?;
        parts.push(TemplatePart::Placeholder(parse_placeholder(&after[..end])?));
        rest = &after[end + 2..];
    }
    if !rest.is_empty() {
        parts.push(TemplatePart::Text(rest));
    }
    Ok(parts)
}

/// テンプレートに差し込みがあるか。
pub fn is_template(text: &str) -> bool {
    text.contains("{{")
}

fn parse_placeholder(inner: &str) -> Result<Placeholder, String> {
    let name = inner.trim();
    if let Some(var) = name.strip_prefix("vars.") {
        return if is_valid_var_name(var) {
            Ok(Placeholder::Var(var.to_string()))
        } else {
            Err(format!("変数名 `{var}` は英数字と `_` で書いてください"))
        };
    }
    if let Some(secret) = name.strip_prefix("secret:") {
        return Ok(Placeholder::Secret(secret.trim().to_string()));
    }
    match name {
        "item.id" => Ok(Placeholder::Item(ItemField::Id)),
        "item.url" => Ok(Placeholder::Item(ItemField::Url)),
        "item.title" => Ok(Placeholder::Item(ItemField::Title)),
        "page" => Ok(Placeholder::Page),
        "value" => Ok(Placeholder::Value),
        pointer if pointer.starts_with('/') => Ok(Placeholder::Pointer(pointer.to_string())),
        other => Err(format!(
            "`{{{{{other}}}}}` を解釈できません (vars.名前・item.id・item.url・item.title・/pointer・page・value が使えます)"
        )),
    }
}

/// `prepare` の変数名として使えるか。
pub fn is_valid_var_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
}

/// ソース ID として使えるか。
pub fn is_valid_id(id: &str) -> bool {
    (1..=64).contains(&id.len())
        && id
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_' || b == b'-')
}

/// `26.11.6` のような版を数値の並びとして比べる。数字でない部分は 0 とみなす。
pub fn compare_versions(a: &str, b: &str) -> Ordering {
    let parse = |text: &str| -> Vec<u64> {
        text.trim()
            .split(['.', '-', '+'])
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    };
    let (a, b) = (parse(a), parse(b));
    let len = a.len().max(b.len());
    for index in 0..len {
        let ordering = a
            .get(index)
            .copied()
            .unwrap_or(0)
            .cmp(&b.get(index).copied().unwrap_or(0));
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
}

// ---- 検査 ----

/// テンプレートで使ってよい差し込み。
#[derive(Clone, Copy, Default)]
struct Allow<'a> {
    vars: &'a [&'a str],
    item: bool,
    pointer: bool,
    page: bool,
    value: bool,
    secret: bool,
}

struct Checker {
    issues: Vec<ConfigIssue>,
}

impl Checker {
    fn push(&mut self, field: impl Into<String>, message: impl Into<String>) {
        self.issues.push(ConfigIssue {
            field: field.into(),
            message: message.into(),
        });
    }

    /// テンプレートの差し込みを確かめる。
    fn template(&mut self, field: &str, template: &str, allow: Allow<'_>) {
        let parts = match template_parts(template) {
            Ok(parts) => parts,
            Err(message) => {
                self.push(field, message);
                return;
            }
        };
        for part in parts {
            let TemplatePart::Placeholder(placeholder) = part else {
                continue;
            };
            let problem = match &placeholder {
                Placeholder::Var(name) if !allow.vars.contains(&name.as_str()) => Some(format!(
                    "変数 `{name}` はここより前の prepare で取り出していません"
                )),
                Placeholder::Item(_) if !allow.item => {
                    Some("`{{item.*}}` は詳細の取得 (detail) だけで使えます".to_string())
                }
                Placeholder::Pointer(_) if !allow.pointer => Some(
                    "`{{/pointer}}` は JSON の記事を読む項目と、JSON の一覧に続く詳細の取得だけで使えます"
                        .to_string(),
                ),
                Placeholder::Page if !allow.page => {
                    Some("`{{page}}` はページ送り (pages.kind = page) の一覧だけで使えます".to_string())
                }
                Placeholder::Value if !allow.value => {
                    Some("`{{value}}` は HTML の項目の template だけで使えます".to_string())
                }
                Placeholder::Secret(_) if !allow.secret => {
                    Some("`{{secret:...}}` はヘッダの値だけで使えます".to_string())
                }
                Placeholder::Secret(name) if name.is_empty() => {
                    Some("`{{secret:名前}}` の名前がありません".to_string())
                }
                _ => None,
            };
            if let Some(problem) = problem {
                self.push(field, problem);
            }
        }
    }

    /// URL のテンプレートを確かめる。ホストには差し込めない。
    fn url(&mut self, field: &str, template: &str, allow: Allow<'_>) {
        if template.trim().is_empty() {
            self.push(field, "URL を入れてください");
            return;
        }
        self.template(field, template, allow);
        let Ok(parts) = template_parts(template) else {
            return;
        };
        // URL 全体を記事の URL などで置き換える形 (`{{item.url}}`) はそのまま認める。
        if let [TemplatePart::Placeholder(placeholder)] = parts.as_slice() {
            if !matches!(placeholder, Placeholder::Item(ItemField::Url)) {
                self.push(field, "URL 全体を差し込めるのは `{{item.url}}` だけです");
            }
            return;
        }
        let first_placeholder = template.find("{{");
        let authority_end = template
            .find("://")
            .map(|index| {
                let rest = &template[index + 3..];
                index + 3 + rest.find(['/', '?', '#']).unwrap_or(rest.len())
            })
            .unwrap_or(0);
        if first_placeholder.is_some_and(|index| index < authority_end) {
            self.push(field, "URL のスキームとホストには差し込めません");
        }
        let mut resolved = String::new();
        for part in parts {
            match part {
                TemplatePart::Text(text) => resolved.push_str(text),
                TemplatePart::Placeholder(_) => resolved.push('x'),
            }
        }
        match url::Url::parse(&resolved) {
            Ok(url) if matches!(url.scheme(), "http" | "https") => {}
            Ok(_) => self.push(
                field,
                format!("http か https の URL にしてください: {template}"),
            ),
            Err(_) => self.push(field, format!("URL `{template}` を解釈できません")),
        }
    }

    fn request(&mut self, prefix: &str, request: &RequestSpec, allow: Allow<'_>) {
        self.url(&format!("{prefix}.url"), &request.url, allow);
        for (name, value) in &request.query {
            if name.trim().is_empty() {
                self.push(
                    format!("{prefix}.query"),
                    "問い合わせの名前を入れてください",
                );
            }
            self.template(&format!("{prefix}.query.{name}"), value, allow);
        }
        for (name, value) in &request.headers {
            if reqwest_header_name_is_invalid(name) {
                self.push(
                    format!("{prefix}.headers"),
                    format!("ヘッダ名 `{name}` が不正です"),
                );
            }
            self.template(
                &format!("{prefix}.headers.{name}"),
                value,
                Allow {
                    secret: true,
                    ..allow
                },
            );
        }
        if let Some(body) = &request.body {
            self.template(&format!("{prefix}.body"), body, allow);
            if request.method == HttpMethod::Get {
                self.push(
                    format!("{prefix}.body"),
                    "本文を送るなら method を POST にしてください",
                );
            }
        }
        // 文字コードの名前は `gnn-fetch` の検査で確かめる (encoding_rs が知っている名前を使うため)。
    }

    fn pointer(&mut self, field: &str, pointer: &str) {
        if !pointer.is_empty() && !pointer.starts_with('/') {
            self.push(
                field,
                format!("JSON Pointer は `/` で始めてください (`{pointer}` → `/{pointer}`)"),
            );
        }
    }

    /// JSON の項目の値 (JSON Pointer かテンプレート)。
    fn json_value(&mut self, field: &str, expr: &str, vars: &[&str]) {
        if is_template(expr) {
            self.template(
                field,
                expr,
                Allow {
                    vars,
                    pointer: true,
                    ..Allow::default()
                },
            );
        } else {
            self.pointer(field, expr);
        }
    }

    fn json_fields(&mut self, prefix: &str, fields: &JsonFields, vars: &[&str]) {
        let simple = [
            ("id", &fields.id),
            ("title", &fields.title),
            ("url", &fields.url),
            ("published", &fields.published),
            ("image", &fields.image),
            ("note", &fields.note),
        ];
        for (name, value) in simple {
            if let Some(expr) = value {
                self.json_value(&format!("{prefix}.{name}"), expr, vars);
            }
        }
        match &fields.body {
            Some(JsonBody::Value(expr)) => self.json_value(&format!("{prefix}.body"), expr, vars),
            Some(JsonBody::Blocks(blocks)) => {
                self.pointer(&format!("{prefix}.body.blocks"), &blocks.blocks);
                for (name, value) in [("text", &blocks.text), ("image", &blocks.image)] {
                    if let Some(pointer) = value {
                        self.pointer(&format!("{prefix}.body.{name}"), pointer);
                    }
                }
                if let Some(condition) = &blocks.image_when {
                    self.conditions(
                        &format!("{prefix}.body.image_when"),
                        std::slice::from_ref(condition),
                    );
                }
            }
            None => {}
        }
    }

    fn html_fields(&mut self, prefix: &str, fields: &HtmlFields, vars: &[&str]) {
        for (name, value) in fields.entries() {
            let Some(HtmlValue::Spec(spec)) = value else {
                continue;
            };
            if let Some(template) = &spec.template {
                self.template(
                    &format!("{prefix}.{name}.template"),
                    template,
                    Allow {
                        vars,
                        value: true,
                        ..Allow::default()
                    },
                );
                if !template.contains("{{value}}") {
                    self.push(
                        format!("{prefix}.{name}.template"),
                        "template には取り出した値を入れる `{{value}}` を書いてください",
                    );
                }
            }
        }
    }

    fn conditions(&mut self, prefix: &str, conditions: &[Condition]) {
        for (index, condition) in conditions.iter().enumerate() {
            let field = format!("{prefix}[{index}]");
            self.pointer(&format!("{field}.pointer"), &condition.pointer);
            let ops = [
                condition.eq.is_some(),
                condition.ne.is_some(),
                condition.exists.is_some(),
                condition.contains.is_some(),
            ];
            if ops.iter().filter(|set| **set).count() != 1 {
                self.push(
                    field,
                    "eq・ne・exists・contains のどれか 1 つを指定してください",
                );
            }
        }
    }

    fn pages(&mut self, prefix: &str, list: &ListSpec) {
        let Some(pages) = list.pages() else { return };
        let field = format!("{prefix}.pages");
        if !(1..=MAX_PAGES).contains(&pages.max_pages()) {
            self.push(
                format!("{field}.max_pages"),
                format!("1〜{MAX_PAGES} にしてください"),
            );
        }
        match pages {
            PagesSpec::Page(numbers) => {
                if numbers.step == 0 {
                    self.push(format!("{field}.step"), "1 以上にしてください");
                }
                let in_url = list.request().url.contains("{{page}}")
                    || list
                        .request()
                        .query
                        .values()
                        .any(|value| value.contains("{{page}}"));
                if numbers.param.is_none() && !in_url {
                    self.push(
                        field,
                        "param を書くか、URL か問い合わせに `{{page}}` を入れてください",
                    );
                }
            }
            PagesSpec::NextLink(next) => match list.kind() {
                ListKind::Html => {
                    if next.selector.is_none() {
                        self.push(
                            field,
                            "HTML の一覧では次のページへのリンクの selector を書いてください",
                        );
                    }
                }
                _ => match &next.pointer {
                    Some(pointer) => self.pointer(&format!("{field}.pointer"), pointer),
                    None => self.push(
                        field,
                        "JSON の一覧では次のページの URL の pointer を書いてください",
                    ),
                },
            },
            PagesSpec::Cursor(cursor) => {
                if !list.kind().has_json_items() {
                    self.push(&field, "カーソルは JSON の一覧だけで使えます");
                }
                self.pointer(&format!("{field}.pointer"), &cursor.pointer);
                if cursor.param.trim().is_empty() {
                    self.push(format!("{field}.param"), "問い合わせの名前を入れてください");
                }
            }
        }
    }
}

/// HTTP のヘッダ名として使えない文字があるか。
fn reqwest_header_name_is_invalid(name: &str) -> bool {
    name.is_empty()
        || !name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&b))
}

impl SourceConfig {
    /// CSS セレクタ以外の設定を検査する。CSS セレクタは `gnn-fetch` が検査する。
    pub fn validate(&self) -> Vec<ConfigIssue> {
        let mut check = Checker { issues: Vec::new() };

        if self.schema != SCHEMA_VERSION {
            check.push(
                "schema",
                format!(
                    "このアプリが読める書式の版は {SCHEMA_VERSION} です (今は {})",
                    self.schema
                ),
            );
        }
        if let Some(version) = &self.min_app_version
            && !version
                .split('.')
                .all(|part| !part.is_empty() && part.bytes().all(|b| b.is_ascii_digit()))
        {
            check.push(
                "min_app_version",
                "`26.12.0` のように数字と `.` で書いてください",
            );
        }
        if !is_valid_id(&self.id) {
            check.push(
                "id",
                "英小文字・数字・`_`・`-` だけで 1〜64 文字にしてください",
            );
        }
        if self.name.trim().is_empty() {
            check.push("name", "名前を入れてください");
        }
        if let Some(general) = &self.general_edition {
            if !self.adult {
                check.push(
                    "general_edition",
                    "一般向けの版を書けるのは、成人向けのソース (`adult: true`) だけです",
                );
            }
            if !is_valid_id(general) {
                check.push(
                    "general_edition",
                    "一般向けの版のソースの ID (英小文字・数字・`_`・`-`) を書いてください",
                );
            } else if *general == self.id {
                check.push(
                    "general_edition",
                    "自分とは別の、一般向けの版のソースの ID を書いてください",
                );
            }
        }
        if self.inherit_articles && self.general_edition.is_none() {
            check.push(
                "inherit_articles",
                "記事を引き継げるのは、一般向けの版 (general_edition) を書いた成人向けのソースだけです",
            );
        }
        for (field, tz) in [
            ("timezone", &self.timezone),
            ("schedule.timezone", &self.schedule.timezone),
            ("schedule.server_timezone", &self.schedule.server_timezone),
        ] {
            if let Some(tz) = tz
                && tz.parse::<chrono_tz::Tz>().is_err()
            {
                check.push(field, format!("タイムゾーン `{tz}` を解釈できません"));
            }
        }
        if self.interval_minutes == Some(0) {
            check.push("interval_minutes", "1 分以上にしてください");
        }
        if self.max_items == Some(0) {
            check.push("max_items", "1 件以上にしてください");
        }
        for (field, value) in [("homepage", &self.homepage), ("icon", &self.icon)] {
            if let Some(value) = value
                && !url::Url::parse(value).is_ok_and(|url| matches!(url.scheme(), "http" | "https"))
            {
                check.push(
                    field,
                    format!("http か https の URL にしてください: {value}"),
                );
            }
        }

        // 1. 事前の取得。先に取り出した変数だけを使える。
        if self.prepare.len() > MAX_PREPARE {
            check.push("prepare", format!("事前の取得は {MAX_PREPARE} 個までです"));
        }
        let mut vars: Vec<&str> = Vec::new();
        for (index, step) in self.prepare.iter().enumerate() {
            let prefix = format!("prepare[{index}]");
            check.request(
                &format!("{prefix}.request"),
                &step.request,
                Allow {
                    vars: &vars,
                    ..Allow::default()
                },
            );
            if step.bind.is_empty() {
                check.push(
                    format!("{prefix}.bind"),
                    "取り出す値を 1 つ以上書いてください",
                );
            }
            let mut bound = Vec::new();
            for (name, value) in &step.bind {
                let field = format!("{prefix}.bind.{name}");
                if !is_valid_var_name(name) {
                    check.push(&field, "変数名は英数字と `_` で書いてください");
                }
                if vars.contains(&name.as_str()) {
                    check.push(&field, format!("変数 `{name}` はもう取り出しています"));
                }
                let spec = value.spec(step.kind);
                match step.kind {
                    DocumentKind::Json => match &spec.pointer {
                        Some(pointer) => check.pointer(&format!("{field}.pointer"), pointer),
                        None => check.push(&field, "JSON の応答では pointer を書いてください"),
                    },
                    DocumentKind::Html => {
                        if spec.selector.is_none() {
                            check.push(&field, "HTML の応答では selector を書いてください");
                        }
                    }
                    DocumentKind::Text => {
                        if spec.regex.is_none() {
                            check.push(&field, "テキストの応答では regex を書いてください");
                        }
                    }
                }
                if let Some(pattern) = &spec.regex {
                    match regex::Regex::new(pattern) {
                        Ok(regex) if regex.captures_len() < 2 => check.push(
                            format!("{field}.regex"),
                            "値にする部分を括弧で囲んでください",
                        ),
                        Ok(_) => {}
                        Err(err) => {
                            check.push(format!("{field}.regex"), format!("正規表現の誤り: {err}"))
                        }
                    }
                }
                bound.push(name.as_str());
            }
            vars.extend(bound);
        }

        // 2. 一覧。
        if self.lists.is_empty() {
            check.push("lists", "一覧を 1 つ以上書いてください");
        }
        if self.lists.len() > MAX_LISTS {
            check.push("lists", format!("一覧は {MAX_LISTS} 個までです"));
        }
        for (index, list) in self.lists.iter().enumerate() {
            let prefix = format!("lists[{index}]");
            let page = matches!(list.pages(), Some(PagesSpec::Page(_)));
            check.request(
                &format!("{prefix}.request"),
                list.request(),
                Allow {
                    vars: &vars,
                    page,
                    ..Allow::default()
                },
            );
            match list {
                ListSpec::Feed(_) => {}
                ListSpec::Json(list) => {
                    check.pointer(&format!("{prefix}.items"), &list.items);
                    check.json_fields(&format!("{prefix}.fields"), &list.fields, &vars);
                    check.conditions(&format!("{prefix}.filter"), &list.filter);
                }
                ListSpec::Html(list) => {
                    if list.items.trim().is_empty() {
                        check.push(
                            format!("{prefix}.items"),
                            "記事の要素を表す CSS セレクタを入れてください",
                        );
                    }
                    check.html_fields(&format!("{prefix}.fields"), &list.fields, &vars);
                }
                ListSpec::HtmlJson(list) => {
                    if list.selector.trim().is_empty() {
                        check.push(
                            format!("{prefix}.selector"),
                            "JSON を持つ要素の CSS セレクタを入れてください",
                        );
                    }
                    check.pointer(&format!("{prefix}.items"), &list.items);
                    check.json_fields(&format!("{prefix}.fields"), &list.fields, &vars);
                    check.conditions(&format!("{prefix}.filter"), &list.filter);
                }
            }
            check.pages(&prefix, list);
        }

        // 4. 詳細。JSON の一覧だけなら `{{/pointer}}` を使える。
        if let Some(detail) = &self.detail {
            let pointer = !self.lists.is_empty()
                && self.lists.iter().all(|list| list.kind().has_json_items());
            if let Some(request) = detail.request() {
                check.request(
                    "detail.request",
                    request,
                    Allow {
                        vars: &vars,
                        item: true,
                        pointer,
                        ..Allow::default()
                    },
                );
            }
            match detail {
                DetailSpec::Html(detail) => {
                    check.html_fields("detail.fields", &detail.fields, &vars);
                }
                DetailSpec::Json(detail) => {
                    check.json_fields("detail.fields", &detail.fields, &vars);
                }
                DetailSpec::HtmlJson(detail) => {
                    if detail.selector.trim().is_empty() {
                        check.push(
                            "detail.selector",
                            "JSON を持つ要素の CSS セレクタを入れてください",
                        );
                    }
                    check.pointer("detail.item", &detail.item);
                    check.json_fields("detail.fields", &detail.fields, &vars);
                }
            }
        }

        // 5. 本文。
        for (index, rule) in self.body.replace.iter().enumerate() {
            if let Err(err) = regex::Regex::new(&rule.pattern) {
                check.push(
                    format!("body.replace[{index}].pattern"),
                    format!("正規表現の誤り: {err}"),
                );
            }
        }
        if let Some(base) = &self.body.base_url
            && url::Url::parse(base).is_err()
        {
            check.push("body.base_url", format!("URL `{base}` を解釈できません"));
        }

        check.url_identity("url_identity.images", &self.url_identity.images);
        check.issues
    }
}

impl Checker {
    /// 同じとみなす URL の決まりを確かめる。同じオリジンでパスの範囲が重なる決まりは断る
    /// (どちらの決まりで見分けるかが決まらなくなるため)。
    fn url_identity(&mut self, prefix: &str, rules: &[UrlIdentityRule]) {
        for (index, rule) in rules.iter().enumerate() {
            let field = format!("{prefix}[{index}]");
            match url::Url::parse(&rule.origin) {
                Ok(url)
                    if matches!(url.scheme(), "http" | "https")
                        && url.origin().ascii_serialization() == rule.origin =>
                {
                    // `.invalid` はアプリの中の画像の参照 (`source-image.invalid` など) に使う。
                    if url.host_str().is_some_and(|host| host.ends_with(".invalid")) {
                        self.push(
                            format!("{field}.origin"),
                            "`.invalid` のホストは書けません (アプリの中で使うため)",
                        );
                    }
                }
                _ => self.push(
                    format!("{field}.origin"),
                    format!(
                        "`https://cdn.example.com` のように、スキームとホストだけを書いてください (今は `{}`)",
                        rule.origin
                    ),
                ),
            }
            if let Some(path) = &rule.path_prefix
                && (!path.starts_with('/') || path.contains(['?', '#']))
            {
                self.push(
                    format!("{field}.path_prefix"),
                    "`/` で始まるパスを書いてください (問い合わせとフラグメントは書けません)",
                );
            }
            if rule.ignore_query.is_empty() {
                self.push(
                    format!("{field}.ignore_query"),
                    "問い合わせの名前を 1 つ以上書いてください",
                );
            }
            for (position, name) in rule.ignore_query.iter().enumerate() {
                if name.is_empty() {
                    self.push(format!("{field}.ignore_query"), "空の名前は書けません");
                } else if rule.ignore_query[..position].contains(name) {
                    self.push(
                        format!("{field}.ignore_query"),
                        format!("`{name}` が重なっています"),
                    );
                }
            }
            for (other_index, other) in rules[..index].iter().enumerate() {
                let overlaps = other.origin == rule.origin
                    && (rule.path_prefix().starts_with(other.path_prefix())
                        || other.path_prefix().starts_with(rule.path_prefix()));
                if overlaps {
                    self.push(
                        field.clone(),
                        format!(
                            "{prefix}[{other_index}] と同じオリジンで、パスの範囲が重なっています"
                        ),
                    );
                }
            }
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_page_start() -> u32 {
    1
}

fn is_page_start(value: &u32) -> bool {
    *value == 1
}

fn default_page_step() -> u32 {
    1
}

fn is_page_step(value: &u32) -> bool {
    *value == 1
}

fn is_true(value: &bool) -> bool {
    *value
}

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_default<T: Default + PartialEq>(value: &T) -> bool {
    *value == T::default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const MIST: &str = r#"
# ミストトレインガールズ
schema: 2
id: mist_train_girls
name: ミストトレインガールズ
tags: [dmm]
lists:
  - kind: json
    request:
      url: 'https://example.com/api/info/news?gameVersion=2'
    items: /articles
    filter:
      - { pointer: /visible, eq: true }   # 非公開の記事を除く
    fields:
      id: /articleUrl
      title: /title
      published: /date
      image: /bannerImgUrl
detail:
  kind: html
  request:
    url: 'https://example.com/api/info/news?id={{item.id}}'
body:
  image_in_body: true
"#;

    const MUV: &str = r##"
schema: 2
id: muv_luv
name: マブラヴ ガールズガーデン
lists:
  - kind: html_json
    request:
      url: 'https://example.com/content/AtomGuide'
    selector: '#atom-data'
    attr: data-announcements
    fields:
      id: /id
      title: /title
      published: /publishDate
      body: { blocks: /bodies, text: /text, image: /imageUrl, image_when: { pointer: /type, eq: 6 } }
"##;

    const ANGELIC: &str = r#"
schema: 2
id: angelic_link
name: エンジェリックリンク
prepare:
  - request:
      url: 'https://example.com/announce/catalog/'
    bind:
      catalog: /data/info
lists:
  - name: お知らせ
    kind: json
    request:
      url: 'https://example.com/announce/info/{{vars.catalog}}/'
      query:
        limit: '30'
    items: /data
    fields:
      id: /time
      title: /title
      url: 'https://example.com/news/{{/time}}'
      body: /contents
    pages:
      kind: page
      param: page
      max_pages: 3
  - kind: html
    request:
      url: 'https://example.com/events/'
    items: 'ul.events > li'
    fields:
      url: { selector: a, attr: href }
      title: '#title'
body:
  format: text
  remove: ['.share']
  replace:
    - pattern: '([^\r\n]+\.jpg)[\r\n]+'
      with: '<img src="https://example.com/news/$1">'
"#;

    #[test]
    fn parses_and_roundtrips_sources() {
        for text in [MIST, MUV, ANGELIC] {
            let source = SourceConfig::from_yaml(text).expect("parse");
            assert_eq!(source.validate(), vec![], "{}", source.id);
            let yaml = source.to_yaml().expect("serialize");
            let again = SourceConfig::from_yaml(&yaml).expect("reparse");
            assert_eq!(again, source, "{yaml}");
        }

        let mist = SourceConfig::from_yaml(MIST).unwrap();
        assert_eq!(mist.kind(), ListKind::Json);
        assert!(mist.body.image_in_body);
        assert!(mist.body.tidy);
        let ListSpec::Json(list) = &mist.lists[0] else {
            panic!("json")
        };
        assert_eq!(list.filter[0].eq, Some(serde_json::Value::Bool(true)));

        let muv = SourceConfig::from_yaml(MUV).unwrap();
        let ListSpec::HtmlJson(list) = &muv.lists[0] else {
            panic!("html_json")
        };
        let Some(JsonBody::Blocks(blocks)) = &list.fields.body else {
            panic!("blocks")
        };
        assert_eq!(
            blocks.image_when.as_ref().unwrap().eq,
            Some(serde_json::json!(6))
        );

        let angelic = SourceConfig::from_yaml(ANGELIC).unwrap();
        assert_eq!(angelic.kind_label(), "JSON API ほか 1 件の一覧");
        assert_eq!(
            angelic.prepare[0].bind["catalog"]
                .spec(DocumentKind::Json)
                .pointer
                .as_deref(),
            Some("/data/info")
        );
        assert_eq!(angelic.lists[0].pages().map(PagesSpec::max_pages), Some(3));
        let ListSpec::Html(events) = &angelic.lists[1] else {
            panic!("html")
        };
        assert_eq!(
            events.fields.title,
            Some(HtmlValue::Selector("#title".into()))
        );
    }

    #[test]
    fn rejects_unknown_fields_and_old_schema() {
        let typo = MIST.replace("items: /articles", "itemz: /articles");
        assert!(SourceConfig::from_yaml(&typo).is_err());
        let old = SourceConfig::from_yaml(&MIST.replace("schema: 2", "schema: 1")).unwrap();
        assert_eq!(old.validate()[0].field, "schema");
    }

    #[test]
    fn validation_reports_problems() {
        let text = r#"
schema: 2
id: Bad Id
name: ' '
timezone: Mars/Base
interval_minutes: 0
lists:
  - kind: json
    request:
      url: 'https://{{vars.host}}/{{vars.missing}}'
      body: x
    items: articles
    fields:
      title: title
      url: 'https://example.com/{{id}}'
    filter:
      - pointer: /visible
    pages:
      kind: page
      max_pages: 20
detail:
  kind: json
  request:
    url: '{{item.title}}'
body:
  replace:
    - pattern: '('
"#;
        let source = SourceConfig::from_yaml(text).unwrap();
        let fields: Vec<String> = source
            .validate()
            .into_iter()
            .map(|issue| format!("{}: {}", issue.field, issue.message))
            .collect();
        let names: Vec<&str> = fields
            .iter()
            .map(|line| line.split(':').next().unwrap())
            .collect();
        assert_eq!(
            names,
            vec![
                "id",
                "name",
                "timezone",
                "interval_minutes",
                "lists[0].request.url",
                "lists[0].request.url",
                "lists[0].request.url",
                "lists[0].request.body",
                "lists[0].items",
                "lists[0].fields.title",
                "lists[0].fields.url",
                "lists[0].filter[0]",
                "lists[0].pages.max_pages",
                "lists[0].pages",
                "detail.request.url",
                "body.replace[0].pattern",
            ],
            "{fields:#?}"
        );
    }

    #[test]
    fn parses_templates() {
        assert_eq!(
            template_parts("https://x/{{vars.a}}/{{ /b/c }}?p={{page}}&v={{value}}").unwrap(),
            vec![
                TemplatePart::Text("https://x/"),
                TemplatePart::Placeholder(Placeholder::Var("a".into())),
                TemplatePart::Text("/"),
                TemplatePart::Placeholder(Placeholder::Pointer("/b/c".into())),
                TemplatePart::Text("?p="),
                TemplatePart::Placeholder(Placeholder::Page),
                TemplatePart::Text("&v="),
                TemplatePart::Placeholder(Placeholder::Value),
            ]
        );
        assert_eq!(
            template_parts(r"\{\s*IMAGE\s*\}").unwrap(),
            vec![TemplatePart::Text(r"\{\s*IMAGE\s*\}")]
        );
        assert!(template_parts("{{vars.a").is_err());
        assert!(template_parts("{{unknown}}").is_err());
        assert_eq!(
            template_parts("a={{secret:dmm}}").unwrap()[1],
            TemplatePart::Placeholder(Placeholder::Secret("dmm".into()))
        );
    }

    #[test]
    fn detects_secrets_and_versions() {
        let mut source = SourceConfig::from_yaml(MIST).unwrap();
        assert!(!source.uses_secrets());
        source.lists[0]
            .request_mut()
            .headers
            .insert("Cookie".into(), "{{secret:dmm}}".into());
        assert!(source.uses_secrets());
        assert_eq!(source.validate(), vec![]);

        assert_eq!(compare_versions("26.12.0", "26.11.6"), Ordering::Greater);
        assert_eq!(compare_versions("26.11", "26.11.0"), Ordering::Equal);
        source.min_app_version = Some("99.1.0".into());
        assert!(source.requires_newer_app("26.11.6"));
        source.min_app_version = Some("26.11.6".into());
        assert!(!source.requires_newer_app("26.11.6"));
    }

    #[test]
    fn reads_schedule_timezones() {
        let mist = SourceConfig::from_yaml(MIST).unwrap();
        assert_eq!(mist.schedule, ScheduleSpec::default());
        assert!(mist.later_features().is_empty());
        assert_eq!(mist.schedule_timezone(), chrono_tz::Asia::Tokyo);
        assert_eq!(mist.server_timezone(), None);
        // 書いていなければ YAML にも出さない。
        assert!(!mist.to_yaml().unwrap().contains("schedule"));

        let text = MIST.replace(
            "tags: [dmm]",
            "tags: [dmm]\ntimezone: Asia/Shanghai\nschedule:\n  timezone: Asia/Tokyo\n  server_timezone: Asia/Shanghai",
        );
        let source = SourceConfig::from_yaml(&text).unwrap();
        assert_eq!(source.validate(), vec![]);
        assert_eq!(source.schedule_timezone(), chrono_tz::Asia::Tokyo);
        assert_eq!(source.server_timezone(), Some(chrono_tz::Asia::Shanghai));
        assert_eq!(
            source.later_features(),
            vec!["本文の予定の地域時刻 (schedule)"]
        );
        assert_eq!(source.required_app_version(), "26.11.6");
        let yaml = source.to_yaml().unwrap();
        assert_eq!(SourceConfig::from_yaml(&yaml).unwrap(), source, "{yaml}");

        // 本文用の指定がなければ、投稿日時と同じ地域時刻で読む。
        let shanghai = SourceConfig::from_yaml(
            &MIST.replace("tags: [dmm]", "tags: [dmm]\ntimezone: Asia/Shanghai"),
        )
        .unwrap();
        assert_eq!(shanghai.schedule_timezone(), chrono_tz::Asia::Shanghai);

        let broken = SourceConfig::from_yaml(&MIST.replace(
            "tags: [dmm]",
            "tags: [dmm]\nschedule:\n  timezone: Mars/Base\n  server_timezone: Moon/Base",
        ))
        .unwrap();
        let fields: Vec<String> = broken
            .validate()
            .into_iter()
            .map(|issue| issue.field)
            .collect();
        assert_eq!(
            fields,
            vec!["schedule.timezone", "schedule.server_timezone"]
        );
        // 解釈できない名前は飛ばして既定に戻る。
        assert_eq!(broken.schedule_timezone(), chrono_tz::Asia::Tokyo);
        assert_eq!(broken.server_timezone(), None);

        let typo = MIST.replace("tags: [dmm]", "tags: [dmm]\nschedule:\n  tz: Asia/Tokyo");
        assert!(SourceConfig::from_yaml(&typo).is_err());
    }

    #[test]
    fn reads_and_checks_url_identity() {
        let mist = SourceConfig::from_yaml(MIST).unwrap();
        assert_eq!(mist.url_identity, UrlIdentity::default());
        assert!(!mist.to_yaml().unwrap().contains("url_identity"));

        let text = format!(
            "{MIST}url_identity:\n  images:\n    - origin: 'https://cdn.example.com'\n      path_prefix: '/secure/'\n      ignore_query: ['s', 't']\n"
        );
        let source = SourceConfig::from_yaml(&text).unwrap();
        assert_eq!(source.validate(), vec![]);
        assert_eq!(
            source.url_identity.images,
            vec![UrlIdentityRule {
                origin: "https://cdn.example.com".into(),
                path_prefix: Some("/secure/".into()),
                ignore_query: vec!["s".into(), "t".into()],
            }]
        );
        assert_eq!(source.later_features(), vec!["画像の同一性 (url_identity)"]);
        assert_eq!(source.required_app_version(), "26.11.6");
        let yaml = source.to_yaml().unwrap();
        assert_eq!(SourceConfig::from_yaml(&yaml).unwrap(), source, "{yaml}");

        let rule = |origin: &str, path_prefix: Option<&str>, ignore: &[&str]| UrlIdentityRule {
            origin: origin.into(),
            path_prefix: path_prefix.map(str::to_string),
            ignore_query: ignore.iter().map(|name| name.to_string()).collect(),
        };
        let mut broken = source.clone();
        broken.url_identity.images = vec![
            rule("https://cdn.example.com/", None, &["s"]),
            rule("cdn.example.com", None, &["s"]),
            rule("ftp://cdn.example.com", None, &["s"]),
            rule("https://img.example.com", Some("img/"), &[]),
            rule("https://img2.example.com", None, &["s", "", "s"]),
            rule("https://source-image.invalid", None, &["s"]),
            // 同じオリジンでパスの範囲が重なる決まりは、どちらで見分けるかが決まらない。
            rule("https://cdn2.example.com", Some("/a/"), &["s"]),
            rule("https://cdn2.example.com", Some("/a/b/"), &["t"]),
            rule("https://cdn2.example.com", Some("/c/"), &["t"]),
        ];
        let issues: Vec<(String, String)> = broken
            .validate()
            .into_iter()
            .map(|issue| (issue.field, issue.message))
            .collect();
        let fields: Vec<&str> = issues.iter().map(|(field, _)| field.as_str()).collect();
        assert_eq!(
            fields,
            vec![
                "url_identity.images[0].origin",
                "url_identity.images[1].origin",
                "url_identity.images[2].origin",
                "url_identity.images[3].path_prefix",
                "url_identity.images[3].ignore_query",
                "url_identity.images[4].ignore_query",
                "url_identity.images[4].ignore_query",
                "url_identity.images[5].origin",
                "url_identity.images[7]",
            ],
            "{issues:#?}"
        );
        assert!(issues[8].1.contains("images[6]"), "{issues:#?}");

        // 知らない項目は読まない。
        let typo = text.replace("ignore_query", "ignore");
        assert!(SourceConfig::from_yaml(&typo).is_err());
    }

    #[test]
    fn accepts_only_the_news_channel() {
        let source = crate::templates::blank(ListKind::Feed, "news_only".into());
        let text = source.to_yaml().unwrap();
        assert!(SourceConfig::from_yaml(&text).is_ok());
        assert!(SourceConfig::from_yaml(&format!("{text}\nchannel: releases\n")).is_err());
        assert_eq!(Channel::parse("releases"), None);
    }
}
