//! 検索用の正規化と、HTML からの文字列抽出。

use std::ops::Range;

use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

/// 検索用に正規化する。書記素ごとに NFKC をかけて小文字にする。
///
/// 全角英数と半角カナを揃え、大文字小文字を区別しない。書記素単位にするのは、
/// 半角カナの濁点のような結合文字を、元の文字列の位置へ対応づけられるようにするため。
pub fn normalize(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for grapheme in text.graphemes(true) {
        push_normalized(&mut out, grapheme);
    }
    out
}

fn push_normalized(out: &mut String, grapheme: &str) {
    for ch in grapheme.nfkc() {
        for lower in ch.to_lowercase() {
            out.push(lower);
        }
    }
}

/// `text` の中で `query` に一致する範囲を、`text` のバイト位置で返す。
///
/// 比較は [`normalize`] で正規化した文字列同士で行う。
pub fn find_matches(text: &str, query: &str) -> Vec<Range<usize>> {
    let needle = normalize(query.trim());
    if needle.is_empty() {
        return Vec::new();
    }

    // 正規化後の各書記素が、元の文字列のどの範囲から来たかを覚えておく。
    let mut normalized = String::with_capacity(text.len());
    let mut origins: Vec<(usize, Range<usize>)> = Vec::new();
    for (offset, grapheme) in text.grapheme_indices(true) {
        let start = normalized.len();
        push_normalized(&mut normalized, grapheme);
        if normalized.len() > start {
            origins.push((start, offset..offset + grapheme.len()));
        }
    }

    let origin_at = |position: usize| -> usize {
        match origins.binary_search_by(|(start, _)| start.cmp(&position)) {
            Ok(index) => index,
            Err(index) => index.saturating_sub(1),
        }
    };

    let mut matches = Vec::new();
    let mut search_from = 0;
    while let Some(found) = normalized[search_from..].find(&needle) {
        let start = search_from + found;
        let end = start + needle.len();
        let first = origin_at(start);
        let last = origin_at(end - 1);
        let range = origins[first].1.start..origins[last].1.end;
        if matches
            .last()
            .is_none_or(|previous: &Range<usize>| previous.end <= range.start)
        {
            matches.push(range);
        }
        search_from = end;
    }
    matches
}

/// HTML から表示文字列を取り出す。ブロック要素と `<br>` は改行にする。
///
/// 検索と一覧の抜粋に使う。崩れた HTML でも止まらないことを優先した簡易な実装。
pub fn html_to_text(html: &str) -> String {
    let mut out = String::with_capacity(html.len() / 2);
    let mut rest = html;
    let mut skip_until: Option<&'static str> = None;

    while !rest.is_empty() {
        if let Some(end_tag) = skip_until {
            match find_ascii_case_insensitive(rest, end_tag) {
                Some(index) => {
                    rest = &rest[index + end_tag.len()..];
                    skip_until = None;
                }
                None => break,
            }
            continue;
        }

        let Some(lt) = rest.find('<') else {
            push_text(&mut out, rest);
            break;
        };
        push_text(&mut out, &rest[..lt]);
        rest = &rest[lt..];

        if rest.starts_with("<!--") {
            rest = match rest.find("-->") {
                Some(index) => &rest[index + 3..],
                None => "",
            };
            continue;
        }

        let Some(gt) = rest.find('>') else {
            push_text(&mut out, rest);
            break;
        };
        let tag = &rest[1..gt];
        rest = &rest[gt + 1..];

        let closing = tag.starts_with('/');
        let name: String = tag
            .trim_start_matches('/')
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric())
            .map(|ch| ch.to_ascii_lowercase())
            .collect();

        match name.as_str() {
            "script" if !closing => skip_until = Some("</script>"),
            "style" if !closing => skip_until = Some("</style>"),
            "br" => push_newline(&mut out),
            "td" | "th" if !closing => push_separator(&mut out),
            name if BLOCK_TAGS.contains(&name) => push_newline(&mut out),
            _ => {}
        }
    }

    let mut text = String::with_capacity(out.len());
    for line in out.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(line);
    }
    text
}

const BLOCK_TAGS: &[&str] = &[
    "p",
    "div",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "ul",
    "ol",
    "li",
    "dl",
    "dt",
    "dd",
    "table",
    "tr",
    "article",
    "section",
    "header",
    "footer",
    "blockquote",
    "pre",
    "figure",
    "figcaption",
    "hr",
];

fn push_text(out: &mut String, raw: &str) {
    let decoded = decode_entities(raw);
    for ch in decoded.chars() {
        // 改行も含め、空白の並びは 1 つの空白にする。改行はブロック要素と <br> だけで作る。
        if ch.is_whitespace() {
            if !out.ends_with([' ', '\n']) && !out.is_empty() {
                out.push(' ');
            }
        } else {
            out.push(ch);
        }
    }
}

fn push_newline(out: &mut String) {
    while out.ends_with(' ') {
        out.pop();
    }
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
}

fn push_separator(out: &mut String) {
    if !out.is_empty() && !out.ends_with([' ', '\n']) {
        out.push(' ');
    }
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    haystack
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

/// HTML の文字参照を戻す。
pub fn decode_entities(raw: &str) -> String {
    if !raw.contains('&') {
        return raw.to_string();
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(amp) = rest.find('&') {
        out.push_str(&rest[..amp]);
        rest = &rest[amp..];
        let semi = rest
            .char_indices()
            .take_while(|(index, _)| *index < 12)
            .find(|(_, ch)| *ch == ';')
            .map(|(index, _)| index);
        let Some(semi) = semi else {
            out.push('&');
            rest = &rest[1..];
            continue;
        };
        let entity = &rest[1..semi];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" | "#39" => Some('\''),
            "nbsp" => Some('\u{a0}'),
            _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                u32::from_str_radix(&entity[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if entity.starts_with('#') => entity[1..].parse().ok().and_then(char::from_u32),
            _ => None,
        };
        match decoded {
            Some(ch) => {
                out.push(ch);
                rest = &rest[semi + 1..];
            }
            None => {
                out.push('&');
                rest = &rest[1..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// HTML の文字の部分で `query` に一致する箇所を `<mark>` で囲む。タグの中は触らない。
///
/// 本文の検索語を目立たせるのに使う。比較は [`find_matches`] と同じ正規化で行う。
pub fn mark_html(html: &str, query: &str) -> String {
    if normalize(query.trim()).is_empty() {
        return html.to_string();
    }
    let mut out = String::with_capacity(html.len() + 64);
    let mut rest = html;
    while !rest.is_empty() {
        match rest.find('<') {
            Some(0) => {
                let end = rest.find('>').map_or(rest.len(), |index| index + 1);
                out.push_str(&rest[..end]);
                rest = &rest[end..];
            }
            Some(lt) => {
                mark_text(&mut out, &rest[..lt], query);
                rest = &rest[lt..];
            }
            None => {
                mark_text(&mut out, rest, query);
                break;
            }
        }
    }
    out
}

fn mark_text(out: &mut String, escaped: &str, query: &str) {
    let text = decode_entities(escaped);
    let matches = find_matches(&text, query);
    if matches.is_empty() {
        out.push_str(escaped);
        return;
    }
    let mut last = 0;
    for range in matches {
        out.push_str(&escape_html(&text[last..range.start]));
        out.push_str("<mark>");
        out.push_str(&escape_html(&text[range.clone()]));
        out.push_str("</mark>");
        last = range.end;
    }
    out.push_str(&escape_html(&text[last..]));
}

/// HTML の特殊文字を逃がす。
pub fn escape_html(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn normalizes_width_and_case() {
        assert_eq!(normalize("ＡＢＣ１２３"), "abc123");
        assert_eq!(normalize("ｶﾞﾁｬ"), "ガチャ");
        assert_eq!(normalize("Maintenance"), "maintenance");
    }

    #[test]
    fn finds_matches_in_original_positions() {
        let text = "定期メンテナンスのお知らせ（ＭＡＩＮＴＥＮＡＮＣＥ）";
        let matches = find_matches(text, "maintenance");
        assert_eq!(matches.len(), 1);
        assert_eq!(&text[matches[0].clone()], "ＭＡＩＮＴＥＮＡＮＣＥ");

        let text = "ｶﾞﾁｬ更新とガチャ追加";
        let matches = find_matches(text, "ガチャ");
        let found: Vec<&str> = matches.iter().map(|range| &text[range.clone()]).collect();
        assert_eq!(found, vec!["ｶﾞﾁｬ", "ガチャ"]);

        assert!(find_matches(text, "  ").is_empty());
    }

    #[test]
    fn extracts_text_from_html() {
        let html = "<div>告知<br>本文&amp;詳細</div><p>二段落目</p><script>var x = '<p>';</script><!-- c --><table><tr><td>A</td><td>B</td></tr></table>";
        assert_eq!(html_to_text(html), "告知\n本文&詳細\n二段落目\nA B");
    }

    #[test]
    fn marks_matches_outside_tags() {
        assert_eq!(
            mark_html(
                r#"<p class="ガチャ">ガチャ&amp;ｶﾞﾁｬ</p><img alt="ガチャ">"#,
                "ガチャ"
            ),
            r#"<p class="ガチャ"><mark>ガチャ</mark>&amp;<mark>ｶﾞﾁｬ</mark></p><img alt="ガチャ">"#
        );
        assert_eq!(
            mark_html("<p>a&lt;b</p>", "<"),
            "<p>a<mark>&lt;</mark>b</p>"
        );
        assert_eq!(mark_html("<p>x</p>", " "), "<p>x</p>");
    }

    #[test]
    fn decodes_and_escapes_entities() {
        assert_eq!(
            decode_entities("a&amp;b&#12354;&#x3042;&unknown;&"),
            "a&bああ&unknown;&"
        );
        assert_eq!(
            decode_entities("新作 & 予約開始のお知らせ"),
            "新作 & 予約開始のお知らせ"
        );
        assert_eq!(
            escape_html("<a href=\"x\">'&'</a>"),
            "&lt;a href=&quot;x&quot;&gt;&#39;&amp;&#39;&lt;/a&gt;"
        );
    }
}
