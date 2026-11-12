//! 文の読み (`Reading`)。節ごとに「対象・動作・述べ方・日時」の組にする。
//!
//! 形態素解析で読む [`from_parse`] と、比べるための正規表現だけの読み [`baseline`] がある。
//! どちらも同じ `Reading` を返し、後ろの結び付け (`facts`) は共通。

use std::ops::Range;

use gnn_core::schedule::PeriodKind;

use crate::clause::{ChunkKind, Marker, Mood, Parse, Relation, Tense};
use crate::lexicon::{self, Action, TimeRole};
use crate::temporal::{Temporal, TemporalKind};

/// 読みの対象。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TargetKind {
    /// メンテナンス (`update` はアップデートの語で、停止を伴うかは文脈で決める)。
    Maint { update: bool },
    /// メンテナンスではない対象 (イベント・販売・受け取りなど)。`generic` は「開催」だけで決めた種類。
    Other { kind: PeriodKind, generic: bool },
    /// 対象を書かずに「時」だけを書いた名詞 (`終了は` `終了時間は` `所要時間は`)。
    Role(TimeRole),
    /// 書かれていない。
    None,
}

/// 1 つの節の読み。
#[derive(Debug, Clone)]
pub(crate) struct Reading {
    /// 対象の名詞 (文の中の番号。同じ名詞を指す読みは 1 つの記述にまとめる)。
    pub target: Option<usize>,
    pub target_text: String,
    /// 対象の名詞を修飾する名詞 (`iOS版・Android版のメンテナンス` の `iOS版・Android版`)。対象の名前を拾う。
    pub scope_text: String,
    pub kind: TargetKind,
    /// 対象の「時」の役割 (`メンテナンスの終了予定は` の `終了`)。
    pub role: Option<TimeRole>,
    pub action: Option<Action>,
    pub negated: bool,
    pub mood: Mood,
    pub tense: Tense,
    pub relation: ReadingRelation,
    /// 日時 (単位の日時の番号と、付いた助詞)。
    pub times: Vec<(usize, Marker)>,
    /// `未定` と書かれている。
    pub undecided: bool,
    /// 文の範囲 (単位の文字列)。
    pub span: Range<usize>,
    /// 対象の名詞に付いた修飾 (`実施中の` `予定していた` などの名詞の修飾の語)。
    pub modifier_actions: Vec<Action>,
    /// 対象の名詞を主語・目的語・主題として受ける述語の動作 (修飾節の名詞が、主節で何をされたか)。
    pub governor_action: Option<Action>,
    /// その述語の塊 (文の中の番号)。
    pub governor_pred: Option<usize>,
    /// この読みの述語の塊 (文の中の番号。述語のない名詞句の読みは `None`)。
    pub pred: Option<usize>,
    /// 述語のない名詞句の読み (見出し・題名・`1月5日 14:00~17:00 メンテナンス`)。
    pub nominal: bool,
    /// 対象の名詞が文の最後の名詞 (`〇11月6日に行った臨時メンテナンス` のような名詞句の見出し)。
    pub phrase_head: bool,
    /// 対象の分からない名詞が主語・目的語として書かれている (`審査が完了し` `対応を実施`)。
    /// その節は書かれた名詞について述べているので、見出しや前の文の対象を受け継がない。
    pub foreign_subject: bool,
}

/// 節の関係 (結び付けで使う分だけ)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadingRelation {
    /// 主節・続く節 (言い切る)。
    Main,
    /// 名詞を修飾する節 (`予定していたメンテナンス`)。
    Relative,
    /// 条件・時・理由の従属節。
    Subordinate,
}

/// 名詞句の意味。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NounSense {
    pub kind: TargetKind,
    /// `メンテナンス後` `メンテナンス中` のような、メンテナンスの前後を指す語。
    pub reference: bool,
}

/// 名詞句の意味。後ろの語ほど主 (`メンテナンス延長のお詫び` はお詫び、`補填の受け取り期間` は受け取り)。
pub(crate) fn noun_sense(text: &str) -> NounSense {
    let reference = lexicon::reference_of(text).is_some();
    let maint_at = text.rfind("メンテ").map(|at| at + "メンテ".len());
    let update_at = ["アップデート", "バージョンアップ"]
        .iter()
        .filter_map(|word| text.rfind(word).map(|at| at + word.len()))
        .max();
    let kind = lexicon::period_kind(text);
    let kind_at = kind.and_then(|_| last_kind_word_end(text));
    let maint_end = maint_at.max(update_at);
    let target = match (maint_end, kind_at) {
        (Some(m), Some(k)) if k > m => TargetKind::Other {
            kind: kind.unwrap(),
            generic: lexicon::is_generic_kind(text, kind.unwrap()),
        },
        (Some(_), _) => TargetKind::Maint {
            update: maint_at.is_none(),
        },
        (None, Some(_)) => TargetKind::Other {
            kind: kind.unwrap(),
            generic: lexicon::is_generic_kind(text, kind.unwrap()),
        },
        (None, None) => match lexicon::time_role(text) {
            Some(role) => TargetKind::Role(role),
            None => TargetKind::None,
        },
    };
    NounSense {
        kind: target,
        reference,
    }
}

fn last_kind_word_end(text: &str) -> Option<usize> {
    // `period_kind` と同じ語の、最後の出現の終わり。
    let words = [
        "ガチャ",
        "召喚",
        "跳躍",
        "祈願",
        "調律",
        "スカウト",
        "ピックアップ",
        "変調",
        "イベント",
        "開催",
        "解放",
        "開放",
        "公開",
        "総力戦",
        "討伐",
        "ランキング",
        "レイド",
        "大会",
        "キャンペーン",
        "ミッション",
        "ログインボーナス",
        "ログボ",
        "投票",
        "販売",
        "ショップ",
        "パック",
        "セール",
        "商品",
        "価格",
        "購入",
        "発売",
        "SALE",
        "受け取り",
        "受取",
        "受け取",
        "配布",
        "補填",
        "補てん",
        "補償",
        "お詫び",
        "交換",
        "プレゼント",
        "報酬",
        "シリアルコード",
        "使用期限",
        "有効期限",
        "有効期間",
    ];
    words
        .iter()
        .filter_map(|word| text.rfind(word).map(|at| at + word.len()))
        .max()
}

/// 形態素解析の読みから、節ごとの読みを作る。
pub(crate) fn from_parse(parse: &Parse, span: Range<usize>, text: &str) -> Vec<Reading> {
    let mut out = Vec::new();
    // 述語の連なり (`ログインしてお受け取りください`): 動作の分からない連用・て形の節に係る日時は、
    // 続く節のものとして読む。
    let mut moved: Vec<(usize, usize)> = Vec::new();
    for clause in &parse.clauses {
        if let Relation::Coordinate(next) = clause.relation
            && clause.action.is_none()
        {
            for &arg in &clause.args {
                if parse.chunks[arg].kind == ChunkKind::Temporal {
                    moved.push((arg, next));
                }
            }
        }
    }
    for clause in &parse.clauses {
        let pred = &parse.chunks[clause.pred];
        let relation = match clause.relation {
            Relation::Main | Relation::Coordinate(_) => ReadingRelation::Main,
            Relation::Relative(_) => ReadingRelation::Relative,
            Relation::Condition | Relation::Cause => ReadingRelation::Subordinate,
        };
        // 対象の候補: 修飾節なら係る名詞、ほかは主語・目的語・主題の名詞。
        let mut candidates: Vec<usize> = Vec::new();
        if let Relation::Relative(noun) = clause.relation {
            candidates.push(noun);
        }
        for &arg in clause.args.iter().rev() {
            let chunk = &parse.chunks[arg];
            if chunk.kind != ChunkKind::Noun {
                continue;
            }
            if matches!(
                chunk.marker(),
                Marker::Ga | Marker::Wa | Marker::Mo | Marker::Wo | Marker::Topic | Marker::None
            ) || (chunk.marker() == Marker::No
                && (pred.copula || parse.chunks.get(arg + 1).is_some_and(|next| next.is_pred())))
            {
                candidates.push(arg);
            }
        }
        // `メンテナンス中です` `メンテナンス実施予定です` は述語そのものが対象。
        if pred.copula && lexicon::has_maintenance(&pred.text) {
            candidates.insert(0, clause.pred);
        }
        let mut target = None;
        let mut sense = NounSense {
            kind: TargetKind::None,
            reference: false,
        };
        for &candidate in &candidates {
            let candidate_sense = noun_sense(&parse.chunks[candidate].bare);
            if candidate_sense.reference && candidate != clause.pred {
                continue;
            }
            let better = match (sense.kind, candidate_sense.kind) {
                (TargetKind::Maint { .. }, _) => false,
                (_, TargetKind::Maint { .. }) => true,
                (TargetKind::Other { .. }, _) => false,
                (_, TargetKind::Other { .. }) => true,
                (TargetKind::Role(_), _) => false,
                (_, TargetKind::Role(_)) => true,
                _ => false,
            };
            if better || target.is_none() && candidate_sense.kind != TargetKind::None {
                target = Some(candidate);
                sense = candidate_sense;
            }
        }
        let foreign_subject = target.is_none()
            && candidates.iter().any(|&candidate| {
                let chunk = &parse.chunks[candidate];
                let candidate_sense = noun_sense(&chunk.bare);
                chunk.kind == ChunkKind::Noun
                    && candidate_sense.kind == TargetKind::None
                    && !candidate_sense.reference
                    && matches!(
                        chunk.marker(),
                        Marker::Ga | Marker::Wa | Marker::Wo | Marker::Mo
                    )
                    && chunk.bare.chars().count() >= 2
            });
        let mut role = None;
        let mut target_text = String::new();
        if let Some(chunk) = target {
            target_text = parse.chunks[chunk].bare.clone();
            role = lexicon::time_role(&target_text).filter(|_| chunk != clause.pred);
            if let TargetKind::Role(found) = sense.kind {
                role = Some(found);
                // `メンテナンスの終了予定は` のように、役割の名詞を修飾する対象。
                for modifier in parse.modifiers(chunk) {
                    let modifier_sense = noun_sense(&parse.chunks[modifier].bare);
                    if parse.chunks[modifier].kind == ChunkKind::Noun
                        && matches!(
                            modifier_sense.kind,
                            TargetKind::Maint { .. } | TargetKind::Other { .. }
                        )
                        && !modifier_sense.reference
                    {
                        target = Some(modifier);
                        target_text = parse.chunks[modifier].bare.clone();
                        sense = modifier_sense;
                    }
                }
            }
        }
        // 日時: 述語に係る日時、対象の名詞・役割の名詞に係る日時、述語そのものの日時 (`17時です`)。
        let mut times: Vec<(usize, Marker)> = Vec::new();
        let push_time = |chunk: usize, times: &mut Vec<(usize, Marker)>| {
            let chunk = &parse.chunks[chunk];
            if let Some(temporal) = chunk.temporal
                && !times.iter().any(|(t, _)| *t == temporal)
            {
                times.push((temporal, chunk.marker()));
            }
        };
        for &arg in &clause.args {
            if parse.chunks[arg].kind == ChunkKind::Temporal
                && !moved.iter().any(|(moved_arg, _)| *moved_arg == arg)
            {
                push_time(arg, &mut times);
            }
        }
        for &(arg, next) in &moved {
            if next == clause.pred {
                push_time(arg, &mut times);
            }
        }
        if pred.kind == ChunkKind::Pred && pred.temporal.is_some() {
            push_time(clause.pred, &mut times);
        }
        let mut modifier_actions = Vec::new();
        let mut targets_for_time: Vec<usize> = target.into_iter().collect();
        if let Some(chunk) = target
            && let Some(role_chunk) = candidates.iter().copied().find(|&candidate| {
                candidate != chunk
                    && matches!(
                        noun_sense(&parse.chunks[candidate].bare).kind,
                        TargetKind::Role(_)
                    )
            })
        {
            targets_for_time.push(role_chunk);
            if role.is_none() {
                role = lexicon::time_role(&parse.chunks[role_chunk].bare);
            }
        }
        if relation != ReadingRelation::Relative {
            for &noun in &targets_for_time {
                for modifier in parse.modifiers(noun) {
                    let chunk = &parse.chunks[modifier];
                    match chunk.kind {
                        ChunkKind::Temporal => push_time(modifier, &mut times),
                        ChunkKind::Noun => {
                            // `実施中のメンテナンス` `予定のメンテナンス`。`現在の臨時メンテナンス` も
                            // 行っている途中 (`現在のメンテナンス予定` は除く)。
                            modifier_actions.extend(lexicon::actions_in(&chunk.bare));
                            if chunk.bare.ends_with('中')
                                || (chunk.bare == "現在"
                                    && !parse.chunks[noun].bare.contains("予定"))
                            {
                                modifier_actions.push(Action::Execute);
                            }
                            for inner in parse.modifiers(modifier) {
                                if parse.chunks[inner].kind == ChunkKind::Temporal {
                                    push_time(inner, &mut times);
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        let undecided = pred.copula && pred.text.contains("未定")
            || clause
                .args
                .iter()
                .any(|&arg| parse.chunks[arg].text.ends_with("未定"));
        let mut action = clause.action;
        if pred.copula && pred.text.contains("未定") {
            action = None;
        }
        if pred.copula && pred.text.ends_with('中') && lexicon::has_maintenance(&pred.text) {
            action = Some(Action::Execute);
        }
        let tense = if pred.copula && pred.text.ends_with('中') {
            Tense::Progressive
        } else {
            clause.tense
        };
        let governor_action = target.and_then(|chunk| governor_action(parse, chunk));
        let governor_pred = target.and_then(|chunk| governor_pred(parse, chunk));
        // どこにも係らない名詞 (`〇11月6日に行った臨時メンテナンス` の `臨時メンテナンス`)。
        let phrase_head = target.is_some_and(|chunk| {
            parse.heads[chunk].is_none() && parse.chunks[chunk].kind == ChunkKind::Noun
        });
        let mut scope_text = target.map_or(String::new(), |chunk| noun_modifiers(parse, chunk));
        // 主題・主語に書かれた対象の名前 (`Android版は引き続きメンテナンスを実施しております` の `Android版は`)。
        for &arg in &clause.args {
            let chunk = &parse.chunks[arg];
            if Some(arg) != target
                && chunk.kind == ChunkKind::Noun
                && matches!(
                    chunk.marker(),
                    Marker::Wa | Marker::Ga | Marker::Mo | Marker::Topic
                )
                && lexicon::scope_names(&chunk.bare)
                    .iter()
                    .any(Option::is_some)
            {
                if !scope_text.is_empty() {
                    scope_text.push('・');
                }
                scope_text.push_str(&chunk.bare);
            }
        }
        out.push(Reading {
            target,
            target_text,
            scope_text,
            kind: sense.kind,
            role,
            action,
            negated: clause.negated,
            mood: clause.mood,
            tense,
            relation,
            times,
            undecided,
            span: span.clone(),
            modifier_actions,
            governor_action,
            governor_pred,
            pred: Some(clause.pred),
            nominal: false,
            phrase_head,
            foreign_subject,
        });
    }
    // 述語のない名詞句 (見出し・題名・値の行) の、メンテナンスの名詞。
    if parse.clauses.is_empty() {
        for (index, chunk) in parse.chunks.iter().enumerate() {
            if chunk.kind != ChunkKind::Noun {
                continue;
            }
            let sense = noun_sense(&chunk.bare);
            // アップデートの語だけの名詞句 (`アップデートのお知らせ`) はメンテナンスとして読まない。
            if sense.reference || !matches!(sense.kind, TargetKind::Maint { update: false }) {
                continue;
            }
            if matches!(
                chunk.marker(),
                Marker::Cause | Marker::CauseAdn | Marker::Ni | Marker::De | Marker::Nite
            ) {
                continue;
            }
            let mut times = Vec::new();
            for modifier in parse.modifiers(index) {
                let modifier_chunk = &parse.chunks[modifier];
                if let Some(temporal) = modifier_chunk.temporal {
                    times.push((temporal, modifier_chunk.marker()));
                }
            }
            let actions = lexicon::actions_in(&chunk.bare);
            out.push(Reading {
                target: Some(index),
                target_text: chunk.text.clone(),
                scope_text: noun_modifiers(parse, index),
                kind: sense.kind,
                role: lexicon::time_role(&chunk.bare),
                action: actions
                    .iter()
                    .copied()
                    .rfind(|action| *action != Action::Plan),
                negated: false,
                mood: if actions.contains(&Action::Plan) {
                    Mood::Plan
                } else {
                    Mood::Assert
                },
                tense: Tense::Present,
                relation: ReadingRelation::Main,
                times,
                undecided: chunk.bare.ends_with("未定"),
                span: span.clone(),
                modifier_actions: if actions.contains(&Action::Extend) {
                    vec![Action::Extend]
                } else {
                    Vec::new()
                },
                governor_action: None,
                governor_pred: None,
                pred: None,
                nominal: true,
                phrase_head: false,
                foreign_subject: false,
            });
        }
    }
    let _ = text;
    out
}

/// 名詞を修飾する名詞を、修飾の修飾までたどってつなぐ (`iOS版・Android版の` の `iOS版` と `Android版`)。
/// `の` で係る名詞と、並べた名詞 (助詞のない名詞) だけをたどる。
fn noun_modifiers(parse: &Parse, noun: usize) -> String {
    let mut found: Vec<usize> = Vec::new();
    let mut stack = vec![noun];
    while let Some(current) = stack.pop() {
        for modifier in parse.modifiers(current) {
            let chunk = &parse.chunks[modifier];
            if chunk.kind == ChunkKind::Noun
                && matches!(chunk.marker(), Marker::No | Marker::None)
                && found.len() < 8
                && !found.contains(&modifier)
            {
                found.push(modifier);
                stack.push(modifier);
            }
        }
    }
    // 書かれた順にする。
    found.sort_unstable();
    found
        .iter()
        .map(|&index| parse.chunks[index].bare.as_str())
        .collect::<Vec<_>>()
        .join("・")
}

/// 名詞の塊を主語・目的語・主題として受ける述語の動作。
fn governor_action(parse: &Parse, chunk: usize) -> Option<Action> {
    let chunk_ref = &parse.chunks[chunk];
    if chunk_ref.kind == ChunkKind::Pred {
        return parse
            .clauses
            .iter()
            .find(|clause| clause.pred == chunk)
            .and_then(|clause| clause.action);
    }
    if !matches!(
        chunk_ref.marker(),
        Marker::Ga | Marker::Wo | Marker::Wa | Marker::Mo | Marker::Topic | Marker::None
    ) {
        return None;
    }
    let head = parse.heads[chunk]?;
    let clause = parse.clauses.iter().find(|clause| clause.pred == head)?;
    if matches!(
        clause.mood,
        Mood::Condition | Mood::Possibility | Mood::Request
    ) {
        return None;
    }
    clause.action
}

/// 名詞の塊を主語・目的語・主題として受ける述語の塊。
fn governor_pred(parse: &Parse, chunk: usize) -> Option<usize> {
    let chunk_ref = &parse.chunks[chunk];
    if chunk_ref.kind == ChunkKind::Pred
        || !matches!(
            chunk_ref.marker(),
            Marker::Ga | Marker::Wo | Marker::Wa | Marker::Mo | Marker::Topic | Marker::None
        )
    {
        return None;
    }
    let head = parse.heads[chunk]?;
    parse.chunks[head].is_pred().then_some(head)
}

/// 比べるための読み: 正規表現と語の一覧だけで読む (形態素解析を使わない)。
///
/// 日時から前向きに最初の動作の語を探し、文にメンテナンスの語があればメンテナンスの記述とする。
/// 否定は文の中の「ません」「ない」、過去は「ました」「した」の有無で決める。
pub(crate) fn baseline(text: &str, span: Range<usize>, temporals: &[Temporal]) -> Vec<Reading> {
    let sentence = &text[span.clone()];
    let maint = lexicon::has_maintenance(sentence);
    let kind = if maint {
        TargetKind::Maint { update: false }
    } else if let Some(kind) = lexicon::period_kind(sentence) {
        TargetKind::Other {
            kind,
            generic: lexicon::is_generic_kind(sentence, kind),
        }
    } else if lexicon::has_update(sentence) {
        TargetKind::Maint { update: true }
    } else {
        TargetKind::None
    };
    const WORDS: &[(&str, Action)] = &[
        ("実施", Action::Execute),
        ("行", Action::Execute),
        ("開始", Action::Start),
        ("終了", Action::End),
        ("完了", Action::End),
        ("延長", Action::Extend),
        ("延期", Action::Postpone),
        ("中止", Action::Cancel),
        ("予定", Action::Plan),
        ("開催", Action::Hold),
        ("販売", Action::Sell),
        ("受け取", Action::Distribute),
        ("配布", Action::Distribute),
    ];
    let first_action = |from: usize| -> Option<(usize, Action)> {
        WORDS
            .iter()
            .filter_map(|(word, action)| {
                text[from..span.end]
                    .find(word)
                    .map(|at| (from + at, *action))
            })
            .min_by_key(|(at, _)| *at)
    };
    let tail_flags = |from: usize| {
        let tail = &text[from..span.end];
        let negated = tail.contains("ません") || tail.contains("ない");
        let past = tail.contains("ました") || tail.contains("した");
        (negated, past)
    };
    let mut out = Vec::new();
    let inside: Vec<(usize, &Temporal)> = temporals
        .iter()
        .enumerate()
        .filter(|(_, temporal)| temporal.span.start >= span.start && temporal.span.end <= span.end)
        .collect();
    for (index, temporal) in &inside {
        let action = first_action(temporal.span.end);
        let (negated, past) = action.map_or((false, false), |(at, _)| tail_flags(at));
        let marker = marker_after(text, temporal.span.end);
        out.push(Reading {
            target: Some(0),
            target_text: sentence.to_string(),
            scope_text: String::new(),
            kind,
            role: None,
            action: action.map(|(_, action)| action),
            negated,
            mood: if sentence.contains("予定") {
                Mood::Plan
            } else {
                Mood::Assert
            },
            tense: if past { Tense::Past } else { Tense::Present },
            relation: ReadingRelation::Main,
            times: vec![(*index, marker)],
            undecided: matches!(
                temporal.kind,
                TemporalKind::Range {
                    end: crate::temporal::RangeEnd::Undecided,
                    ..
                }
            ),
            span: span.clone(),
            modifier_actions: Vec::new(),
            governor_action: action.map(|(_, action)| action),
            governor_pred: None,
            pred: None,
            nominal: false,
            phrase_head: false,
            foreign_subject: false,
        });
    }
    if inside.is_empty()
        && maint
        && let Some((at, action)) = first_action(span.start)
    {
        let (negated, past) = tail_flags(at);
        out.push(Reading {
            target: Some(0),
            target_text: sentence.to_string(),
            scope_text: String::new(),
            kind,
            role: None,
            action: Some(action),
            negated,
            mood: Mood::Assert,
            tense: if past { Tense::Past } else { Tense::Present },
            relation: ReadingRelation::Main,
            times: Vec::new(),
            undecided: sentence.contains("未定"),
            span: span.clone(),
            modifier_actions: Vec::new(),
            governor_action: Some(action),
            governor_pred: None,
            pred: None,
            nominal: false,
            phrase_head: false,
            foreign_subject: false,
        });
    }
    out
}

/// 日時の直後の助詞 (比べるための読みで使う)。
fn marker_after(text: &str, at: usize) -> Marker {
    let rest = text[at..].trim_start();
    for (word, marker) in [
        ("より", Marker::Yori),
        ("から", Marker::Kara),
        ("までに", Marker::Madeni),
        ("まで", Marker::Made),
        ("をもちまして", Marker::Upto),
        ("をもって", Marker::Upto),
        ("に", Marker::Ni),
        ("の", Marker::No),
        ("を", Marker::Wo),
    ] {
        if rest.starts_with(word) {
            return marker;
        }
    }
    Marker::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn senses_nouns() {
        assert!(matches!(
            noun_sense("定期メンテナンス").kind,
            TargetKind::Maint { update: false }
        ));
        assert!(noun_sense("メンテナンス終了後").reference);
        assert!(matches!(
            noun_sense("メンテナンス延長のお詫び").kind,
            TargetKind::Other {
                kind: PeriodKind::Claim,
                ..
            }
        ));
        assert!(matches!(
            noun_sense("アップデート").kind,
            TargetKind::Maint { update: true }
        ));
        assert_eq!(noun_sense("終了時間").kind, TargetKind::Role(TimeRole::End));
    }
}
