//! 本文 HTML の整形。
//!
//! 表示に使う gpui-kit の `TextView` が描ける範囲のタグに寄せ、スクリプトや装飾、
//! 空の要素、連続した改行を取り除く。相対 URL は絶対 URL にし、遅延読み込みの画像は
//! 本来の URL に戻す。元の Python 実装は正規表現と BeautifulSoup を収束するまで
//! 繰り返していたが、ここでは 1 度の DOM 走査で同じ結果を作る。

use scraper::{ElementRef, Html, Node};
use url::Url;

use gnn_core::text::escape_html;

/// 整形の設定。
#[derive(Debug, Clone, Copy, Default)]
pub struct CleanOptions<'a> {
    /// 相対 URL の基準。
    pub base_url: Option<&'a Url>,
    /// 文字列中の改行を `<br>` にする。
    pub newlines_to_br: bool,
    /// 空の要素と余分な改行を取り除く。`false` でも危険なタグは除き、URL は直す。
    pub tidy: bool,
}

/// 本文 HTML を整形する。
pub fn clean_html(html: &str, options: CleanOptions<'_>) -> String {
    let fragment = Html::parse_fragment(html);
    let root = fragment.root_element();
    let mut nodes = convert_children(root, &options, Context::default());
    if options.tidy {
        tidy_children(&mut nodes, true);
    }
    let mut out = String::with_capacity(html.len());
    for node in &nodes {
        serialize(node, &mut out);
    }
    out.trim().to_string()
}

#[derive(Debug, Clone, PartialEq)]
enum CNode {
    Text(String),
    Element {
        name: &'static str,
        attrs: Vec<(&'static str, String)>,
        children: Vec<CNode>,
    },
}

impl CNode {
    fn br() -> Self {
        CNode::Element {
            name: "br",
            attrs: Vec::new(),
            children: Vec::new(),
        }
    }

    fn is_br(&self) -> bool {
        matches!(self, CNode::Element { name: "br", .. })
    }

    fn is_blank_text(&self) -> bool {
        matches!(self, CNode::Text(text) if text.trim().is_empty())
    }

    fn is_block(&self) -> bool {
        matches!(self, CNode::Element { name, .. } if BLOCK_OUTPUT.contains(name))
    }

    /// 文字・画像・罫線・表のいずれかを含むか。
    fn has_content(&self) -> bool {
        match self {
            CNode::Text(text) => !text.trim().is_empty(),
            CNode::Element { name, children, .. } => {
                matches!(*name, "img" | "hr" | "table") || children.iter().any(CNode::has_content)
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct Context {
    in_pre: bool,
    in_table_cell: bool,
}

/// 出力するブロック要素。
const BLOCK_OUTPUT: &[&str] = &[
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
    "table",
    "thead",
    "tbody",
    "tr",
    "td",
    "th",
    "blockquote",
    "pre",
    "hr",
    "section",
    "article",
    "header",
    "footer",
    "aside",
    "figure",
    "figcaption",
];

/// 中身ごと捨てる要素。
const DROP: &[&str] = &[
    "script", "style", "noscript", "template", "iframe", "object", "embed", "svg", "canvas",
    "form", "input", "button", "select", "textarea", "link", "meta", "head", "title", "audio",
    "video", "source", "track", "map", "base",
];

/// 出力するタグ名に置き換える。`None` は子だけを残す (タグを外す)。
fn output_name(name: &str) -> Option<&'static str> {
    Some(match name {
        "p" => "p",
        "div" | "center" | "main" | "nav" | "address" | "details" | "summary" | "dl" | "dd"
        | "fieldset" => "div",
        "dt" => "p",
        "h1" => "h1",
        "h2" => "h2",
        "h3" => "h3",
        "h4" => "h4",
        "h5" => "h5",
        "h6" => "h6",
        "ul" => "ul",
        "ol" => "ol",
        "li" => "li",
        "table" => "table",
        "thead" => "thead",
        "tbody" | "tfoot" => "tbody",
        "tr" => "tr",
        "td" => "td",
        "th" => "th",
        "blockquote" => "blockquote",
        "pre" => "pre",
        "hr" => "hr",
        "section" => "section",
        "article" => "article",
        "header" => "header",
        "footer" => "footer",
        "aside" => "aside",
        "figure" => "figure",
        "figcaption" => "figcaption",
        "a" => "a",
        "b" | "strong" => "strong",
        "i" | "em" | "cite" | "var" => "em",
        "u" | "ins" => "u",
        "s" | "del" | "strike" => "s",
        "code" | "kbd" | "samp" | "tt" => "code",
        "mark" => "mark",
        "br" => "br",
        "img" => "img",
        _ => return None,
    })
}

fn convert_children(
    element: ElementRef<'_>,
    options: &CleanOptions<'_>,
    context: Context,
) -> Vec<CNode> {
    let mut out = Vec::new();
    for child in element.children() {
        match child.value() {
            Node::Text(text) => push_text(&mut out, text, options, context),
            Node::Element(_) => {
                if let Some(child) = ElementRef::wrap(child) {
                    out.extend(convert_element(child, options, context));
                }
            }
            _ => {}
        }
    }
    out
}

fn push_text(out: &mut Vec<CNode>, text: &str, options: &CleanOptions<'_>, context: Context) {
    if text.is_empty() {
        return;
    }
    if !options.newlines_to_br || context.in_pre || context.in_table_cell || !text.contains('\n') {
        out.push(CNode::Text(text.to_string()));
        return;
    }
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = normalized.split('\n').peekable();
    while let Some(line) = lines.next() {
        if !line.is_empty() {
            out.push(CNode::Text(line.to_string()));
        }
        if lines.peek().is_some() {
            out.push(CNode::br());
        }
    }
}

fn convert_element(
    element: ElementRef<'_>,
    options: &CleanOptions<'_>,
    context: Context,
) -> Vec<CNode> {
    let raw_name = element.value().name().to_ascii_lowercase();
    if DROP.contains(&raw_name.as_str()) {
        return Vec::new();
    }

    let Some(name) = output_name(&raw_name) else {
        return convert_children(element, options, context);
    };

    match name {
        "br" => return vec![CNode::br()],
        "img" => return convert_image(element, options).into_iter().collect(),
        "hr" => {
            return vec![CNode::Element {
                name: "hr",
                attrs: Vec::new(),
                children: Vec::new(),
            }];
        }
        _ => {}
    }

    let child_context = Context {
        in_pre: context.in_pre || name == "pre",
        in_table_cell: context.in_table_cell || matches!(name, "td" | "th"),
    };
    let mut children = convert_children(element, options, child_context);

    // 表のセルの中の段落は TextView が描けないため、改行に置き換える。
    if context.in_table_cell
        && matches!(
            name,
            "p" | "div" | "section" | "article" | "header" | "footer" | "li"
        )
    {
        children.push(CNode::br());
        return children;
    }

    let mut attrs = Vec::new();
    match name {
        "a" => {
            let href = element
                .attr("href")
                .and_then(|href| resolve_url(href, options.base_url, LinkKind::Anchor));
            match href {
                Some(href) => attrs.push(("href", href)),
                None => return children,
            }
            if let Some(title) = element
                .attr("title")
                .filter(|title| !title.trim().is_empty())
            {
                attrs.push(("title", title.trim().to_string()));
            }
        }
        "td" | "th" => {
            for attr in ["colspan", "rowspan"] {
                if let Some(value) = element
                    .attr(attr)
                    .filter(|value| value.parse::<u32>().is_ok_and(|n| n > 1))
                {
                    attrs.push((attr, value.to_string()));
                }
            }
        }
        "mark" => {
            if let Some(color) = element.attr("color") {
                attrs.push(("color", color.to_string()));
            }
        }
        _ => {}
    }

    vec![CNode::Element {
        name,
        attrs,
        children,
    }]
}

/// 遅延読み込みで本来の URL を持つ属性。
const LAZY_SRC: &[&str] = &[
    "data-lazy-src",
    "data-src",
    "data-original",
    "data-lazy",
    "data-echo",
];

fn convert_image(element: ElementRef<'_>, options: &CleanOptions<'_>) -> Option<CNode> {
    let lazy = LAZY_SRC
        .iter()
        .find_map(|attr| element.attr(attr))
        .filter(|src| !src.trim().is_empty());
    let srcset = || {
        element
            .attr("data-srcset")
            .or_else(|| element.attr("srcset"))
            .and_then(|srcset| srcset.split(',').next())
            .and_then(|candidate| candidate.split_whitespace().next())
    };
    let src = lazy
        .or_else(|| element.attr("src").filter(|src| !src.trim().is_empty()))
        .or_else(srcset)?;
    // 問い合わせは外さない。署名付きの URL は外すと取れなくなる。取得のたびに変わる問い合わせは
    // 取得の定義の `url_identity.images` で宣言し、保存のときに画像の参照にする。
    let src = resolve_url(src, options.base_url, LinkKind::Image)?;

    let mut attrs = vec![("src", src)];
    if let Some(alt) = element.attr("alt").filter(|alt| !alt.trim().is_empty()) {
        attrs.push(("alt", alt.trim().to_string()));
    }
    for attr in ["width", "height"] {
        if let Some(value) = element
            .attr(attr)
            .map(str::trim)
            .filter(|value| is_length(value))
        {
            attrs.push((attr, value.to_string()));
        }
    }
    Some(CNode::Element {
        name: "img",
        attrs,
        children: Vec::new(),
    })
}

fn is_length(value: &str) -> bool {
    let number = value.trim_end_matches("px").trim_end_matches('%');
    !number.is_empty() && number.parse::<f32>().is_ok_and(|n| n > 0.0)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LinkKind {
    Anchor,
    Image,
}

/// URL を絶対 URL にする。使えないスキームは `None`。
fn resolve_url(raw: &str, base: Option<&Url>, kind: LinkKind) -> Option<String> {
    let raw = raw.trim();
    if raw.is_empty() || raw.starts_with('#') {
        return None;
    }
    if kind == LinkKind::Image && raw.starts_with("data:image/") {
        return Some(raw.to_string());
    }
    let url = match Url::parse(raw) {
        Ok(url) => url,
        Err(url::ParseError::RelativeUrlWithoutBase) => base?.join(raw).ok()?,
        Err(_) => return None,
    };
    let allowed = match kind {
        LinkKind::Anchor => matches!(url.scheme(), "http" | "https" | "mailto"),
        LinkKind::Image => matches!(url.scheme(), "http" | "https" | "file"),
    };
    allowed.then(|| url.to_string())
}

/// 空の要素と余分な改行を取り除く。`block` は、この並びがブロック要素の中身か。
fn tidy_children(children: &mut Vec<CNode>, block: bool) {
    for child in children.iter_mut() {
        if let CNode::Element {
            name,
            children: grand,
            ..
        } = child
        {
            let is_block = BLOCK_OUTPUT.contains(name);
            tidy_children(grand, is_block);
        }
    }

    // 中身のない要素を消す。表の構造と画像・改行・罫線は残す。
    children.retain(|child| match child {
        CNode::Element { name, .. }
            if matches!(*name, "br" | "img" | "hr" | "td" | "th" | "tr") =>
        {
            true
        }
        CNode::Element { .. } => child.has_content(),
        CNode::Text(_) => true,
    });

    // ブロック要素の前後にある改行と空白を消す。
    let mut index = 0;
    while index < children.len() {
        if children[index].is_blank_text() {
            let touches_block = children.get(index + 1).is_some_and(CNode::is_block)
                || index
                    .checked_sub(1)
                    .and_then(|prev| children.get(prev))
                    .is_some_and(CNode::is_block);
            if touches_block {
                children.remove(index);
                continue;
            }
        }
        if children[index].is_br() {
            let before_block =
                next_meaningful(children, index + 1).is_some_and(|next| children[next].is_block());
            let after_block =
                previous_meaningful(children, index).is_some_and(|prev| children[prev].is_block());
            if before_block || after_block {
                children.remove(index);
                continue;
            }
        }
        index += 1;
    }

    // 連続した改行は 2 つまでにする (空行 1 行分)。
    let mut run = 0;
    let mut index = 0;
    while index < children.len() {
        if children[index].is_br() {
            run += 1;
            if run > 2 {
                children.remove(index);
                continue;
            }
        } else if !children[index].is_blank_text() {
            run = 0;
        }
        index += 1;
    }

    // ブロックの先頭と末尾の改行・空白を消す。
    if block {
        while children
            .first()
            .is_some_and(|child| child.is_br() || child.is_blank_text())
        {
            children.remove(0);
        }
        while children
            .last()
            .is_some_and(|child| child.is_br() || child.is_blank_text())
        {
            children.pop();
        }
    }
}

fn next_meaningful(children: &[CNode], from: usize) -> Option<usize> {
    (from..children.len()).find(|&index| !children[index].is_blank_text())
}

fn previous_meaningful(children: &[CNode], before: usize) -> Option<usize> {
    (0..before)
        .rev()
        .find(|&index| !children[index].is_blank_text())
}

fn serialize(node: &CNode, out: &mut String) {
    match node {
        CNode::Text(text) => out.push_str(&escape_html(text)),
        CNode::Element {
            name,
            attrs,
            children,
        } => {
            out.push('<');
            out.push_str(name);
            for (key, value) in attrs {
                out.push(' ');
                out.push_str(key);
                out.push_str("=\"");
                out.push_str(&escape_html(value));
                out.push('"');
            }
            out.push('>');
            if matches!(*name, "br" | "img" | "hr") {
                return;
            }
            for child in children {
                serialize(child, out);
            }
            out.push_str("</");
            out.push_str(name);
            out.push('>');
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn tidy(html: &str) -> String {
        let base = Url::parse("https://example.com/news/1/").unwrap();
        clean_html(
            html,
            CleanOptions {
                base_url: Some(&base),
                newlines_to_br: false,
                tidy: true,
            },
        )
    }

    #[test]
    fn removes_scripts_comments_and_attributes() {
        assert_eq!(
            tidy(
                r#"<div class="x" style="color:red" onclick="evil()"><script>alert(1)</script><!-- c --><p id="a">本文<font color="red">強調</font></p><style>p{}</style></div>"#
            ),
            "<div><p>本文強調</p></div>"
        );
    }

    #[test]
    fn resolves_urls_and_lazy_images() {
        assert_eq!(
            tidy(
                r#"<p><a href="../2/">次</a> <a href="javascript:void(0)">x</a><img src="data:image/gif;base64,R0lGOD" data-lazy-src="/img/a.png" srcset="b.png 2x" width="300" height="auto"></p>"#
            ),
            r#"<p><a href="https://example.com/news/2/">次</a> x<img src="https://example.com/img/a.png" width="300"></p>"#
        );
        assert_eq!(
            tidy(r#"<img srcset="/s.png 1x, /l.png 2x">"#),
            r#"<img src="https://example.com/s.png">"#
        );
        // 署名付きの URL は問い合わせを外すと取れないので、そのまま残す。
        assert_eq!(
            tidy(r#"<img src="https://cdn.example.com/a.png?s=Ab-1_x&amp;t=1793926710">"#),
            r#"<img src="https://cdn.example.com/a.png?s=Ab-1_x&amp;t=1793926710">"#
        );
        assert_eq!(tidy(r#"<img alt="no src">"#), "");
    }

    #[test]
    fn collapses_breaks_and_removes_empty_elements() {
        assert_eq!(
            tidy(
                "<div><br><br>一行目<br><br><br><br>二行目<br><p><br></p><span> </span><p>段落</p><br></div>"
            ),
            "<div>一行目<br><br>二行目<p>段落</p></div>"
        );
        assert_eq!(
            tidy("<p>　</p><div><img src=\"/a.png\"></div>"),
            r#"<div><img src="https://example.com/a.png"></div>"#
        );
    }

    #[test]
    fn flattens_paragraphs_inside_table_cells() {
        assert_eq!(
            tidy("<table><tr><td><p>A</p><p>B</p></td><td colspan=\"2\">C</td></tr></table>"),
            r#"<table><tbody><tr><td>A<br>B</td><td colspan="2">C</td></tr></tbody></table>"#
        );
    }

    #[test]
    fn converts_newlines_when_asked() {
        let html = clean_html(
            "告知\n\n本文&amp;詳細\n<table><tr><td>a\nb</td></tr></table>",
            CleanOptions {
                base_url: None,
                newlines_to_br: true,
                tidy: true,
            },
        );
        assert_eq!(
            html,
            "告知<br><br>本文&amp;詳細<table><tbody><tr><td>a\nb</td></tr></tbody></table>"
        );
    }

    #[test]
    fn untidy_mode_keeps_structure() {
        let html = clean_html(
            "<p><br></p><script>x</script><p>a</p>",
            CleanOptions {
                base_url: None,
                newlines_to_br: false,
                tidy: false,
            },
        );
        assert_eq!(html, "<p><br></p><p>a</p>");
    }
}
