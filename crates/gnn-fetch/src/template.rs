//! テンプレート (`{{...}}`) の展開。
//!
//! 書き方は `gnn_core::source::template_parts` が決める。ここでは値を差し込む。

use std::collections::BTreeMap;

use anyhow::{Result, anyhow};
use gnn_core::source::{ItemField, Placeholder, TemplatePart, template_parts};
use percent_encoding::{AsciiSet, CONTROLS, utf8_percent_encode};
use serde_json::Value;

use crate::json::{pointer, scalar_to_string};

/// URL の中に差し込む値で符号化する文字。`/` と `:` は残す (パスや時刻をそのまま渡すため)。
const URL_VALUE: &AsciiSet = &CONTROLS
    .add(b' ')
    .add(b'"')
    .add(b'#')
    .add(b'%')
    .add(b'&')
    .add(b'+')
    .add(b'<')
    .add(b'>')
    .add(b'?')
    .add(b'[')
    .add(b'\\')
    .add(b']')
    .add(b'^')
    .add(b'`')
    .add(b'{')
    .add(b'|')
    .add(b'}');

/// 一覧で読んだ記事の値。詳細の URL に差し込む。
#[derive(Debug, Clone, Default)]
pub struct ItemValues {
    pub id: Option<String>,
    pub url: Option<String>,
    pub title: Option<String>,
}

/// 差し込める値。
#[derive(Default, Clone, Copy)]
pub struct Scope<'a> {
    pub vars: Option<&'a BTreeMap<String, String>>,
    pub item: Option<&'a ItemValues>,
    /// `{{/pointer}}` で読む JSON。
    pub json: Option<&'a Value>,
    pub page: Option<&'a str>,
    pub value: Option<&'a str>,
}

/// 値の差し込み方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Escape {
    /// そのまま入れる。
    None,
    /// URL の一部として符号化する。テンプレート全体が 1 つの差し込みなら (`{{item.url}}`)
    /// URL そのものなので符号化しない。
    Url,
}

/// テンプレートを展開する。値のない差し込みがあれば誤り。
///
/// `{{secret:名前}}` は展開せずに残す (HTTP の層が `secrets.toml` の値に置き換える)。
pub fn render(template: &str, scope: &Scope<'_>, escape: Escape) -> Result<String> {
    let parts = template_parts(template).map_err(|message| anyhow!(message))?;
    let whole = parts.len() == 1 && matches!(parts[0], TemplatePart::Placeholder(_));
    let mut out = String::with_capacity(template.len());
    for part in &parts {
        match part {
            TemplatePart::Text(text) => out.push_str(text),
            TemplatePart::Placeholder(Placeholder::Secret(name)) => {
                out.push_str("{{secret:");
                out.push_str(name);
                out.push_str("}}");
            }
            TemplatePart::Placeholder(placeholder) => {
                let value = lookup(placeholder, scope).ok_or_else(|| {
                    anyhow!("`{}` の値がありません ({template})", describe(placeholder))
                })?;
                if escape == Escape::Url && !whole {
                    out.extend(utf8_percent_encode(&value, URL_VALUE));
                } else {
                    out.push_str(&value);
                }
            }
        }
    }
    Ok(out)
}

/// 変数だけを先に差し込み、ほかの差し込みは `{{...}}` の形で残す。
pub fn bind_vars(template: &str, vars: &BTreeMap<String, String>) -> Result<String> {
    let parts = template_parts(template).map_err(|message| anyhow!(message))?;
    let mut out = String::with_capacity(template.len());
    for part in &parts {
        match part {
            TemplatePart::Text(text) => out.push_str(text),
            TemplatePart::Placeholder(Placeholder::Var(name)) => out.push_str(
                vars.get(name)
                    .ok_or_else(|| anyhow!("変数 `{name}` の値がありません"))?,
            ),
            TemplatePart::Placeholder(placeholder) => {
                out.push_str("{{");
                out.push_str(&describe(placeholder));
                out.push_str("}}");
            }
        }
    }
    Ok(out)
}

fn lookup(placeholder: &Placeholder, scope: &Scope<'_>) -> Option<String> {
    match placeholder {
        Placeholder::Var(name) => scope.vars?.get(name).cloned(),
        Placeholder::Item(field) => {
            let item = scope.item?;
            match field {
                ItemField::Id => item.id.clone(),
                ItemField::Url => item.url.clone(),
                ItemField::Title => item.title.clone(),
            }
        }
        Placeholder::Pointer(path) => pointer(scope.json?, path).and_then(scalar_to_string),
        Placeholder::Page => scope.page.map(str::to_string),
        Placeholder::Value => scope.value.map(str::to_string),
        Placeholder::Secret(_) => None,
    }
}

fn describe(placeholder: &Placeholder) -> String {
    match placeholder {
        Placeholder::Var(name) => format!("vars.{name}"),
        Placeholder::Item(ItemField::Id) => "item.id".into(),
        Placeholder::Item(ItemField::Url) => "item.url".into(),
        Placeholder::Item(ItemField::Title) => "item.title".into(),
        Placeholder::Pointer(path) => path.clone(),
        Placeholder::Page => "page".into(),
        Placeholder::Value => "value".into(),
        Placeholder::Secret(name) => format!("secret:{name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn renders_values_with_url_escaping() {
        let vars = BTreeMap::from([("catalog".to_string(), "2026 11&x".to_string())]);
        let item = ItemValues {
            id: Some("a/b?c".into()),
            url: Some("https://example.com/n?id=1&x=2".into()),
            title: None,
        };
        let json = json!({"id": 42});
        let scope = Scope {
            vars: Some(&vars),
            item: Some(&item),
            json: Some(&json),
            page: Some("3"),
            value: None,
        };
        assert_eq!(
            render(
                "https://x/{{vars.catalog}}/?id={{item.id}}&p={{page}}&j={{/id}}",
                &scope,
                Escape::Url
            )
            .unwrap(),
            "https://x/2026%2011%26x/?id=a/b%3Fc&p=3&j=42"
        );
        // URL 全体の差し込みは符号化しない。
        assert_eq!(
            render("{{item.url}}", &scope, Escape::Url).unwrap(),
            "https://example.com/n?id=1&x=2"
        );
        assert_eq!(
            render(
                "Bearer {{secret:token}} {{vars.catalog}}",
                &scope,
                Escape::None
            )
            .unwrap(),
            "Bearer {{secret:token}} 2026 11&x"
        );
        assert!(render("{{item.title}}", &scope, Escape::Url).is_err());
        assert!(render("{{vars.missing}}", &scope, Escape::None).is_err());
    }

    #[test]
    fn binds_only_variables() {
        let vars = BTreeMap::from([("base".to_string(), "https://x".to_string())]);
        assert_eq!(
            bind_vars("{{vars.base}}/{{ value }}?{{/id}}", &vars).unwrap(),
            "https://x/{{value}}?{{/id}}"
        );
        assert!(bind_vars("{{vars.nope}}", &vars).is_err());
    }
}
