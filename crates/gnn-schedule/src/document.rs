//! 文書の構造。本文の HTML (なければテキスト) を行に分け、見出し・リスト項目・表のセル・取り消し線・
//! 追記の区画を付ける。
//!
//! - 行は、ブロック要素の境目と `<br>` で分ける。1 行に詰まった `【見出し】内容` と、長い行の途中の
//!   `■` `▼` などは別の行にする。
//! - 見出しは、HTML の見出しのほか、記号 (`■` `▼` `【】` など) で始まる短い行と、`：` で終わる短い行。
//!   見出しの記号の種類ごとに入れ子を作る (同じ記号が出たら、そこまでの入れ子を閉じる)。区切り線で閉じる。
//! - `開催期間　2026年…` `受取期限：2026年…` のような「項目名 + 値」の行は、項目名を行の見出しとして持つ。
//! - 追記の印 (`(11/5 18:50追記)` `【10月6日（火）15:00追記】` など) から、次の印・区切り線・
//!   段落の終わりまでを追記の区画にする。
//!
//! 原文は行を改行でつないだ 1 つの文字列 (`Document::raw`) に持ち、根拠はそこから切り出す。

use std::ops::Range;

use crate::html::{Event, events};
use crate::normalize::Normalized;

/// 解析用の文書。
#[derive(Debug, Clone, Default)]
pub(crate) struct Document {
    /// 原文 (行を改行でつないだもの)。
    pub raw: String,
    pub lines: Vec<Line>,
    /// 追記の区画。0 番は追記の印のない本文。
    pub regions: Vec<Region>,
}

/// 1 行。
#[derive(Debug, Clone)]
pub(crate) struct Line {
    /// `Document::raw` の範囲。
    pub raw: Range<usize>,
    /// 正規化した行 (前後の空白と、見出し・項目の記号を含む)。
    pub norm: Normalized,
    pub kind: LineKind,
    /// HTML のブロック要素の番号。同じ段落の行は同じ番号。
    pub block: usize,
    /// 行全体が取り消し線の中。
    pub struck: bool,
    /// 取り消し線の範囲 (`norm` の位置)。
    pub struck_spans: Vec<Range<usize>>,
    /// この行を治める見出しの行 (外側から)。
    pub headings: Vec<usize>,
    /// 追記の区画の番号。
    pub region: usize,
    /// 「項目名 + 値」の行の、項目名と値の範囲 (`norm` の位置)。
    pub label: Option<(Range<usize>, Range<usize>)>,
    /// 見出しの直後に並ぶ値の行か (その見出しの行)。
    pub value_of: Option<usize>,
}

impl Line {
    pub fn text(&self) -> &str {
        &self.norm.text
    }

    /// 見出しの文字 (括弧と記号を除く)。
    pub fn heading_text(&self) -> &str {
        let text = self.text().trim();
        let text = text.trim_start_matches(|ch: char| {
            ch.is_whitespace() || HEADING_MARKS.contains(ch) || ITEM_MARKS.contains(ch)
        });
        text.trim_end_matches(|ch: char| {
            ch.is_whitespace() || HEADING_CLOSERS.contains(ch) || matches!(ch, ':' | '〓')
        })
    }
}

/// 行の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LineKind {
    Blank,
    Separator,
    Heading,
    /// 追記の印。
    Append,
    /// リスト項目 (`・` で始まる行、`<li>`)。
    Item,
    /// 注意書き (`※` で始まる行)。
    Note,
    Plain,
}

/// 追記の区画。
#[derive(Debug, Clone, Default)]
pub(crate) struct Region {
    /// 追記の印の行 (0 番の区画は `None`)。
    pub marker: Option<usize>,
}

/// 見出しの記号 (行の先頭に来る)。
const HEADING_MARKS: &str = "■□◆◇▼▽▲△●○◎◼◾▪▌▍▎▐〓★☆♦◉▶►〇【[<〈《≪「";
/// 見出しを閉じる括弧。
const HEADING_CLOSERS: &str = "】]>〉》≫」";
/// リスト項目の記号。
const ITEM_MARKS: &str = "・•･‐◦";

/// 本文から文書を作る。HTML が空ならテキストを行に分ける。
pub(crate) fn build(body_html: &str, body_text: &str) -> Document {
    let raw_lines = if body_html.trim().is_empty() {
        text_lines(body_text)
    } else {
        html_lines(body_html)
    };
    let mut document = Document::default();
    for raw_line in raw_lines {
        push_line(&mut document, raw_line);
    }
    classify(&mut document);
    structure(&mut document);
    document
}

/// 1 行の題名を文書にする (題名の字句を本文と同じ方法で読むため)。
pub(crate) fn title(title: &str) -> Document {
    let mut document = Document::default();
    let text = title.trim();
    let start = document.raw.len();
    document.raw.push_str(text);
    document.lines.push(Line {
        raw: start..document.raw.len(),
        norm: Normalized::new(text, start),
        kind: LineKind::Plain,
        block: 0,
        struck: false,
        struck_spans: Vec::new(),
        headings: Vec::new(),
        region: 0,
        label: None,
        value_of: None,
    });
    document.regions.push(Region::default());
    document
}

/// 題名の中心の名詞句 (先頭の `【重要】` などと、末尾の `のお知らせ` `について` などを除く)。
pub(crate) fn title_core(title: &str) -> String {
    let mut text = crate::normalize::normalize(title).trim().to_string();
    loop {
        let trimmed = text.trim_start();
        let stripped = [('【', '】'), ('[', ']'), ('<', '>'), ('(', ')')]
            .iter()
            .find_map(|&(open, close)| {
                trimmed.strip_prefix(open).and_then(|rest| {
                    rest.find(close)
                        .map(|end| rest[end + close.len_utf8()..].to_string())
                })
            });
        match stripped {
            Some(rest) if !rest.trim().is_empty() => text = rest.trim().to_string(),
            _ => break,
        }
    }
    loop {
        let trimmed = text.trim_end();
        let stripped = [('【', '】'), ('(', ')'), ('[', ']')]
            .iter()
            .find_map(|&(open, close)| {
                trimmed
                    .strip_suffix(close)
                    .and_then(|rest| rest.rfind(open).map(|start| rest[..start].to_string()))
            });
        match stripped {
            Some(rest) if !rest.trim().is_empty() => text = rest.trim().to_string(),
            _ => break,
        }
    }
    let text = text.trim_end_matches(['!', '！', ' ']);
    let mut text = text.to_string();
    for suffix in [
        "のお知らせ",
        "お知らせ",
        "について",
        "のご案内",
        "ご案内",
        "案内",
        "のご報告",
        "ご報告",
        "予告",
        "開催",
        "開始",
    ] {
        if let Some(stripped) = text.strip_suffix(suffix)
            && !stripped.trim().is_empty()
        {
            text = stripped.trim().to_string();
        }
    }
    text
}

/// 取り出した行 (正規化の前)。
#[derive(Debug, Default)]
struct RawLine {
    text: String,
    block: usize,
    struck: Vec<Range<usize>>,
    html_heading: bool,
    item: bool,
    separator: bool,
    cell: Option<(usize, usize)>,
}

fn text_lines(text: &str) -> Vec<RawLine> {
    text.lines()
        .enumerate()
        .map(|(index, line)| RawLine {
            text: line.to_string(),
            // テキストでは段落が分からないので、行ごとに分ける。
            block: index + 1,
            ..RawLine::default()
        })
        .collect()
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
    "thead",
    "tbody",
    "tfoot",
    "tr",
    "td",
    "th",
    "blockquote",
    "pre",
    "section",
    "article",
    "header",
    "footer",
    "aside",
    "figure",
    "figcaption",
    "center",
    "main",
    "nav",
];

#[derive(Default)]
struct HtmlState {
    lines: Vec<RawLine>,
    current: RawLine,
    block: usize,
    strike: usize,
    heading: usize,
    item: usize,
    row: usize,
    column: usize,
    cell: usize,
}

impl HtmlState {
    fn flush(&mut self, keep_empty: bool) {
        let mut line = std::mem::take(&mut self.current);
        let trimmed_end = line.text.trim_end_matches([' ', '\u{3000}']).len();
        line.text.truncate(trimmed_end);
        let leading = line.text.len() - line.text.trim_start_matches([' ', '\u{3000}']).len();
        if leading > 0 {
            line.text.drain(..leading);
            for span in &mut line.struck {
                span.start = span.start.saturating_sub(leading);
                span.end = span.end.saturating_sub(leading);
            }
        }
        line.struck.retain(|span| span.start < span.end);
        for span in &mut line.struck {
            span.end = span.end.min(line.text.len());
        }
        if !line.text.is_empty() || keep_empty {
            line.block = self.block;
            line.html_heading = self.heading > 0;
            line.item = self.item > 0;
            line.cell = (self.cell > 0).then_some((self.row, self.column));
            self.lines.push(line);
        }
    }

    fn text(&mut self, text: &str) {
        let start = self.current.text.len();
        for ch in text.chars() {
            if matches!(ch, ' ' | '\t' | '\n' | '\r' | '\u{c}') {
                if !self.current.text.is_empty() && !self.current.text.ends_with(' ') {
                    self.current.text.push(' ');
                }
            } else {
                self.current.text.push(ch);
            }
        }
        if self.strike > 0 && self.current.text.len() > start {
            self.current.struck.push(start..self.current.text.len());
        }
    }
}

fn html_lines(html: &str) -> Vec<RawLine> {
    let mut state = HtmlState::default();
    for event in events(html) {
        match event {
            Event::Text(text) => state.text(&text),
            Event::Start(name) => match name.as_str() {
                "br" => state.flush(true),
                "hr" => {
                    state.flush(false);
                    state.lines.push(RawLine {
                        separator: true,
                        block: state.block,
                        ..RawLine::default()
                    });
                }
                "s" | "del" | "strike" => state.strike += 1,
                name if BLOCK_TAGS.contains(&name) => {
                    state.flush(false);
                    state.block += 1;
                    match name {
                        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => state.heading += 1,
                        "li" | "dt" => state.item += 1,
                        "tr" => {
                            state.row += 1;
                            state.column = 0;
                        }
                        "td" | "th" => {
                            state.cell += 1;
                            state.column += 1;
                        }
                        _ => {}
                    }
                }
                _ => {}
            },
            Event::End(name) => match name.as_str() {
                "s" | "del" | "strike" => state.strike = state.strike.saturating_sub(1),
                name if BLOCK_TAGS.contains(&name) => {
                    state.flush(false);
                    state.block += 1;
                    match name {
                        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                            state.heading = state.heading.saturating_sub(1);
                        }
                        "li" | "dt" => state.item = state.item.saturating_sub(1),
                        "td" | "th" => state.cell = state.cell.saturating_sub(1),
                        _ => {}
                    }
                }
                _ => {}
            },
        }
    }
    state.flush(false);
    state.lines
}

/// 行を文書に足す。1 行に詰まった見出しは分ける。
fn push_line(document: &mut Document, line: RawLine) {
    if !document.raw.is_empty() {
        document.raw.push('\n');
    }
    let base = document.raw.len();
    document.raw.push_str(&line.text);
    if line.separator {
        document.lines.push(new_line(
            &document.raw,
            base..base,
            LineKind::Separator,
            &line,
        ));
        return;
    }
    if line.text.is_empty() {
        document
            .lines
            .push(new_line(&document.raw, base..base, LineKind::Blank, &line));
        return;
    }
    let mut kind_hint = if line.html_heading {
        LineKind::Heading
    } else if line.item {
        LineKind::Item
    } else {
        LineKind::Plain
    };
    for piece in split_inline(&line.text) {
        let range = base + piece.start..base + piece.end;
        let mut new = new_line(&document.raw, range, kind_hint, &line);
        new.struck_spans = struck_spans(&line, base, &new.norm);
        new.struck = !new.struck_spans.is_empty()
            && new
                .struck_spans
                .iter()
                .map(|span| span.len())
                .sum::<usize>()
                >= new.norm.text.trim().len();
        document.lines.push(new);
        kind_hint = LineKind::Plain;
    }
}

fn new_line(raw: &str, range: Range<usize>, kind: LineKind, line: &RawLine) -> Line {
    Line {
        norm: Normalized::new(&raw[range.clone()], range.start),
        raw: range,
        kind,
        block: line.block,
        struck: false,
        struck_spans: Vec::new(),
        headings: Vec::new(),
        region: 0,
        label: None,
        value_of: None,
    }
}

/// 取り消し線の範囲を、分けた行の正規化した位置にする。`base` は行が原文のどこから始まるか。
fn struck_spans(line: &RawLine, base: usize, norm: &Normalized) -> Vec<Range<usize>> {
    let mut out: Vec<Range<usize>> = Vec::new();
    if line.struck.is_empty() {
        return out;
    }
    for (at, ch) in norm.text.char_indices() {
        let until = at + ch.len_utf8();
        let raw = norm.to_raw(at..until);
        let local = raw.start.saturating_sub(base);
        if !line
            .struck
            .iter()
            .any(|span| local >= span.start && local < span.end)
        {
            continue;
        }
        match out.last_mut() {
            Some(last) if last.end == at => last.end = until,
            _ => out.push(at..until),
        }
    }
    out
}

/// 1 行に詰まった見出しの位置で行を分ける (原文の行の中の範囲を返す)。
fn split_inline(text: &str) -> Vec<Range<usize>> {
    let mut cuts = vec![0];
    let long = text.chars().count() > 40;
    let mut previous: Option<char> = None;
    let mut bracket_heading_open = false;
    for (at, ch) in text.char_indices() {
        let after_space = previous.is_none_or(|p| p == ' ' || p == '\u{3000}' || p == '。');
        if ch == '【' && after_space {
            // 行の先頭か空白の後の【】は見出し。内容が短いときだけ。
            if let Some(close) = text[at..].find('】') {
                let inner = &text[at + '【'.len_utf8()..at + close];
                if inner.chars().count() <= 24 && !inner.contains('。') {
                    if at > 0 {
                        cuts.push(at);
                    }
                    bracket_heading_open = true;
                }
            }
        } else if ch == '】' && bracket_heading_open {
            bracket_heading_open = false;
            let after = at + ch.len_utf8();
            if !text[after..].trim().is_empty() {
                cuts.push(after);
            }
        } else if long && at > 0 && after_space && "■◆▼◇●◼◾▌★・".contains(ch) {
            cuts.push(at);
        }
        previous = Some(ch);
    }
    cuts.push(text.len());
    cuts.dedup();
    let mut out = Vec::new();
    for pair in cuts.windows(2) {
        let piece = &text[pair[0]..pair[1]];
        let leading = piece.len() - piece.trim_start_matches([' ', '\u{3000}']).len();
        let trimmed = piece.trim_matches([' ', '\u{3000}']);
        if !trimmed.is_empty() {
            let start = pair[0] + leading;
            out.push(start..start + trimmed.len());
        }
    }
    if out.is_empty() {
        out.push(0..text.len());
    }
    out
}

/// 行の種類を決める。
fn classify(document: &mut Document) {
    for line in &mut document.lines {
        if matches!(line.kind, LineKind::Separator | LineKind::Blank) {
            continue;
        }
        let text = line.norm.text.trim().to_string();
        if is_separator(&text) {
            line.kind = LineKind::Separator;
            continue;
        }
        if is_append_marker(&text) {
            line.kind = LineKind::Append;
            continue;
        }
        if line.kind == LineKind::Heading {
            continue;
        }
        let first = text.chars().next().unwrap_or(' ');
        let chars = text.chars().count();
        let ends_sentence = text.ends_with(['。', '!', '?', '！', '？']);
        if is_bracket_heading(&text) {
            line.kind = LineKind::Heading;
        } else if first == '※' || first == '*' || first == '＊' {
            line.kind = LineKind::Note;
        } else if ITEM_MARKS.contains(first) || line.kind == LineKind::Item {
            line.kind = LineKind::Item;
        } else if (HEADING_MARKS.contains(first) && chars <= 48 && !ends_sentence)
            || (text.ends_with(':') && chars <= 30 && !has_digit_before_colon(&text))
            // 記号のない短い項目名だけの行 (`開催期間` `メンテナンス時間`)。
            || (chars <= 16
                && !ends_sentence
                && crate::lexicon::is_time_label(&text)
                && !crate::temporal::has_temporal(&text))
        {
            line.kind = LineKind::Heading;
        } else {
            line.kind = LineKind::Plain;
        }
        if matches!(
            line.kind,
            LineKind::Plain | LineKind::Item | LineKind::Note | LineKind::Heading
        ) {
            line.label = find_label(&line.norm.text);
        }
    }
}

fn has_digit_before_colon(text: &str) -> bool {
    text.trim_end_matches(':')
        .chars()
        .last()
        .is_some_and(|ch| ch.is_ascii_digit())
}

/// 区切り線 (同じ記号の繰り返し)。
fn is_separator(text: &str) -> bool {
    let chars: Vec<char> = text.chars().filter(|ch| !ch.is_whitespace()).collect();
    chars.len() >= 3
        && chars
            .iter()
            .all(|ch| "-ー―─━=＝_~*・.·━─＿+".contains(*ch) || *ch == '‐' || *ch == '—')
}

/// 行全体が括弧の見出し (`【見出し】` `<ご注意>` `[見出し]`)。
fn is_bracket_heading(text: &str) -> bool {
    let pairs = [
        ('【', '】'),
        ('[', ']'),
        ('<', '>'),
        ('〈', '〉'),
        ('《', '》'),
        ('≪', '≫'),
    ];
    pairs.iter().any(|&(open, close)| {
        text.starts_with(open)
            && text.ends_with(close)
            && text.chars().count() <= 30
            && text[open.len_utf8()..text.len() - close.len_utf8()]
                .chars()
                .all(|ch| ch != open && ch != close)
    })
}

/// 追記の印の行 (`(11/5(木) 18:50追記)` `■追記 2026年10月14日 18時00分` `【追記】` など)。
pub(crate) fn is_append_marker(text: &str) -> bool {
    if text.chars().count() > 48 {
        return false;
    }
    let Some(word) = ["追記", "更新", "訂正"]
        .iter()
        .find(|word| text.contains(*word))
    else {
        return false;
    };
    // 印の語・日時・括弧・記号だけでできているか。
    let rest = text.replacen(word, "", 1);
    let rest = rest.replace("あり", "");
    let stripped: String = crate::temporal::strip_temporals(&rest)
        .chars()
        .filter(|ch| {
            !ch.is_whitespace()
                && !"()[]【】<>〈〉◆■▼●・:：-~、,".contains(*ch)
                && !HEADING_MARKS.contains(*ch)
        })
        .collect();
    stripped.is_empty() && (crate::temporal::has_temporal(text) || text.contains('【'))
}

/// 「項目名 + 値」の行の、項目名と値の範囲。
fn find_label(text: &str) -> Option<(Range<usize>, Range<usize>)> {
    let body_start = text.len()
        - text
            .trim_start_matches(|ch: char| {
                ch.is_whitespace() || ITEM_MARKS.contains(ch) || "■◆▼◇●◼◾▌★〇".contains(ch)
            })
            .len();
    let body = &text[body_start..];
    // `項目名:値` (時刻の `:` は数字の間にあるので除く)。
    let mut previous: Option<char> = None;
    for (at, ch) in body.char_indices() {
        if body[..at].chars().count() > 20 {
            break;
        }
        if ch == ':' && previous.is_some_and(|p| !p.is_ascii_digit()) {
            let label = body_start..body_start + at;
            let value_start = body_start + at + 1;
            let value = &text[value_start..];
            let lead = value.len() - value.trim_start().len();
            if value.trim().is_empty() {
                return None;
            }
            return Some((label, value_start + lead..text.len()));
        }
        previous = Some(ch);
    }
    // `項目名 値` (項目名が期間・日時などで終わり、値が日時で始まる)。
    let space = body.find(' ')?;
    let label = &body[..space];
    let value = body[space..].trim_start();
    if label.chars().count() <= 16
        && crate::lexicon::is_time_label(label)
        && crate::temporal::starts_with_temporal(value)
    {
        let value_start = text.len() - value.len();
        return Some((body_start..body_start + space, value_start..text.len()));
    }
    None
}

/// 見出しの入れ子・値の行・追記の区画を付ける。
fn structure(document: &mut Document) {
    // 見出しの入れ子。
    let mut stack: Vec<(String, usize)> = Vec::new();
    for index in 0..document.lines.len() {
        let kind = document.lines[index].kind;
        match kind {
            LineKind::Separator => stack.clear(),
            LineKind::Heading => {
                let class = heading_class(&document.lines[index]);
                if let Some(position) = stack.iter().position(|(c, _)| *c == class) {
                    stack.truncate(position);
                } else if let Some(level) = class.strip_prefix('h') {
                    let level: u32 = level.parse().unwrap_or(9);
                    while stack.last().is_some_and(|(c, _)| {
                        c.strip_prefix('h')
                            .and_then(|l| l.parse::<u32>().ok())
                            .is_some_and(|l| l >= level)
                    }) {
                        stack.pop();
                    }
                }
                document.lines[index].headings = stack.iter().map(|(_, at)| *at).collect();
                stack.push((class, index));
            }
            _ => {
                document.lines[index].headings = stack.iter().map(|(_, at)| *at).collect();
            }
        }
    }

    // 見出しの直後の値の行。
    let mut current: Option<usize> = None;
    let mut seen_value = false;
    for index in 0..document.lines.len() {
        let line = &document.lines[index];
        match line.kind {
            LineKind::Heading => {
                current = Some(index);
                seen_value = false;
                continue;
            }
            LineKind::Separator | LineKind::Append => {
                current = None;
                continue;
            }
            LineKind::Blank => {
                if seen_value {
                    current = None;
                }
                continue;
            }
            _ => {}
        }
        if let Some(heading) = current {
            if index - heading > 8 {
                current = None;
                continue;
            }
            document.lines[index].value_of = Some(heading);
            seen_value = true;
        }
    }

    // 追記の区画。印から、次の印・区切り線まで。印が段落の途中にある (段落にほかの行がある) ときは、
    // その段落の終わりまで。
    document.regions = vec![Region::default()];
    let mut region = 0;
    let mut closes_with_block: Option<usize> = None;
    for index in 0..document.lines.len() {
        let (kind, block) = (document.lines[index].kind, document.lines[index].block);
        match kind {
            LineKind::Append => {
                document.regions.push(Region {
                    marker: Some(index),
                });
                region = document.regions.len() - 1;
                let shared = document
                    .lines
                    .iter()
                    .enumerate()
                    .any(|(other, line)| other != index && line.block == block);
                closes_with_block = shared.then_some(block);
            }
            LineKind::Separator => {
                region = 0;
                closes_with_block = None;
            }
            _ => {
                if closes_with_block.is_some_and(|start| start != block) {
                    region = 0;
                    closes_with_block = None;
                }
            }
        }
        document.lines[index].region = region;
    }
}

/// 見出しの種類 (入れ子を作る単位)。
fn heading_class(line: &Line) -> String {
    let text = line.text().trim();
    let first = text.chars().next().unwrap_or(' ');
    if HEADING_MARKS.contains(first) {
        return first.to_string();
    }
    if text.ends_with(':') {
        return ":".to_string();
    }
    // HTML の見出し (記号のないもの) はまとめて 1 つの種類にする。
    "h".to_string()
}

/// 文の単位 (見出し・項目・段落の文)。
#[derive(Debug, Clone)]
pub(crate) struct Unit {
    /// 正規化した文 (原文への対応つき)。
    pub norm: Normalized,
    /// 含む行。
    pub lines: Vec<usize>,
}

/// 行を文の単位にまとめる。段落の中で文の途中で折り返した行はつなぐ。
pub(crate) fn units(document: &Document) -> Vec<Unit> {
    let mut out: Vec<Unit> = Vec::new();
    let mut open = false;
    for (index, line) in document.lines.iter().enumerate() {
        match line.kind {
            LineKind::Blank | LineKind::Separator => {
                open = false;
                continue;
            }
            LineKind::Heading | LineKind::Append => {
                out.push(Unit {
                    norm: line.norm.clone(),
                    lines: vec![index],
                });
                open = false;
                continue;
            }
            _ => {}
        }
        let joinable = open
            && out.last().is_some_and(|unit| {
                let last = &document.lines[*unit.lines.last().unwrap()];
                last.block == line.block
                    && !unit.norm.text.trim_end().ends_with(['。', '!', '?'])
                    && line.kind == LineKind::Plain
                    && line.label.is_none()
                    && last.label.is_none()
                    && last.kind != LineKind::Heading
                    && line.value_of == last.value_of
                    // 日時で始まる値の行どうし (`11月10日 14:00~17:00` の次の `11月12日 …`) はつながない。
                    // 読点や続く形で終わる行の次 (`…定期メンテナンスは、` `現在の臨時メンテナンスにつきまして`
                    // の次の `2026年11月4日 16:00に終了`) はつなぐ。
                    && (continues(unit.norm.text.trim_end())
                        || !crate::temporal::starts_with_temporal(line.text()))
            });
        if joinable {
            let unit = out.last_mut().unwrap();
            unit.norm
                .push(&document.raw[line.raw.clone()], line.raw.start);
            unit.lines.push(index);
        } else {
            out.push(Unit {
                norm: line.norm.clone(),
                lines: vec![index],
            });
        }
        open = line.label.is_none();
    }
    out
}

/// 文が次の行へ続く終わり方か (読点、`につきまして` `ですが` `のため` などの続く形)。
fn continues(text: &str) -> bool {
    text.ends_with(['、', ','])
        || [
            "につきまして",
            "につきましては",
            "について",
            "については",
            "におきまして",
            "におきましては",
            "に関しまして",
            "に関しましては",
            "ですが",
            "ましたが",
            "ますが",
            "のため",
        ]
        .iter()
        .any(|ending| text.ends_with(ending))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn kinds(document: &Document) -> Vec<(LineKind, &str)> {
        document
            .lines
            .iter()
            .map(|line| (line.kind, line.text()))
            .collect()
    }

    #[test]
    fn splits_lines_and_headings() {
        let document = build(
            "<p>下記の日程でメンテナンスを行います。<br>▼メンテナンス日程<br>・2026/11/12(木) 14:00～17:00 終了予定<br>----<br>■11/8(日) 23:59に終了</p>",
            "",
        );
        assert_eq!(
            kinds(&document),
            vec![
                (LineKind::Plain, "下記の日程でメンテナンスを行います。"),
                (LineKind::Heading, "▼メンテナンス日程"),
                (LineKind::Item, "・2026/11/12(木) 14:00~17:00 終了予定"),
                (LineKind::Separator, "----"),
                (LineKind::Heading, "■11/8(日) 23:59に終了"),
            ]
        );
        assert_eq!(document.lines[2].headings, vec![1]);
        assert_eq!(document.lines[2].value_of, Some(1));
        assert!(document.lines[4].headings.is_empty());
    }

    #[test]
    fn splits_bracket_headings_in_one_line() {
        let document = build(
            "<p>以下の日時でメンテナンスを実施させていただきます。 【メンテナンス実施日時】 2026/11/06（金）14:00 ～ 17:00 【注意事項】 ・メンテナンス中はログインできません。</p>",
            "",
        );
        let texts: Vec<&str> = document.lines.iter().map(Line::text).collect();
        assert_eq!(
            texts,
            vec![
                "以下の日時でメンテナンスを実施させていただきます。",
                "【メンテナンス実施日時】",
                "2026/11/06(金)14:00 ~ 17:00",
                "【注意事項】",
                "・メンテナンス中はログインできません。",
            ]
        );
        assert_eq!(document.lines[1].kind, LineKind::Heading);
        assert_eq!(document.lines[2].value_of, Some(1));
        assert_eq!(document.lines[4].headings, vec![3]);
    }

    #[test]
    fn finds_labels_and_append_regions() {
        let document = build(
            "(11/5(木) 18:50追記)<br>先ほどメンテナンスが完了いたしました。<br>受取期限：2026年11月12日 23:59:59まで<br>----<br>開催期間　2026年10月30日メンテナンス後 ～ 2026年11月9日11:59まで",
            "",
        );
        assert_eq!(document.lines[0].kind, LineKind::Append);
        assert_eq!(document.lines[1].region, 1);
        let line = &document.lines[2];
        let (label, value) = line.label.clone().unwrap();
        assert_eq!(&line.text()[label], "受取期限");
        assert_eq!(&line.text()[value], "2026年11月12日 23:59:59まで");
        assert_eq!(document.lines[4].region, 0);
        let line = &document.lines[4];
        let (label, _) = line.label.clone().unwrap();
        assert_eq!(&line.text()[label], "開催期間");
    }

    #[test]
    fn joins_wrapped_sentences() {
        let document = build(
            "<p>2026年11月4日（水）12:00より実施していた定期メンテナンスは、<br>2026年11月4日（水）16:00に終了いたしました。</p><p>次の段落です。</p>",
            "",
        );
        let found = units(&document);
        assert_eq!(found.len(), 2);
        assert_eq!(
            found[0].norm.text,
            "2026年11月4日(水)12:00より実施していた定期メンテナンスは、2026年11月4日(水)16:00に終了いたしました。"
        );
        let document = build(
            "<p>本日14:00より行っておりましたメンテナンスにおきまして、<br>先ほど無事メンテナンスが完了いたしました。</p>",
            "",
        );
        let found = units(&document);
        assert_eq!(found.len(), 1);
        let range = found[0].norm.to_raw(0..found[0].norm.text.len());
        assert_eq!(
            &document.raw[range],
            "本日14:00より行っておりましたメンテナンスにおきまして、\n先ほど無事メンテナンスが完了いたしました。"
        );
    }

    #[test]
    fn marks_struck_text() {
        let document = build(
            "<p><s>11月10日(火) 14:00～17:00</s><br>11月12日(木) 14:00～17:00</p>",
            "",
        );
        assert!(document.lines[0].struck);
        assert!(!document.lines[1].struck);
    }
}
