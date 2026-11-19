//! ひな形 (about.hbs) から一覧を書き出す。

use anyhow::{Context as _, Result};
use handlebars::Handlebars;
use serde_json::Value;

use crate::allowlist::StandardText;

/// cargo-about の JSON をひな形に当てはめる。cargo-about がひな形に渡すデータと同じものに、
/// 標準の文面を載せたクレート (`standard_texts`) を足して渡す。改行は LF にそろえる
/// (CRLF の本文のファイルを持つクレートがある)。
pub fn render(template: &str, mut data: Value, standard_texts: &[StandardText]) -> Result<String> {
    if let Some(licenses) = data.get_mut("licenses").and_then(Value::as_array_mut) {
        for license in licenses {
            if let Some(Value::String(text)) = license.get_mut("text") {
                *text = tidy_text(text);
            }
        }
    }
    data["standard_texts"] = serde_json::to_value(standard_texts)?;
    let mut handlebars = Handlebars::new();
    // ひな形が持たない値を参照したら止める (書き誤りを見つける)
    handlebars.set_strict_mode(true);
    let rendered = handlebars
        .render_template(template, &data)
        .context("ひな形 (about.hbs) を当てはめられません")?;
    let mut out = normalize_newlines(&rendered).trim_end().to_owned();
    out.push('\n');
    Ok(out)
}

/// 本文の改行を LF にし、先頭の空行と末尾の空白を除く (囲みのすぐ内側に空行を作らない)。中身は変えない。
pub fn tidy_text(text: &str) -> String {
    let text = normalize_newlines(text);
    let mut start = 0;
    for line in text.split_inclusive('\n') {
        if !line.trim().is_empty() {
            break;
        }
        start += line.len();
    }
    text[start..].trim_end().to_owned()
}

pub fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn tidies_texts_without_changing_them() {
        assert_eq!(
            tidy_text("\r\n\r\n   Apache License\r\n  text  \r\n\r\n"),
            "   Apache License\n  text"
        );
        assert_eq!(tidy_text("MIT\n"), "MIT");
        assert_eq!(tidy_text(""), "");
    }

    /// 実際のひな形が、cargo-about の JSON の形で当てはめられるか。
    #[test]
    fn renders_the_template() {
        let template = include_str!("../../../packaging/licenses/about.hbs");
        let package = |name: &str, source: &str, repository: Option<&str>| {
            json!({
                "name": name, "version": "1.0.0", "id": format!("{source}#{name}@1.0.0"),
                "source": source, "repository": repository, "license": "MIT",
            })
        };
        let data = json!({
            "overview": [
                { "count": 2, "name": "MIT License", "id": "MIT", "indices": [0], "text": "x" },
            ],
            "licenses": [
                {
                    "name": "MIT License", "id": "MIT", "first_of_kind": true,
                    "text": "\r\nMIT License\r\n\r\nCopyright (c) 2026 a\r\n", "source_path": null,
                    "used_by": [
                        { "crate": package("a", crate::checks::CRATES_IO, None), "path": null },
                        { "crate": package("g", "git+https://example.com/g?tag=v1#abc", Some("https://example.com/g")), "path": null },
                    ],
                },
            ],
            "crates": [],
        });
        let standard = vec![StandardText {
            crates: vec!["mac".into(), "seahash".into()],
            license: "MIT".into(),
            reason: "原文がない".into(),
        }];
        let out = render(template, data, &standard).unwrap();
        assert!(out.contains("| MIT License | MIT | 2 |"), "{out}");
        assert!(
            out.contains("[a 1.0.0](https://crates.io/crates/a/1.0.0)"),
            "{out}"
        );
        assert!(out.contains("[g 1.0.0](https://example.com/g)"), "{out}");
        assert!(
            out.contains("````text\nMIT License\n\nCopyright (c) 2026 a\n````\n"),
            "{out}"
        );
        assert!(out.contains("mac・seahash"), "{out}");
        assert!(!out.contains('\r'));
        assert!(out.ends_with("````\n"), "{out}");
    }
}
