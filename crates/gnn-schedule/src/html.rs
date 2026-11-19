//! 整形済みの本文 HTML (`gnn-fetch` の `clean_html` の出力) を、タグと文字の並びに分ける。
//!
//! 整形済みの HTML は閉じたタグと逃がした文字だけからなるが、旧形式から取り込んだ本文などで崩れて
//! いても止まらないようにする。タグの属性は読まない。

use gnn_core::text::decode_entities;

/// HTML の 1 つの要素。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Event {
    /// 開きタグ (名前は小文字)。`<br>` `<hr>` `<img>` もここ。
    Start(String),
    /// 閉じタグ (名前は小文字)。
    End(String),
    /// 文字 (文字参照を戻したもの)。
    Text(String),
}

/// HTML をタグと文字の並びにする。スクリプトとスタイルの中身、コメントは捨てる。
pub(crate) fn events(html: &str) -> Vec<Event> {
    let mut out = Vec::new();
    let mut rest = html;
    while !rest.is_empty() {
        let Some(lt) = rest.find('<') else {
            push_text(&mut out, rest);
            break;
        };
        push_text(&mut out, &rest[..lt]);
        rest = &rest[lt..];
        if let Some(after) = rest.strip_prefix("<!--") {
            rest = after.find("-->").map_or("", |end| &after[end + 3..]);
            continue;
        }
        let bytes = rest.as_bytes();
        let closing = bytes.get(1) == Some(&b'/');
        let name_start = if closing { 2 } else { 1 };
        if !bytes
            .get(name_start)
            .is_some_and(|byte| byte.is_ascii_alphabetic())
        {
            // タグではない `<` は文字として読む。
            push_text(&mut out, "<");
            rest = &rest[1..];
            continue;
        }
        let Some(gt) = rest.find('>') else {
            push_text(&mut out, rest);
            break;
        };
        let name: String = rest[name_start..gt]
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric())
            .map(|ch| ch.to_ascii_lowercase())
            .collect();
        rest = &rest[gt + 1..];
        if closing {
            out.push(Event::End(name));
            continue;
        }
        if matches!(name.as_str(), "script" | "style") {
            let end_tag = format!("</{name}");
            rest = find_ascii_case_insensitive(rest, &end_tag)
                .and_then(|index| rest[index..].find('>').map(|gt| &rest[index + gt + 1..]))
                .unwrap_or("");
            continue;
        }
        out.push(Event::Start(name));
    }
    out
}

fn push_text(out: &mut Vec<Event>, raw: &str) {
    if raw.is_empty() {
        return;
    }
    let text = decode_entities(raw);
    if let Some(Event::Text(previous)) = out.last_mut() {
        previous.push_str(&text);
    } else {
        out.push(Event::Text(text));
    }
}

fn find_ascii_case_insensitive(haystack: &str, needle: &str) -> Option<usize> {
    let haystack = haystack.as_bytes();
    let needle = needle.as_bytes();
    haystack
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_tags_and_text() {
        assert_eq!(
            events("<p>A&amp;B<br>C</p><script>x<y</script>< D<!-- c -->"),
            vec![
                Event::Start("p".into()),
                Event::Text("A&B".into()),
                Event::Start("br".into()),
                Event::Text("C".into()),
                Event::End("p".into()),
                Event::Text("< D".into()),
            ]
        );
    }
}
