//! 解析用の正規化と、正規化した位置から原文の位置へ戻す対応。
//!
//! 字句の規則 (日時の正規表現・語の一覧) は正規化した文字列に当てる。根拠 (`Evidence.text`) は
//! 正規化する前の原文のまま切り出すので、正規化した書記素ごとに原文の範囲を覚えておく。
//!
//! 正規化は書記素ごとの NFKC に、次を足したもの。
//! - 波線の異体 (`〜` `∼` `～`) は `~` にそろえる (範囲の区切りとして読む)。
//! - 空白の類 (全角空白・タブ・改行・NBSP) は半角空白 1 つにする。幅のない字は捨てる。
//! - 丸数字 (`①` など) は NFKC で数字になり、隣の数字とつながって日付に見えるので、そのまま残す。

use std::ops::Range;

use unicode_normalization::UnicodeNormalization;
use unicode_segmentation::UnicodeSegmentation;

/// 正規化した文字列と、原文への対応。
#[derive(Debug, Clone, Default)]
pub(crate) struct Normalized {
    pub text: String,
    /// 正規化した書記素ごとの (正規化後の開始位置, 原文の範囲)。開始位置の昇順。
    spans: Vec<(usize, Range<usize>)>,
}

impl Normalized {
    /// `raw` を正規化する。`offset` は `raw` が原文のどこから始まるか。
    pub fn new(raw: &str, offset: usize) -> Self {
        let mut normalized = Normalized::default();
        normalized.push(raw, offset);
        normalized
    }

    /// `raw` (原文の `offset` から) を正規化して後ろに足す。
    pub fn push(&mut self, raw: &str, offset: usize) {
        for (at, grapheme) in raw.grapheme_indices(true) {
            let start = self.text.len();
            push_grapheme(&mut self.text, grapheme);
            if self.text.len() > start {
                let origin = offset + at..offset + at + grapheme.len();
                self.spans.push((start, origin));
            }
        }
    }

    /// 正規化後の範囲を、原文の範囲に戻す。
    pub fn to_raw(&self, range: Range<usize>) -> Range<usize> {
        if self.spans.is_empty() {
            return 0..0;
        }
        let first = self.span_at(range.start);
        let last = self.span_at(range.end.saturating_sub(1).max(range.start));
        let start = self.spans[first].1.start;
        let end = self.spans[last].1.end.max(start);
        start..end
    }

    /// 正規化後の範囲の原文 (`raw` は対応の基準の原文全体)。
    pub fn raw_slice<'a>(&self, raw: &'a str, range: Range<usize>) -> &'a str {
        let range = self.to_raw(range);
        raw.get(range).unwrap_or("")
    }

    fn span_at(&self, position: usize) -> usize {
        match self
            .spans
            .binary_search_by(|(start, _)| start.cmp(&position))
        {
            Ok(index) => index,
            Err(index) => index.saturating_sub(1),
        }
    }
}

/// 1 つの書記素を正規化して足す。
fn push_grapheme(out: &mut String, grapheme: &str) {
    let mut chars = grapheme.chars();
    if let (Some(ch), None) = (chars.next(), chars.clone().next())
        && keeps_as_is(ch)
    {
        out.push(ch);
        return;
    }
    for ch in grapheme.nfkc() {
        match ch {
            '〜' | '∼' | '～' | '〰' => out.push('~'),
            '\u{200b}'..='\u{200d}' | '\u{2060}' | '\u{feff}' => {}
            ch if ch.is_whitespace() => {
                if !out.ends_with(' ') {
                    out.push(' ');
                }
            }
            ch => out.push(ch),
        }
    }
}

/// NFKC をかけずに残す字 (丸数字・括弧つき数字など。数字にすると隣の数字とつながる)。
fn keeps_as_is(ch: char) -> bool {
    matches!(ch,
        '\u{2460}'..='\u{24ff}'   // ① ⑴ ⒈ ⓐ など
        | '\u{2776}'..='\u{2793}' // ❶ など
        | '\u{3251}'..='\u{325f}' // ㉑ など
        | '\u{32b1}'..='\u{32bf}' // ㊱ など
        | '\u{3280}'..='\u{32b0}' // ㊀ ㊊ など (曜日の丸文字を含む)
    )
}

/// 正規化した文字列だけがほしいとき。
pub(crate) fn normalize(raw: &str) -> String {
    Normalized::new(raw, 0).text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_width_and_waves() {
        assert_eq!(
            normalize("２０２６年１１月１２日（木）１４：００～１７：００"),
            "2026年11月12日(木)14:00~17:00"
        );
        assert_eq!(normalize("14:00〜17:00　終了予定"), "14:00~17:00 終了予定");
        assert_eq!(normalize("ﾒﾝﾃﾅﾝｽ"), "メンテナンス");
        assert_eq!(normalize("①2026/10/26"), "①2026/10/26");
    }

    #[test]
    fn maps_back_to_raw_positions() {
        let raw = "（１１月１２日）ﾒﾝﾃﾅﾝｽ";
        let normalized = Normalized::new(raw, 0);
        let found = normalized.text.find("11月12日").unwrap();
        let range = normalized.to_raw(found..found + "11月12日".len());
        assert_eq!(&raw[range], "１１月１２日");
        let found = normalized.text.find("メンテナンス").unwrap();
        let range = normalized.to_raw(found..found + "メンテナンス".len());
        assert_eq!(&raw[range], "ﾒﾝﾃﾅﾝｽ");
    }

    #[test]
    fn joins_pieces_with_offsets() {
        let doc = "前の行\n後ろの行";
        let mut normalized = Normalized::new(&doc[..9], 0);
        normalized.push(&doc[10..], 10);
        assert_eq!(normalized.text, "前の行後ろの行");
        let found = normalized.text.find("行後").unwrap();
        let range = normalized.to_raw(found..found + "行後".len());
        assert_eq!(&doc[range], "行\n後");
    }
}
