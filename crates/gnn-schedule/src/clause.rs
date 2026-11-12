//! 文の意味。hasami の形態素から、限られた構文の形で「何が・どうした・いつ」を読む。
//!
//! 一般的な係り受け解析は作らない。次の段で読む。
//! 1. 形態素を文節に近い塊 (`Chunk`) にまとめる。日時の表記 (`temporal` が拾った範囲) は 1 つの塊にする。
//!    塊は、内容語 (名詞の並び・述語の語幹) と、後ろに付く助詞・助動詞を持つ。述語は
//!    `実施させていただきます` `行っておりました` のような連なりを 1 つの塊にする。
//! 2. 塊の係り先を、日本語の後ろ係りの性質で近似する。
//!    - `の` の付いた塊・助詞のない名詞・連体形で終わる述語は、直後の名詞に係る (修飾節)。
//!    - `は` `について` など主題の塊は、文の最後の述語に係る。
//!    - ほかの助詞の付いた塊は、右で最も近い述語に係る。
//! 3. 述語ごとに節 (`Clause`) を作る。述語の意味 (`Action`)、肯定・否定、予定・推量・条件・完了、
//!    主節・修飾節・従属節を決め、係っている塊 (日時・対象) を集める。
//!
//! 「日時から前向きに最初の述語を探す」読み方はしない。日時は、係り先の述語の節に入り、その節が
//! 修飾する名詞 (`予定していたメンテナンス`) や、節の対象 (`販売が終了する`) と組で読む。

use std::ops::Range;

use hasami::{Analyzer, Token};

use crate::lexicon::{self, Action};
use crate::temporal::Temporal;

/// 塊の種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ChunkKind {
    Noun,
    Temporal,
    Pred,
    Other,
}

/// 塊に付く助詞の意味。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Marker {
    None,
    Ga,
    Wo,
    Ni,
    De,
    Nite,
    Kara,
    Yori,
    Made,
    Madeni,
    He,
    To,
    No,
    Wa,
    Mo,
    /// について・につきまして・に関して・におきまして・では。
    Topic,
    /// に伴い・により・によって。
    Cause,
    /// に関する・についての (名詞に係る)。
    TopicAdn,
    /// に伴う・による (名詞に係る)。
    CauseAdn,
    /// をもちまして・をもって (終わりの時)。
    Upto,
    Other,
}

impl Marker {
    fn is_topic(self) -> bool {
        matches!(self, Marker::Wa | Marker::Topic | Marker::Mo)
    }

    fn is_adnominal(self) -> bool {
        matches!(self, Marker::No | Marker::TopicAdn | Marker::CauseAdn)
    }
}

/// 形態素の大まかな種類。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Class {
    Temporal,
    Num,
    Noun,
    Sahen,
    Formal,
    Suffix,
    Prefix,
    Verb,
    VerbAux,
    VerbSuffix,
    Adj,
    Aux,
    Case,
    Conj,
    Bind,
    Adv,
    Para,
    Final,
    Adverb,
    Adnominal,
    Conjunction,
    Interj,
    Period,
    Comma,
    Open,
    Close,
    Symbol,
}

/// 形態素 (日時の表記はまとめて 1 つ)。
#[derive(Debug, Clone)]
pub(crate) struct Tok {
    pub class: Class,
    pub surface: String,
    pub base: String,
    pub form: String,
    pub span: Range<usize>,
    pub negation: bool,
    pub temporal: Option<usize>,
}

/// 文節に近い塊。
#[derive(Debug, Clone)]
pub(crate) struct Chunk {
    /// 単位の文字列での範囲。
    pub span: Range<usize>,
    pub kind: ChunkKind,
    /// 内容語の表層をつないだもの。
    pub text: String,
    /// `text` から括弧 (`「」` `『』`) の中を除いたもの。括弧の中は名前なので、意味の判定には使わない。
    pub bare: String,
    /// 述語の中心の語 (サ変名詞・動詞の原形・コピュラの名詞)。
    pub lemma: String,
    /// 内容語の後ろの助詞の表層。
    pub particles: Vec<String>,
    /// 述語の連なりの形態素 (語幹の後ろ)。
    pub chain: Vec<Tok>,
    pub comma: bool,
    pub ends: bool,
    pub temporal: Option<usize>,
    /// 名詞の後ろにコピュラ (`です` `だ` `となる`) が付いて述語になった。
    pub copula: bool,
    /// `に + つく/伴う/関する` などで助詞にまとめた語。
    pub compound: Option<String>,
    /// 内容語の最後の形態素の種類。
    pub last_content: Class,
    /// 括弧で囲んだ名前を含む。
    pub quoted: bool,
    /// 内容語が動詞・形容詞のとき、その活用形 (`なる` の 基本形)。
    pub head_form: String,
}

impl Chunk {
    fn new(kind: ChunkKind, tok: &Tok) -> Self {
        let text = if tok.class == Class::Temporal || tok.class == Class::Open {
            String::new()
        } else {
            tok.surface.clone()
        };
        Chunk {
            span: tok.span.clone(),
            kind,
            bare: text.clone(),
            text,
            lemma: tok.base.clone(),
            particles: Vec::new(),
            chain: Vec::new(),
            comma: false,
            ends: false,
            temporal: tok.temporal,
            copula: false,
            compound: None,
            last_content: tok.class,
            quoted: false,
            head_form: tok.form.clone(),
        }
    }

    pub fn is_pred(&self) -> bool {
        self.kind == ChunkKind::Pred
    }

    pub fn is_nominal(&self) -> bool {
        matches!(self.kind, ChunkKind::Noun | ChunkKind::Temporal)
    }

    /// 助詞の意味。
    pub fn marker(&self) -> Marker {
        if let Some(compound) = &self.compound {
            return match compound.as_str() {
                "つく" | "関する" | "おく" | "対する" | "際する" | "関わる" => {
                    if self.adnominal_compound() {
                        Marker::TopicAdn
                    } else {
                        Marker::Topic
                    }
                }
                _ => {
                    if self.adnominal_compound() {
                        Marker::CauseAdn
                    } else {
                        Marker::Cause
                    }
                }
            };
        }
        let joined: String = self.particles.concat();
        match joined.as_str() {
            "" => Marker::None,
            "が" => Marker::Ga,
            "を" => Marker::Wo,
            "に" | "には" | "にも" => Marker::Ni,
            "で" => Marker::De,
            "にて" => Marker::Nite,
            "から" => Marker::Kara,
            "より" | "よりの" => Marker::Yori,
            "まで" => Marker::Made,
            "までに" | "までには" => Marker::Madeni,
            "へ" | "へと" => Marker::He,
            "と" | "とは" => Marker::To,
            "の" | "からの" | "までの" | "での" | "への" | "との" | "にての" | "より後の" => {
                Marker::No
            }
            "は" | "が、" => Marker::Wa,
            "も" => Marker::Mo,
            "について" | "におきまして" | "において" | "につき" | "に関して" | "に関しまして"
            | "につきまして" | "にあたり" | "にあたって" => Marker::Topic,
            "に関する" | "についての" | "に関しての" => Marker::TopicAdn,
            "に伴い" | "に伴いまして" | "により" | "によって" | "によりまして" => {
                Marker::Cause
            }
            "に伴う" | "による" | "に伴った" => Marker::CauseAdn,
            "では" | "にては" | "でも" => Marker::De,
            "をもちまして" | "をもって" | "をもつ" => Marker::Upto,
            other if other.ends_with("は") => Marker::Wa,
            other if other.starts_with("まで") => Marker::Made,
            other if other.starts_with("から") => Marker::Kara,
            other if other.starts_with("より") => Marker::Yori,
            _ => Marker::Other,
        }
    }

    fn adnominal_compound(&self) -> bool {
        self.chain
            .last()
            .is_some_and(|tok| matches!(tok.form.as_str(), "基本形" | "連体形" | "体言接続"))
            && !self
                .chain
                .iter()
                .any(|tok| tok.class == Class::Conj || tok.base == "ます")
    }

    /// 否定を含むか (述語の連なりの中)。
    pub fn negated(&self) -> bool {
        self.chain.iter().any(|tok| tok.negation)
    }

    /// 過去 (`た`、`たら` は除く)。
    pub fn past(&self) -> bool {
        self.chain
            .iter()
            .any(|tok| tok.class == Class::Aux && tok.base == "た" && tok.form != "仮定形")
    }

    /// 進行・結果の状態 (`ている` `ております` `中です`)。
    pub fn progressive(&self) -> bool {
        let mut after_te = false;
        for tok in &self.chain {
            if tok.class == Class::Conj && matches!(tok.base.as_str(), "て" | "で") {
                after_te = true;
                continue;
            }
            if after_te
                && tok.class == Class::VerbAux
                && matches!(tok.base.as_str(), "いる" | "おる" | "おる。")
            {
                return true;
            }
            if tok.class != Class::Aux {
                after_te = false;
            }
        }
        self.copula && self.text.ends_with('中')
    }

    /// 条件 (`たら` `ば` `なら` `ても`)。
    pub fn conditional(&self) -> bool {
        self.chain.iter().any(|tok| {
            (tok.class == Class::Aux && tok.form == "仮定形")
                || (tok.class == Class::Conj && matches!(tok.base.as_str(), "ば" | "ても" | "と"))
        }) || self.particles.iter().any(|p| p == "ば" || p == "ても")
    }

    /// 依頼・勧め (`ください` `お願い`)。
    pub fn request(&self) -> bool {
        self.chain
            .iter()
            .any(|tok| matches!(tok.base.as_str(), "くださる" | "下さる" | "ちょうだい"))
            || self.text.contains("お願い")
    }

    /// 連用形・`て` で終わり、次の述語に続く。
    fn continuative(&self) -> bool {
        if !self.particles.is_empty() {
            return self
                .particles
                .last()
                .is_some_and(|p| p == "て" || p == "で");
        }
        match self.chain.last() {
            Some(tok) if tok.class == Class::Conj => matches!(tok.base.as_str(), "て" | "で"),
            Some(tok) => tok.form.starts_with("連用"),
            None => {
                self.head_form.starts_with("連用")
                    || (self.last_content == Class::Sahen && self.head_form.is_empty())
            }
        }
    }

    /// 連体形で終わる (直後の名詞を修飾できる)。
    fn adnominal_form(&self) -> bool {
        if !self.particles.is_empty() || self.ends {
            return false;
        }
        let form = match self.chain.last() {
            Some(tok) if tok.class == Class::Conj => return false,
            Some(tok) => tok.form.as_str(),
            None => self.head_form.as_str(),
        };
        matches!(form, "基本形" | "連体形" | "体言接続" | "連体化")
    }
}

/// 節の関係。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Relation {
    /// 主節 (文の最後の述語、または主題を受ける述語)。
    Main,
    /// 修飾節 (名詞に係る述語)。係り先の塊。
    Relative(usize),
    /// 条件・時の従属節 (`終了したら` `終了した後に` `終了次第`)。
    Condition,
    /// 理由の従属節 (`終了するため`)。
    Cause,
    /// 連用・て形で次の述語に続く。続く述語の塊。
    Coordinate(usize),
}

/// 節の述べ方。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mood {
    /// 言い切り。
    Assert,
    /// 予定・見込み (`予定` `見込み` `予想`)。
    Plan,
    /// 条件・時 (`たら` `後に` `次第`)。
    Condition,
    /// 可能性 (`場合があります` `可能性がございます`)。
    Possibility,
    /// 依頼 (`ください`)。
    Request,
}

/// 時制・相。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Tense {
    Present,
    Past,
    Progressive,
    PastProgressive,
}

/// 節。
#[derive(Debug, Clone)]
pub(crate) struct Clause {
    /// 述語の塊。
    pub pred: usize,
    pub action: Option<Action>,
    pub negated: bool,
    pub mood: Mood,
    pub tense: Tense,
    pub relation: Relation,
    /// 係っている塊 (日時・名詞)。
    pub args: Vec<usize>,
}

/// 文の読み。
#[derive(Debug, Clone, Default)]
pub(crate) struct Parse {
    pub chunks: Vec<Chunk>,
    pub heads: Vec<Option<usize>>,
    pub clauses: Vec<Clause>,
}

impl Parse {
    /// 名詞の塊に係る塊 (の・連体形・並び)。
    pub fn modifiers(&self, noun: usize) -> Vec<usize> {
        (0..noun)
            .filter(|&index| self.heads[index] == Some(noun))
            .collect()
    }
}

/// 文を読む。`range` は単位の文字列 `text` の中の文の範囲。`temporals` は単位の日時の表記。
pub(crate) fn parse(
    analyzer: &mut Analyzer,
    text: &str,
    range: Range<usize>,
    temporals: &[Temporal],
) -> Parse {
    let toks = unquote_temporals(tokens(analyzer, text, range, temporals));
    let mut chunks = chunk(&toks);
    merge_compound_particles(&mut chunks);
    let heads = heads(&chunks);
    let clauses = clauses(&chunks, &heads);
    Parse {
        chunks,
        heads,
        clauses,
    }
}

/// 形態素に分け、日時の表記を 1 つにまとめる。
fn tokens(
    analyzer: &mut Analyzer,
    text: &str,
    range: Range<usize>,
    temporals: &[Temporal],
) -> Vec<Tok> {
    let sentence = &text[range.clone()];
    let base = range.start;
    let mut out: Vec<Tok> = Vec::new();
    for token in analyzer.tokenize(sentence) {
        let span = base + token.start..base + token.end;
        if let Some((index, temporal)) = temporals.iter().enumerate().find(|(_, temporal)| {
            !temporal.span.is_empty()
                && span.start >= temporal.span.start
                && span.start < temporal.span.end
        }) {
            if let Some(last) = out.last_mut()
                && last.temporal == Some(index)
            {
                last.span.end = span.end.max(last.span.end);
                last.surface.push_str(&token.surface);
                continue;
            }
            let _ = temporal;
            out.push(Tok {
                class: Class::Temporal,
                surface: token.surface.to_string(),
                base: String::new(),
                form: String::new(),
                span,
                negation: false,
                temporal: Some(index),
            });
            continue;
        }
        out.push(Tok {
            class: class_of(&token),
            surface: token.surface.to_string(),
            base: token.base_form.to_string(),
            form: token.conj_form.to_string(),
            span,
            negation: token.is_negation(),
            temporal: None,
        });
    }
    out
}

/// 日時だけを囲んだ括弧 (`『~2026年10月22日4:59』` `「2026/10/31 23:59」`) は外す。括弧の中が名前なら残す。
fn unquote_temporals(toks: Vec<Tok>) -> Vec<Tok> {
    let mut out = Vec::with_capacity(toks.len());
    let mut index = 0;
    while index < toks.len() {
        let tok = &toks[index];
        if tok.class == Class::Open
            && toks
                .get(index + 1)
                .is_some_and(|next| next.class == Class::Temporal)
            && toks
                .get(index + 2)
                .is_some_and(|close| close.class == Class::Close)
        {
            out.push(toks[index + 1].clone());
            index += 3;
            continue;
        }
        out.push(tok.clone());
        index += 1;
    }
    out
}

fn class_of(token: &Token) -> Class {
    let mut fields = token.pos.split(',');
    let pos1 = fields.next().unwrap_or("");
    let pos2 = fields.next().unwrap_or("");
    match (pos1, pos2) {
        ("名詞", "数") => Class::Num,
        ("名詞", "接尾") => Class::Suffix,
        ("名詞", "非自立") => Class::Formal,
        ("名詞", "サ変接続") => {
            if token.surface.chars().all(|ch| !ch.is_alphanumeric()) {
                symbol_class(token)
            } else {
                Class::Sahen
            }
        }
        ("名詞", _) => {
            if !token.is_known && token.surface.chars().all(|ch| !ch.is_alphanumeric()) {
                symbol_class(token)
            } else {
                Class::Noun
            }
        }
        ("接頭詞", _) => Class::Prefix,
        ("動詞", "自立") => Class::Verb,
        ("動詞", "非自立") => Class::VerbAux,
        ("動詞", "接尾") => Class::VerbSuffix,
        ("動詞", _) => Class::Verb,
        ("形容詞", _) => Class::Adj,
        ("助動詞", _) => Class::Aux,
        ("助詞", "格助詞") | ("助詞", "連体化") | ("助詞", "副詞化") => Class::Case,
        ("助詞", "接続助詞") => Class::Conj,
        ("助詞", "係助詞") => Class::Bind,
        ("助詞", "並立助詞") => Class::Para,
        ("助詞", "終助詞") => Class::Final,
        ("助詞", _) => Class::Adv,
        ("副詞", _) => Class::Adverb,
        ("連体詞", _) => Class::Adnominal,
        ("接続詞", _) => Class::Conjunction,
        ("感動詞", _) | ("フィラー", _) => Class::Interj,
        _ => symbol_class(token),
    }
}

fn symbol_class(token: &Token) -> Class {
    match token.coarse_pos() {
        hasami::CoarsePos::Period => Class::Period,
        hasami::CoarsePos::Comma => Class::Comma,
        hasami::CoarsePos::OpenBracket => Class::Open,
        hasami::CoarsePos::CloseBracket => Class::Close,
        _ => Class::Symbol,
    }
}

fn chunk(toks: &[Tok]) -> Vec<Chunk> {
    let mut out: Vec<Chunk> = Vec::new();
    let mut current: Option<Chunk> = None;
    let mut quote = 0usize;
    let close = |current: &mut Option<Chunk>, out: &mut Vec<Chunk>| {
        if let Some(chunk) = current.take() {
            out.push(chunk);
        }
    };
    for tok in toks {
        if quote > 0 {
            if let Some(chunk) = current.as_mut() {
                chunk.text.push_str(&tok.surface);
                chunk.span.end = tok.span.end;
                chunk.quoted = true;
            }
            match tok.class {
                Class::Open => quote += 1,
                Class::Close => quote -= 1,
                _ => {}
            }
            continue;
        }
        match tok.class {
            Class::Temporal => {
                close(&mut current, &mut out);
                current = Some(Chunk::new(ChunkKind::Temporal, tok));
            }
            Class::Open => {
                let name_bracket = matches!(tok.surface.as_str(), "「" | "『" | "【" | "\"" | "“");
                if !name_bracket {
                    // `(` `（` は塊の切れ目 (読点と同じく、前の名詞を後ろの名詞に係らせない)。
                    if let Some(chunk) = current.as_mut() {
                        chunk.comma = true;
                    }
                    close(&mut current, &mut out);
                    continue;
                }
                let extend = current.as_ref().is_some_and(|chunk| {
                    chunk.kind == ChunkKind::Noun && chunk.particles.is_empty() && !chunk.comma
                });
                if !extend {
                    close(&mut current, &mut out);
                    let mut chunk = Chunk::new(ChunkKind::Noun, tok);
                    chunk.text = tok.surface.clone();
                    chunk.last_content = Class::Noun;
                    chunk.quoted = true;
                    current = Some(chunk);
                } else if let Some(chunk) = current.as_mut() {
                    chunk.text.push_str(&tok.surface);
                    chunk.span.end = tok.span.end;
                    chunk.quoted = true;
                }
                quote = 1;
            }
            Class::Close => {}
            Class::Num | Class::Noun | Class::Sahen | Class::Prefix | Class::Formal => {
                // 形式名詞 (`ため` `こと`) の後ろの名詞は別の塊 (`遅れているためメンテナンスを`)。
                let extend = current.as_ref().is_some_and(|chunk| {
                    chunk.kind == ChunkKind::Noun
                        && chunk.particles.is_empty()
                        && chunk.chain.is_empty()
                        && !chunk.comma
                        && !(chunk.last_content == Class::Formal
                            && matches!(tok.class, Class::Noun | Class::Sahen))
                });
                if extend {
                    let chunk = current.as_mut().unwrap();
                    chunk.text.push_str(&tok.surface);
                    chunk.bare.push_str(&tok.surface);
                    chunk.span.end = tok.span.end;
                    chunk.lemma = tok.base.clone();
                    chunk.last_content = tok.class;
                } else {
                    close(&mut current, &mut out);
                    current = Some(Chunk::new(ChunkKind::Noun, tok));
                }
            }
            Class::Suffix => {
                let extend = current.as_ref().is_some_and(|chunk| {
                    chunk.particles.is_empty() && chunk.chain.is_empty() && !chunk.comma
                });
                if extend {
                    let chunk = current.as_mut().unwrap();
                    chunk.text.push_str(&tok.surface);
                    chunk.bare.push_str(&tok.surface);
                    if chunk.kind != ChunkKind::Temporal {
                        chunk.lemma = tok.base.clone();
                    }
                    chunk.span.end = tok.span.end;
                    chunk.last_content = Class::Suffix;
                } else {
                    close(&mut current, &mut out);
                    current = Some(Chunk::new(ChunkKind::Noun, tok));
                }
            }
            Class::Verb => {
                let light_on_noun = current.as_ref().is_some_and(|chunk| {
                    chunk.kind == ChunkKind::Noun
                        && chunk.particles.is_empty()
                        && chunk.chain.is_empty()
                        && !chunk.comma
                        && matches!(chunk.last_content, Class::Sahen | Class::Noun)
                        && lexicon::is_light_verb(&tok.base)
                });
                if light_on_noun {
                    let chunk = current.as_mut().unwrap();
                    chunk.kind = ChunkKind::Pred;
                    chunk.chain.push(tok.clone());
                    chunk.span.end = tok.span.end;
                } else {
                    close(&mut current, &mut out);
                    let mut chunk = Chunk::new(ChunkKind::Pred, tok);
                    chunk.text = tok.surface.clone();
                    chunk.bare = tok.surface.clone();
                    chunk.lemma = tok.base.clone();
                    current = Some(chunk);
                }
            }
            Class::VerbAux | Class::VerbSuffix => match current.as_mut() {
                Some(chunk) if chunk.particles.is_empty() || chunk.is_pred() => {
                    if chunk.kind != ChunkKind::Pred {
                        chunk.kind = ChunkKind::Pred;
                    }
                    if !chunk.particles.is_empty() {
                        // `て` の後ろの補助動詞 (`ております`)。助詞を連なりに戻す。
                        for particle in std::mem::take(&mut chunk.particles) {
                            chunk.chain.push(Tok {
                                class: Class::Conj,
                                base: particle.clone(),
                                surface: particle,
                                form: String::new(),
                                span: tok.span.clone(),
                                negation: false,
                                temporal: None,
                            });
                        }
                    }
                    chunk.chain.push(tok.clone());
                    chunk.span.end = tok.span.end;
                }
                _ => {
                    close(&mut current, &mut out);
                    let mut chunk = Chunk::new(ChunkKind::Pred, tok);
                    chunk.text = tok.surface.clone();
                    current = Some(chunk);
                }
            },
            Class::Adj => {
                let negative_exist = matches!(tok.base.as_str(), "ない" | "無い");
                match current.as_mut() {
                    Some(chunk) if chunk.is_pred() && chunk.particles.is_empty() => {
                        chunk.chain.push(tok.clone());
                        chunk.span.end = tok.span.end;
                    }
                    Some(chunk) if negative_exist && chunk.is_nominal() => {
                        // `未定ではない` `予定はない` は名詞の述語の否定。
                        chunk.kind = ChunkKind::Pred;
                        chunk.copula = true;
                        chunk.chain.push(tok.clone());
                        chunk.span.end = tok.span.end;
                    }
                    _ => {
                        close(&mut current, &mut out);
                        let mut chunk = Chunk::new(ChunkKind::Pred, tok);
                        chunk.text = tok.surface.clone();
                        current = Some(chunk);
                    }
                }
            }
            Class::Aux => {
                if let Some(chunk) = current.as_mut() {
                    let copula = matches!(tok.base.as_str(), "です" | "だ" | "でござる" | "である");
                    if chunk.is_nominal() && copula && chunk.particles.is_empty() {
                        chunk.kind = ChunkKind::Pred;
                        chunk.copula = true;
                    } else if chunk.is_nominal() && copula && chunk.particles.concat() == "で" {
                        // `未定でございます` の `で` は助詞ではなくコピュラ。
                        chunk.particles.clear();
                        chunk.kind = ChunkKind::Pred;
                        chunk.copula = true;
                    }
                    if !chunk.particles.is_empty() && chunk.is_pred() {
                        for particle in std::mem::take(&mut chunk.particles) {
                            chunk.chain.push(Tok {
                                class: Class::Conj,
                                base: particle.clone(),
                                surface: particle,
                                form: String::new(),
                                span: tok.span.clone(),
                                negation: false,
                                temporal: None,
                            });
                        }
                    }
                    chunk.chain.push(tok.clone());
                    chunk.span.end = tok.span.end;
                }
            }
            Class::Case | Class::Bind | Class::Adv | Class::Para | Class::Final | Class::Conj => {
                if let Some(chunk) = current.as_mut() {
                    if chunk.is_pred() && tok.class == Class::Conj {
                        chunk.chain.push(tok.clone());
                    } else {
                        chunk.particles.push(tok.surface.clone());
                    }
                    chunk.span.end = tok.span.end;
                }
            }
            Class::Comma => {
                if let Some(chunk) = current.as_mut() {
                    chunk.comma = true;
                }
                close(&mut current, &mut out);
            }
            Class::Period => {
                if let Some(chunk) = current.as_mut() {
                    chunk.ends = true;
                } else if let Some(last) = out.last_mut() {
                    last.ends = true;
                }
                close(&mut current, &mut out);
            }
            Class::Symbol => {
                close(&mut current, &mut out);
            }
            Class::Adverb | Class::Adnominal | Class::Conjunction | Class::Interj => {
                close(&mut current, &mut out);
                let mut chunk = Chunk::new(ChunkKind::Other, tok);
                chunk.text = tok.surface.clone();
                current = Some(chunk);
            }
        }
    }
    close(&mut current, &mut out);
    // 文の最後の名詞 (体言止め) は述語として読む (`…を予定。` `メンテナンス実施のお知らせ` は除く)。
    if let Some(last) = out.last_mut()
        && last.kind == ChunkKind::Noun
        && last.particles.is_empty()
        && last.ends
        && (matches!(last.last_content, Class::Sahen) || lexicon::action_of(&last.lemma).is_some())
    {
        last.kind = ChunkKind::Pred;
        last.copula = true;
    }
    out
}

/// `に + つく/伴う/関する/よる` を 1 つの助詞にまとめる。
fn merge_compound_particles(chunks: &mut Vec<Chunk>) {
    const VERBS: &[&str] = &[
        "つく",
        "関する",
        "伴う",
        "よる",
        "対する",
        "際する",
        "おく",
        "関わる",
        "基づく",
    ];
    let mut index = 0;
    while index + 1 < chunks.len() {
        let joinable = chunks[index].is_nominal()
            && chunks[index].particles.last().is_some_and(|p| p == "に")
            && !chunks[index].comma
            && chunks[index + 1].is_pred()
            && VERBS.contains(&chunks[index + 1].lemma.as_str());
        if joinable {
            let next = chunks.remove(index + 1);
            let chunk = &mut chunks[index];
            chunk.particles.pop();
            chunk.compound = Some(next.lemma.clone());
            chunk.chain = next.chain;
            chunk.chain.insert(
                0,
                Tok {
                    class: Class::Verb,
                    surface: next.text.clone(),
                    base: next.lemma.clone(),
                    form: String::new(),
                    span: next.span.clone(),
                    negation: false,
                    temporal: None,
                },
            );
            chunk.particles.extend(next.particles);
            chunk.comma = next.comma;
            chunk.ends = next.ends;
            chunk.span.end = next.span.end;
        }
        index += 1;
    }
}

/// 塊の係り先。
fn heads(chunks: &[Chunk]) -> Vec<Option<usize>> {
    let count = chunks.len();
    let mut heads = vec![None; count];
    let last_pred = (0..count).rev().find(|&index| chunks[index].is_pred());
    for index in 0..count {
        let chunk = &chunks[index];
        if chunk.ends || index + 1 == count {
            continue;
        }
        let next = &chunks[index + 1];
        let next_pred = (index + 1..count).find(|&j| chunks[j].is_pred());
        let marker = chunk.marker();
        // 直後の名詞に係る: `の` の付いた塊、助詞のない名詞の並び、連体形で終わる述語 (修飾節)。
        let adnominal = (marker.is_adnominal() && next.is_nominal())
            || (chunk.is_nominal()
                && marker == Marker::None
                && !chunk.comma
                && next.kind == ChunkKind::Noun)
            || (chunk.is_pred()
                && chunk.adnominal_form()
                && next.kind == ChunkKind::Noun
                && (!chunk.comma || chunk.past()));
        let head = if adnominal {
            Some(index + 1)
        } else if chunk.is_pred() && chunk.compound.is_none() {
            // 述語は、次の述語 (連用・て形) か文の最後の述語に係る。
            next_pred
        } else if marker.is_topic() {
            // 主題は文の最後の述語に係る。後ろに別の主題があれば (`Aは終了しましたが、Bは解消していません`)、
            // その主題より前の最後の述語に係る。
            let next_topic = (index + 1..count).find(|&j| {
                chunks[j].is_nominal() && matches!(chunks[j].marker(), Marker::Wa | Marker::Topic)
            });
            match next_topic {
                Some(limit) => (index + 1..limit)
                    .rev()
                    .find(|&j| chunks[j].is_pred() && !chunks[j].adnominal_form())
                    .or(last_pred.filter(|&j| j > index))
                    .or(next_pred),
                None => last_pred.filter(|&j| j > index).or(next_pred),
            }
        } else if chunk.comma && chunk.is_nominal() {
            // 読点の後ろの修飾節 (`22:00頃より、…を目的とした措置として`) を越えて係る。
            (index + 1..count)
                .find(|&j| {
                    chunks[j].is_pred()
                        && !(chunks[j].adnominal_form()
                            && j + 1 < count
                            && chunks[j + 1].kind == ChunkKind::Noun)
                })
                .or(next_pred)
        } else {
            next_pred
        };
        heads[index] = head;
    }
    heads
}

/// 時・条件を表す形式名詞 (修飾節が係ると、その節は条件・時になる)。
const CONDITION_NOUNS: &[&str] = &[
    "後", "あと", "際", "時", "とき", "次第", "まで", "前", "間", "うち", "直後", "以降", "瞬間",
    "時点", "ころ", "頃",
];
/// 可能性を表す名詞 (`場合があります` `可能性がございます`)。
const POSSIBILITY_NOUNS: &[&str] = &["場合", "可能性", "恐れ", "おそれ", "こと", "見込み"];

fn clauses(chunks: &[Chunk], heads: &[Option<usize>]) -> Vec<Clause> {
    let mut out = Vec::new();
    for (index, chunk) in chunks.iter().enumerate() {
        if !chunk.is_pred() {
            continue;
        }
        let head = heads[index];
        let relation = match head {
            Some(target) if !chunks[target].is_pred() => Relation::Relative(target),
            Some(target) if chunk.conditional() => {
                let _ = target;
                Relation::Condition
            }
            Some(target) if chunk.continuative() || chunk.comma => Relation::Coordinate(target),
            Some(target) => Relation::Coordinate(target),
            None => Relation::Main,
        };
        let args: Vec<usize> = (0..index)
            .filter(|&arg| heads[arg] == Some(index))
            .collect();
        let action = action_of_pred(chunks, index, &args);
        let mut mood = Mood::Assert;
        let mut relation = relation;
        if chunk.conditional() {
            mood = Mood::Condition;
            relation = Relation::Condition;
        }
        if let Relation::Relative(noun) = relation {
            let noun_chunk = &chunks[noun];
            let noun_text = noun_chunk.text.as_str();
            if POSSIBILITY_NOUNS
                .iter()
                .any(|word| noun_text.ends_with(word))
                && possibility_governor(chunks, heads, noun)
            {
                mood = Mood::Possibility;
            } else if CONDITION_NOUNS.contains(&noun_text) || noun_text == "場合" {
                // `延長となった場合におきましても` の `場合` は条件。
                mood = Mood::Condition;
                relation = Relation::Condition;
            } else if noun_text == "ため" || noun_text == "ので" {
                relation = Relation::Cause;
            }
        }
        if chunk.particles.iter().any(|p| p == "ので" || p == "ため")
            || chunk.chain.iter().any(|tok| tok.base == "ので")
        {
            relation = Relation::Cause;
        }
        if chunk.text.contains("次第") || chunk.lemma == "次第" {
            mood = Mood::Condition;
        }
        if mood == Mood::Assert {
            if chunk.request() {
                mood = Mood::Request;
            } else if is_plan(chunk) {
                mood = Mood::Plan;
            } else if chunk
                .chain
                .iter()
                .any(|tok| tok.surface.starts_with("かもしれ"))
            {
                mood = Mood::Possibility;
            }
        }
        let tense = match (chunk.past(), chunk.progressive()) {
            (true, true) => Tense::PastProgressive,
            (true, false) => Tense::Past,
            (false, true) => Tense::Progressive,
            (false, false) => Tense::Present,
        };
        out.push(Clause {
            pred: index,
            action,
            negated: chunk.negated(),
            mood,
            tense,
            relation,
            args,
        });
    }
    // 連用・て形の節は、続く節の時制と言い切りを受け継ぐ (`終了し、…再開いたしました`)。
    for index in (0..out.len()).rev() {
        if let Relation::Coordinate(next) = out[index].relation
            && let Some(following) = out.iter().find(|clause| clause.pred == next).cloned()
        {
            let clause = &mut out[index];
            if clause.tense == Tense::Present && !chunks[clause.pred].conditional() {
                clause.tense = following.tense;
            }
            if clause.mood == Mood::Assert {
                clause.mood = match following.mood {
                    Mood::Possibility => Mood::Possibility,
                    Mood::Plan => Mood::Plan,
                    _ => Mood::Assert,
                };
            }
            if matches!(following.relation, Relation::Condition) {
                clause.mood = Mood::Condition;
            }
        }
    }
    out
}

/// 名詞 (`場合` など) を受ける述語が `ある` `ござる` か。
fn possibility_governor(chunks: &[Chunk], heads: &[Option<usize>], noun: usize) -> bool {
    let Some(governor) = heads[noun] else {
        return false;
    };
    let governor = &chunks[governor];
    matches!(
        governor.lemma.as_str(),
        "ある" | "ござる" | "あり" | "ござい"
    ) || governor.text.starts_with("ござい")
        || governor.text.starts_with("あり")
}

/// 予定・見込みの述語か。
fn is_plan(chunk: &Chunk) -> bool {
    chunk.text.contains("予定")
        || chunk.text.contains("見込")
        || chunk.text.contains("予想")
        || chunk.lemma == "予定"
        || chunk.chain.iter().any(|tok| tok.base == "予定")
}

/// 述語の意味。軽い動詞 (`いたしません` `行います`) は係っている名詞の動作で読む。
fn action_of_pred(chunks: &[Chunk], index: usize, args: &[usize]) -> Option<Action> {
    let chunk = &chunks[index];
    let text = chunk.text.as_str();
    // サ変名詞 + する / 名詞 + です の語幹 (`実施` `中止` `終了予定` `実施中`)。
    if chunk.copula
        || matches!(
            chunk.last_content,
            Class::Sahen | Class::Noun | Class::Suffix
        )
    {
        let actions = lexicon::actions_in(text);
        if let Some(action) = actions
            .iter()
            .copied()
            .find(|action| *action != Action::Plan)
            .or_else(|| actions.first().copied())
        {
            return Some(action);
        }
        if let Some(action) = lexicon::action_of(&chunk.lemma) {
            return Some(action);
        }
        if let Some(action) = lexicon::action_of(text) {
            return Some(action);
        }
    }
    let lemma = chunk.lemma.as_str();
    if let Some(action) = lexicon::action_of(lemma)
        && !lexicon::is_light_verb(lemma)
    {
        return Some(action);
    }
    if lexicon::is_light_verb(lemma) || lemma == "なる" {
        // 目的語・主題の名詞の動作 (`延長はいたしません` `メンテナンスを行います`)。
        for &arg in args.iter().rev() {
            let arg_chunk = &chunks[arg];
            if !matches!(
                arg_chunk.marker(),
                Marker::Wo | Marker::Wa | Marker::Mo | Marker::Ga | Marker::To
            ) {
                continue;
            }
            if let Some(action) = lexicon::actions_in(&arg_chunk.text)
                .into_iter()
                .find(|action| *action != Action::Plan)
                .or_else(|| lexicon::action_of(&arg_chunk.text))
            {
                return Some(action);
            }
        }
        if matches!(lemma, "行う" | "おこなう") {
            return Some(Action::Execute);
        }
    }
    lexicon::action_of(lemma)
}

pub(crate) mod debug {
    use super::*;

    /// 読みを人が見る形にする (テストの失敗の調査用)。
    pub(crate) fn render(parse: &Parse) -> String {
        let mut out = String::new();
        for (index, chunk) in parse.chunks.iter().enumerate() {
            out.push_str(&format!(
                "[{index}] {:?} {:?} lemma={} particles={:?} marker={:?} head={:?}{}{}\n",
                chunk.kind,
                chunk.text,
                chunk.lemma,
                chunk.particles,
                chunk.marker(),
                parse.heads[index],
                if chunk.comma { " 、" } else { "" },
                if chunk.ends { " 。" } else { "" },
            ));
        }
        for clause in &parse.clauses {
            out.push_str(&format!(
                "clause pred={} action={:?} neg={} mood={:?} tense={:?} rel={:?} args={:?}\n",
                clause.pred,
                clause.action,
                clause.negated,
                clause.mood,
                clause.tense,
                clause.relation,
                clause.args
            ));
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::temporal;

    fn read(text: &str) -> Parse {
        let mut analyzer = crate::dict::analyzer();
        let normalized = crate::normalize::normalize(text);
        let temporals = temporal::scan(&normalized);
        parse(&mut analyzer, &normalized, 0..normalized.len(), &temporals)
    }

    fn main_clause(parse: &Parse) -> &Clause {
        parse
            .clauses
            .iter()
            .find(|clause| clause.relation == Relation::Main)
            .unwrap_or_else(|| panic!("主節がない:\n{}", debug::render(parse)))
    }

    #[test]
    fn reads_relative_clause_and_main_clause() {
        let parse = read("11/12に予定していたメンテナンスを中止します。");
        let main = main_clause(&parse);
        assert_eq!(
            main.action,
            Some(Action::Cancel),
            "{}",
            debug::render(&parse)
        );
        let relative = parse
            .clauses
            .iter()
            .find(|clause| matches!(clause.relation, Relation::Relative(_)))
            .unwrap_or_else(|| panic!("{}", debug::render(&parse)));
        assert_eq!(relative.action, Some(Action::Plan));
        assert!(
            relative
                .args
                .iter()
                .any(|&arg| parse.chunks[arg].kind == ChunkKind::Temporal)
        );
    }

    #[test]
    fn reads_negation_condition_and_possibility() {
        let parse = read("メンテナンスは終了していません。");
        let main = main_clause(&parse);
        assert_eq!(main.action, Some(Action::End));
        assert!(main.negated);
        let parse = read("延長はいたしません。");
        let main = main_clause(&parse);
        assert_eq!(
            main.action,
            Some(Action::Extend),
            "{}",
            debug::render(&parse)
        );
        assert!(main.negated);
        let parse = read("メンテナンスが終了した後に配布します。");
        let end = parse
            .clauses
            .iter()
            .find(|clause| clause.action == Some(Action::End))
            .unwrap();
        assert_eq!(end.mood, Mood::Condition, "{}", debug::render(&parse));
        let parse = read("終了したらお知らせします。");
        let end = parse
            .clauses
            .iter()
            .find(|clause| clause.action == Some(Action::End))
            .unwrap();
        assert_eq!(end.mood, Mood::Condition, "{}", debug::render(&parse));
        let parse = read("状況により中止する場合があります。");
        let cancel = parse
            .clauses
            .iter()
            .find(|clause| clause.action == Some(Action::Cancel))
            .unwrap();
        assert_eq!(cancel.mood, Mood::Possibility, "{}", debug::render(&parse));
    }

    #[test]
    fn reads_predicate_chains() {
        let parse = read(
            "2026年11月4日（水）12:00より実施していた定期メンテナンスは、2026年11月4日（水）16:00に終了いたしました。",
        );
        let main = main_clause(&parse);
        assert_eq!(main.action, Some(Action::End), "{}", debug::render(&parse));
        assert_eq!(main.tense, Tense::Past);
        let relative = parse
            .clauses
            .iter()
            .find(|clause| matches!(clause.relation, Relation::Relative(_)))
            .unwrap();
        assert_eq!(relative.action, Some(Action::Execute));
        assert_eq!(relative.tense, Tense::PastProgressive);
        let parse = read("中止させていただきます。");
        assert_eq!(main_clause(&parse).action, Some(Action::Cancel));
        let parse = read("現在、緊急メンテナンスを実施しております。");
        let main = main_clause(&parse);
        assert_eq!(main.action, Some(Action::Execute));
        assert_eq!(main.tense, Tense::Progressive);
    }
}
