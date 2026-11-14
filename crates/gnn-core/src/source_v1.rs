//! 旧形式 (書式 1、TOML) の取得定義と、新形式 (書式 2、YAML) への変換。
//!
//! 書式 1 は `sources.toml` に `[[source]]` を並べる形で、段は
//! `vars → request + list → detail → body` に固定されていた。差し込みは `{名前}`
//! (`{id}` `{url}` `{title}` `{/pointer}` と変数名)、HTML の値の template は `{}`。
//! 既存の利用者の設定を移すためだけに読む。

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::source::{self as v2, SCHEMA_2_SINCE};

/// `sources.toml` (書式 1)。
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SourcesFile {
    #[serde(default, rename = "source")]
    pub sources: Vec<SourceConfig>,
}

impl SourcesFile {
    pub fn parse(text: &str) -> Result<Self, toml::de::Error> {
        toml::from_str(text)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceConfig {
    pub id: String,
    pub name: String,
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default)]
    pub channel: v2::Channel,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub interval_minutes: Option<u32>,
    #[serde(default)]
    pub max_age_days: Option<u32>,
    #[serde(default)]
    pub max_items: Option<usize>,
    #[serde(default)]
    pub timezone: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub note: Option<String>,
    pub request: RequestSpec,
    #[serde(default)]
    pub vars: Vec<VarSpec>,
    pub list: ListSpec,
    #[serde(default)]
    pub detail: Option<DetailSpec>,
    #[serde(default)]
    pub body: BodySpec,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RequestSpec {
    pub url: String,
    #[serde(default)]
    pub method: v2::HttpMethod,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub encoding: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VarSpec {
    pub name: String,
    pub url: String,
    pub pointer: String,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ListSpec {
    Feed(FeedList),
    Json(JsonList),
    Html(HtmlList),
    HtmlJson(HtmlJsonList),
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FeedList {
    #[serde(default)]
    pub content: v2::FeedContent,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonList {
    pub items: String,
    pub fields: JsonFields,
    #[serde(default)]
    pub filter: Vec<Condition>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HtmlList {
    pub items: String,
    #[serde(default)]
    pub require: Option<String>,
    pub fields: HtmlFields,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HtmlJsonList {
    pub selector: String,
    #[serde(default)]
    pub attr: Option<String>,
    #[serde(default)]
    pub items: String,
    pub fields: JsonFields,
    #[serde(default)]
    pub filter: Vec<Condition>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonFields {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub published: Option<String>,
    #[serde(default)]
    pub body: Option<JsonBody>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub note: Option<String>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum JsonBody {
    Value(String),
    Blocks(BlocksSpec),
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BlocksSpec {
    pub blocks: String,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub image: Option<String>,
    #[serde(default)]
    pub image_when: Option<Condition>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HtmlFields {
    #[serde(default)]
    pub id: Option<HtmlValue>,
    #[serde(default)]
    pub title: Option<HtmlValue>,
    #[serde(default)]
    pub url: Option<HtmlValue>,
    #[serde(default)]
    pub published: Option<HtmlValue>,
    #[serde(default)]
    pub body: Option<HtmlValue>,
    #[serde(default)]
    pub image: Option<HtmlValue>,
    #[serde(default)]
    pub note: Option<HtmlValue>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum HtmlValue {
    Selector(String),
    Spec(HtmlValueSpec),
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HtmlValueSpec {
    #[serde(default)]
    pub selector: Option<String>,
    #[serde(default)]
    pub extract: Option<v2::Extract>,
    #[serde(default)]
    pub attr: Option<String>,
    #[serde(default)]
    pub remove: Vec<String>,
    #[serde(default)]
    pub template: Option<String>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    pub path: String,
    #[serde(default)]
    pub eq: Option<serde_json::Value>,
    #[serde(default)]
    pub ne: Option<serde_json::Value>,
    #[serde(default)]
    pub exists: Option<bool>,
    #[serde(default)]
    pub contains: Option<String>,
}

#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DetailSpec {
    Html(HtmlDetail),
    Json(JsonDetail),
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct HtmlDetail {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub fields: HtmlFields,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonDetail {
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
    #[serde(default)]
    pub fields: JsonFields,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BodySpec {
    #[serde(default)]
    pub format: v2::BodyFormat,
    #[serde(default)]
    pub newlines_to_br: bool,
    #[serde(default)]
    pub replace: Vec<ReplaceRule>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub image_in_body: bool,
    #[serde(default = "default_true")]
    pub clean: bool,
}

impl Default for BodySpec {
    fn default() -> Self {
        Self {
            format: v2::BodyFormat::default(),
            newlines_to_br: false,
            replace: Vec::new(),
            base_url: None,
            image_in_body: false,
            clean: true,
        }
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReplaceRule {
    pub pattern: String,
    #[serde(default)]
    pub with: String,
}

fn default_true() -> bool {
    true
}

/// 書式 1 の差し込み `{名前}` を、書式 2 の `{{...}}` に書き換える。
///
/// `item` が真なら `{id}` `{url}` `{title}` を記事の値とみなす (詳細の URL)。
/// `{/pointer}` は `{{/pointer}}`、それ以外の名前は変数 `{{vars.名前}}` にする。
fn convert_template(template: &str, item: bool) -> String {
    let mut out = String::with_capacity(template.len() + 8);
    let mut rest = template;
    while let Some(start) = rest.find('{') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('}') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let name = &after[..end];
        let converted = match name {
            "id" | "url" | "title" if item => format!("{{{{item.{name}}}}}"),
            pointer if pointer.starts_with('/') => format!("{{{{{pointer}}}}}"),
            var => format!("{{{{vars.{var}}}}}"),
        };
        out.push_str(&converted);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// JSON の項目の値。テンプレートなら差し込みを書き換える。
fn convert_value(expr: &str) -> String {
    if expr.contains('{') {
        convert_template(expr, false)
    } else {
        expr.to_string()
    }
}

fn convert_condition(condition: &Condition) -> v2::Condition {
    v2::Condition {
        pointer: condition.path.clone(),
        eq: condition.eq.clone(),
        ne: condition.ne.clone(),
        exists: condition.exists,
        contains: condition.contains.clone(),
    }
}

fn convert_json_fields(fields: &JsonFields) -> v2::JsonFields {
    let value = |expr: &Option<String>| expr.as_deref().map(convert_value);
    v2::JsonFields {
        id: value(&fields.id),
        title: value(&fields.title),
        url: value(&fields.url),
        published: value(&fields.published),
        body: fields.body.as_ref().map(|body| match body {
            JsonBody::Value(expr) => v2::JsonBody::Value(convert_value(expr)),
            JsonBody::Blocks(blocks) => v2::JsonBody::Blocks(v2::BlocksSpec {
                blocks: blocks.blocks.clone(),
                text: blocks.text.clone(),
                image: blocks.image.clone(),
                image_when: blocks.image_when.as_ref().map(convert_condition),
            }),
        }),
        image: value(&fields.image),
        note: value(&fields.note),
    }
}

fn convert_html_value(value: &HtmlValue) -> v2::HtmlValue {
    match value {
        HtmlValue::Selector(selector) => v2::HtmlValue::Selector(selector.clone()),
        HtmlValue::Spec(spec) => v2::HtmlValue::Spec(v2::HtmlValueSpec {
            selector: spec.selector.clone(),
            extract: spec.extract,
            attr: spec.attr.clone(),
            remove: spec.remove.clone(),
            template: spec
                .template
                .as_deref()
                .map(|template| template.replace("{}", "{{value}}")),
        }),
    }
}

fn convert_html_fields(fields: &HtmlFields) -> v2::HtmlFields {
    let value = |value: &Option<HtmlValue>| value.as_ref().map(convert_html_value);
    v2::HtmlFields {
        id: value(&fields.id),
        title: value(&fields.title),
        url: value(&fields.url),
        published: value(&fields.published),
        body: value(&fields.body),
        image: value(&fields.image),
        note: value(&fields.note),
    }
}

fn convert_request(request: &RequestSpec) -> v2::RequestSpec {
    v2::RequestSpec {
        url: convert_template(&request.url, false),
        method: request.method,
        query: BTreeMap::new(),
        headers: request.headers.clone(),
        body: request.body.clone(),
        encoding: request.encoding.clone(),
    }
}

fn detail_request(
    url: &Option<String>,
    headers: &BTreeMap<String, String>,
) -> Option<v2::RequestSpec> {
    if url.is_none() && headers.is_empty() {
        return None;
    }
    Some(v2::RequestSpec {
        url: convert_template(url.as_deref().unwrap_or("{url}"), true),
        headers: headers.clone(),
        ..v2::RequestSpec::default()
    })
}

/// 書式 1 から変換した結果。
#[derive(Debug, Clone, PartialEq)]
pub struct Converted {
    pub source: v2::SourceConfig,
    /// on/off。書式 2 では取得定義に持たず、購読 (`subscriptions.toml`) に移す。
    pub enabled: bool,
    /// 手元の画像のアイコン。書式 2 では購読に移す。
    pub local_icon: Option<String>,
}

impl SourceConfig {
    /// 書式 2 にする。
    pub fn to_v2(&self) -> Converted {
        let request = convert_request(&self.request);
        let list = match &self.list {
            ListSpec::Feed(feed) => v2::ListSpec::Feed(v2::FeedList {
                name: None,
                request,
                content: feed.content,
            }),
            ListSpec::Json(list) => v2::ListSpec::Json(v2::JsonList {
                name: None,
                request,
                items: list.items.clone(),
                filter: list.filter.iter().map(convert_condition).collect(),
                fields: convert_json_fields(&list.fields),
                pages: None,
            }),
            ListSpec::Html(list) => v2::ListSpec::Html(v2::HtmlList {
                name: None,
                request,
                items: list.items.clone(),
                require: list.require.clone(),
                fields: convert_html_fields(&list.fields),
                pages: None,
            }),
            ListSpec::HtmlJson(list) => v2::ListSpec::HtmlJson(v2::HtmlJsonList {
                name: None,
                request,
                selector: list.selector.clone(),
                attr: list.attr.clone(),
                items: list.items.clone(),
                filter: list.filter.iter().map(convert_condition).collect(),
                fields: convert_json_fields(&list.fields),
                pages: None,
            }),
        };
        let detail = self.detail.as_ref().map(|detail| match detail {
            DetailSpec::Html(detail) => v2::DetailSpec::Html(v2::HtmlDetail {
                request: detail_request(&detail.url, &detail.headers),
                fields: convert_html_fields(&detail.fields),
            }),
            DetailSpec::Json(detail) => v2::DetailSpec::Json(v2::JsonDetail {
                request: detail_request(&detail.url, &detail.headers),
                fields: convert_json_fields(&detail.fields),
            }),
        });
        let prepare = self
            .vars
            .iter()
            .map(|var| v2::PrepareStep {
                kind: v2::DocumentKind::Json,
                request: v2::RequestSpec {
                    url: convert_template(&var.url, false),
                    headers: var.headers.clone(),
                    ..v2::RequestSpec::default()
                },
                bind: BTreeMap::from([(
                    var.name.clone(),
                    v2::BindValue::Short(var.pointer.clone()),
                )]),
            })
            .collect();
        let is_web = |value: &str| value.starts_with("https://") || value.starts_with("http://");
        let (icon, local_icon) = match &self.icon {
            Some(icon) if is_web(icon) => (Some(icon.clone()), None),
            Some(icon) => (None, Some(icon.clone())),
            None => (None, None),
        };
        Converted {
            source: v2::SourceConfig {
                schema: v2::SCHEMA_VERSION,
                min_app_version: Some(SCHEMA_2_SINCE.to_string()),
                id: self.id.clone(),
                name: self.name.clone(),
                channel: self.channel,
                homepage: self.homepage.clone(),
                icon,
                tags: self.tags.clone(),
                note: self.note.clone(),
                adult: false,
                general_edition: None,
                inherit_articles: false,
                timezone: self.timezone.clone(),
                schedule: v2::ScheduleSpec::default(),
                interval_minutes: self.interval_minutes,
                max_age_days: self.max_age_days,
                max_items: self.max_items,
                prepare,
                lists: vec![list],
                detail,
                body: v2::BodySpec {
                    format: self.body.format,
                    newlines_to_br: self.body.newlines_to_br,
                    replace: self
                        .body
                        .replace
                        .iter()
                        .map(|rule| v2::ReplaceRule {
                            pattern: rule.pattern.clone(),
                            with: rule.with.clone(),
                        })
                        .collect(),
                    remove: Vec::new(),
                    base_url: self.body.base_url.clone(),
                    image_in_body: self.body.image_in_body,
                    tidy: self.body.clean,
                },
                // 書式 1 には画像の同一性の決まりがない。要る配信元は移したあとで書き足す。
                url_identity: v2::UrlIdentity::default(),
            },
            enabled: self.enabled,
            local_icon,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    const SAMPLE: &str = r##"
[[source]]
id = "angelic_link"
name = "エンジェリックリンク"
enabled = false
icon = 'C:\Users\me\icons\angelic.png'

[[source.vars]]
name = "catalog"
url = "https://example.com/announce/catalog/"
pointer = "/data/info"

[source.request]
url = "https://example.com/announce/info/{catalog}/"
headers = { Cookie = "{{secret:dmm}}" }

[source.list]
kind = "json"
items = "/data"
filter = [{ path = "/visible", eq = true }]

[source.list.fields]
id = "/time"
title = "/title"
url = "https://example.com/news/{/time}"

[source.detail]
kind = "html"
url = "https://example.com/detail?id={id}&u={url}"

[source.detail.fields]
body = { selector = ".body", template = "<div>{}</div>" }

[source.body]
clean = false
replace = [{ pattern = '\{\s*IMAGE_URL\s*\}', with = "https://example.com/images" }]
"##;

    #[test]
    fn converts_to_schema_2() {
        let file = SourcesFile::parse(SAMPLE).unwrap();
        let converted = file.sources[0].to_v2();
        assert!(!converted.enabled);
        assert_eq!(
            converted.local_icon.as_deref(),
            Some(r"C:\Users\me\icons\angelic.png")
        );
        let source = &converted.source;
        assert_eq!(source.icon, None);
        assert_eq!(source.min_app_version.as_deref(), Some(SCHEMA_2_SINCE));
        assert_eq!(
            source.prepare[0].request.url,
            "https://example.com/announce/catalog/"
        );
        let v2::ListSpec::Json(list) = &source.lists[0] else {
            panic!("json")
        };
        assert_eq!(
            list.request.url,
            "https://example.com/announce/info/{{vars.catalog}}/"
        );
        assert_eq!(list.request.headers["Cookie"], "{{secret:dmm}}");
        assert_eq!(list.filter[0].pointer, "/visible");
        assert_eq!(
            list.fields.url.as_deref(),
            Some("https://example.com/news/{{/time}}")
        );
        let Some(v2::DetailSpec::Html(detail)) = &source.detail else {
            panic!("detail")
        };
        assert_eq!(
            detail.request.as_ref().unwrap().url,
            "https://example.com/detail?id={{item.id}}&u={{item.url}}"
        );
        let Some(v2::HtmlValue::Spec(body)) = &detail.fields.body else {
            panic!("body")
        };
        assert_eq!(body.template.as_deref(), Some("<div>{{value}}</div>"));
        assert!(!source.body.tidy);
        // 正規表現は差し込みではないので書き換えない。
        assert_eq!(source.body.replace[0].pattern, r"\{\s*IMAGE_URL\s*\}");
        assert_eq!(source.validate(), vec![]);
    }

    #[test]
    fn omits_default_detail_request() {
        let text = r#"
[[source]]
id = "x"
name = "x"
[source.request]
url = "https://example.com/list"
[source.list]
kind = "html"
items = "li"
[source.list.fields]
title = "a"
[source.detail]
kind = "html"
"#;
        let converted = SourcesFile::parse(text).unwrap().sources[0].to_v2();
        let Some(v2::DetailSpec::Html(detail)) = &converted.source.detail else {
            panic!("detail")
        };
        assert_eq!(detail.request, None);
    }
}
