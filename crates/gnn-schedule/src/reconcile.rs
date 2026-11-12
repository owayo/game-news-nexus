//! 記事どうしの突き合わせ。別の記事で報告された完了・中止・延期・実施中・延長・日程変更を、予告の記事の
//! メンテナンスにも反映する。
//!
//! 結び付けるのは、次の条件がそろうときだけ (日付が近いだけでは結び付けない)。
//! - 報告の記事が、予告の記事より後 (投稿日時、同じなら初めて見つけた日時、記事の番号の順)。
//! - 対象: 対象の書かれていない報告はどの対象の予告にも当て、名前のある報告は同じ名前の予告にだけ当てる
//!   (`PC版` の完了は、対象の書かれていない予告や `iOS版` の予告に当てない)。
//! - 時: 開始の日時が同じ (時刻まで分かれば時刻まで、片方が日付だけなら日付)。報告に開始がなく
//!   実際の終わりだけなら、予告の開始か終わりと同じ日。
//! - 報告の対象・時に合う、それより前の記事のメンテナンスの開始が 1 つだけ。同じメンテナンスを
//!   告知した記事 (予告と再告知) と、対象ごとに分けた件 (`iOS版` と `Android版`) はどれも結び付ける。
//!
//! 実施中の報告は、予告自身と結び付けた報告のうち時点のいちばん新しいもの (結果が予定どおりのときだけ)。
//! 別の記事の延長で前の終わりが書かれていないものは、予告のそれまでの予定の終わりを推定の値として入れる。
//!
//! 記事の数が数百でも 2 乗で遅くならないよう、開始・終わりの日付で引く索引から候補を探す。

use std::collections::{BTreeSet, HashMap};

use chrono::{DateTime, NaiveDate, Utc};
use gnn_core::schedule::{
    Certainty, Change, Disposition, Maintenance, Ongoing, Scope, Slot, TimeValue, When, Window,
};

use crate::{Resolved, SourceArticle};

struct Item<'a> {
    entry_id: i64,
    index: usize,
    maintenance: &'a Maintenance,
    order: (DateTime<Utc>, DateTime<Utc>, i64),
}

/// 同じソースの記事の読み取り結果を突き合わせる。返す件数は入力のメンテナンスの件数と同じ。
pub(crate) fn reconcile(articles: &[SourceArticle<'_>]) -> Vec<Resolved> {
    let items: Vec<Item<'_>> = articles
        .iter()
        .flat_map(|article| {
            let order = (
                article
                    .published
                    .map_or(article.first_seen_at, |published| published.at),
                article.first_seen_at,
                article.entry_id,
            );
            article
                .schedule
                .maintenance
                .iter()
                .enumerate()
                .map(move |(index, maintenance)| Item {
                    entry_id: article.entry_id,
                    index,
                    maintenance,
                    order,
                })
        })
        .collect();

    // 開始の日付・終わりの日付で引く索引。
    let mut by_start: HashMap<NaiveDate, Vec<usize>> = HashMap::new();
    let mut by_end: HashMap<NaiveDate, Vec<usize>> = HashMap::new();
    for (index, item) in items.iter().enumerate() {
        if let Some(start) = plan_start(item.maintenance) {
            by_start.entry(local_date(start)).or_default().push(index);
        }
        if let Some(end) = plan_end(item.maintenance) {
            by_end.entry(local_date(end)).or_default().push(index);
        }
    }

    // 報告ごとに、当てる予告を探す。
    let mut applied: Vec<Vec<usize>> = vec![Vec::new(); items.len()];
    for (report_index, report) in items.iter().enumerate() {
        if !is_report(report.maintenance) {
            continue;
        }
        let mut candidates: Vec<usize> = Vec::new();
        let push = |indices: Option<&Vec<usize>>, candidates: &mut Vec<usize>| {
            for &index in indices.into_iter().flatten() {
                if !candidates.contains(&index) {
                    candidates.push(index);
                }
            }
        };
        let starts = report_starts(report.maintenance);
        if starts.is_empty() {
            if let Some(end) =
                value(&report.maintenance.actual.end).or(value(&report.maintenance.planned.end))
            {
                let date = local_date(end);
                push(by_start.get(&date), &mut candidates);
                push(by_end.get(&date), &mut candidates);
            }
        } else {
            for start in starts {
                push(by_start.get(&local_date(start)), &mut candidates);
            }
        }
        candidates.retain(|&index| {
            let plan = &items[index];
            plan.entry_id != report.entry_id
                && plan.order < report.order
                && matches(plan.maintenance, report.maintenance).is_some()
        });
        // 対象の書かれていない報告は、対象の書かれていない予告を優先する (`Android版のみ延長` の件に、
        // 対象を書かない「終了しました」を当てない)。
        let named = |scope: &Scope| matches!(scope, Scope::Named(_));
        if candidates.iter().any(|&index| {
            named(&items[index].maintenance.scope) == named(&report.maintenance.scope)
        }) {
            candidates.retain(|&index| {
                named(&items[index].maintenance.scope) == named(&report.maintenance.scope)
            });
        }
        if candidates.is_empty() || !unique_identity(&candidates, &items) {
            continue;
        }
        for index in candidates {
            applied[index].push(report_index);
        }
    }

    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let mut reports: Vec<&Item<'_>> = applied[index]
                .iter()
                .map(|&report| &items[report])
                .collect();
            reports.sort_by_key(|report| report.order);
            fold(item, &reports)
        })
        .collect()
}

/// 状態・変更・実施中を報告している (予定の再告知だけではない)。
fn is_report(maintenance: &Maintenance) -> bool {
    maintenance.disposition != Disposition::Scheduled
        || !maintenance.changes.is_empty()
        || maintenance.ongoing.is_some()
}

/// 結び付きの強さ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Match {
    /// 開始の時刻が同じ。
    Exact,
    /// 日付が同じ (片方が日付だけ、または報告に開始がない)。
    Date,
}

/// 予告の開始 (予定、なければ実際)。
fn plan_start(maintenance: &Maintenance) -> Option<&TimeValue> {
    value(&maintenance.planned.start).or(value(&maintenance.actual.start))
}

fn plan_end(maintenance: &Maintenance) -> Option<&TimeValue> {
    value(&maintenance.planned.end).or(value(&maintenance.actual.end))
}

/// 報告の開始 (実際・予定・日程変更の前)。
fn report_starts(report: &Maintenance) -> Vec<&TimeValue> {
    let mut starts: Vec<&TimeValue> = [&report.actual.start, &report.planned.start]
        .into_iter()
        .filter_map(value)
        .collect();
    for change in &report.changes {
        if let Change::Rescheduled { from, .. } = change
            && let Some(start) = value(&from.start)
        {
            starts.push(start);
        }
    }
    starts
}

/// 予告 `plan` に報告 `report` が当たるか。
fn matches(plan: &Maintenance, report: &Maintenance) -> Option<Match> {
    if !covers(&report.scope, &plan.scope) {
        return None;
    }
    let plan_start = plan_start(plan)?;
    let starts = report_starts(report);
    if !starts.is_empty() {
        let mut best = None;
        for start in starts {
            match same_time(plan_start, start) {
                Some(Match::Exact) => return Some(Match::Exact),
                Some(Match::Date) => best = Some(Match::Date),
                None => {}
            }
        }
        return best;
    }
    // 開始のない報告 (`16:30に完了いたしました`) は、予告の開始か終わりと同じ日。
    let report_end = value(&report.actual.end).or(value(&report.planned.end))?;
    let end_date = local_date(report_end);
    let same_day = local_date(plan_start) == end_date
        || plan_end(plan).is_some_and(|end| local_date(end) == end_date);
    same_day.then_some(Match::Date)
}

/// 報告の対象が予告の対象を含む。
fn covers(report: &Scope, plan: &Scope) -> bool {
    match (report, plan) {
        (Scope::Unknown | Scope::All, _) => true,
        (Scope::Named(_), Scope::Unknown | Scope::All) => false,
        (Scope::Named(report), Scope::Named(plan)) => report == plan,
    }
}

fn value(slot: &Slot<TimeValue>) -> Option<&TimeValue> {
    slot.value()
}

fn local_date(value: &TimeValue) -> NaiveDate {
    match &value.when {
        When::Date { date, .. } => *date,
        When::Instant { at, zone } => match zone {
            gnn_core::schedule::Zone::Named(name) => match name.parse::<chrono_tz::Tz>() {
                Ok(tz) => at.with_timezone(&tz).date_naive(),
                Err(_) => at.date_naive(),
            },
            gnn_core::schedule::Zone::Offset(seconds) => chrono::FixedOffset::east_opt(*seconds)
                .map_or(at.date_naive(), |offset| {
                    at.with_timezone(&offset).date_naive()
                }),
        },
    }
}

fn same_time(a: &TimeValue, b: &TimeValue) -> Option<Match> {
    match (&a.when, &b.when) {
        (When::Instant { at: a, .. }, When::Instant { at: b, .. }) => {
            (a == b).then_some(Match::Exact)
        }
        _ => (local_date(a) == local_date(b)).then_some(Match::Date),
    }
}

/// 候補の予告が指すメンテナンスの開始が 1 つだけか。日付だけの値と時刻のある値が同じ日に混ざるのは、
/// 同じメンテナンスを日付だけで書いた記事と時刻まで書いた記事なので、時刻のある開始が 1 つまでなら
/// 1 つと数える。対象は比べない (候補は報告の対象に合うものだけで、対象ごとに分けた件は同じ
/// メンテナンスの別の対象)。
fn unique_identity(candidates: &[usize], items: &[Item<'_>]) -> bool {
    let mut dates: BTreeSet<NaiveDate> = BTreeSet::new();
    let mut instants: BTreeSet<DateTime<Utc>> = BTreeSet::new();
    for &index in candidates {
        let Some(start) = plan_start(items[index].maintenance) else {
            continue;
        };
        dates.insert(local_date(start));
        if let When::Instant { at, .. } = start.when {
            instants.insert(at);
        }
    }
    dates.len() <= 1 && instants.len() <= 1
}

/// 実施中の時点の新しさ (日付、同じ日なら時刻のあるものを新しいとする)。
fn as_of_key(ongoing: &Ongoing) -> (NaiveDate, Option<DateTime<Utc>>) {
    let value = &ongoing.as_of;
    let at = match value.when {
        When::Instant { at, .. } => Some(at),
        When::Date { .. } => None,
    };
    (local_date(value), at)
}

fn fold(item: &Item<'_>, reports: &[&Item<'_>]) -> Resolved {
    let own = item.maintenance;
    let mut disposition = own.disposition;
    let mut planned = own.planned.clone();
    let mut actual = own.actual.clone();
    let mut changes = own.changes.clone();
    let mut ongoing = own.ongoing.clone();
    let mut resolved_by = None;
    for report in reports {
        let other = report.maintenance;
        let mut changed = false;
        if let Some(reported) = &other.ongoing
            && ongoing
                .as_ref()
                .is_none_or(|known| as_of_key(reported) >= as_of_key(known))
            && ongoing.as_ref() != Some(reported)
        {
            ongoing = Some(reported.clone());
            changed = true;
        }
        // 新しい報告の結果を採る (完了したものを、後の延長の再告知で予定に戻さない)。
        if (other.disposition != Disposition::Scheduled || disposition != Disposition::Completed)
            && other.disposition != disposition
        {
            disposition = other.disposition;
            changed = true;
        }
        for change in &other.changes {
            let mut change = change.clone();
            match &mut change {
                Change::Extended { from, end, .. } => {
                    // 前の終わりが書かれていない延長は、予告のそれまでの予定の終わりから推す。
                    if *from == Slot::Unspecified
                        && let Slot::Value(value) = &planned.end
                        && !same_when(&planned.end, end)
                    {
                        *from = Slot::Value(TimeValue {
                            certainty: Certainty::Inferred,
                            ..value.clone()
                        });
                    }
                    if *end != Slot::Unspecified && planned.end != *end {
                        planned.end = end.clone();
                        changed = true;
                    }
                }
                Change::Rescheduled { .. } => {
                    if !other.planned.is_unspecified() && planned != other.planned {
                        planned = other.planned.clone();
                        changed = true;
                    }
                }
            }
            // 別の記事の延長・日程変更は、書かれた順に足す (同じものは 1 つだけ)。
            if !changes.iter().any(|known| same_change(known, &change)) {
                changes.push(change);
                changed = true;
            }
        }
        changed |= fill(&mut actual, &other.actual);
        if changed {
            resolved_by = Some(report.entry_id);
        }
    }
    if disposition != Disposition::Scheduled {
        ongoing = None;
    }
    Resolved {
        entry_id: item.entry_id,
        index: item.index,
        disposition,
        planned,
        actual,
        changes,
        ongoing,
        resolved_by,
    }
}

/// 変更が同じか (根拠と、推定で入れた前の終わりは比べない)。
fn same_change(a: &Change, b: &Change) -> bool {
    match (a, b) {
        (Change::Extended { end: a_end, .. }, Change::Extended { end: b_end, .. }) => {
            same_when(a_end, b_end)
        }
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
            same_when(&a_from.start, &b_from.start)
                && same_when(&a_from.end, &b_from.end)
                && same_when(&a_to.start, &b_to.start)
                && same_when(&a_to.end, &b_to.end)
        }
        _ => false,
    }
}

/// 日時が同じか (根拠と確かさは比べない)。
fn same_when(a: &Slot<TimeValue>, b: &Slot<TimeValue>) -> bool {
    match (a, b) {
        (Slot::Value(a), Slot::Value(b)) => a.when == b.when,
        (Slot::Value(_), _) | (_, Slot::Value(_)) => false,
        (a, b) => a == b,
    }
}

fn fill(window: &mut Window, other: &Window) -> bool {
    let mut changed = false;
    if window.start == Slot::Unspecified && other.start != Slot::Unspecified {
        window.start = other.start.clone();
        changed = true;
    }
    if window.end == Slot::Unspecified && other.end != Slot::Unspecified {
        window.end = other.end.clone();
        changed = true;
    }
    changed
}
