//! JSON の値の取り出しと条件。

use std::collections::BTreeMap;

use gnn_core::source::{Condition, is_template};
use serde_json::Value;

use crate::template::{Escape, Scope, render};

/// JSON Pointer で値を取る。空文字は値そのもの。
pub fn pointer<'a>(value: &'a Value, pointer: &str) -> Option<&'a Value> {
    if pointer.is_empty() {
        Some(value)
    } else {
        value.pointer(pointer)
    }
}

/// 文字列・数値・真偽値を文字列にする。空文字と null、配列・オブジェクトは `None`。
pub fn scalar_to_string(value: &Value) -> Option<String> {
    let text = match value {
        Value::String(text) => text.trim().to_string(),
        Value::Number(number) => number.to_string(),
        Value::Bool(flag) => flag.to_string(),
        Value::Null | Value::Array(_) | Value::Object(_) => return None,
    };
    (!text.is_empty()).then_some(text)
}

/// `/pointer` か、`{{/pointer}}` や `{{vars.名前}}` を含むテンプレートを、記事の値で評価する。
/// 値のない差し込みがあれば `None`。
pub fn eval(item: &Value, expr: &str, vars: &BTreeMap<String, String>) -> Option<String> {
    if is_template(expr) {
        let scope = Scope {
            vars: Some(vars),
            json: Some(item),
            ..Scope::default()
        };
        render(expr, &scope, Escape::None).ok()
    } else {
        pointer(item, expr).and_then(scalar_to_string)
    }
}

/// 条件を満たすか。
pub fn matches(item: &Value, condition: &Condition) -> bool {
    let value = pointer(item, &condition.pointer);
    if let Some(exists) = condition.exists {
        return value.is_some_and(|value| !value.is_null()) == exists;
    }
    if let Some(expected) = &condition.eq {
        return value.is_some_and(|value| loosely_equal(value, expected));
    }
    if let Some(expected) = &condition.ne {
        return !value.is_some_and(|value| loosely_equal(value, expected));
    }
    if let Some(needle) = &condition.contains {
        return value
            .and_then(scalar_to_string)
            .is_some_and(|text| text.contains(needle.as_str()));
    }
    true
}

/// 配信元によって `true` と `"True"`、`6` と `"6"` が混ざるため、型をゆるく比べる。
fn loosely_equal(actual: &Value, expected: &Value) -> bool {
    if actual == expected {
        return true;
    }
    match (actual, expected) {
        (Value::String(text), Value::Bool(flag)) | (Value::Bool(flag), Value::String(text)) => text
            .trim()
            .eq_ignore_ascii_case(if *flag { "true" } else { "false" }),
        (Value::String(text), Value::Number(number))
        | (Value::Number(number), Value::String(text)) => {
            text.trim().parse::<f64>().ok() == number.as_f64()
        }
        (Value::Number(a), Value::Number(b)) => a.as_f64() == b.as_f64(),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn evaluates_pointers_and_templates() {
        let item = json!({"id": 42, "node": {"path": "a/b", "title": "  告知 "}, "empty": ""});
        let vars = BTreeMap::from([("host".to_string(), "https://x".to_string())]);
        assert_eq!(eval(&item, "/id", &vars).as_deref(), Some("42"));
        assert_eq!(eval(&item, "/node/title", &vars).as_deref(), Some("告知"));
        assert_eq!(eval(&item, "/empty", &vars), None);
        assert_eq!(eval(&item, "/missing", &vars), None);
        assert_eq!(
            eval(&item, "{{vars.host}}/{{/node/path}}.html?id={{/id}}", &vars).as_deref(),
            Some("https://x/a/b.html?id=42")
        );
        assert_eq!(eval(&item, "https://x/{{/missing}}", &vars), None);
    }

    #[test]
    fn compares_loosely() {
        let item = json!({"visible": "True", "type": "6", "tag": "メンテ情報"});
        let condition = |pointer: &str, eq: Value| Condition {
            pointer: pointer.into(),
            eq: Some(eq),
            ..Condition::default()
        };
        assert!(matches(&item, &condition("/visible", json!(true))));
        assert!(!matches(&item, &condition("/visible", json!(false))));
        assert!(matches(&item, &condition("/type", json!(6))));
        assert!(matches(
            &item,
            &Condition {
                pointer: "/tag".into(),
                contains: Some("メンテ".into()),
                ..Condition::default()
            }
        ));
        assert!(matches(
            &item,
            &Condition {
                pointer: "/missing".into(),
                exists: Some(false),
                ..Condition::default()
            }
        ));
        assert!(matches(
            &item,
            &Condition {
                pointer: "/type".into(),
                ne: Some(json!(3)),
                ..Condition::default()
            }
        ));
    }
}
