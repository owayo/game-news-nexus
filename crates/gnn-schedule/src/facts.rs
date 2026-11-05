//! 結び付け。文書の構造と節の読みから、メンテナンスの記述 (`MaintFact`) と期間 (`PeriodFact`) を作る。
//!
//! 日時の役割は、次の順で決める。
//! 1. 節の読み: 対象 (メンテナンス・ほかの対象) と動作・述べ方・助詞から (`12:00より実施` は始まり、
//!    `16:00に終了いたしました` は実際の終わり、`11/12に予定していたメンテナンスを中止` は中止した予定)。
//! 2. 「項目名 + 値」の行と見出しの値の行: 項目名・見出しの意味から (`受取期限：…` は受け取りの期限、
//!    `▼メンテナンス日程` の下の値はメンテナンスの時間帯)。節の読みが別の対象に結び付けた日時は使わない。
//!
//! 対象を書いていない節 (`終了は17:00を予定しております`) は、同じ段落の直前の対象、見出しの対象、
//! 題名の対象の順に受け継ぐ。

use std::ops::Range;

use chrono::NaiveDate;
use gnn_core::schedule::{Evidence, Field, PeriodKind, Scope};
use hasami::Analyzer;

use crate::clause::{self, Marker, Mood, Tense};
use crate::document::{self, Document, LineKind, Unit};
use crate::lexicon::{self, Action, TimeRole};
use crate::reading::{self, NounSense, Reading, ReadingRelation, TargetKind};
use crate::resolve::{Local, Resolver};
use crate::temporal::{self, RangeEnd, Temporal, TemporalKind, ZoneSpec};

/// 開始・終了の値 (確定前)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum Slot {
    #[default]
    Unspecified,
    Undecided,
    Value(Local, Evidence),
}

impl Slot {
    pub fn is_set(&self) -> bool {
        !matches!(self, Slot::Unspecified)
    }

    pub fn value(&self) -> Option<&Local> {
        match self {
            Slot::Value(local, _) => Some(local),
            _ => None,
        }
    }

    /// 空いていれば入れる。
    pub fn fill(&mut self, other: Slot) {
        if !self.is_set() {
            *self = other;
        }
    }
}

/// 時間帯 (確定前)。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Slots {
    pub start: Slot,
    pub end: Slot,
}

impl Slots {
    pub fn is_empty(&self) -> bool {
        !self.start.is_set() && !self.end.is_set()
    }

    pub fn fill(&mut self, other: &Slots) {
        self.start.fill(other.start.clone());
        self.end.fill(other.end.clone());
    }
}

/// 状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Status {
    /// 予告・予定 (行う予定)。
    Announced,
    /// 行っている途中 (`実施しております` `終了していません`)。
    Ongoing,
    /// 延期した。
    Postponed,
    /// 中止した。
    Cancelled,
    /// 終わった。
    Completed,
}

/// メンテナンスの記述。
#[derive(Debug, Clone)]
pub(crate) struct MaintFact {
    pub scope: Scope,
    pub planned: Slots,
    pub actual: Slots,
    /// 予定か実際かを文脈で決める時間帯 (見出しの値・題名)。
    pub neutral: Slots,
    /// 日程を変える前の時間帯。
    pub previous: Option<Slots>,
    pub status: Option<Status>,
    /// 延長の記述がある。
    pub extended: bool,
    /// 延長した後の終わり。
    pub extended_end: Slot,
    /// 延長の始まり (`17:00より延長` の 17:00。延長する前の終わり)。
    pub extended_from: Slot,
    /// 「実施中」と述べている (`現在メンテナンスを実施しております` `終了していません` `延長しております`)。
    pub ongoing: bool,
    /// 所要時間 (分)。
    pub duration: Option<u32>,
    /// 題名の日付 (完了のお知らせの日付。開始の日とは決めない)。
    pub label_date: Option<(Local, Evidence)>,
    /// 変更前・取り消し線の記述。
    pub before: bool,
    pub region: usize,
    pub position: usize,
    /// 書かれた順 (同じ単位の中の記述の前後を比べる。統合の初めに付ける)。
    pub order: usize,
    pub title: bool,
    /// 言い切った記述か (見出しの値・主節の述語)。
    pub strong: bool,
    /// 記述の動作 (対象の一覧の見出しと照らす)。
    pub actions: Vec<Action>,
    pub evidence: Vec<Evidence>,
}

impl MaintFact {
    fn new(position: usize, region: usize, title: bool) -> Self {
        MaintFact {
            scope: Scope::Unknown,
            planned: Slots::default(),
            actual: Slots::default(),
            neutral: Slots::default(),
            previous: None,
            status: None,
            extended: false,
            extended_end: Slot::Unspecified,
            extended_from: Slot::Unspecified,
            ongoing: false,
            duration: None,
            label_date: None,
            before: false,
            region,
            position,
            order: 0,
            title,
            strong: false,
            actions: Vec::new(),
            evidence: Vec::new(),
        }
    }

    pub fn has_times(&self) -> bool {
        !self.planned.is_empty()
            || !self.actual.is_empty()
            || !self.neutral.is_empty()
            || self.label_date.is_some()
    }

    fn set_status(&mut self, status: Status) {
        if status == Status::Ongoing {
            self.ongoing = true;
        }
        // 同じ記述の中では、先へ進んだ状態を採る (予定 → 途中 → 延期・中止・完了)。
        if self.status.is_none_or(|current| status > current) {
            self.status = Some(status);
        }
    }
}

/// 期間の記述。
#[derive(Debug, Clone)]
pub(crate) struct PeriodFact {
    pub kind: PeriodKind,
    pub label: Option<String>,
    pub window: Slots,
    /// 変更前・取り消し線の期間。
    pub before: bool,
    /// 変更後・延長後と書かれた期間。
    pub after: bool,
    /// 延長の文脈 (`延長後の販売期間` `開催期間の延長対応` の下、`延長しました` の文)。
    pub extension: bool,
    /// この記述そのものが述べる変更 (`開催期間を『~10/22』から『~10/25』へ延長`)。`window` が変更の後。
    pub changes: Vec<PeriodChange>,
    pub region: usize,
    pub position: usize,
    pub evidence: Vec<Evidence>,
}

/// 期間の変更 (確定前)。変更の後は記述の `window`。
#[derive(Debug, Clone)]
pub(crate) enum PeriodChange {
    Extended { from: Slot },
    Rescheduled { from: Slots },
}

/// 対象の一覧 (`◼メンテナンス延長対象プラットフォーム` の下の `・SPクラウド版`)。
#[derive(Debug, Clone)]
pub(crate) struct ScopeList {
    pub region: usize,
    /// 見出しの動作 (なければ区画のメンテナンスすべてに当てる)。
    pub actions: Vec<Action>,
    pub names: Vec<Option<String>>,
    pub position: usize,
}

/// 記事から集めた記述。
#[derive(Debug, Default)]
pub(crate) struct Collected {
    pub maintenance: Vec<MaintFact>,
    pub periods: Vec<PeriodFact>,
    pub scope_lists: Vec<ScopeList>,
    /// 区画の新しさの順位 (0 は追記の印のない本文。日時のある追記ほど大きい)。
    pub region_rank: Vec<u32>,
    /// 区画の追記の日時と、その印の根拠 (0 番の区画と日時のない印は `None`)。
    pub region_dates: Vec<Option<(Local, Evidence)>>,
    pub notes: Vec<gnn_core::schedule::Note>,
}

/// 見出しの意味。
#[derive(Debug, Clone)]
struct Sense {
    noun: NounSense,
    role: Option<TimeRole>,
    actions: Vec<Action>,
    scope_list: bool,
    before: bool,
    after: bool,
    label: String,
    /// 見出しそのものの対象の一覧 (`・SPクラウド版` の行の見出しは一覧ではない)。
    names: Vec<Option<String>>,
}

fn sense(text: &str) -> Sense {
    let noun = reading::noun_sense(text);
    let before = text.contains("変更前") || text.contains("延長前") || text.contains("修正前");
    let after = text.contains("変更後") || text.contains("延長後") || text.contains("修正後");
    Sense {
        noun,
        role: lexicon::time_role(text).or((before || after).then_some(TimeRole::Window)),
        actions: lexicon::actions_in(text),
        scope_list: (text.contains("プラットフォーム")
            || text.ends_with("対象")
            || text.contains("対象環境"))
            && lexicon::time_role(text).is_none_or(|role| role == TimeRole::Eligibility),
        before,
        after,
        label: text.to_string(),
        names: lexicon::scope_names(text),
    }
}

/// 読み方。
pub(crate) enum Engine<'a> {
    /// hasami の形態素で読む。
    Morphology(&'a mut Analyzer),
    /// 正規表現と語の一覧だけで読む (比べるため)。
    Baseline,
}

/// 記事から記述を集める。
pub(crate) fn collect(
    article: &crate::Article<'_>,
    mut engine: Engine<'_>,
) -> (Collected, Resolver) {
    let mut resolver = Resolver::new(&article.context);
    let body = document::build(article.body_html, article.body_text);
    let title = document::title(article.title);
    let mut collected = Collected::default();

    // 区画の基準の日と新しさ。
    let region_dates = region_dates(&body, &mut resolver);
    collected.region_rank = rank_regions(&region_dates);
    collected.region_dates = region_dates
        .iter()
        .zip(&body.regions)
        .map(|(date, region)| {
            let local = date.clone()?;
            let line = &body.lines[region.marker?];
            let evidence = Evidence {
                field: Field::Body,
                text: clip(&body.raw[line.raw.clone()]),
            };
            Some((local, evidence))
        })
        .collect();

    let article_mentions_maint = lexicon::has_maintenance(article.title)
        || lexicon::has_maintenance(article.body_text)
        || lexicon::has_maintenance(&body.raw);
    let senses: Vec<Option<Sense>> = body
        .lines
        .iter()
        .map(|line| (line.kind == LineKind::Heading).then(|| sense(line.heading_text())))
        .collect();

    let mut reader = Reader {
        resolver: &mut resolver,
        collected: &mut collected,
        senses: &senses,
        article_mentions_maint,
        title_maint: false,
        title_period: None,
        title_label: None,
        previous: None,
        last_maint: None,
        date_headings: Vec::new(),
        title_change: ["変更", "延期", "日程"]
            .iter()
            .any(|word| article.title.contains(word)),
        title_extension: article.title.contains("延長"),
    };

    // 題名。
    let title_units = document::units(&title);
    for unit in &title_units {
        reader.unit(&title, unit, Field::Title, 0, &region_dates, &mut engine);
    }
    reader.title_maint = reader.collected.maintenance.iter().any(|fact| fact.title);
    let title_sense = reading::noun_sense(&document::title_core(article.title));
    if let TargetKind::Other { kind, .. } = title_sense.kind {
        reader.title_period = Some(kind);
        reader.title_label = Some(document::title_core(article.title));
    }

    // 本文。
    let units = document::units(&body);
    let mut previous_block = None;
    for (index, unit) in units.iter().enumerate() {
        let first = &body.lines[unit.lines[0]];
        if previous_block != Some(first.block) || first.kind == LineKind::Heading {
            reader.previous = None;
        }
        previous_block = Some(first.block);
        reader.unit(
            &body,
            unit,
            Field::Body,
            index + 1,
            &region_dates,
            &mut engine,
        );
    }
    drop(reader);
    (collected, resolver)
}

/// 記事の読みを人が見る形にする (調査用)。単位ごとに、日時・塊・節・読みを並べる。
pub(crate) fn debug(article: &crate::Article<'_>, analyzer: &mut Analyzer) -> String {
    let mut out = String::new();
    for (field, document) in [
        ("題名", document::title(article.title)),
        (
            "本文",
            document::build(article.body_html, article.body_text),
        ),
    ] {
        for unit in document::units(&document) {
            let line = &document.lines[unit.lines[0]];
            let text = unit.norm.text.as_str();
            out.push_str(&format!(
                "== {field} {:?} 行{:?} 見出し{:?} 値{:?} 区画{} 項目{:?}
{}
",
                line.kind,
                unit.lines,
                line.headings,
                line.value_of,
                line.region,
                line.label
                    .as_ref()
                    .map(|(label, value)| (&text[label.clone()], &text[value.clone()])),
                text
            ));
            let temporals = temporal::scan(text);
            for temporal in &temporals {
                out.push_str(&format!(
                    "  日時 {:?}
",
                    &text[temporal.span.clone()]
                ));
            }
            for span in sentences(text) {
                let parse = clause::parse(analyzer, text, span.clone(), &temporals);
                out.push_str(&clause::debug::render(&parse));
                for reading in reading::from_parse(&parse, span.clone(), text) {
                    out.push_str(&format!(
                        "  読み 対象{:?} {:?} 種類{:?} 役割{:?} 動作{:?} 否定{} {:?} {:?} {:?} 時{:?} 受け{:?} 名詞{}
",
                        reading.target,
                        reading.target_text,
                        reading.kind,
                        reading.role,
                        reading.action,
                        reading.negated,
                        reading.mood,
                        reading.tense,
                        reading.relation,
                        reading.times,
                        reading.governor_action,
                        reading.nominal
                    ));
                }
            }
        }
    }
    let (collected, _) = collect(article, Engine::Morphology(analyzer));
    for fact in &collected.maintenance {
        out.push_str(&format!(
            "記述 {:?} 予定{:?} 実際{:?} 中立{:?} 前{:?} 状態{:?} 延長{} 動作{:?} 区画{} 位置{} 題名{} 変更前{} 根拠{:?}
",
            fact.scope,
            show_slots(&fact.planned),
            show_slots(&fact.actual),
            show_slots(&fact.neutral),
            fact.previous.as_ref().map(show_slots),
            fact.status,
            fact.extended,
            fact.actions,
            fact.region,
            fact.position,
            fact.title,
            fact.before,
            fact.evidence.first().map(|evidence| evidence.text.clone())
        ));
    }
    for list in &collected.scope_lists {
        out.push_str(&format!(
            "対象の一覧 {list:?}
"
        ));
    }
    out
}

fn show_slots(slots: &Slots) -> String {
    let show = |slot: &Slot| match slot {
        Slot::Unspecified => "-".to_string(),
        Slot::Undecided => "未定".to_string(),
        Slot::Value(local, _) => format!("{} {:?}", local.date, local.time),
    };
    format!("[{} ~ {}]", show(&slots.start), show(&slots.end))
}

/// 追記の区画の基準の日 (印の日付)。
fn region_dates(document: &Document, resolver: &mut Resolver) -> Vec<Option<Local>> {
    document
        .regions
        .iter()
        .map(|region| {
            let marker = region.marker?;
            let line = &document.lines[marker];
            let temporals = temporal::scan(line.text());
            temporals.iter().find_map(|temporal| match &temporal.kind {
                TemporalKind::Point(point) if point.date.is_some() => {
                    let reference = resolver.reference;
                    resolver.point(point, None, reference, None, &[])
                }
                _ => None,
            })
        })
        .collect()
}

/// 区画の新しさの順位。印の日時の順。日時のない印は本文の次。
fn rank_regions(dates: &[Option<Local>]) -> Vec<u32> {
    let mut ranks = vec![0u32; dates.len()];
    let mut dated: Vec<(usize, chrono::NaiveDateTime)> = dates
        .iter()
        .enumerate()
        .skip(1)
        .filter_map(|(index, date)| date.as_ref().map(|local| (index, local.naive())))
        .collect();
    dated.sort_by_key(|(_, at)| *at);
    for (index, rank) in ranks.iter_mut().enumerate().skip(1) {
        if dates[index].is_none() {
            *rank = 1;
        }
    }
    for (order, (index, _)) in dated.into_iter().enumerate() {
        ranks[index] = 2 + order as u32;
    }
    ranks
}

/// 解決した日時の表記。
#[derive(Debug, Clone)]
struct ResolvedTemporal {
    start: Slot,
    end: Slot,
    /// 範囲か (点なら `start` だけ)。
    range: bool,
    duration: Option<u32>,
    /// メンテナンスを起点にした日付 (`メンテナンス後`)。
    anchored: bool,
}

struct Reader<'r> {
    resolver: &'r mut Resolver,
    collected: &'r mut Collected,
    senses: &'r [Option<Sense>],
    article_mentions_maint: bool,
    title_maint: bool,
    title_period: Option<PeriodKind>,
    title_label: Option<String>,
    /// 同じ段落の直前の対象。
    previous: Option<TargetKind>,
    /// 直前に作ったメンテナンスの記述 (`※Android版のみ実施` を当てる)。
    last_maint: Option<usize>,
    /// 日時だけの見出し (`■11/8(日) 23:59に終了`)。下の見出し (`▼ショップ`) の期間の時。
    date_headings: Vec<(usize, Slots)>,
    /// 題名が日程の変更・延期を述べる。
    title_change: bool,
    /// 題名が延長を述べる (`イベント開催期間等の延長について`)。
    title_extension: bool,
}

impl Reader<'_> {
    fn unit(
        &mut self,
        document: &Document,
        unit: &Unit,
        field: Field,
        position: usize,
        region_dates: &[Option<Local>],
        engine: &mut Engine<'_>,
    ) {
        let text = unit.norm.text.as_str();
        let first_line = &document.lines[unit.lines[0]];
        let region = first_line.region;
        let reference = region_dates
            .get(region)
            .cloned()
            .flatten()
            .map_or(self.resolver.reference, |local| local.date);
        let struck = unit.lines.iter().all(|&line| document.lines[line].struck);

        // 見出しの意味 (内側から)。
        let heading_senses: Vec<(usize, &Sense)> = first_line
            .headings
            .iter()
            .rev()
            .filter_map(|&line| self.senses[line].as_ref().map(|sense| (line, sense)))
            .collect();
        let value_sense = first_line
            .value_of
            .and_then(|line| self.senses[line].as_ref());
        let before = struck
            || heading_senses
                .first()
                .is_some_and(|(_, sense)| sense.before)
            || value_sense.is_some_and(|sense| sense.before);

        // 対象の一覧の見出しの下の値。
        if let Some(heading) = first_line.value_of
            && let Some(sense) = self.senses[heading].as_ref()
            && sense.scope_list
        {
            let names = lexicon::scope_names(text);
            if !names.is_empty() {
                let actions = sense.actions.clone();
                if let Some(list) = self
                    .collected
                    .scope_lists
                    .iter_mut()
                    .find(|list| list.position == heading)
                {
                    list.names.extend(names);
                } else {
                    self.collected.scope_lists.push(ScopeList {
                        region,
                        actions,
                        names,
                        position: heading,
                    });
                }
                return;
            }
        }

        let maint_count_before = self.collected.maintenance.len();
        let temporals = temporal::scan(text);
        let resolved = self.resolve_all(unit, document, &temporals, reference, field);
        let mut consumed = vec![false; temporals.len()];

        // 文ごとの読み。
        let relevant = |sentence: &str| {
            !temporal::scan(sentence).is_empty()
                || [
                    "メンテ",
                    "アップデート",
                    "終了",
                    "完了",
                    "延長",
                    "中止",
                    "延期",
                    "実施",
                    "再開",
                    "未定",
                    "見送",
                ]
                .iter()
                .any(|word| sentence.contains(word))
        };
        let label_range = if unit.lines.len() == 1 {
            first_line.label.clone()
        } else {
            None
        };
        for span in sentences(text) {
            if !relevant(&text[span.clone()]) {
                continue;
            }
            // 「項目名:値」の行は値の部分だけを文として読む。
            let span = match &label_range {
                Some((_, value)) => value.start.max(span.start)..span.end,
                None => span,
            };
            if span.is_empty() {
                continue;
            }
            let readings = match engine {
                Engine::Morphology(analyzer) => {
                    let parse = clause::parse(analyzer, text, span.clone(), &temporals);
                    reading::from_parse(&parse, span.clone(), text)
                }
                Engine::Baseline => reading::baseline(text, span.clone(), &temporals),
            };
            self.readings(
                &readings,
                document,
                unit,
                text,
                field,
                position,
                region,
                &resolved,
                &mut consumed,
                before,
                &heading_senses,
                value_sense,
            );
        }

        // 日時だけの見出し (`■11/8(日) 23:59に終了`) と、その下の対象の見出し (`▼ショップ`)。
        if first_line.kind == LineKind::Heading && field == Field::Body {
            let line = unit.lines[0];
            if !temporals.is_empty() && consumed.iter().all(|used| !used) {
                let end_like = text.contains("終了") || text.contains("まで");
                let value = &resolved[0];
                let mut window = Slots::default();
                if value.range {
                    window.start = value.start.clone();
                    window.end = value.end.clone();
                } else if end_like {
                    window.end = value.start.clone();
                } else {
                    window.start = value.start.clone();
                }
                if !window.is_empty() {
                    self.date_headings.push((line, window));
                }
            } else if let Some(sense) = self.senses[line].as_ref()
                && let TargetKind::Other { kind, .. } = sense.noun.kind
                && let Some((_, window)) = self
                    .date_headings
                    .iter()
                    .rev()
                    .find(|(heading, _)| first_line.headings.contains(heading))
            {
                let evidence = Evidence {
                    field,
                    text: clip(&unit_raw(document, unit, 0..text.len())),
                };
                self.collected.periods.push(PeriodFact {
                    kind,
                    label: None,
                    window: window.clone(),
                    before,
                    after: false,
                    extension: false,
                    changes: Vec::new(),
                    region,
                    position,
                    evidence: vec![evidence],
                });
            }
        }

        // 「項目名:値」と見出しの値の行。
        let local_sense = label_range
            .as_ref()
            .map(|(label, _)| sense(&text[label.clone()]));
        let governing = local_sense.as_ref().or(value_sense).or_else(|| {
            // 見出しの行そのものの日時は、見出しが時の項目名のとき (`■メンテナンス日時 11/12 14:00~`) だけ。
            (first_line.kind == LineKind::Heading)
                .then(|| self.senses[unit.lines[0]].as_ref())
                .flatten()
                .filter(|sense| {
                    sense.role.is_some_and(|role| role != TimeRole::Eligibility)
                        && lexicon::is_time_label(&sense.label)
                })
        });
        if let Some(governing) = governing {
            self.values(
                governing,
                &heading_senses,
                document,
                unit,
                text,
                field,
                position,
                region,
                &temporals,
                &resolved,
                &consumed,
                before
                    || local_sense.as_ref().is_some_and(|sense| sense.before)
                    || value_sense.is_some_and(|sense| sense.before),
                local_sense.is_some(),
            );
        }

        // 本文の `メンテナンス延長` (`メンテナンス延長のお詫び`) は、延長があったことを述べる。
        if field == Field::Body
            && self.title_maint
            && self.collected.maintenance.len() == maint_count_before
            && mentions_extension(text)
        {
            let mut fact = MaintFact::new(position, region, false);
            fact.extended = true;
            fact.evidence.push(Evidence {
                field,
                text: clip(&unit_raw(document, unit, 0..text.len())),
            });
            self.collected.maintenance.push(fact);
        }

        // `※Android版のみ実施` (それだけの行) は直前のメンテナンスの対象 (複数なら対象ごとに分ける)。
        let maint_count_after = self.collected.maintenance.len();
        let names: Vec<String> = lexicon::scope_names(text).into_iter().flatten().collect();
        if maint_count_after == maint_count_before
            && (text.contains("版のみ実施")
                || text.contains("版のみの実施")
                || text.contains("版のみ対象"))
            && let Some(index) = self.last_maint
            && let Some(fact) = self.collected.maintenance.get_mut(index)
            && fact.scope == Scope::Unknown
            && position.saturating_sub(fact.position) <= 2
            && let Some((first, rest)) = names.split_first()
        {
            fact.scope = Scope::Named(first.clone());
            let template = fact.clone();
            for (offset, name) in rest.iter().enumerate() {
                self.collected.maintenance.insert(
                    index + 1 + offset,
                    MaintFact {
                        scope: Scope::Named(name.clone()),
                        ..template.clone()
                    },
                );
            }
        }
    }

    /// 単位の日時をすべて地域の日時にする。`翌日` `同日` と時刻だけの値は直前の日付から。
    fn resolve_all(
        &mut self,
        unit: &Unit,
        document: &Document,
        temporals: &[Temporal],
        reference: NaiveDate,
        field: Field,
    ) -> Vec<ResolvedTemporal> {
        let mut anchor: Option<NaiveDate> = None;
        let zone_hint = article_zone_hint(&unit.norm.text);
        let mut out = Vec::new();
        for temporal in temporals {
            let raw = unit.norm.to_raw(temporal.span.clone());
            let evidence = Evidence {
                field,
                text: clip(&document.raw[raw]),
            };
            let evidence_list = [evidence.clone()];
            let mut resolved = ResolvedTemporal {
                start: Slot::Unspecified,
                end: Slot::Unspecified,
                range: false,
                duration: None,
                anchored: false,
            };
            match &temporal.kind {
                TemporalKind::Point(point) => {
                    resolved.anchored = point.anchor.is_some();
                    if let Some(local) =
                        self.resolver
                            .point(point, anchor, reference, zone_hint, &evidence_list)
                    {
                        anchor = Some(local.date);
                        resolved.start = Slot::Value(local, evidence.clone());
                    }
                }
                TemporalKind::Range { start, end } => {
                    resolved.range = true;
                    resolved.anchored = start.as_ref().is_some_and(|point| point.anchor.is_some());
                    let (start_local, end_local) = self.resolver.range(
                        start.as_ref(),
                        end,
                        anchor,
                        reference,
                        zone_hint,
                        &evidence_list,
                    );
                    if let Some(local) = start_local {
                        anchor = Some(local.date);
                        resolved.start = Slot::Value(local, evidence.clone());
                    }
                    match (end, end_local) {
                        (_, Some(local)) => {
                            anchor = Some(local.date);
                            resolved.end = Slot::Value(local, evidence.clone());
                        }
                        (RangeEnd::Undecided, None) => resolved.end = Slot::Undecided,
                        _ => {}
                    }
                }
                TemporalKind::Duration { minutes } => resolved.duration = Some(*minutes),
            }
            out.push(resolved);
        }
        out
    }

    #[allow(clippy::too_many_arguments)]
    fn readings(
        &mut self,
        readings: &[Reading],
        document: &Document,
        unit: &Unit,
        text: &str,
        field: Field,
        position: usize,
        region: usize,
        resolved: &[ResolvedTemporal],
        consumed: &mut [bool],
        before: bool,
        heading_senses: &[(usize, &Sense)],
        value_sense: Option<&Sense>,
    ) {
        // 同じ名詞を指す読みを 1 つの記述にまとめる。
        let mut groups: Vec<(Option<usize>, Vec<&Reading>)> = Vec::new();
        for reading in readings {
            match groups
                .iter_mut()
                .find(|(target, _)| *target == reading.target)
            {
                Some((_, group)) => group.push(reading),
                None => groups.push((reading.target, vec![reading])),
            }
        }
        // 修飾節だけの記述 (`17:00より延長しておりましたメンテナンスにつきまして`) は、その名詞を主題などで
        // 受ける主節の記述 (`…メンテナンスを終了いたしました`) と同じメンテナンス。
        let mut index = 0;
        while index < groups.len() {
            let relative_only = groups[index]
                .1
                .iter()
                .all(|reading| reading.relation == ReadingRelation::Relative);
            let governor = groups[index]
                .1
                .iter()
                .find_map(|reading| reading.governor_pred);
            let maint = groups[index]
                .1
                .iter()
                .any(|reading| matches!(reading.kind, TargetKind::Maint { update: false }));
            if relative_only
                && maint
                && let Some(governor) = governor
                && let Some(other) = groups.iter().position(|(target, group)| {
                    *target != groups[index].0
                        && group.iter().any(|reading| {
                            reading.pred == Some(governor)
                                && reading.relation == ReadingRelation::Main
                                && matches!(reading.kind, TargetKind::Maint { update: false })
                        })
                })
            {
                let (_, moved) = groups.remove(index);
                let other = if other > index { other - 1 } else { other };
                groups[other].1.splice(0..0, moved);
                continue;
            }
            index += 1;
        }
        for (_, group) in groups {
            let explicit = group
                .iter()
                .map(|reading| reading.kind)
                .find(|kind| matches!(kind, TargetKind::Maint { .. } | TargetKind::Other { .. }));
            let correction = text[group[0].span.clone()].starts_with("正しくは")
                && group.iter().any(|reading| !reading.times.is_empty());
            let kind = match explicit {
                Some(kind) => Some(kind),
                // `正しくは13:00~14:00となります` は直前の対象の時の訂正。
                None if correction && self.previous.is_some() => self.previous,
                None => self.inherit(&group, heading_senses, value_sense),
            };
            let Some(kind) = kind else {
                continue;
            };
            let sentence_span = group[0].span.clone();
            let evidence = Evidence {
                field,
                text: clip(&unit_raw(document, unit, sentence_span.clone())),
            };
            match kind {
                TargetKind::Maint { update } => {
                    if explicit.is_some() && !maint_semantics(&group, field == Field::Title) {
                        // `メンテナンスにて修正します` `メンテナンスを伴わないデータ更新` のように、
                        // メンテナンスを主語・目的語にして何かを述べていない。
                        continue;
                    }
                    if update && !self.update_is_maintenance(text, heading_senses, &group) {
                        continue;
                    }
                    if !self.title_maint
                        && group
                            .iter()
                            .any(|reading| reading.action == Some(Action::Exist))
                        && group.iter().all(|reading| {
                            reading.action == Some(Action::Exist)
                                || reading.relation != ReadingRelation::Main
                        })
                    {
                        // `※10月28日にメンテナンスはございます` (イベントのお知らせの中) は予告ではない。
                        continue;
                    }
                    let index = self.maint_from_readings(
                        &group, unit, text, field, position, region, resolved, consumed, before,
                        evidence,
                    );
                    if explicit.is_some() {
                        self.previous = Some(kind);
                    }
                    if let Some(index) = index {
                        self.last_maint = Some(index);
                    }
                }
                TargetKind::Other { kind, generic } => {
                    let kind = if generic {
                        self.context_kind(heading_senses, value_sense)
                            .unwrap_or(kind)
                    } else {
                        kind
                    };
                    self.period_from_readings(
                        &group,
                        kind,
                        position,
                        region,
                        resolved,
                        consumed,
                        before,
                        heading_senses,
                        evidence,
                    );
                    if explicit.is_some() {
                        self.previous = Some(TargetKind::Other {
                            kind,
                            generic: false,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    /// 対象を書いていない節の対象 (直前の対象 → 見出し → 題名)。
    fn inherit(
        &self,
        group: &[&Reading],
        heading_senses: &[(usize, &Sense)],
        value_sense: Option<&Sense>,
    ) -> Option<TargetKind> {
        // 主語・目的語に別の名詞が書かれた節 (`審査が完了し` `対応を実施`) は受け継がない。
        let own: Vec<&Reading> = group
            .iter()
            .copied()
            .filter(|reading| !reading.foreign_subject)
            .collect();
        let group = own.as_slice();
        if group.is_empty() {
            return None;
        }
        // 時か動作のどちらかがなければ受け継がない。
        let has_times = group.iter().any(|reading| !reading.times.is_empty());
        let maint_action = group.iter().any(|reading| {
            reading.mood != Mood::Condition
                && reading.mood != Mood::Possibility
                && matches!(
                    reading.action,
                    Some(
                        Action::End
                            | Action::Extend
                            | Action::Postpone
                            | Action::Cancel
                            | Action::Execute
                            | Action::Plan
                            | Action::Start
                            | Action::Resume
                    )
                )
        });
        let has_role = group
            .iter()
            .any(|reading| reading.role.is_some() || reading.undecided);
        let period_action = group.iter().any(|reading| {
            matches!(
                reading.action,
                Some(Action::Hold | Action::Sell | Action::Distribute)
            )
        });
        let _ = has_times;
        if !maint_action && !has_role && !period_action {
            return None;
        }
        // 受け継ぐ対象は、動作に合うものだけ (開催・販売・配布はメンテナンスを受け継がない)。
        let compatible = |kind: TargetKind| match kind {
            TargetKind::Maint { .. } => maint_action || has_role,
            TargetKind::Other { .. } => period_action || has_role || maint_action,
            _ => false,
        };
        // 対象の条件 (`補償対象`) の見出しの下の日時は、何かの期間ではない。
        if value_sense.is_some_and(|sense| sense.role == Some(TimeRole::Eligibility))
            || heading_senses
                .first()
                .is_some_and(|(_, sense)| sense.role == Some(TimeRole::Eligibility))
        {
            return None;
        }
        if let Some(previous) = self.previous
            && compatible(previous)
        {
            return Some(previous);
        }
        if let Some(sense) = value_sense
            && matches!(
                sense.noun.kind,
                TargetKind::Maint { .. } | TargetKind::Other { .. }
            )
        {
            return compatible(sense.noun.kind).then_some(sense.noun.kind);
        }
        for (_, sense) in heading_senses {
            if matches!(
                sense.noun.kind,
                TargetKind::Maint { .. } | TargetKind::Other { .. }
            ) {
                return compatible(sense.noun.kind).then_some(sense.noun.kind);
            }
        }
        // 題名がメンテナンスの記事で、終了・完了・延長などを述べる節 (時の役割の名詞だけでは受け継がない)。
        if self.title_maint && maint_action {
            let only_plain_time = group.iter().all(|reading| {
                reading.action.is_none_or(|action| {
                    matches!(action, Action::Hold | Action::Sell | Action::Distribute)
                })
            });
            if !only_plain_time {
                return Some(TargetKind::Maint { update: false });
            }
        }
        None
    }

    /// アップデートの語を、停止を伴うメンテナンスとして読むか。
    ///
    /// アップデートはふつう内容の更新なので、次のときだけメンテナンスとして読む。
    /// - 所要時間を述べる (`アップデートの所要時間は約5時間`)。
    /// - 見出しが時の項目名 (`【アップデート開始日時】`) で、記事がメンテナンスか所要時間に触れる。
    /// - 行っていたアップデートの完了を述べ (`本日実施しておりましたアップデートが完了`)、記事がメンテナンスに触れる。
    fn update_is_maintenance(
        &self,
        text: &str,
        heading_senses: &[(usize, &Sense)],
        group: &[&Reading],
    ) -> bool {
        let duration = text.contains("所要時間")
            || group
                .iter()
                .any(|reading| reading.role == Some(TimeRole::Duration));
        if duration {
            return true;
        }
        let timed_heading = heading_senses.first().is_some_and(|(_, sense)| {
            matches!(sense.noun.kind, TargetKind::Maint { .. })
                && sense.role.is_some_and(|role| role != TimeRole::Eligibility)
        });
        if timed_heading && self.article_mentions_maint {
            return true;
        }
        let completed = group.iter().any(|reading| {
            reading.relation == ReadingRelation::Main
                && reading.action == Some(Action::End)
                && matches!(reading.tense, Tense::Past | Tense::PastProgressive)
        }) && group.iter().any(|reading| {
            reading.relation == ReadingRelation::Relative
                && reading.action == Some(Action::Execute)
                && matches!(reading.tense, Tense::PastProgressive | Tense::Progressive)
        });
        completed && self.article_mentions_maint
    }

    fn context_kind(
        &self,
        heading_senses: &[(usize, &Sense)],
        value_sense: Option<&Sense>,
    ) -> Option<PeriodKind> {
        value_sense
            .into_iter()
            .chain(heading_senses.iter().map(|(_, sense)| *sense))
            .find_map(|sense| match sense.noun.kind {
                TargetKind::Other {
                    kind,
                    generic: false,
                } => Some(kind),
                _ => None,
            })
            .or(self.title_period)
    }

    #[allow(clippy::too_many_arguments)]
    fn maint_from_readings(
        &mut self,
        group: &[&Reading],
        unit: &Unit,
        text: &str,
        field: Field,
        position: usize,
        region: usize,
        resolved: &[ResolvedTemporal],
        consumed: &mut [bool],
        before: bool,
        evidence: Evidence,
    ) -> Option<usize> {
        let title = field == Field::Title;
        let mut fact = MaintFact::new(position, region, title);
        fact.before = before;
        let main_action = group
            .iter()
            .filter(|reading| reading.relation == ReadingRelation::Main)
            .filter_map(|reading| reading.action)
            .next_back();
        let rescheduling = matches!(main_action, Some(Action::Postpone | Action::Change));
        let mut any = false;
        for reading in group {
            let asserted = !matches!(
                reading.mood,
                Mood::Condition | Mood::Possibility | Mood::Request
            ) && reading.relation != ReadingRelation::Subordinate;
            // 時。
            for &(temporal, marker) in &reading.times {
                let Some(value) = resolved.get(temporal) else {
                    continue;
                };
                if value.anchored {
                    // `メンテナンス後` の日付はメンテナンスの時間帯ではない。
                    continue;
                }
                if reading.mood == Mood::Condition
                    && reading.relation == ReadingRelation::Subordinate
                {
                    continue;
                }
                consumed[temporal] = true;
                any = true;
                place_time(&mut fact, reading, value, marker, rescheduling, title);
            }
            if reading.undecided {
                any = true;
                match reading.role {
                    Some(TimeRole::Start) => fact.planned.start.fill(Slot::Undecided),
                    _ => fact.planned.end.fill(Slot::Undecided),
                }
            }
            if !asserted {
                continue;
            }
            // 状態。
            let action = reading.action;
            if reading.relation == ReadingRelation::Relative {
                if action == Some(Action::Extend) {
                    fact.extended = true;
                    // `17:00より延長しておりましたメンテナンス` の 17:00 は延長の始まり。
                    for &(temporal, marker) in &reading.times {
                        if matches!(marker, Marker::Yori | Marker::Kara)
                            && let Some(value) = resolved.get(temporal)
                            && !value.range
                        {
                            fact.extended_from.fill(value.start.clone());
                        }
                    }
                }
                if action == Some(Action::Execute) && reading.tense == Tense::Progressive {
                    fact.set_status(Status::Ongoing);
                }
                continue;
            }
            if reading.modifier_actions.contains(&Action::Extend) {
                fact.extended = true;
                any = true;
            }
            if reading.modifier_actions.contains(&Action::Execute)
                && reading.target_text.contains("メンテ")
            {
                fact.set_status(Status::Ongoing);
            }
            if let Some(action) = action {
                fact.actions.push(action);
                any |= status_from(&mut fact, reading, action, title);
            }
        }
        if title && fact.status.is_none() && !fact.extended {
            // 題名の「メンテナンスのお知らせ」は予告。
            fact.set_status(Status::Announced);
            any = true;
        }
        if !any && !fact.extended && fact.status.is_none() {
            return None;
        }
        fact.strong = group
            .iter()
            .any(|reading| reading.relation == ReadingRelation::Main);
        fact.evidence.push(evidence);
        // 対象の名前 (`Android版臨時メンテナンス` `Android版・iOS版のメンテナンス`)。対象ごとに別の記述にする。
        let mut names: Vec<Option<String>> = Vec::new();
        for reading in group {
            let found = lexicon::scope_names(&reading.target_text)
                .into_iter()
                .chain(lexicon::scope_names(&reading.scope_text));
            for name in found {
                if !names.contains(&name) {
                    names.push(name);
                }
            }
        }
        if names.is_empty() {
            let sentence = &text[group[0].span.clone()];
            names = scope_only(sentence).into_iter().map(Some).collect();
            if names.is_empty() && sentence.contains("全プラットフォーム") {
                names.push(None);
            }
        }
        let _ = unit;
        self.push_maint(fact, &names)
    }

    /// メンテナンスの記述を足す。対象が複数なら対象ごとに分ける (時間帯・状態・根拠は同じ)。
    /// 足した最後の記述の番号を返す。
    fn push_maint(&mut self, fact: MaintFact, names: &[Option<String>]) -> Option<usize> {
        if names.iter().any(Option::is_none) {
            let mut fact = fact;
            fact.scope = Scope::All;
            self.collected.maintenance.push(fact);
        } else if names.is_empty() {
            self.collected.maintenance.push(fact);
        } else {
            for name in names.iter().flatten() {
                let mut copy = fact.clone();
                copy.scope = Scope::Named(name.clone());
                self.collected.maintenance.push(copy);
            }
        }
        Some(self.collected.maintenance.len() - 1)
    }

    /// 期間の変更の読み (`AからBへ延長` `AからBに変更`)。前の日時と後の日時がそろったときだけ。
    #[allow(clippy::too_many_arguments)]
    fn period_change(
        &self,
        reading: &Reading,
        kind: PeriodKind,
        region: usize,
        position: usize,
        resolved: &[ResolvedTemporal],
        consumed: &mut [bool],
        heading_senses: &[(usize, &Sense)],
        evidence: &Evidence,
    ) -> Option<PeriodFact> {
        let from = reading
            .times
            .iter()
            .find(|(_, marker)| matches!(marker, Marker::Kara | Marker::Yori))?;
        let to = reading.times.iter().find(|(_, marker)| {
            matches!(
                marker,
                Marker::He | Marker::Ni | Marker::To | Marker::Made | Marker::Madeni
            )
        })?;
        let as_window = |value: &ResolvedTemporal| {
            let mut window = Slots::default();
            if value.range {
                window.start = value.start.clone();
                window.end = value.end.clone();
            } else {
                // 期間の変更の日時だけの値は終わり (`~11月1日4:59まで` の延長)。
                window.end = value.start.clone();
            }
            window
        };
        let before = as_window(resolved.get(from.0)?);
        let after = as_window(resolved.get(to.0)?);
        if before.is_empty() || after.is_empty() {
            return None;
        }
        consumed[from.0] = true;
        consumed[to.0] = true;
        let extension = reading.action == Some(Action::Extend)
            || self.title_extension
            || heading_senses
                .iter()
                .any(|(_, sense)| sense.label.contains("延長"));
        let change = change_between(&before, &after, extension);
        let mut after = after;
        if !after.start.is_set() {
            after.start = before.start.clone();
        }
        Some(PeriodFact {
            kind,
            label: self.period_label(heading_senses, Some(&reading.target_text)),
            window: after,
            before: false,
            after: true,
            extension,
            changes: vec![change],
            region,
            position,
            evidence: vec![evidence.clone()],
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn period_from_readings(
        &mut self,
        group: &[&Reading],
        kind: PeriodKind,
        position: usize,
        region: usize,
        resolved: &[ResolvedTemporal],
        consumed: &mut [bool],
        before: bool,
        heading_senses: &[(usize, &Sense)],
        evidence: Evidence,
    ) {
        for reading in group {
            if matches!(reading.mood, Mood::Condition | Mood::Possibility) || reading.negated {
                continue;
            }
            // `開催期間を『~10月22日4:59』から『~10月25日4:59』へ延長しました`: 前と後の期間を 1 つの記述にする。
            if matches!(
                reading.action,
                Some(Action::Extend | Action::Change | Action::Postpone)
            ) && let Some(fact) = self.period_change(
                reading,
                kind,
                region,
                position,
                resolved,
                consumed,
                heading_senses,
                &evidence,
            ) {
                self.collected.periods.push(fact);
                continue;
            }
            // 期間を述べる動作 (開催・販売・配布・開始・終了・延長) か、期間の項目名 (`開催期間は`)。
            let period_action = matches!(
                reading.action,
                Some(
                    Action::Hold
                        | Action::Sell
                        | Action::Distribute
                        | Action::Start
                        | Action::End
                        | Action::Extend
                        | Action::Execute
                )
            );
            let period_role = matches!(
                reading.role,
                Some(TimeRole::Window | TimeRole::Start | TimeRole::End | TimeRole::Deadline)
            );
            if !period_action && !period_role {
                continue;
            }
            for &(temporal, marker) in &reading.times {
                let Some(value) = resolved.get(temporal) else {
                    continue;
                };
                if value.duration.is_some() {
                    continue;
                }
                // 範囲でない日時は、始まり・終わりの助詞 (より・から・まで) か役割の名詞があるときだけ。
                let explicit_marker = matches!(
                    marker,
                    Marker::Yori | Marker::Kara | Marker::Made | Marker::Madeni | Marker::Upto
                );
                if !value.range && !explicit_marker && !period_role {
                    continue;
                }
                let mut window = Slots::default();
                if value.range {
                    window.start = value.start.clone();
                    window.end = value.end.clone();
                } else {
                    let end_like = matches!(marker, Marker::Made | Marker::Madeni | Marker::Upto)
                        || matches!(reading.action, Some(Action::End))
                        || matches!(reading.role, Some(TimeRole::End | TimeRole::Deadline));
                    if end_like {
                        window.end = value.start.clone();
                    } else {
                        window.start = value.start.clone();
                    }
                }
                consumed[temporal] = true;
                if window.is_empty() {
                    continue;
                }
                let label = self.period_label(heading_senses, Some(&reading.target_text));
                // `販売期間を9月24日11:59まで延長しました` は延長 (前の終わりは書かれていない)。
                // 延長の文脈の変更 (`◼イベント開催期間の延長対応` の下の `期間を…に変更しました`) も延長。
                let extension = reading.action == Some(Action::Extend)
                    || (matches!(reading.action, Some(Action::Change | Action::Postpone))
                        && (self.title_extension
                            || heading_senses
                                .iter()
                                .any(|(_, sense)| sense.label.contains("延長"))));
                let after_marker = matches!(
                    marker,
                    Marker::Made
                        | Marker::Madeni
                        | Marker::Upto
                        | Marker::He
                        | Marker::Ni
                        | Marker::To
                );
                let changes = if extension && after_marker && !before && window.end.is_set() {
                    vec![PeriodChange::Extended {
                        from: Slot::Unspecified,
                    }]
                } else {
                    Vec::new()
                };
                self.collected.periods.push(PeriodFact {
                    kind,
                    label,
                    window,
                    before,
                    after: false,
                    extension,
                    changes,
                    region,
                    position,
                    evidence: vec![evidence.clone()],
                });
            }
        }
    }

    /// 期間の名前 (見出し → 題名)。
    fn period_label(
        &self,
        heading_senses: &[(usize, &Sense)],
        target_text: Option<&str>,
    ) -> Option<String> {
        for (_, sense) in heading_senses {
            if matches!(sense.noun.kind, TargetKind::Other { .. })
                && !is_generic_label(&sense.label)
            {
                return Some(clean_label(&sense.label));
            }
        }
        if let Some(text) = target_text
            && let Some(name) = quoted_name(text)
        {
            return Some(name);
        }
        self.title_label.clone()
    }

    /// 「項目名:値」・見出しの値の行の日時。
    #[allow(clippy::too_many_arguments)]
    fn values(
        &mut self,
        governing: &Sense,
        heading_senses: &[(usize, &Sense)],
        document: &Document,
        unit: &Unit,
        text: &str,
        field: Field,
        position: usize,
        region: usize,
        temporals: &[Temporal],
        resolved: &[ResolvedTemporal],
        consumed: &[bool],
        before: bool,
        is_label: bool,
    ) {
        let pending: Vec<usize> = (0..temporals.len())
            .filter(|&index| !consumed[index])
            .collect();
        let undecided_value = text.contains("未定") && temporals.is_empty();
        if pending.is_empty() && !undecided_value {
            return;
        }
        // 項目名の対象。なければ親の見出しの対象。
        let mut kind = governing.noun.kind;
        if !matches!(kind, TargetKind::Maint { .. } | TargetKind::Other { .. }) {
            kind = heading_senses
                .iter()
                .map(|(_, sense)| sense.noun.kind)
                .find(|kind| matches!(kind, TargetKind::Maint { .. } | TargetKind::Other { .. }))
                .unwrap_or(TargetKind::None);
        }
        if kind == TargetKind::None && !lexicon::is_change_label(&governing.label) {
            if self.title_maint
                && (!governing.names.is_empty()
                    || (governing.role.is_some() && !governing.noun.reference))
            {
                kind = TargetKind::Maint { update: false };
            } else if let Some(period) = self.title_period
                && governing.role.is_some()
            {
                kind = TargetKind::Other {
                    kind: period,
                    generic: false,
                };
            }
        }
        if governing.role == Some(TimeRole::Eligibility) {
            return;
        }
        // 時の項目名 (`日時` `期間` `期限`) か、対象の名前の項目名 (`・Android版:`) の値だけを読む
        // (`◼メンテナンス内容` `【Google Play版の審査状況について】` の下の文の日時は読まない)。
        // 見出しの下の値の行 (`〓メンテナンス情報〓` の下の `2026/11/04 06:00からおよそ5時間`) は、
        // 日時で始まる文でない行なら読む。
        let value_line = temporal::starts_with_temporal(text) && !text.contains('。');
        if governing.role.is_none()
            && (governing.names.is_empty() || !is_label)
            && !(value_line && !is_label)
        {
            return;
        }
        // `変更前:` `変更後:` は、何の変更かを見出しか直前の行から決める (決まらなければ読まない)。
        if lexicon::is_change_label(&governing.label) && kind == TargetKind::None {
            match context_line_kind(document, unit) {
                Some(found) => kind = found,
                // 題名がメンテナンスの日程の変更 (`メンテナンス日程変更のお知らせ`) なら、メンテナンスの変更。
                None if self.title_maint && self.title_change => {
                    kind = TargetKind::Maint { update: false }
                }
                None => return,
            }
        }
        let evidence = Evidence {
            field,
            text: clip(&unit_raw(document, unit, 0..text.len())),
        };
        match kind {
            TargetKind::Maint { update } => {
                if update
                    && !(self.article_mentions_maint
                        || text.contains("所要時間")
                        || governing.label.contains("所要時間"))
                {
                    return;
                }
                if update && governing.role.is_none() {
                    return;
                }
                if governing.role == Some(TimeRole::Reserve) {
                    self.collected.notes.push(gnn_core::schedule::Note {
                        code: "reserve_day".to_string(),
                        message: format!(
                            "「{}」は予備日のため、メンテナンスの予定には入れていません",
                            evidence.text
                        ),
                        evidence: vec![evidence],
                    });
                    return;
                }
                let mut fact = MaintFact::new(position, region, field == Field::Title);
                fact.before = before || governing.before;
                fact.strong = true;
                let plan = governing.actions.contains(&Action::Plan) || text.contains("予定");
                let slots = if plan {
                    &mut fact.planned
                } else {
                    &mut fact.neutral
                };
                for &index in &pending {
                    let value = &resolved[index];
                    if value.anchored {
                        continue;
                    }
                    if let Some(minutes) = value.duration {
                        fact.duration = Some(minutes);
                        continue;
                    }
                    if value.range {
                        slots.start.fill(value.start.clone());
                        slots.end.fill(value.end.clone());
                    } else {
                        match governing.role {
                            Some(TimeRole::End | TimeRole::Deadline) => {
                                slots.end.fill(value.start.clone())
                            }
                            _ => {
                                if text.contains("まで") && !text.contains("から") {
                                    slots.end.fill(value.start.clone());
                                } else {
                                    slots.start.fill(value.start.clone());
                                }
                            }
                        }
                    }
                }
                if undecided_value {
                    match governing.role {
                        Some(TimeRole::Start) => slots.start.fill(Slot::Undecided),
                        _ => slots.end.fill(Slot::Undecided),
                    }
                }
                if governing.actions.contains(&Action::Extend) {
                    fact.extended = true;
                }
                if !fact.has_times() && fact.duration.is_none() && !undecided_value {
                    return;
                }
                fact.evidence.push(evidence);
                let names = governing.names.clone();
                self.last_maint = self.push_maint(fact, &names);
            }
            TargetKind::Other { kind, generic } => {
                let kind = if generic {
                    self.context_kind(heading_senses, Some(governing))
                        .unwrap_or(kind)
                } else {
                    kind
                };
                // `変更前:` `変更後:` の日時だけの値は、延長の文脈 (`期間の延長`) なら終わり。
                let extension = governing.label.contains("延長")
                    || heading_senses
                        .iter()
                        .any(|(_, sense)| sense.label.contains("延長"))
                    || self.title_extension
                    || context_mentions_extension(document, unit);
                let point_is_end = lexicon::is_change_label(&governing.label) && extension;
                for &index in &pending {
                    let value = &resolved[index];
                    if value.duration.is_some() {
                        continue;
                    }
                    let mut window = Slots::default();
                    if value.range {
                        window.start = value.start.clone();
                        window.end = value.end.clone();
                    } else if matches!(governing.role, Some(TimeRole::End | TimeRole::Deadline))
                        || text.contains("まで")
                        || point_is_end
                    {
                        window.end = value.start.clone();
                    } else {
                        window.start = value.start.clone();
                    }
                    if window.is_empty() {
                        continue;
                    }
                    let label = self.period_label(heading_senses, None);
                    self.collected.periods.push(PeriodFact {
                        kind,
                        label,
                        window,
                        before: before || governing.before,
                        after: governing.after,
                        extension,
                        changes: Vec::new(),
                        region,
                        position,
                        evidence: vec![evidence.clone()],
                    });
                }
            }
            _ => {}
        }
    }
}

/// メンテナンスの名詞を対象にした読みの組が、メンテナンスについて何かを述べているか。
///
/// 主節の動作がメンテナンスの動作 (実施・終了・延長・中止など) か、修飾節で時を述べた名詞を主節が
/// 主語・目的語・主題として受けて、メンテナンスの動作で述べているとき。`メンテナンスにて修正`
/// `メンテナンスの期間中に` `メンテナンスを伴わない` のような参照は除く。
fn maint_semantics(group: &[&Reading], title: bool) -> bool {
    let maint_action = |action: Option<Action>| {
        matches!(
            action,
            Some(
                Action::Execute
                    | Action::Start
                    | Action::End
                    | Action::Extend
                    | Action::Postpone
                    | Action::Cancel
                    | Action::Resume
                    | Action::Plan
                    | Action::Change
                    | Action::Exist
            )
        )
    };
    title
        || group.iter().any(|reading| {
            (reading.relation == ReadingRelation::Main && maint_action(reading.action))
                || maint_action(reading.governor_action)
                || reading.undecided
                || (reading.nominal && !reading.times.is_empty())
                || (reading.phrase_head
                    && reading.relation == ReadingRelation::Relative
                    && maint_action(reading.action))
                // `行っておりますメンテナンス` は、行っている途中であることを述べる。
                || (reading.relation == ReadingRelation::Relative
                    && reading.action == Some(Action::Execute)
                    && reading.tense == Tense::Progressive)
                || (reading.role.is_some() && reading.relation == ReadingRelation::Main)
                || reading.modifier_actions.contains(&Action::Extend)
        })
}

/// 節の時を記述に入れる。
fn place_time(
    fact: &mut MaintFact,
    reading: &Reading,
    value: &ResolvedTemporal,
    marker: Marker,
    rescheduling: bool,
    title: bool,
) {
    if let Some(minutes) = value.duration {
        fact.duration = Some(minutes);
        return;
    }
    let action = reading.action;
    let past = matches!(reading.tense, Tense::Past | Tense::PastProgressive);
    let progressive = reading.tense == Tense::Progressive;
    // 予定か実際か。
    enum Target {
        Planned,
        Actual,
        Neutral,
        Previous,
        Extended,
        Label,
    }
    let target = if action == Some(Action::Extend) && matches!(marker, Marker::Yori | Marker::Kara)
    {
        Target::Extended
    } else if rescheduling
        && (reading.relation == ReadingRelation::Relative
            || (reading.relation == ReadingRelation::Main
                && matches!(marker, Marker::Kara | Marker::Yori)
                && !value.range
                && matches!(action, Some(Action::Postpone | Action::Change))))
    {
        Target::Previous
    } else if action == Some(Action::Extend) && reading.relation == ReadingRelation::Main {
        Target::Extended
    } else if matches!(action, Some(Action::Cancel)) || reading.mood == Mood::Plan {
        Target::Planned
    } else if past
        && matches!(
            action,
            Some(Action::Execute | Action::Start | Action::End | Action::Resume)
        )
    {
        Target::Actual
    } else if progressive && matches!(action, Some(Action::Execute | Action::Extend | Action::End))
    {
        // `実施しております` は始まった、`終了しております` は終わった。
        Target::Actual
    } else if title && matches!(action, Some(Action::End)) && !value.range {
        Target::Label
    } else if matches!(marker, Marker::No | Marker::None)
        && reading.relation == ReadingRelation::Main
    {
        Target::Neutral
    } else {
        Target::Planned
    };
    // 始まりか終わりか。
    let end_like = matches!(marker, Marker::Made | Marker::Madeni | Marker::Upto)
        || (matches!(
            marker,
            Marker::Ni | Marker::None | Marker::De | Marker::No | Marker::Wo | Marker::To
        ) && (matches!(action, Some(Action::End | Action::Resume))
            || matches!(reading.role, Some(TimeRole::End | TimeRole::Deadline))));
    let slots = match target {
        Target::Planned => &mut fact.planned,
        Target::Actual => &mut fact.actual,
        Target::Neutral => &mut fact.neutral,
        Target::Previous => fact.previous.get_or_insert_with(Slots::default),
        Target::Extended => {
            if value.range {
                fact.planned.start.fill(value.start.clone());
                fact.extended_end = value.end.clone();
            } else if matches!(marker, Marker::Yori | Marker::Kara) {
                // `17:00より延長` の時刻は延長の始まり (もとの終わり)。時間帯には入れない。
                fact.extended_from.fill(value.start.clone());
            } else {
                fact.extended_end = value.start.clone();
            }
            return;
        }
        Target::Label => {
            if let Slot::Value(local, evidence) = &value.start {
                fact.label_date = Some((
                    crate::resolve::Local {
                        time: None,
                        ..local.clone()
                    },
                    evidence.clone(),
                ));
            }
            return;
        }
    };
    if value.range {
        slots.start.fill(value.start.clone());
        slots.end.fill(value.end.clone());
    } else if end_like {
        slots.end.fill(value.start.clone());
    } else {
        slots.start.fill(value.start.clone());
    }
}

/// 主節の動作から状態を決める。記述に何か足したら `true`。
fn status_from(fact: &mut MaintFact, reading: &Reading, action: Action, title: bool) -> bool {
    let past = matches!(reading.tense, Tense::Past | Tense::PastProgressive);
    let progressive = matches!(reading.tense, Tense::Progressive | Tense::PastProgressive);
    let plan = reading.mood == Mood::Plan;
    match action {
        Action::End | Action::Resume => {
            if reading.negated {
                fact.set_status(Status::Ongoing);
            } else if past || progressive || title {
                fact.set_status(Status::Completed);
            } else if !plan {
                // `終了いたします` は終わりの予告。
                return !reading.times.is_empty();
            } else {
                return !reading.times.is_empty();
            }
            true
        }
        Action::Cancel => {
            if reading.negated {
                return false;
            }
            fact.set_status(Status::Cancelled);
            true
        }
        Action::Execute | Action::Start => {
            if reading.negated {
                fact.set_status(Status::Cancelled);
            } else if progressive && !past {
                fact.set_status(Status::Ongoing);
            } else if progressive && past {
                // `実施しておりました` は行っていたことを述べるだけ (終わったかは別の述語で決める)。
                return !reading.times.is_empty();
            } else if past && action == Action::Execute && !title {
                fact.set_status(Status::Completed);
            } else if past && action == Action::Start {
                fact.set_status(Status::Ongoing);
            } else {
                fact.set_status(Status::Announced);
            }
            true
        }
        Action::Postpone => {
            if reading.negated {
                return false;
            }
            fact.set_status(Status::Postponed);
            true
        }
        Action::Extend => {
            if reading.negated {
                return false;
            }
            fact.extended = true;
            fact.set_status(Status::Ongoing);
            true
        }
        Action::Plan | Action::Change => {
            if reading.negated {
                return false;
            }
            fact.set_status(Status::Announced);
            true
        }
        Action::Exist => {
            if reading.negated {
                return false;
            }
            fact.set_status(Status::Announced);
            true
        }
        Action::Hold | Action::Sell | Action::Distribute => false,
    }
}

/// `メンテナンス延長` の語 (可能性・条件の `メンテナンス延長の可能性` `延長する場合` は除く)。
fn mentions_extension(text: &str) -> bool {
    ["メンテナンス延長", "メンテ延長"].iter().any(|word| {
        text.match_indices(word).any(|(at, _)| {
            let rest = &text[at + word.len()..];
            ![
                "の可能性",
                "の場合",
                "となる可能性",
                "する場合",
                "の恐れ",
                "が発生した場合",
            ]
            .iter()
            .any(|tail| rest.starts_with(tail))
        })
    })
}

/// 前と後の時間帯から変更を決める。延長と書かれ (`延長後の販売期間` `1日延長いたします`)、始まりが
/// 変わらず終わりが動いたなら延長、ほかは日程の変更。後の時間帯に始まりがなければ、始まりは変わらない。
pub(crate) fn change_between(before: &Slots, after: &Slots, extension_word: bool) -> PeriodChange {
    let same_start = match (before.start.value(), after.start.value()) {
        (Some(a), Some(b)) => a.naive() == b.naive(),
        (_, None) => true,
        (None, Some(_)) => false,
    };
    let end_moved = match (before.end.value(), after.end.value()) {
        (Some(a), Some(b)) => a.naive() != b.naive(),
        (_, Some(_)) => true,
        (_, None) => after.end == Slot::Undecided,
    };
    if extension_word && same_start && end_moved {
        PeriodChange::Extended {
            from: before.end.clone(),
        }
    } else {
        PeriodChange::Rescheduled {
            from: before.clone(),
        }
    }
}

/// `変更前:` の行の前 (同じ見出しの下の 6 行まで) に `延長` の語があるか。
fn context_mentions_extension(document: &Document, unit: &Unit) -> bool {
    let first = unit.lines[0];
    let headings = &document.lines[first].headings;
    (first.saturating_sub(6)..first).rev().any(|index| {
        let line = &document.lines[index];
        &line.headings == headings && line.text().contains("延長")
    })
}

/// `変更前:` の行の直前 (同じ見出しの下の 4 行まで) に書かれた対象。
fn context_line_kind(document: &Document, unit: &Unit) -> Option<TargetKind> {
    let first = unit.lines[0];
    let headings = &document.lines[first].headings;
    for index in (first.saturating_sub(4)..first).rev() {
        let line = &document.lines[index];
        if line.kind == LineKind::Separator || &line.headings != headings {
            break;
        }
        if line
            .label
            .as_ref()
            .is_some_and(|(label, _)| lexicon::is_change_label(&line.text()[label.clone()]))
        {
            continue;
        }
        let sense = reading::noun_sense(line.heading_text());
        if matches!(
            sense.kind,
            TargetKind::Maint { .. } | TargetKind::Other { .. }
        ) && !sense.reference
        {
            return Some(sense.kind);
        }
    }
    None
}

/// `Android版のみメンテナンスを継続` `iOS版・Android版のみ完了` の対象。`のみ` の直後がメンテナンスか
/// 動作の語のときだけ (`Google Play版のみ旧バージョンのままでメンテナンスを終了` の `のみ` は対象ではない)。
fn scope_only(sentence: &str) -> Vec<String> {
    let Some(at) = sentence.find("版のみ") else {
        return Vec::new();
    };
    let rest = sentence[at + "版のみ".len()..].trim_start_matches(['、', ' ', 'の']);
    // `iOS版のみ完了いたしました` のように、動作の語が直後に来るときも対象。
    let maint_follows = [
        "メンテ",
        "臨時メンテ",
        "定期メンテ",
        "緊急メンテ",
        "完了",
        "終了",
        "延長",
        "継続",
        "実施",
        "中止",
    ]
    .iter()
    .any(|word| rest.starts_with(word));
    if !maint_follows {
        return Vec::new();
    }
    // 直前の名前と、`・` `、` `と` でつながる名前 (`iOS版・Android版のみ`)。
    let mut names: Vec<String> = Vec::new();
    let mut end = at + "版".len();
    loop {
        let head = &sentence[..end];
        let core_end = head.len() - "版".len();
        let start = head[..core_end]
            .char_indices()
            .rev()
            .take_while(|(_, ch)| {
                ch.is_ascii_alphanumeric()
                    || *ch == ' '
                    || ('ァ'..='ヺ').contains(ch)
                    || matches!(ch, 'ー' | '/' | '.')
            })
            .last()
            .map_or(core_end, |(index, _)| index);
        match lexicon::scope_names(&head[start..])
            .into_iter()
            .flatten()
            .next()
        {
            Some(name) => names.push(name),
            None => break,
        }
        let before = head[..start].trim_end();
        let Some(separator) = ["・", "、", "と", "及び", "および", "&"]
            .iter()
            .find(|separator| before.ends_with(**separator))
        else {
            break;
        };
        let previous = before[..before.len() - separator.len()].trim_end();
        if !previous.ends_with('版') {
            break;
        }
        end = previous.len();
    }
    names.reverse();
    names
}

/// 文の区切り (単位の文字列の範囲)。
fn sentences(text: &str) -> Vec<Range<usize>> {
    let splitter = hasami::sentence::Splitter::default();
    splitter
        .split(text)
        .into_iter()
        .map(|sentence| sentence.range)
        .filter(|range| !range.is_empty())
        .collect()
}

/// 単位の文字列の範囲を、原文にして返す。
fn unit_raw(document: &Document, unit: &Unit, range: Range<usize>) -> String {
    unit.norm.raw_slice(&document.raw, range).to_string()
}

/// 本文全体に当てる地域時刻の明記 (`※時間はすべて日本時間です`)。
fn article_zone_hint(text: &str) -> Option<ZoneSpec> {
    let mentions_japan = text.contains("日本時間") || text.contains("JST");
    let whole = text.contains("すべて") || text.contains("全て") || text.contains("表記");
    (mentions_japan && whole).then_some(ZoneSpec::Japan)
}

/// 根拠の文を短くする。文の途中で折り返した改行は詰める (画面では 1 つの根拠を 2 行まで出す)。
fn clip(text: &str) -> String {
    const LIMIT: usize = 100;
    let mut joined = String::with_capacity(text.len());
    for (index, line) in text.trim().lines().enumerate() {
        let line = line.trim_matches([' ', '\u{3000}']);
        if index > 0 && !joined.is_empty() {
            let ascii_join = joined.ends_with(|ch: char| ch.is_ascii_alphanumeric())
                && line.starts_with(|ch: char| ch.is_ascii_alphanumeric());
            if ascii_join {
                joined.push(' ');
            }
        }
        joined.push_str(line);
    }
    if joined.chars().count() <= LIMIT {
        return joined;
    }
    let mut out: String = joined.chars().take(LIMIT).collect();
    out.push('…');
    out
}

/// 期間の名前にしない語 (項目名そのもの)。
fn is_generic_label(text: &str) -> bool {
    let text = text.trim();
    lexicon::is_time_label(text)
        || [
            "期間",
            "開催期間",
            "販売期間",
            "受け取り期間",
            "受取期間",
            "配布期間",
            "交換期間",
            "補填内容",
            "補てん内容",
            "お詫び",
            "内容",
            "対象",
            "注意事項",
        ]
        .contains(&text)
}

/// 括弧で囲んだ名前 (`イベント「ハロウィンパレード」` の `ハロウィンパレード`)。
fn quoted_name(text: &str) -> Option<String> {
    for (open, close) in [('「', '」'), ('『', '』'), ('【', '】')] {
        if let Some(start) = text.find(open) {
            let rest = &text[start + open.len_utf8()..];
            if let Some(end) = rest.find(close) {
                let name = rest[..end].trim();
                if !name.is_empty() {
                    return Some(name.to_string());
                }
            }
        }
    }
    None
}

/// 期間の名前を整える。
fn clean_label(text: &str) -> String {
    let mut text = text.trim().to_string();
    for suffix in [
        "のお知らせ",
        "について",
        "の開始",
        "開催のお知らせ",
        "開催",
        "開始",
        "の開催期間",
        "開催期間",
        "の期間",
        "期間",
    ] {
        if let Some(stripped) = text.strip_suffix(suffix)
            && !stripped.is_empty()
        {
            text = stripped.to_string();
        }
    }
    text.trim().to_string()
}
