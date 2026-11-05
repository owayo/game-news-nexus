//! HTML からの値の取り出し。

use std::collections::BTreeMap;

use anyhow::{Result, anyhow};
use gnn_core::source::{Extract, HtmlFields, HtmlValue};
use scraper::{ElementRef, Html, Selector};

/// CSS セレクタを解釈する。
pub fn selector(text: &str) -> Result<Selector> {
    Selector::parse(text).map_err(|err| anyhow!("CSS セレクタ `{text}` を解釈できません: {err}"))
}

/// 取り出す項目。項目ごとに既定の取り出し方が違う。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FieldKind {
    Id,
    Title,
    Url,
    Published,
    Body,
    Image,
    Note,
}

/// 解釈済みの [`HtmlValue`]。
#[derive(Debug)]
pub struct CompiledValue {
    selector: Option<Selector>,
    extract: Option<Extract>,
    attr: Option<String>,
    remove: Vec<Selector>,
    template: Option<String>,
    kind: FieldKind,
}

impl CompiledValue {
    /// 解釈する。`template` の `{{vars.名前}}` はここで差し込む。
    pub fn compile(
        value: &HtmlValue,
        kind: FieldKind,
        vars: &BTreeMap<String, String>,
    ) -> Result<Self> {
        let spec = value.spec();
        let template = spec
            .template
            .as_deref()
            .map(|template| crate::template::bind_vars(template, vars))
            .transpose()?;
        Ok(Self {
            selector: spec
                .selector
                .as_deref()
                .filter(|text| !text.trim().is_empty())
                .map(selector)
                .transpose()?,
            extract: spec.extract,
            attr: spec.attr,
            remove: spec
                .remove
                .iter()
                .map(|text| selector(text))
                .collect::<Result<_>>()?,
            template,
            kind,
        })
    }

    /// 記事要素 `scope` から値を取り出す。
    pub fn extract(&self, scope: ElementRef<'_>) -> Option<String> {
        let target = match &self.selector {
            Some(selector) => scope.select(selector).next()?,
            None => scope,
        };

        let attr = self.attr.as_deref().or(match (self.kind, self.extract) {
            (FieldKind::Url, None) => Some("href"),
            (FieldKind::Image, None) => Some("src"),
            _ => None,
        });

        let value = if let Some(attr) = attr {
            let found =
                target
                    .attr(attr)
                    .or_else(|| match (self.kind, self.selector.is_none()) {
                        // 記事要素が <a> でなければ、中の最初のリンクを見る。
                        (FieldKind::Url, true) => scope
                            .select(&LINK_SELECTOR)
                            .next()
                            .and_then(|link| link.attr("href")),
                        (FieldKind::Image, true) => scope
                            .select(&IMG_SELECTOR)
                            .next()
                            .and_then(|img| img.attr("src")),
                        _ => None,
                    })?;
            found.trim().to_string()
        } else {
            let extract = self.extract.unwrap_or(match self.kind {
                FieldKind::Body => Extract::Html,
                _ => Extract::Text,
            });
            match extract {
                // 取り除く要素があれば、除いたあとの HTML から文字列を取る。
                Extract::Text if !self.remove.is_empty() => collapse_whitespace(
                    &gnn_core::text::html_to_text(&self.html_without_removed(target, false)),
                ),
                Extract::Text => collapse_whitespace(&target.text().collect::<String>()),
                Extract::Html => self.html_without_removed(target, false),
                Extract::OuterHtml => self.html_without_removed(target, true),
            }
        };

        if value.is_empty() {
            return None;
        }
        Some(match &self.template {
            Some(template) => template.replace("{{value}}", &value),
            None => value,
        })
    }

    fn html_without_removed(&self, target: ElementRef<'_>, outer: bool) -> String {
        let html = if outer {
            target.html()
        } else {
            target.inner_html()
        };
        remove_elements(&html, &self.remove)
    }
}

/// HTML の断片から、セレクタに合う要素を取り除く。
pub fn remove_elements(html: &str, selectors: &[Selector]) -> String {
    if selectors.is_empty() {
        return html.to_string();
    }
    let mut fragment = Html::parse_fragment(html);
    let ids: Vec<_> = selectors
        .iter()
        .flat_map(|selector| {
            fragment
                .select(selector)
                .map(|element| element.id())
                .collect::<Vec<_>>()
        })
        .collect();
    if ids.is_empty() {
        return html.to_string();
    }
    for id in ids {
        if let Some(mut node) = fragment.tree.get_mut(id) {
            node.detach();
        }
    }
    fragment.root_element().inner_html()
}

static LINK_SELECTOR: std::sync::LazyLock<Selector> =
    std::sync::LazyLock::new(|| Selector::parse("a[href]").expect("selector"));
static IMG_SELECTOR: std::sync::LazyLock<Selector> =
    std::sync::LazyLock::new(|| Selector::parse("img[src]").expect("selector"));

/// 解釈済みの [`HtmlFields`]。
#[derive(Debug, Default)]
pub struct CompiledFields {
    pub id: Option<CompiledValue>,
    pub title: Option<CompiledValue>,
    pub url: Option<CompiledValue>,
    pub published: Option<CompiledValue>,
    pub body: Option<CompiledValue>,
    pub image: Option<CompiledValue>,
    pub note: Option<CompiledValue>,
}

impl CompiledFields {
    pub fn compile(fields: &HtmlFields, vars: &BTreeMap<String, String>) -> Result<Self> {
        let compile = |value: &Option<HtmlValue>, kind| {
            value
                .as_ref()
                .map(|value| CompiledValue::compile(value, kind, vars))
                .transpose()
        };
        Ok(Self {
            id: compile(&fields.id, FieldKind::Id)?,
            title: compile(&fields.title, FieldKind::Title)?,
            url: compile(&fields.url, FieldKind::Url)?,
            published: compile(&fields.published, FieldKind::Published)?,
            body: compile(&fields.body, FieldKind::Body)?,
            image: compile(&fields.image, FieldKind::Image)?,
            note: compile(&fields.note, FieldKind::Note)?,
        })
    }
}

/// 空白の連続を 1 つの半角空白にし、前後を削る。全角空白も空白として扱う。
pub fn collapse_whitespace(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// 文書の `<body>` の中身。`<body>` がなければ文書全体。
pub fn body_inner_html(document: &Html) -> String {
    static BODY: std::sync::LazyLock<Selector> =
        std::sync::LazyLock::new(|| Selector::parse("body").expect("selector"));
    document
        .select(&BODY)
        .next()
        .map(|body| body.inner_html())
        .unwrap_or_else(|| document.root_element().inner_html())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gnn_core::source::HtmlValueSpec;

    fn compile(value: HtmlValue, kind: FieldKind) -> CompiledValue {
        CompiledValue::compile(&value, kind, &BTreeMap::new()).unwrap()
    }

    #[test]
    fn extracts_text_attributes_and_templates() {
        let document = Html::parse_document(
            r#"<ul><li class="item" data-id="7"><span class="date"> 2026/11/06 </span><a href="/news/7">告知　タイトル</a><img src="/a.png"></li></ul>"#,
        );
        let item_selector = selector("li.item").unwrap();
        let item = document.select(&item_selector).next().unwrap();

        let title = compile(HtmlValue::Selector("a".into()), FieldKind::Title);
        assert_eq!(title.extract(item).as_deref(), Some("告知 タイトル"));

        let url = compile(HtmlValue::Spec(HtmlValueSpec::default()), FieldKind::Url);
        assert_eq!(url.extract(item).as_deref(), Some("/news/7"));

        let image = compile(HtmlValue::Spec(HtmlValueSpec::default()), FieldKind::Image);
        assert_eq!(image.extract(item).as_deref(), Some("/a.png"));

        let detail = compile(
            HtmlValue::Spec(HtmlValueSpec {
                attr: Some("data-id".into()),
                template: Some("https://example.com/detail?id={{value}}".into()),
                ..HtmlValueSpec::default()
            }),
            FieldKind::Url,
        );
        assert_eq!(
            detail.extract(item).as_deref(),
            Some("https://example.com/detail?id=7")
        );

        let missing = compile(HtmlValue::Selector(".nothing".into()), FieldKind::Title);
        assert_eq!(missing.extract(item), None);
    }

    #[test]
    fn removes_elements_before_extracting_html() {
        let document = Html::parse_document(
            "<div id=\"x\"><h2>題</h2><p class=\"date\">日付</p><p>本文</p></div>",
        );
        let root = document.root_element();
        let body = compile(
            HtmlValue::Spec(HtmlValueSpec {
                selector: Some("#x".into()),
                remove: vec!["h2".into(), ".date".into()],
                ..HtmlValueSpec::default()
            }),
            FieldKind::Body,
        );
        assert_eq!(body.extract(root).as_deref(), Some("<p>本文</p>"));
        assert_eq!(
            body_inner_html(&document),
            "<div id=\"x\"><h2>題</h2><p class=\"date\">日付</p><p>本文</p></div>"
        );
        // 文字列を取るときも、取り除いてから取る。
        let title = compile(
            HtmlValue::Spec(HtmlValueSpec {
                selector: Some("#x".into()),
                remove: vec![".date".into()],
                ..HtmlValueSpec::default()
            }),
            FieldKind::Title,
        );
        assert_eq!(title.extract(root).as_deref(), Some("題 本文"));
    }

    #[test]
    fn rejects_broken_selectors() {
        assert!(selector("ul >").is_err());
        assert!(
            CompiledValue::compile(
                &HtmlValue::Selector("[".into()),
                FieldKind::Title,
                &BTreeMap::new()
            )
            .is_err()
        );
    }
}
