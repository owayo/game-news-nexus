//! 記事の中の統合。メンテナンスの記述を対象と時間帯ごとにまとめ、予定と実際・状態・実施中・延長と
//! 日程変更を決める。
//!
//! - 対象: 1 件のメンテナンスの対象は 1 つ。複数の対象を挙げた記述 (`iOS版・Android版のメンテナンス`、
//!   対象の一覧 `◼メンテナンス延長対象プラットフォーム`、題名の対象) は対象ごとの件に分け、どの件にも
//!   同じ時間帯・状態・根拠を持たせる。一部の対象だけの完了・延長 (`iOS版のみ完了`) は、その対象の件に
//!   だけ当てる。対象の書かれていない記述は、時間帯が食い違わなければ名前のある対象すべての文脈として
//!   使う (状態は、その対象に状態の記述がないときだけ)。
//! - 状態: 新しい区画 (日時のある追記ほど新しい) の記述を優先する。同じ区画の中では、先へ進んだ状態
//!   (予定 → 途中 → 延期・中止・完了) を採る。完了と中止が同じ新しさで並べば決めない (`Unresolved`)。
//! - 実施中: 結果が予定どおりで「実施中」の記述 (`現在メンテナンスを実施しております`) があれば、いちばん
//!   新しい記述の区画の追記の日時 (なければ投稿日時、それもなければ初めて見つけた日時) を時点にする。
//!   この時点は開始の時刻には使わない。
//! - 時間帯: 予定と実際を分ける。見出しの値のように予定か実際かが書かれていない時間帯は、同じ区画で
//!   完了を報告していれば実際、ほかは予定。所要時間しかない終わりは概算。題名の日付 (完了のお知らせ)
//!   は、開始の日と違うときだけ実際の終わりの日にする。
//! - 延長: 延長を述べた区画ごとに 1 つの `Change::Extended`。延長する前の終わりは、書かれていれば
//!   (`17:00より延長`) それ、なければ延長より前の記述の予定の終わり (推定)、2 回目からは前の延長の終わり。
//! - 日程変更: 変更前・取り消し線・延期の前の予定を古い順に並べ、それぞれの変更の後を次の変更の前
//!   (最後は今の予定) にする。変更の前と同じ開始の古い記述は、変更で置き換わったものとして使わない。
//! - 期間: 変更前と変更後 (延長後) の期間を組にして、延長か日程変更を `Period::changes` に持つ。

use std::cmp::Reverse;

use chrono::NaiveDate;
use gnn_core::schedule::{
    Certainty, Change, Disposition, Evidence, Maintenance, Note, Ongoing, Period, Schedule, Scope,
    Slot as CoreSlot, TimeValue, When, Window, Zone,
};

use crate::facts::{
    self, Collected, MaintFact, PeriodChange, PeriodFact, ScopeList, Slot, Slots, Status,
};
use crate::lexicon::Action;
use crate::resolve::{Local, Resolver};

/// 区画の新しさと追記の日時。
struct Regions {
    rank: Vec<u32>,
    dates: Vec<Option<(Local, Evidence)>>,
}

impl Regions {
    fn rank(&self, region: usize) -> u32 {
        self.rank.get(region).copied().unwrap_or(0)
    }

    fn date(&self, region: usize) -> Option<&(Local, Evidence)> {
        self.dates.get(region).and_then(Option::as_ref)
    }

    /// 書かれた順の鍵 (区画の新しさ・単位の位置・単位の中の順)。
    fn written(&self, fact: &MaintFact) -> (u32, usize, usize) {
        (self.rank(fact.region), fact.position, fact.order)
    }
}

/// 日程を変える前の時間帯 (変更前・取り消し線の記述と、延期の文の前の予定)。
#[derive(Debug, Clone)]
struct Before {
    scope: Scope,
    region: usize,
    position: usize,
    order: usize,
    slots: Slots,
    evidence: Vec<Evidence>,
    /// 延期の文の前の予定なら、その文の記述 (`current` の番号)。
    owner: Option<usize>,
}

/// 記事の記述を統合する。
pub(crate) fn merge(collected: Collected, resolver: &mut Resolver) -> Schedule {
    let Collected {
        maintenance,
        periods,
        scope_lists,
        region_rank,
        region_dates,
        notes,
    } = collected;
    let regions = Regions {
        rank: region_rank,
        dates: region_dates,
    };
    let mut maintenance = maintenance;
    for (order, fact) in maintenance.iter_mut().enumerate() {
        fact.order = order;
    }
    let mut maintenance = assign_scopes(maintenance, &scope_lists);
    propagate_title_scopes(&mut maintenance);
    let (before_facts, current): (Vec<MaintFact>, Vec<MaintFact>) =
        maintenance.into_iter().partition(|fact| fact.before);
    let (current, befores) = supersede(current, &before_facts, &regions);
    let entities = group(&current);
    let named = entities
        .iter()
        .filter(|entity| matches!(entity.scope, Scope::Named(_)))
        .count();
    let assigned = assign_befores(&befores, &entities, &current);
    let mut schedule = Schedule::default();
    let mut extra_notes: Vec<Note> = Vec::new();
    for (entity, indices) in entities.iter().zip(&assigned) {
        let entity_befores: Vec<&Before> = indices.iter().map(|&index| &befores[index]).collect();
        if let Some(maintenance) = build(
            entity,
            &current,
            &entity_befores,
            &regions,
            named == 1,
            resolver,
            &mut extra_notes,
        ) {
            schedule.maintenance.push(maintenance);
        }
    }
    schedule.periods = build_periods(periods, &regions, resolver);
    schedule.notes = std::mem::take(&mut resolver.notes);
    schedule.notes.extend(notes);
    schedule.notes.extend(extra_notes);
    schedule
}

/// 対象の一覧を、動作の合う記述に当てる。一覧に対象が複数あれば、対象ごとの記述に分ける。
fn assign_scopes(facts: Vec<MaintFact>, lists: &[ScopeList]) -> Vec<MaintFact> {
    let mut out: Vec<MaintFact> = Vec::with_capacity(facts.len());
    for fact in facts {
        if fact.scope != Scope::Unknown || fact.title {
            out.push(fact);
            continue;
        }
        let matching: Vec<&ScopeList> = lists
            .iter()
            .filter(|list| list.region == fact.region && !list.names.is_empty())
            .filter(|list| {
                list.actions.is_empty()
                    || list.actions.iter().any(|action| {
                        fact.actions.contains(action)
                            || (*action == Action::End && fact.status == Some(Status::Completed))
                            || (*action == Action::Extend && fact.extended)
                    })
            })
            .collect();
        // 動作の合う一覧が 1 つだけのとき (動作のない一覧は、動作の合う一覧がないときだけ)。
        let with_action: Vec<&&ScopeList> = matching
            .iter()
            .filter(|list| !list.actions.is_empty())
            .collect();
        let chosen = match (with_action.len(), matching.len()) {
            (1, _) => Some(*with_action[0]),
            (0, 1) => Some(matching[0]),
            _ => None,
        };
        match chosen {
            Some(list) => split(fact, &list.names, &mut out),
            None => out.push(fact),
        }
    }
    out
}

/// 記述を対象ごとに分ける (`None` は全体)。
fn split(fact: MaintFact, names: &[Option<String>], out: &mut Vec<MaintFact>) {
    if names.iter().any(Option::is_none) {
        out.push(MaintFact {
            scope: Scope::All,
            ..fact
        });
        return;
    }
    let names: Vec<&String> = names.iter().flatten().collect();
    if names.is_empty() {
        out.push(fact);
        return;
    }
    for name in names {
        out.push(MaintFact {
            scope: Scope::Named(name.clone()),
            ..fact.clone()
        });
    }
}

/// 題名の対象 (`Android版臨時メンテナンス終了` `【iOS版・Android版】…`) は、本文に題名にない対象が
/// なければ、対象の書かれていない本文の記述にも当てる (対象が複数なら対象ごとに分ける)。
fn propagate_title_scopes(facts: &mut Vec<MaintFact>) {
    let mut title_scopes: Vec<Option<String>> = Vec::new();
    for fact in facts.iter().filter(|fact| fact.title) {
        if let Scope::Named(name) = &fact.scope
            && !title_scopes.contains(&Some(name.clone()))
        {
            title_scopes.push(Some(name.clone()));
        }
    }
    let foreign = facts.iter().any(|fact| {
        !fact.title
            && matches!(&fact.scope, Scope::Named(name) if !title_scopes.contains(&Some(name.clone())))
    });
    if title_scopes.is_empty() || foreign {
        return;
    }
    let mut out: Vec<MaintFact> = Vec::with_capacity(facts.len() * title_scopes.len());
    for fact in facts.drain(..) {
        if fact.scope == Scope::Unknown {
            split(fact, &title_scopes, &mut out);
        } else {
            out.push(fact);
        }
    }
    *facts = out;
}

/// 日程の変更で置き換わった記述を除き、変更の前の時間帯を集める。置き換わったのは、変更の前と同じ
/// 開始の、それより古い区画の記述 (同じ区画なら時間帯まで同じ記述)。
fn supersede(
    current: Vec<MaintFact>,
    before_facts: &[MaintFact],
    regions: &Regions,
) -> (Vec<MaintFact>, Vec<Before>) {
    let mut befores: Vec<Before> = before_facts
        .iter()
        .filter_map(|fact| {
            let mut slots = fact.planned.clone();
            slots.fill(&fact.neutral);
            slots.fill(&fact.actual);
            (!slots.is_empty()).then(|| Before {
                scope: fact.scope.clone(),
                region: fact.region,
                position: fact.position,
                order: fact.order,
                evidence: first_evidence(&slots).into_iter().collect(),
                slots,
                owner: None,
            })
        })
        .collect();
    for (index, fact) in current.iter().enumerate() {
        if let Some(slots) = &fact.previous
            && !slots.is_empty()
        {
            befores.push(Before {
                scope: fact.scope.clone(),
                region: fact.region,
                position: fact.position,
                order: fact.order,
                evidence: first_evidence(slots)
                    .or_else(|| fact.evidence.first().cloned())
                    .into_iter()
                    .collect(),
                slots: slots.clone(),
                owner: Some(index),
            });
        }
    }
    let superseded: Vec<bool> = current
        .iter()
        .enumerate()
        .map(|(index, fact)| {
            let Some(start) = start_of(fact) else {
                return false;
            };
            let window = window_of(fact);
            befores.iter().any(|before| {
                let Some(before_start) = before.slots.start.value() else {
                    return false;
                };
                before.owner != Some(index)
                    && scopes_meet(&before.scope, &fact.scope)
                    && same_start(Some(before_start), Some(start))
                    && match regions.rank(before.region).cmp(&regions.rank(fact.region)) {
                        std::cmp::Ordering::Greater => true,
                        std::cmp::Ordering::Equal => same_slots(&before.slots, &window),
                        std::cmp::Ordering::Less => false,
                    }
            })
        })
        .collect();
    let mut remap: Vec<Option<usize>> = Vec::with_capacity(current.len());
    let mut kept: Vec<MaintFact> = Vec::with_capacity(current.len());
    for (fact, superseded) in current.into_iter().zip(superseded) {
        if superseded {
            remap.push(None);
        } else {
            remap.push(Some(kept.len()));
            kept.push(fact);
        }
    }
    for before in &mut befores {
        before.owner = before.owner.and_then(|owner| remap[owner]);
    }
    (kept, befores)
}

/// 変更の前の対象が記述の対象に当たるか (どちらかが書かれていなければ当たる)。
fn scopes_meet(before: &Scope, fact: &Scope) -> bool {
    before == fact || *before == Scope::Unknown || *fact == Scope::Unknown
}

/// 記述の時間帯 (予定・実際・どちらでもない を合わせたもの)。
fn window_of(fact: &MaintFact) -> Slots {
    let mut slots = fact.planned.clone();
    slots.fill(&fact.actual);
    slots.fill(&fact.neutral);
    slots
}

/// 変更の前の時間帯を、それに続く件に割り当てる。延期の文の前の予定はその文の件に、ほかは同じ対象
/// (書かれていなければどの対象でも) の件のうち、同じ区画の近い位置に記述のある件に。
fn assign_befores(befores: &[Before], entities: &[Entity], facts: &[MaintFact]) -> Vec<Vec<usize>> {
    let mut out: Vec<Vec<usize>> = vec![Vec::new(); entities.len()];
    for (index, before) in befores.iter().enumerate() {
        if let Some(owner) = before.owner {
            for (entity, assigned) in entities.iter().zip(out.iter_mut()) {
                if entity.facts.contains(&owner) {
                    assigned.push(index);
                }
            }
            continue;
        }
        let distances: Vec<Option<(bool, usize)>> = entities
            .iter()
            .map(|entity| {
                if before.scope != entity.scope && before.scope != Scope::Unknown {
                    return None;
                }
                entity
                    .facts
                    .iter()
                    .map(|&fact| {
                        let fact = &facts[fact];
                        (
                            fact.region != before.region,
                            fact.position.abs_diff(before.position),
                        )
                    })
                    .min()
            })
            .collect();
        let best = distances.iter().flatten().min().copied();
        for (distance, assigned) in distances.iter().zip(out.iter_mut()) {
            if distance.is_some() && *distance == best {
                assigned.push(index);
            }
        }
    }
    out
}

/// 1 件のメンテナンスにまとめる記述の組。
struct Entity {
    scope: Scope,
    /// まとめる記述 (`current` の番号)。
    facts: Vec<usize>,
    /// 対象の書かれていない文脈の記述 (状態は使わないことがある)。
    shared: Vec<usize>,
}

/// 記述の始まり (予定・実際・どちらでもない の順)。
fn start_of(fact: &MaintFact) -> Option<&Local> {
    fact.planned
        .start
        .value()
        .or(fact.actual.start.value())
        .or(fact.neutral.start.value())
}

fn same_start(a: Option<&Local>, b: Option<&Local>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => {
            if a.time.is_some() && b.time.is_some() {
                a.naive() == b.naive()
            } else {
                a.date == b.date
            }
        }
        _ => true,
    }
}

/// 値が同じか (根拠と確かさは比べない)。
fn same_slot(a: &Slot, b: &Slot) -> bool {
    match (a, b) {
        (Slot::Value(a, _), Slot::Value(b, _)) => a.date == b.date && a.time == b.time,
        (Slot::Value(..), _) | (_, Slot::Value(..)) => false,
        (a, b) => a == b,
    }
}

/// 時間帯が同じか (片方にない端は比べない)。
fn same_slots(a: &Slots, b: &Slots) -> bool {
    let meets = |a: &Slot, b: &Slot| !a.is_set() || !b.is_set() || same_slot(a, b);
    meets(&a.start, &b.start) && meets(&a.end, &b.end)
}

/// 始まりが食い違わないか (片方になければ食い違わない)。
fn starts_meet(a: &Slot, b: &Slot) -> bool {
    !a.is_set() || !b.is_set() || same_slot(a, b)
}

/// 書かれた値を推定の値にする (値でなければ「分からない」)。
fn inferred(slot: &Slot) -> Slot {
    match slot {
        Slot::Value(local, evidence) => Slot::Value(
            Local {
                certainty: Certainty::Inferred,
                ..local.clone()
            },
            evidence.clone(),
        ),
        _ => Slot::Unspecified,
    }
}

/// 時間帯の最初の根拠。
fn first_evidence(slots: &Slots) -> Option<Evidence> {
    [&slots.start, &slots.end]
        .into_iter()
        .find_map(|slot| match slot {
            Slot::Value(_, evidence) => Some(evidence.clone()),
            _ => None,
        })
}

fn group(facts: &[MaintFact]) -> Vec<Entity> {
    let mut entities: Vec<Entity> = Vec::new();
    // 名前のある対象。
    for (index, fact) in facts.iter().enumerate() {
        let Scope::Named(_) = &fact.scope else {
            continue;
        };
        match entities.iter_mut().find(|entity| {
            entity.scope == fact.scope
                && entity
                    .facts
                    .iter()
                    .all(|&other| same_start(start_of(&facts[other]), start_of(fact)))
        }) {
            Some(entity) => entity.facts.push(index),
            None => entities.push(Entity {
                scope: fact.scope.clone(),
                facts: vec![index],
                shared: Vec::new(),
            }),
        }
    }
    let named = entities.len();
    // 全体・対象の書かれていない記述。名前のある対象があるときは、その開始の文脈として共有する。
    // 自分の状態 (完了・中止・延期) や終わりを持つ記述は、名前のある対象とは別の 1 件にする
    // (`Android版のみ継続` のほかは予定どおり終了、など)。
    let mut pending: Vec<usize> = Vec::new();
    for (index, fact) in facts.iter().enumerate() {
        if matches!(fact.scope, Scope::Named(_)) {
            continue;
        }
        let start = start_of(fact);
        if named > 0 {
            for entity in entities.iter_mut().take(named) {
                let compatible = entity
                    .facts
                    .iter()
                    .all(|&other| same_start(start_of(&facts[other]), start));
                if compatible {
                    entity.shared.push(index);
                }
            }
            let own = !fact.title
                && (matches!(
                    fact.status,
                    Some(Status::Completed | Status::Cancelled | Status::Postponed)
                ) || fact.planned.end.is_set()
                    || fact.actual.end.is_set()
                    || fact.neutral.end.is_set());
            if !own {
                pending.push(index);
                continue;
            }
        }
        push_unnamed(&mut entities, named, facts, index);
    }
    // 共有だけにした記述は、名前のない組があればそこにも入れる。
    for index in pending {
        let start = start_of(&facts[index]);
        if let Some(entity) = entities.iter_mut().skip(named).find(|entity| {
            entity
                .facts
                .iter()
                .all(|&other| same_start(start_of(&facts[other]), start))
        }) {
            entity.facts.push(index);
        }
    }
    // 時のない記述だけの組 (状態だけ) は、時のある組が 1 つならそこへ入れる。
    let timed: Vec<usize> = (0..entities.len())
        .filter(|&entity| {
            entities[entity]
                .facts
                .iter()
                .any(|&fact| facts[fact].has_times())
        })
        .collect();
    let mut index = 0;
    while index < entities.len() {
        let untimed = !entities[index]
            .facts
            .iter()
            .any(|&fact| facts[fact].has_times());
        if untimed
            && entities[index].scope == Scope::Unknown
            && timed.len() == 1
            && timed[0] != index
        {
            let moved = entities.remove(index);
            let target = if timed[0] > index {
                timed[0] - 1
            } else {
                timed[0]
            };
            entities[target].facts.extend(moved.facts);
            continue;
        }
        index += 1;
    }
    entities.sort_by_key(|entity| {
        entity
            .facts
            .iter()
            .map(|&fact| (!facts[fact].title, facts[fact].position))
            .min()
    });
    entities
}

/// 名前のない記述を、開始の合う名前のない組に入れる (なければ新しい組)。
fn push_unnamed(entities: &mut Vec<Entity>, named: usize, facts: &[MaintFact], index: usize) {
    let fact = &facts[index];
    let start = start_of(fact);
    let found = entities.iter_mut().skip(named).find(|entity| {
        entity
            .facts
            .iter()
            .all(|&other| same_start(start_of(&facts[other]), start))
    });
    match found {
        Some(entity) => {
            entity.facts.push(index);
            if fact.scope == Scope::All {
                entity.scope = Scope::All;
            }
        }
        None => entities.push(Entity {
            scope: fact.scope.clone(),
            facts: vec![index],
            shared: Vec::new(),
        }),
    }
}

/// 1 回の延長 (確定前)。
struct Extension {
    /// 書かれた順の鍵 (`Regions::written`)。
    key: (u32, usize, usize),
    from: Slot,
    end: Slot,
    evidence: Vec<Evidence>,
}

#[allow(clippy::too_many_arguments)]
fn build(
    entity: &Entity,
    facts: &[MaintFact],
    befores: &[&Before],
    regions: &Regions,
    single_named: bool,
    resolver: &mut Resolver,
    notes: &mut Vec<Note>,
) -> Option<Maintenance> {
    let reference = resolver.reference;
    let rank = |fact: &MaintFact| regions.rank(fact.region);
    // 新しい記述から順に (同じ新しさなら本文の前から)。
    let mut ordered: Vec<(&MaintFact, bool)> = entity
        .facts
        .iter()
        .map(|&index| (&facts[index], false))
        .chain(entity.shared.iter().map(|&index| (&facts[index], true)))
        .collect();
    ordered.sort_by_key(|(fact, _)| (Reverse(rank(fact)), fact.title, fact.position));

    // 状態。共有の記述 (対象の書かれていない文脈) は、開始の時だけに使う。
    let status_facts: Vec<&MaintFact> = ordered
        .iter()
        .filter(|(fact, shared)| fact.status.is_some() && !shared)
        .map(|(fact, _)| *fact)
        .collect();
    let top_rank = status_facts.iter().map(|fact| rank(fact)).max();
    let top: Vec<Status> = status_facts
        .iter()
        .filter(|fact| Some(rank(fact)) == top_rank)
        .filter_map(|fact| fact.status)
        .collect();
    let has = |status: Status| top.contains(&status);
    let mut disposition = if has(Status::Completed) && has(Status::Cancelled) {
        notes.push(Note {
            code: "conflicting_status".to_string(),
            message:
                "完了と中止の両方の記述があり、どちらが新しいか分からないため、結果を決めていません"
                    .to_string(),
            evidence: status_facts
                .iter()
                .flat_map(|fact| fact.evidence.iter().cloned())
                .take(3)
                .collect(),
        });
        Disposition::Unresolved
    } else if has(Status::Cancelled) {
        Disposition::Cancelled
    } else if has(Status::Completed) {
        Disposition::Completed
    } else if has(Status::Postponed) {
        Disposition::Postponed
    } else {
        Disposition::Scheduled
    };
    let completion_rank = status_facts
        .iter()
        .filter(|fact| fact.status == Some(Status::Completed))
        .map(|fact| rank(fact))
        .max();

    // 時間帯。
    let mut planned = Slots::default();
    let mut actual = Slots::default();
    let mut duration = None;
    let mut label = None;
    // 予定の終わりを入れた記述の書かれた順と、延長の記述か (延長より後に書かれた終わりかを見る)。
    let mut planned_end_from: Option<((u32, usize, usize), bool)> = None;
    for (fact, shared) in &ordered {
        if label.is_none() {
            // 題名の日付は、対象の書かれていない題名でも使う (`10月30日メンテナンス延長終了`)。
            label = fact.label_date.clone();
        }
        if *shared {
            planned.start.fill(fact.planned.start.clone());
            actual.start.fill(fact.actual.start.clone());
            planned.start.fill(fact.neutral.start.clone());
            continue;
        }
        let had_end = planned.end.is_set();
        planned.fill(&fact.planned);
        actual.fill(&fact.actual);
        let neutral_is_actual = disposition == Disposition::Completed
            && !fact.title
            && completion_rank.is_some_and(|completion| rank(fact) >= completion)
            && fact.status != Some(Status::Announced);
        if neutral_is_actual {
            actual.fill(&fact.neutral);
        } else {
            planned.fill(&fact.neutral);
        }
        if !had_end && planned.end.is_set() {
            planned_end_from = Some((regions.written(fact), fact.extended));
        }
        if duration.is_none() {
            duration = fact.duration;
        }
    }
    // 予定の終わりがなく、所要時間があれば概算の終わり。
    if !planned.end.is_set()
        && let (Some(minutes), Some(start)) = (duration, planned.start.value())
        && let Some(end) = start.plus_minutes(minutes)
    {
        let evidence = match &planned.start {
            Slot::Value(_, evidence) => evidence.clone(),
            _ => unreachable!(),
        };
        planned.end = Slot::Value(end, evidence);
    }
    // 題名の日付 (`10/30 メンテナンス完了のお知らせ`)。投稿日と同じなら完了の日、違えばメンテナンスの予定の日
    // (`10月30日メンテナンス延長終了のお知らせ` が 11/4 に出たなら、10/30 は終わった日ではない)。
    if let Some((date, evidence)) = label {
        if date.date == reference {
            if disposition == Disposition::Completed && !actual.end.is_set() {
                actual.end = Slot::Value(date, evidence);
            }
        } else if !planned.start.is_set() && !actual.start.is_set() {
            planned.start = Slot::Value(date, evidence);
        }
    }

    // 変更 (書かれた順に並べるため、書かれた順の鍵を添える)。
    let mut changes: Vec<((u32, usize, usize), Change)> = Vec::new();

    // 日程変更。変更の前を古い順に並べ、それぞれの変更の後は次の変更の前 (最後は今の予定)。
    let chain = reschedule_chain(befores, regions);
    for (index, before) in chain.iter().enumerate() {
        let to = chain
            .get(index + 1)
            .map_or_else(|| planned.clone(), |next| next.slots.clone());
        let mut evidence = before.evidence.clone();
        if let Some(item) = first_evidence(&to)
            && !evidence.contains(&item)
        {
            evidence.push(item);
        }
        changes.push((
            (regions.rank(before.region), before.position, before.order),
            Change::Rescheduled {
                from: window(&before.slots, resolver),
                to: window(&to, resolver),
                evidence,
            },
        ));
    }
    if disposition == Disposition::Postponed && !planned.is_empty() {
        // 延期先が書かれている。
        disposition = Disposition::Scheduled;
    }

    // 延長。対象の書かれていない本文の延長 (`17:00より延長しておりましたメンテナンス`) は、名前のある
    // 対象が 1 つだけなら、その対象の延長として読む。
    let extended: Vec<&MaintFact> = ordered
        .iter()
        .filter(|(fact, shared)| fact.extended && (!shared || (single_named && !fact.title)))
        .map(|(fact, _)| *fact)
        .collect();
    let mut extensions = extensions(&extended, &ordered, regions);
    if let Some(first) = extensions.first_mut()
        && !first.from.is_set()
    {
        // 延長する前の終わりは、延長より前の記述の予定の終わり。
        let key = first.key;
        let new_end = first.end.value().map(Local::naive);
        first.from = ordered
            .iter()
            .filter(|(fact, _)| regions.written(fact) <= key)
            .flat_map(|(fact, _)| {
                [&fact.planned.end, &fact.neutral.end]
                    .into_iter()
                    .map(move |slot| (regions.written(fact), slot))
            })
            .filter(|(_, slot)| {
                slot.value()
                    .is_some_and(|local| Some(local.naive()) != new_end)
            })
            .max_by_key(|(key, _)| *key)
            .map_or(Slot::Unspecified, |(_, slot)| inferred(slot));
    }
    if let Some(last) = extensions.last_mut() {
        let key = last.key;
        if last.end.is_set() {
            planned.end = last.end.clone();
        } else if planned_end_from.is_some_and(|(from, extension)| from >= key && !extension) {
            // 延長より後に書かれた予定の終わり (`終了は19:00を予定しております`) が、延長した後の終わり。
            last.end = planned.end.clone();
        } else if disposition != Disposition::Completed {
            // 延長して終わりが書かれていない。もとの予定の終わりはもう当てにならない。
            planned.end = Slot::Unspecified;
        }
    }
    for extension in extensions {
        changes.push((
            extension.key,
            Change::Extended {
                from: core_slot(&extension.from, resolver),
                end: core_slot(&extension.end, resolver),
                evidence: extension.evidence,
            },
        ));
    }
    changes.sort_by_key(|(key, _)| *key);
    let changes: Vec<Change> = changes.into_iter().map(|(_, change)| change).collect();

    // 行っている途中のメンテナンス (`14:00より行っております`) は、予定の開始が書かれていなくても
    // 書かれた実際の開始を予定の開始とする (画面の「メンテ中」は予定の時間帯で決める)。
    if matches!(disposition, Disposition::Scheduled) && !planned.start.is_set() {
        planned.start = actual.start.clone();
    }
    let planned = window(&planned, resolver);
    let actual = window(&actual, resolver);
    // 開始より前の時点の「実施中」は、始まる前の予告 (`終了時間を19:00に延長いたします`) なので使わない。
    let ongoing = if disposition == Disposition::Scheduled {
        ongoing(&ordered, &status_facts, regions, resolver).filter(|ongoing| {
            planned
                .start
                .value()
                .or(actual.start.value())
                .is_none_or(|start| !before_start(&ongoing.as_of.when, &start.when))
        })
    } else {
        None
    };
    let mut evidence: Vec<Evidence> = Vec::new();
    for shared_pass in [false, true] {
        for (fact, shared) in &ordered {
            if *shared != shared_pass {
                continue;
            }
            for item in &fact.evidence {
                if !evidence.contains(item) {
                    evidence.push(item.clone());
                }
            }
        }
    }
    evidence.truncate(4);
    let has_content = !planned.is_unspecified()
        || !actual.is_unspecified()
        || !changes.is_empty()
        || !status_facts.is_empty();
    if !has_content {
        return None;
    }
    Some(Maintenance {
        scope: entity.scope.clone(),
        planned,
        actual,
        disposition,
        ongoing,
        changes,
        evidence,
    })
}

/// 日程変更の前の時間帯を古い順に並べる (同じ開始のものは 1 つにまとめる)。
fn reschedule_chain(befores: &[&Before], regions: &Regions) -> Vec<Before> {
    let mut sorted: Vec<&Before> = befores.to_vec();
    sorted.sort_by_key(|before| (regions.rank(before.region), before.position, before.order));
    let mut chain: Vec<Before> = Vec::new();
    for before in sorted {
        let duplicate = chain.iter_mut().find(|known| {
            match (known.slots.start.value(), before.slots.start.value()) {
                (Some(a), Some(b)) => same_start(Some(a), Some(b)),
                (None, None) => same_slots(&known.slots, &before.slots),
                _ => false,
            }
        });
        match duplicate {
            Some(known) => {
                // 時刻のある方を採る (`10月30日に予定していた` と `【変更前】10月30日 14:00～17:00`)。
                if known
                    .slots
                    .start
                    .value()
                    .is_some_and(|local| local.time.is_none())
                    && before
                        .slots
                        .start
                        .value()
                        .is_some_and(|local| local.time.is_some())
                {
                    known.slots.start = before.slots.start.clone();
                }
                known.slots.fill(&before.slots);
            }
            None => chain.push(before.clone()),
        }
    }
    chain
}

/// 延長を区画ごとに 1 つにまとめる (古い区画から)。題名の延長は、本文に延長の記述がないときだけ。
/// 後の区画で同じ延長を述べ直したもの (`延長しておりましたメンテナンスが完了`) は足さない。
fn extensions(
    extended: &[&MaintFact],
    ordered: &[(&MaintFact, bool)],
    regions: &Regions,
) -> Vec<Extension> {
    let body: Vec<&MaintFact> = extended
        .iter()
        .copied()
        .filter(|fact| !fact.title)
        .collect();
    let source: Vec<&MaintFact> = if body.is_empty() {
        extended.to_vec()
    } else {
        body
    };
    // 区画ごとの、いちばん前の延長の記述の鍵。
    let mut keys: Vec<((u32, usize, usize), usize)> = Vec::new();
    for fact in &source {
        let key = regions.written(fact);
        match keys.iter_mut().find(|(_, region)| *region == fact.region) {
            Some((known, _)) => *known = (*known).min(key),
            None => keys.push((key, fact.region)),
        }
    }
    keys.sort();
    let mut out: Vec<Extension> = Vec::new();
    for (key, region) in keys {
        let mut group: Vec<&MaintFact> = source
            .iter()
            .copied()
            .filter(|fact| fact.region == region)
            .collect();
        group.sort_by_key(|fact| regions.written(fact));
        let end = group
            .iter()
            .map(|fact| fact.extended_end.clone())
            .find(Slot::is_set)
            .or_else(|| {
                // 延長の記述と同じ区画に `▼終了時間 未定` があれば、延長した後の終わりは未定。
                ordered
                    .iter()
                    .filter(|(fact, shared)| !shared && fact.region == region)
                    .flat_map(|(fact, _)| [fact.planned.end.clone(), fact.neutral.end.clone()])
                    .find(|slot| *slot == Slot::Undecided)
            })
            .unwrap_or(Slot::Unspecified);
        let written_from = group
            .iter()
            .map(|fact| fact.extended_from.clone())
            .find(Slot::is_set)
            .unwrap_or(Slot::Unspecified);
        if let Some(last) = out.last()
            && !written_from.is_set()
            && (!end.is_set() || same_slot(&end, &last.end))
        {
            continue;
        }
        let from = if written_from.is_set() {
            written_from
        } else {
            out.last()
                .map_or(Slot::Unspecified, |last| inferred(&last.end))
        };
        out.push(Extension {
            key,
            from,
            end,
            evidence: group[0].evidence.clone(),
        });
    }
    out
}

/// 実施中の報告。いちばん新しい「実施中」の記述の、区画の追記の日時 (なければ記事の日時) を時点にする。
/// 延期・完了などの記述がそれより新しい区画にあれば、実施中の報告は古いので使わない。
fn ongoing(
    ordered: &[(&MaintFact, bool)],
    status_facts: &[&MaintFact],
    regions: &Regions,
    resolver: &mut Resolver,
) -> Option<Ongoing> {
    let rank = |fact: &MaintFact| regions.rank(fact.region);
    let settled = status_facts
        .iter()
        .filter(|fact| {
            matches!(
                fact.status,
                Some(Status::Completed | Status::Cancelled | Status::Postponed)
            )
        })
        .map(|fact| rank(fact))
        .max();
    // 対象の書かれていない文脈の記述は、その件の状態の記述より新しい区画にあるときだけ。
    let own_rank = status_facts.iter().map(|fact| rank(fact)).max();
    let fact = ordered
        .iter()
        .filter(|(fact, shared)| {
            fact.ongoing && (!shared || own_rank.is_none_or(|own| rank(fact) > own))
        })
        .map(|(fact, _)| *fact)
        .filter(|fact| settled.is_none_or(|settled| rank(fact) >= settled))
        .max_by_key(|fact| (rank(fact), !fact.title, Reverse(regions.written(fact))))?;
    let as_of = match regions.date(fact.region) {
        Some((local, evidence)) if !fact.title => {
            let when = resolver.when(local, std::slice::from_ref(evidence))?;
            TimeValue {
                when,
                certainty: Certainty::Explicit,
                evidence: vec![evidence.clone()],
            }
        }
        _ => TimeValue {
            when: resolver.article_moment(),
            certainty: Certainty::Inferred,
            evidence: Vec::new(),
        },
    };
    Some(Ongoing {
        as_of,
        evidence: fact.evidence.iter().take(2).cloned().collect(),
    })
}

/// 時点 `at` が開始 `start` より前か (どちらかが日付だけなら日付で比べる)。
fn before_start(at: &When, start: &When) -> bool {
    match (at, start) {
        (When::Instant { at, .. }, When::Instant { at: start, .. }) => at < start,
        _ => local_date(at) < local_date(start),
    }
}

/// 地域の日付。
fn local_date(when: &When) -> NaiveDate {
    match when {
        When::Date { date, .. } => *date,
        When::Instant { at, zone } => match zone {
            Zone::Named(name) => name
                .parse::<chrono_tz::Tz>()
                .map_or(at.date_naive(), |tz| at.with_timezone(&tz).date_naive()),
            Zone::Offset(seconds) => chrono::FixedOffset::east_opt(*seconds)
                .map_or(at.date_naive(), |offset| {
                    at.with_timezone(&offset).date_naive()
                }),
        },
    }
}

fn window(slots: &Slots, resolver: &mut Resolver) -> Window {
    Window {
        start: core_slot(&slots.start, resolver),
        end: core_slot(&slots.end, resolver),
    }
}

fn core_slot(slot: &Slot, resolver: &mut Resolver) -> CoreSlot<TimeValue> {
    match slot {
        Slot::Unspecified => CoreSlot::Unspecified,
        Slot::Undecided => CoreSlot::Undecided,
        Slot::Value(local, evidence) => {
            match resolver.when(local, std::slice::from_ref(evidence)) {
                Some(when) => CoreSlot::Value(TimeValue {
                    when,
                    certainty: local.certainty,
                    evidence: vec![evidence.clone()],
                }),
                None => CoreSlot::Unspecified,
            }
        }
    }
}

/// 期間をまとめる。変更前・取り消し線の期間は、同じ区画の近くの変更後の期間と組にして変更にする。
/// 前の終わりが書かれていない延長 (`販売期間を9月24日11:59まで延長しました`) は、同じ期間の前の記述の
/// 終わりから推す。変更で置き換わった記述は期間にしない。
fn build_periods(
    facts: Vec<PeriodFact>,
    regions: &Regions,
    resolver: &mut Resolver,
) -> Vec<Period> {
    let mut facts = facts;
    facts.sort_by_key(|fact| fact.position);
    let (befores, mut current): (Vec<PeriodFact>, Vec<PeriodFact>) =
        facts.into_iter().partition(|fact| fact.before);
    // 書かれた順の鍵 (同じ単位の中は集めた順。`current` は位置の順に並べてある)。
    let key = |index: usize, fact: &PeriodFact| (regions.rank(fact.region), fact.position, index);
    let open_extension = |fact: &PeriodFact| {
        matches!(
            fact.changes.as_slice(),
            [PeriodChange::Extended {
                from: Slot::Unspecified
            }]
        )
    };

    // 変更前と変更後の組。
    let mut paired = vec![false; current.len()];
    for before in &befores {
        let found = current
            .iter()
            .enumerate()
            .filter(|(index, fact)| {
                !paired[*index]
                    && fact.kind == before.kind
                    && fact.region == before.region
                    && (fact.changes.is_empty() || open_extension(fact))
                    && (fact.after || fact.position.abs_diff(before.position) <= 2)
            })
            .min_by_key(|(_, fact)| (!fact.after, fact.position.abs_diff(before.position)))
            .map(|(index, _)| index);
        let Some(index) = found else {
            continue;
        };
        paired[index] = true;
        let after = &mut current[index];
        let change = facts::change_between(
            &before.window,
            &after.window,
            before.extension || after.extension,
        );
        if !after.window.start.is_set() {
            after.window.start = before.window.start.clone();
        }
        after.changes = vec![change];
        for item in &before.evidence {
            if !after.evidence.contains(item) {
                after.evidence.push(item.clone());
            }
        }
    }

    // 前の終わりが書かれていない延長は、同じ期間の前の記述の終わりから推す。
    let mut superseded = vec![false; current.len()];
    for index in 0..current.len() {
        if !open_extension(&current[index]) {
            continue;
        }
        let this = &current[index];
        let end = this.window.end.value().map(Local::naive);
        let older = current
            .iter()
            .enumerate()
            .filter(|(other, fact)| {
                *other != index
                    && !superseded[*other]
                    && fact.changes.is_empty()
                    && fact.kind == this.kind
                    && fact.label == this.label
                    && key(*other, fact).0 <= key(index, this).0
                    && (key(*other, fact).0, key(*other, fact).1)
                        <= (key(index, this).0, key(index, this).1)
                    && fact
                        .window
                        .end
                        .value()
                        .is_some_and(|local| end.is_some_and(|end| local.naive() < end))
                    && starts_meet(&fact.window.start, &this.window.start)
            })
            .max_by_key(|(other, fact)| key(*other, fact))
            .map(|(other, _)| other);
        if let Some(other) = older {
            let from = inferred(&current[other].window.end);
            let start = current[other].window.start.clone();
            superseded[other] = true;
            let this = &mut current[index];
            this.changes = vec![PeriodChange::Extended { from }];
            this.window.start.fill(start);
        }
    }
    // 変更の前と同じ期間の古い記述は、置き換わったもの。
    for index in 0..current.len() {
        if current[index].changes.is_empty() {
            continue;
        }
        for other in 0..current.len() {
            if other == index || superseded[other] || !current[other].changes.is_empty() {
                continue;
            }
            let (this, fact) = (&current[index], &current[other]);
            if fact.kind != this.kind || key(other, fact) > key(index, this) {
                continue;
            }
            let replaced = this.changes.iter().any(|change| match change {
                PeriodChange::Rescheduled { from } => {
                    !from.is_empty()
                        && same_slot(&fact.window.start, &from.start)
                        && same_slot(&fact.window.end, &from.end)
                }
                PeriodChange::Extended { from } => {
                    from.value().is_some()
                        && same_slot(&fact.window.end, from)
                        && starts_meet(&fact.window.start, &this.window.start)
                }
            });
            if replaced {
                superseded[other] = true;
                let start = current[other].window.start.clone();
                current[index].window.start.fill(start);
            }
        }
    }

    let mut out: Vec<Period> = Vec::new();
    for (fact, superseded) in current.into_iter().zip(superseded) {
        if superseded {
            continue;
        }
        let window = window(&fact.window, resolver);
        if window.is_unspecified() {
            continue;
        }
        let changes = period_changes(&fact, &window, resolver);
        // 同じ期間の記述は 1 つにまとめる。変更の前が違えば別の期間 (`～10/21` と `～10/22` をどちらも
        // `～10/24` へ延長した、別々の受け取り期限)。
        let same_changes = |period: &Period| {
            period.changes.is_empty()
                || changes.is_empty()
                || (period.changes.len() == changes.len()
                    && period
                        .changes
                        .iter()
                        .zip(&changes)
                        .all(|(a, b)| same_change(a, b)))
        };
        if let Some(existing) = out.iter_mut().find(|period| {
            period.kind == fact.kind && period.window == window && same_changes(period)
        }) {
            for item in fact.evidence {
                if !existing.evidence.contains(&item) && existing.evidence.len() < 4 {
                    existing.evidence.push(item);
                }
            }
            if existing.label.is_none() {
                existing.label = fact.label;
            }
            for change in changes {
                if !existing
                    .changes
                    .iter()
                    .any(|known| same_change(known, &change))
                {
                    existing.changes.push(change);
                }
            }
            continue;
        }
        out.push(Period {
            kind: fact.kind,
            label: fact.label,
            window,
            changes,
            evidence: fact.evidence,
        });
    }
    out
}

/// 期間の記述の変更を `Change` にする (変更の後は記述の期間)。
fn period_changes(fact: &PeriodFact, after: &Window, resolver: &mut Resolver) -> Vec<Change> {
    let evidence: Vec<Evidence> = fact.evidence.iter().take(2).cloned().collect();
    fact.changes
        .iter()
        .map(|change| match change {
            PeriodChange::Extended { from } => Change::Extended {
                from: core_slot(from, resolver),
                end: after.end.clone(),
                evidence: evidence.clone(),
            },
            PeriodChange::Rescheduled { from } => Change::Rescheduled {
                from: window(from, resolver),
                to: after.clone(),
                evidence: evidence.clone(),
            },
        })
        .collect()
}

/// 変更が同じか (根拠は比べない)。
fn same_change(a: &Change, b: &Change) -> bool {
    match (a, b) {
        (
            Change::Extended {
                from: a_from,
                end: a_end,
                ..
            },
            Change::Extended {
                from: b_from,
                end: b_end,
                ..
            },
        ) => same_value(a_from, b_from) && same_value(a_end, b_end),
        (
            Change::Rescheduled {
                from: a_from,
                to: a_to,
                ..
            },
            Change::Rescheduled {
                from: b_from,
                to: b_to,
                ..
            },
        ) => {
            same_value(&a_from.start, &b_from.start)
                && same_value(&a_from.end, &b_from.end)
                && same_value(&a_to.start, &b_to.start)
                && same_value(&a_to.end, &b_to.end)
        }
        _ => false,
    }
}

fn same_value(a: &CoreSlot<TimeValue>, b: &CoreSlot<TimeValue>) -> bool {
    match (a, b) {
        (CoreSlot::Value(a), CoreSlot::Value(b)) => a.when == b.when,
        (CoreSlot::Value(_), _) | (_, CoreSlot::Value(_)) => false,
        (a, b) => a == b,
    }
}
