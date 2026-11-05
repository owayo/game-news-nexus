//! Game News Nexus の取得エンジン。
//!
//! 取得定義 (`gnn_core::SourceConfig`) に従って HTTP で記事を集め、本文を整える。
//! カタログ (game-news-nexus-config) の取り込みも受け持つ。保存と画面は扱わない。

pub mod catalog;
pub mod clean;
pub mod dates;
pub mod engine;
pub mod html;
pub mod http;
pub mod image_cache;
pub mod json;
pub mod template;

pub use catalog::{CatalogFetch, CatalogFiles, CatalogLocation, fetch_catalog};
pub use engine::{Engine, RecheckMiss, RunInput, RunOutput, Stage, TraceStep};
pub use http::{HttpFetcher, HttpOptions, NetworkPolicy};
pub use image_cache::ImageCache;

use gnn_core::source::{
    ConfigIssue, DetailSpec, DocumentKind, HtmlFields, HtmlValue, ListSpec, PagesSpec, SourceConfig,
};

/// CSS セレクタと文字コードの名前を含めて取得定義を検査する。
pub fn validate(source: &SourceConfig) -> Vec<ConfigIssue> {
    let mut issues = source.validate();

    // 文字コードの名前は、応答を読むときと同じ encoding_rs で確かめる。
    let mut encodings: Vec<(String, &str)> = Vec::new();
    for (index, step) in source.prepare.iter().enumerate() {
        encodings.push((
            format!("prepare[{index}].request.encoding"),
            step.request.encoding.as_deref().unwrap_or(""),
        ));
    }
    for (index, list) in source.lists.iter().enumerate() {
        encodings.push((
            format!("lists[{index}].request.encoding"),
            list.request().encoding.as_deref().unwrap_or(""),
        ));
    }
    if let Some(request) = source.detail.as_ref().and_then(DetailSpec::request) {
        encodings.push((
            "detail.request.encoding".into(),
            request.encoding.as_deref().unwrap_or(""),
        ));
    }
    for (field, label) in encodings {
        if !label.is_empty() && encoding_rs::Encoding::for_label(label.trim().as_bytes()).is_none()
        {
            issues.push(ConfigIssue {
                field,
                message: format!("文字コード `{label}` を解釈できません"),
            });
        }
    }

    let mut check = |field: String, text: &str| {
        if let Err(err) = html::selector(text) {
            issues.push(ConfigIssue {
                field,
                message: format!("{err:#}"),
            });
        }
    };

    for (index, step) in source.prepare.iter().enumerate() {
        if step.kind != DocumentKind::Html {
            continue;
        }
        for (name, value) in &step.bind {
            if let Some(selector) = value.spec(step.kind).selector {
                check(format!("prepare[{index}].bind.{name}.selector"), &selector);
            }
        }
    }
    for (index, list) in source.lists.iter().enumerate() {
        let prefix = format!("lists[{index}]");
        match list {
            ListSpec::Html(list) => {
                check(format!("{prefix}.items"), &list.items);
                if let Some(require) = &list.require {
                    check(format!("{prefix}.require"), require);
                }
                check_fields(&list.fields, &format!("{prefix}.fields"), &mut check);
            }
            ListSpec::HtmlJson(list) => check(format!("{prefix}.selector"), &list.selector),
            ListSpec::Feed(_) | ListSpec::Json(_) => {}
        }
        if let Some(PagesSpec::NextLink(next)) = list.pages()
            && let Some(selector) = &next.selector
        {
            check(format!("{prefix}.pages.selector"), selector);
        }
    }
    match &source.detail {
        Some(DetailSpec::Html(detail)) => check_fields(&detail.fields, "detail.fields", &mut check),
        Some(DetailSpec::HtmlJson(detail)) => check("detail.selector".into(), &detail.selector),
        Some(DetailSpec::Json(_)) | None => {}
    }
    for (index, selector) in source.body.remove.iter().enumerate() {
        check(format!("body.remove[{index}]"), selector);
    }
    issues
}

fn check_fields(fields: &HtmlFields, prefix: &str, check: &mut impl FnMut(String, &str)) {
    for (name, value) in fields.entries() {
        let Some(value) = value else { continue };
        let spec = match value {
            HtmlValue::Selector(selector) => {
                check(format!("{prefix}.{name}"), selector);
                continue;
            }
            HtmlValue::Spec(spec) => spec,
        };
        if let Some(selector) = spec
            .selector
            .as_deref()
            .filter(|text| !text.trim().is_empty())
        {
            check(format!("{prefix}.{name}.selector"), selector);
        }
        for (index, selector) in spec.remove.iter().enumerate() {
            check(format!("{prefix}.{name}.remove[{index}]"), selector);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_catalog_has_valid_selectors() {
        let catalog = gnn_core::Catalog::bundled();
        assert!(!catalog.is_empty());
        for entry in catalog.entries() {
            assert_eq!(validate(&entry.source), vec![], "{}", entry.path);
        }
    }

    #[test]
    fn reports_broken_selectors() {
        let mut source = gnn_core::templates::blank(gnn_core::ListKind::Html, "x".into());
        if let ListSpec::Html(list) = &mut source.lists[0] {
            list.items = "ul >".into();
        }
        source.body.remove = vec!["[".into()];
        source.lists[0].request_mut().encoding = Some("no-such-encoding".into());
        let fields: Vec<String> = validate(&source)
            .into_iter()
            .map(|issue| issue.field)
            .collect();
        assert_eq!(
            fields,
            vec![
                "lists[0].request.encoding",
                "lists[0].items",
                "body.remove[0]"
            ]
        );
        // encoding_rs が知っている名前なら通す。
        source.lists[0].request_mut().encoding = Some("windows-1252".into());
        source.lists[0] =
            gnn_core::templates::blank(gnn_core::ListKind::Html, "x".into()).lists[0].clone();
        source.lists[0].request_mut().encoding = Some("gbk".into());
        source.body.remove.clear();
        assert_eq!(validate(&source), vec![]);
    }
}
