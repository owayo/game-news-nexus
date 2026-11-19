//! 取得定義の YAML の読み書き。
//!
//! カタログには第三者が書いたファイルが入るため、YAML 1.2 の決まりで厳格に読み、
//! 資源の上限を設ける。アンカー・エイリアス・マージキー (`<<`)・独自タグ・
//! 1 ファイルに複数の文書は受け付けない。`yes` / `no` / `on` / `off` は真偽値にしない。

use std::fmt;

use serde::Serialize;
use serde::de::DeserializeOwned;

/// 1 ファイルの大きさの上限。
pub const MAX_YAML_BYTES: usize = 256 * 1024;
/// 入れ子の深さの上限。
const MAX_DEPTH: usize = 32;
/// ノード数の上限。
const MAX_NODES: usize = 20_000;

/// YAML を読めなかった理由。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct YamlError {
    /// 1 始まりの行。分かるときだけ。
    pub line: Option<u64>,
    /// 1 始まりの列。分かるときだけ。
    pub column: Option<u64>,
    pub message: String,
}

impl fmt::Display for YamlError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.line, self.column) {
            (Some(line), Some(column)) => write!(f, "{line} 行 {column} 列: {}", self.message),
            (Some(line), None) => write!(f, "{line} 行: {}", self.message),
            _ => f.write_str(&self.message),
        }
    }
}

impl std::error::Error for YamlError {}

impl YamlError {
    fn new(message: impl Into<String>) -> Self {
        Self {
            line: None,
            column: None,
            message: message.into(),
        }
    }
}

/// 厳格に読む。
pub fn from_str<T: DeserializeOwned>(text: &str) -> Result<T, YamlError> {
    if text.len() > MAX_YAML_BYTES {
        return Err(YamlError::new(format!(
            "ファイルが大きすぎます (上限 {} KB)",
            MAX_YAML_BYTES / 1024
        )));
    }
    serde_saphyr::from_str_with_options(text, options()).map_err(|err| {
        let location = err.location();
        let message = err.without_snippet().to_string();
        YamlError {
            line: location.as_ref().map(|location| location.line()),
            column: location.as_ref().map(|location| location.column()),
            message: strip_location(&message),
        }
    })
}

/// YAML にする。
///
/// 人が読んで直す前提の書き方にする。
///
/// - 配列は親の下に字下げし、長い文字列も折り返さない (URL を `>` で折り返すと読みにくい)。
/// - 英字で始まる語だけの文字列 (`json` や ID、名前) は引用符を付けない。
/// - それ以外の文字列は単一引用符で囲む。CSS セレクタの `#` やテンプレートの `{{` を
///   YAML の記号と取り違えず、正規表現の `\` もそのまま書ける。
/// - 改行を含む文字列は `|` のブロックにする。
pub fn to_string<T: Serialize>(value: &T) -> Result<String, YamlError> {
    let value = serde_json::to_value(value).map_err(|err| YamlError::new(err.to_string()))?;
    let mut out = String::new();
    match &value {
        serde_json::Value::Object(map) if !map.is_empty() => write_map(&mut out, map, 0),
        serde_json::Value::Array(items) if !items.is_empty() => write_seq(&mut out, items, 0),
        scalar => {
            write_scalar(&mut out, scalar, 0);
            out.push('\n');
        }
    }
    Ok(out)
}

/// 1 行の YAML (フロー形式) にする。`{ selector: 'a', attr: href }` や `['.ad', '.share']`。
/// フォームの 1 行の入力欄で、表や配列の値を見せるために使う。
pub fn to_flow_string(value: &serde_json::Value) -> String {
    let mut out = String::new();
    write_flow(&mut out, value);
    out
}

fn write_flow(out: &mut String, value: &serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            if map.is_empty() {
                out.push_str("{}");
                return;
            }
            out.push_str("{ ");
            for (index, (key, value)) in map.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_key(out, key);
                out.push(' ');
                write_flow(out, value);
            }
            out.push_str(" }");
        }
        serde_json::Value::Array(items) => {
            out.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push_str(", ");
                }
                write_flow(out, item);
            }
            out.push(']');
        }
        serde_json::Value::String(text) if text.contains('\n') => {
            out.push_str(&serde_json::Value::String(text.clone()).to_string());
        }
        scalar => write_scalar(out, scalar, 0),
    }
}

fn indent(out: &mut String, level: usize) {
    out.extend(std::iter::repeat_n(' ', level));
}

fn write_map(out: &mut String, map: &serde_json::Map<String, serde_json::Value>, level: usize) {
    for (index, (key, value)) in map.iter().enumerate() {
        if index > 0 {
            indent(out, level);
        }
        write_key(out, key);
        write_value(out, value, level);
    }
}

fn write_seq(out: &mut String, items: &[serde_json::Value], level: usize) {
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            indent(out, level);
        }
        out.push_str("- ");
        match item {
            serde_json::Value::Object(map) if !map.is_empty() => write_map(out, map, level + 2),
            serde_json::Value::Array(items) if !items.is_empty() => {
                write_seq(out, items, level + 2)
            }
            scalar => {
                write_scalar(out, scalar, level + 2);
                out.push('\n');
            }
        }
    }
}

/// `key:` の後ろを書く。入れ子は次の行から字下げする。
fn write_value(out: &mut String, value: &serde_json::Value, level: usize) {
    match value {
        serde_json::Value::Object(map) if !map.is_empty() => {
            out.push('\n');
            indent(out, level + 2);
            write_map(out, map, level + 2);
        }
        serde_json::Value::Array(items) if !items.is_empty() => {
            out.push('\n');
            indent(out, level + 2);
            write_seq(out, items, level + 2);
        }
        scalar => {
            out.push(' ');
            write_scalar(out, scalar, level + 2);
            out.push('\n');
        }
    }
}

fn write_key(out: &mut String, key: &str) {
    if is_plain_key(key) {
        out.push_str(key);
    } else {
        write_quoted(out, key);
    }
    out.push(':');
}

fn write_scalar(out: &mut String, value: &serde_json::Value, level: usize) {
    match value {
        serde_json::Value::Null => out.push_str("null"),
        serde_json::Value::Bool(flag) => out.push_str(if *flag { "true" } else { "false" }),
        serde_json::Value::Number(number) => out.push_str(&number.to_string()),
        serde_json::Value::String(text) => write_string(out, text, level),
        serde_json::Value::Array(_) => out.push_str("[]"),
        serde_json::Value::Object(_) => out.push_str("{}"),
    }
}

fn write_string(out: &mut String, text: &str, level: usize) {
    // `|` のブロックで正しく書けるのは、先頭の行が空白で始まらず (字下げの推定がずれる)、
    // 末尾の改行が 1 つ以下 (`|` と `|-` で表せる) で、改行のほかに制御文字がないもの。
    // それ以外の複数行は二重引用符で書く。
    let block = text.contains('\n')
        && !text.starts_with([' ', '\t'])
        && !text.ends_with("\n\n")
        && !text.chars().any(|ch| ch.is_control() && ch != '\n');
    if block {
        // ブロックの中身は字下げして書く。末尾の改行の有無で chomping を選ぶ。
        let keep = if text.ends_with('\n') { "" } else { "-" };
        out.push('|');
        out.push_str(keep);
        let body = text.strip_suffix('\n').unwrap_or(text);
        for line in body.split('\n') {
            out.push('\n');
            if !line.is_empty() {
                indent(out, level);
                out.push_str(line);
            }
        }
        return;
    }
    if text.contains('\n') {
        out.push_str(&serde_json::Value::String(text.to_string()).to_string());
    } else if is_plain_word(text) {
        out.push_str(text);
    } else {
        write_quoted(out, text);
    }
}

/// 単一引用符で囲む。制御文字があれば二重引用符 (JSON と同じ書き方) にする。
fn write_quoted(out: &mut String, text: &str) {
    if text.chars().any(char::is_control) {
        out.push_str(&serde_json::Value::String(text.to_string()).to_string());
        return;
    }
    out.push('\'');
    out.push_str(&text.replace('\'', "''"));
    out.push('\'');
}

/// 引用符なしで書いてよい語か。英字で始まり、文字・数字・`_`・`-`・`.`・語の間の空白だけで
/// できていて、真偽値や null と読まれる語でないもの。
fn is_plain_word(text: &str) -> bool {
    let Some(first) = text.chars().next() else {
        return false;
    };
    if !first.is_alphabetic() || text.ends_with(' ') || text.contains("  ") {
        return false;
    }
    if !text
        .chars()
        .all(|ch| ch.is_alphanumeric() || matches!(ch, '_' | '-' | '.' | ' '))
    {
        return false;
    }
    const RESERVED: [&str; 12] = [
        "y", "n", "yes", "no", "on", "off", "true", "false", "null", "nan", "inf", "infinity",
    ];
    !RESERVED.contains(&text.to_ascii_lowercase().trim_start_matches('.'))
}

fn is_plain_key(key: &str) -> bool {
    is_plain_word(key) && !key.contains(' ')
}

fn options() -> serde_saphyr::Options {
    let mut budget = serde_saphyr::Budget::default();
    budget.max_documents = 1;
    budget.max_anchors = 0;
    budget.max_aliases = 0;
    budget.max_merge_keys = 0;
    budget.max_depth = MAX_DEPTH;
    budget.max_nodes = MAX_NODES;
    budget.max_total_scalar_bytes = MAX_YAML_BYTES;

    let mut options = serde_saphyr::Options::default();
    options.budget = Some(budget);
    options.strict_booleans = true;
    options.legacy_octal_numbers = false;
    options.merge_keys = serde_saphyr::options::MergeKeyPolicy::Error;
    options.reject_unsupported_tags = true;
    options.with_snippet = false;
    options
}

/// 位置を別に持つので、文言の末尾の「at line X column Y」を外す。
fn strip_location(message: &str) -> String {
    let trimmed = message.trim();
    match trimmed.rfind(" at line ") {
        Some(index) => trimmed[..index].trim_end_matches([',', ' ']).to_string(),
        None => trimmed.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;

    #[derive(Debug, Deserialize, PartialEq)]
    #[serde(deny_unknown_fields)]
    struct Sample {
        name: String,
        #[serde(default)]
        flag: bool,
    }

    #[test]
    fn reads_strictly() {
        assert_eq!(
            from_str::<Sample>("name: no\n").unwrap(),
            Sample {
                name: "no".into(),
                flag: false
            }
        );
        // YAML 1.1 の真偽値の書き方は受け付けない。
        assert!(from_str::<Sample>("name: a\nflag: yes\n").is_err());
        assert!(from_str::<Sample>("name: a\nflag: true\n").unwrap().flag);
        // アンカー・エイリアス・マージ・複数文書・未知の項目は読まない。
        assert!(from_str::<Sample>("name: &n a\nflag: false\n").is_err());
        assert!(from_str::<Vec<Sample>>("- &x {name: a}\n- *x\n").is_err());
        assert!(from_str::<Sample>("name: a\n---\nname: b\n").is_err());
        assert!(from_str::<Sample>("name: !custom a\n").is_err());
        let error = from_str::<Sample>("name: a\ntypo: 1\n").unwrap_err();
        assert_eq!(error.line, Some(2), "{error}");
    }

    #[test]
    fn writes_multiline_strings_that_read_back() {
        #[derive(Debug, PartialEq, serde::Serialize, Deserialize)]
        struct Note {
            text: String,
            #[serde(default)]
            list: Vec<String>,
        }
        for text in [
            "a\nb",
            "a\nb\n",
            "  a\nb",
            "a\n\n",
            "a\n\n\nb\n\n",
            "\tindented\nline",
            "# 見出し\n- 項目",
        ] {
            let note = Note {
                text: text.to_string(),
                list: vec![text.to_string()],
            };
            let yaml = to_string(&note).unwrap();
            assert_eq!(from_str::<Note>(&yaml).unwrap(), note, "{yaml}");
        }
    }

    #[test]
    fn writes_flow_values() {
        let value = serde_json::json!({"selector": "div[data-x]", "attr": "href", "remove": ["h2", ".date"], "count": 3, "y": true});
        let text = to_flow_string(&value);
        assert_eq!(
            text,
            "{ selector: 'div[data-x]', attr: href, remove: [h2, '.date'], count: 3, 'y': true }"
        );
        assert_eq!(from_str::<serde_json::Value>(&text).unwrap(), value);
    }

    #[test]
    fn rejects_large_files() {
        let text = format!("name: '{}'\n", "a".repeat(MAX_YAML_BYTES));
        assert!(from_str::<Sample>(&text).is_err());
    }
}
