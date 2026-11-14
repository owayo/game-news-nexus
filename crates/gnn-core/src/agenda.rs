//! 予定の表 (同じソースの記事を突き合わせたあとのメンテナンスと、イベントなどの期間) と、
//! 画面に出す状態の計算。
//!
//! 保存するのは記事から読み取った予定と結果だけで、「今メンテ中か」「今日・明日の予定か」は
//! 保存せず、今の時刻と表示の地域時刻から毎回計算する。
//!
//! 決まり:
//! - 時間帯は `[開始, 終了)`。今日・明日は表示している地域時刻の暦日で区切る。
//! - 予定の時間内でも、完了の報告がなければ「(予定)」として出す。終了予定の時刻を過ぎても完了にしない。
//! - 日付だけの予定から「メンテ中」にしない。終了未定を、終わりのない確定した時間帯として扱わない。
//! - 所要時間から求めた終了 (概算) は「頃」を付けて、明記された終了と分ける。
//! - 開始の時刻がなく「実施中」と報告されたメンテナンス (結果が予定どおりのもの) は、報告の時点から
//!   終わりまで「メンテ中」。終わりが未定・書かれていなければ報告の時点から [`OPEN_FOR`] のあいだ
//!   「メンテ中 (終了未定)」。日付だけの報告からは「メンテ中」にしない。開始の時刻があれば報告は使わない。
//! - 延長と日程変更は前と後を並べて出す。期間の今日・明日は、変更の後の期間で決める。
//! - 同じ記事で、同じ時間帯・同じ状態の、対象だけが違うメンテナンスはバッジを 1 つにまとめる。

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use chrono::{
    DateTime, Datelike as _, Duration, FixedOffset, NaiveDate, NaiveTime, TimeZone, Timelike as _,
    Utc,
};
use serde::{Deserialize, Serialize};

use crate::entry::Published;
use crate::schedule::{
    Certainty, Change, Disposition, Ongoing, Period, PeriodKind, Schedule, Scope, Slot, TimeValue,
    When, Window, Zone,
};

/// 終了予定を過ぎてから「終了予定を過ぎています」と出す長さ。過ぎたら予定の時間帯だけを出す。
pub const OVERDUE_FOR: Duration = Duration::hours(12);
/// 終了が未定・書かれていないメンテナンスを、始まってから「終了未定」などとして扱う長さ。
/// 終了未定を、終わりのない時間帯にしないための区切り。
pub const OPEN_FOR: Duration = Duration::hours(24);
/// 予定の開始のどれだけ前から本文を取り直すか。
pub const RECHECK_AHEAD: Duration = Duration::days(7);
/// 予定の終わりのあと、どれだけのあいだ本文を取り直すか (完了・延長の追記を拾うため)。
pub const RECHECK_AFTER: Duration = Duration::hours(12);
/// 画面に読み込む予定の表の範囲。過ぎた予定はこれだけ前まで、先の予定はこれだけ先まで
/// ([`Store::agenda`](crate::store::Store::agenda) の `from`・`to`)。
pub const LOAD_BEHIND: Duration = Duration::days(3);
pub const LOAD_AHEAD: Duration = Duration::days(32);
/// 旧形式の `time_range` に書く、1 つの時間帯の日数の上限。
const LEGACY_MAX_DAYS: i64 = 14;

/// 予定の表の 1 行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgendaItem {
    pub entry_id: i64,
    pub source_id: String,
    pub detail: AgendaDetail,
}

/// 行の中身。どちらのバリアントも `Box` にして、行の大きさをそろえる (中身の JSON の形は変わらない)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgendaDetail {
    Maintenance(Box<ResolvedMaintenance>),
    Period(Box<AgendaPeriod>),
}

impl From<ResolvedMaintenance> for AgendaDetail {
    fn from(maintenance: ResolvedMaintenance) -> Self {
        AgendaDetail::Maintenance(Box::new(maintenance))
    }
}

impl From<AgendaPeriod> for AgendaDetail {
    fn from(period: AgendaPeriod) -> Self {
        AgendaDetail::Period(Box::new(period))
    }
}

/// 突き合わせたあとのメンテナンス 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedMaintenance {
    /// 記事の `Schedule::maintenance` の位置。
    pub index: usize,
    #[serde(default)]
    pub scope: Scope,
    /// 別の記事の報告も反映した結果。
    pub disposition: Disposition,
    /// 別の記事の延長・日程変更も反映した予定の時間帯。
    #[serde(default)]
    pub planned: Window,
    /// 別の記事の報告で分かったものも含む実際の時間帯。
    #[serde(default)]
    pub actual: Window,
    /// 延長や日程変更 (別の記事で報告されたものを含む)。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<Change>,
    /// 「実施中」の報告 (別の記事のものを含め、いちばん新しいもの)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ongoing: Option<Ongoing>,
    /// 結果や変更を報告した別の記事。この記事の記述だけで決まったなら `None`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolved_by: Option<i64>,
}

impl ResolvedMaintenance {
    /// 延長されたか。
    pub fn extended(&self) -> bool {
        is_extended(&self.changes)
    }

    /// 時刻の判断に使う時間帯。予定が書かれていなければ実際の時間帯。
    pub fn window(&self) -> &Window {
        if self.planned.is_unspecified() {
            &self.actual
        } else {
            &self.planned
        }
    }

    /// 実施中と報告された時点。結果が予定どおりのときだけ (完了・中止などの後に残った報告は使わない)。
    pub fn ongoing_report(&self) -> Option<&Ongoing> {
        self.ongoing
            .as_ref()
            .filter(|_| self.disposition == Disposition::Scheduled)
    }

    /// 実施中の報告の時点を時刻の範囲として見たもの。
    fn ongoing_bound(&self) -> Option<Bound> {
        self.ongoing_report()
            .map(|ongoing| Bound::of_value(&ongoing.as_of))
    }

    /// 実施中と報告された時刻。時刻まで分かるときだけ (日付だけの報告からは「メンテ中」にしない)。
    fn ongoing_at(&self) -> Option<DateTime<Utc>> {
        match self.ongoing_bound()? {
            Bound::At { at, .. } => Some(at),
            Bound::Day { .. } | Bound::Undecided | Bound::Unspecified => None,
        }
    }
}

/// 延長があるか。
fn is_extended(changes: &[Change]) -> bool {
    changes
        .iter()
        .any(|change| matches!(change, Change::Extended { .. }))
}

/// 期間 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgendaPeriod {
    /// 記事の `Schedule::periods` の位置。
    pub index: usize,
    pub period: Period,
}

/// 突き合わせる記事 1 件 (同じソースで、今の中身を読んだ結果があるもの)。
#[derive(Debug, Clone, PartialEq)]
pub struct SourceSchedule {
    pub entry_id: i64,
    pub published: Option<Published>,
    pub first_seen_at: DateTime<Utc>,
    pub schedule: Schedule,
}

/// 突き合わせの結果 (メンテナンス 1 件)。`gnn_schedule::Resolved` と同じ中身を持つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    pub entry_id: i64,
    pub index: usize,
    pub disposition: Disposition,
    pub planned: Window,
    pub actual: Window,
    /// 記事の延長・日程変更に、別の記事で報告されたものを足したもの (古い記事の順、記事の中は書かれた順)。
    pub changes: Vec<Change>,
    /// 「実施中」の報告 (記事のものと、結び付けた別の記事のもののうち、いちばん新しいもの)。
    pub ongoing: Option<Ongoing>,
    pub resolved_by: Option<i64>,
}

/// 読み取り結果と突き合わせの結果から、予定の表の行を作る。
///
/// 突き合わせの結果がないメンテナンスは、記事の記述のままにする。
pub fn build_items(
    source_id: &str,
    articles: &[SourceSchedule],
    resolutions: &[Resolution],
) -> Vec<AgendaItem> {
    let resolved: HashMap<(i64, usize), &Resolution> = resolutions
        .iter()
        .map(|resolution| ((resolution.entry_id, resolution.index), resolution))
        .collect();
    let mut items = Vec::new();
    for article in articles {
        for (index, maintenance) in article.schedule.maintenance.iter().enumerate() {
            let resolution = resolved.get(&(article.entry_id, index));
            items.push(AgendaItem {
                entry_id: article.entry_id,
                source_id: source_id.to_string(),
                detail: AgendaDetail::from(ResolvedMaintenance {
                    index,
                    scope: maintenance.scope.clone(),
                    disposition: resolution
                        .map_or(maintenance.disposition, |resolution| resolution.disposition),
                    planned: resolution.map_or_else(
                        || maintenance.planned.clone(),
                        |resolution| resolution.planned.clone(),
                    ),
                    actual: resolution.map_or_else(
                        || maintenance.actual.clone(),
                        |resolution| resolution.actual.clone(),
                    ),
                    changes: resolution.map_or_else(
                        || maintenance.changes.clone(),
                        |resolution| resolution.changes.clone(),
                    ),
                    ongoing: resolution.map_or_else(
                        || maintenance.ongoing.clone(),
                        |resolution| resolution.ongoing.clone(),
                    ),
                    resolved_by: resolution.and_then(|resolution| resolution.resolved_by),
                }),
            });
        }
        for (index, period) in article.schedule.periods.iter().enumerate() {
            items.push(AgendaItem {
                entry_id: article.entry_id,
                source_id: source_id.to_string(),
                detail: AgendaDetail::from(AgendaPeriod {
                    index,
                    period: period.clone(),
                }),
            });
        }
    }
    items
}

// ---- 開始・終了を時刻にする ----

/// 開始・終了の値を、時刻の範囲として見たもの。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Bound {
    /// 時刻まで分かる。`approximate` は所要時間などから求めた概算。
    At {
        at: DateTime<Utc>,
        approximate: bool,
    },
    /// 日付だけ。その地域の暦日の `[from, to)`。
    Day {
        date: NaiveDate,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    },
    /// 「未定」と書かれている。
    Undecided,
    /// 書かれていない。
    Unspecified,
}

impl Bound {
    pub fn of(slot: &Slot<TimeValue>) -> Self {
        match slot {
            Slot::Unspecified => Bound::Unspecified,
            Slot::Undecided => Bound::Undecided,
            Slot::Value(value) => Bound::of_value(value),
        }
    }

    /// 開始・終了でない日時の値 (実施中の報告の時点など)。
    pub fn of_value(value: &TimeValue) -> Self {
        match &value.when {
            When::Instant { at, .. } => Bound::At {
                at: *at,
                approximate: value.certainty == Certainty::Approximate,
            },
            When::Date { date, zone } => Bound::Day {
                date: *date,
                from: zone_midnight(zone, *date),
                to: zone_midnight(zone, *date + Duration::days(1)),
            },
        }
    }

    /// 範囲の始まり (日付だけならその日の 0 時)。
    fn first(self) -> Option<DateTime<Utc>> {
        match self {
            Bound::At { at, .. } => Some(at),
            Bound::Day { from, .. } => Some(from),
            Bound::Undecided | Bound::Unspecified => None,
        }
    }

    /// 範囲の終わり (日付だけならその翌日の 0 時)。
    fn last(self) -> Option<DateTime<Utc>> {
        match self {
            Bound::At { at, .. } => Some(at),
            Bound::Day { to, .. } => Some(to),
            Bound::Undecided | Bound::Unspecified => None,
        }
    }
}

/// `zone` の `date` の 0 時。夏時間の切り替えで 0 時がない地域は、その日の最初の時刻にする。
fn zone_midnight(zone: &Zone, date: NaiveDate) -> DateTime<Utc> {
    match zone {
        Zone::Named(name) => match name.parse::<chrono_tz::Tz>() {
            Ok(tz) => local_midnight(&tz, date),
            // 抽出器が書いた名前は解釈できるはず。読めなければアプリの既定の地域時刻で読む。
            Err(_) => local_midnight(&chrono_tz::Asia::Tokyo, date),
        },
        Zone::Offset(seconds) => match FixedOffset::east_opt(*seconds) {
            Some(offset) => local_midnight(&offset, date),
            None => local_midnight(&Utc, date),
        },
    }
}

/// `tz` の `date` の 0 時 (夏時間で 0 時がなければ、その日の最初にある時刻)。
pub fn local_midnight<Z: TimeZone>(tz: &Z, date: NaiveDate) -> DateTime<Utc> {
    let midnight = date.and_time(NaiveTime::MIN);
    (0..=3)
        .find_map(|hour| {
            tz.from_local_datetime(&(midnight + Duration::hours(hour)))
                .earliest()
        })
        .map(|at| at.with_timezone(&Utc))
        .unwrap_or_else(|| Utc.from_utc_datetime(&midnight))
}

/// 時刻の範囲 `[from, to)`。`from == to` は一時点。
pub type TimeRange = (DateTime<Utc>, DateTime<Utc>);

/// 時間帯の開始と終了。
fn bounds(window: &Window) -> (Bound, Bound) {
    (Bound::of(&window.start), Bound::of(&window.end))
}

/// 時間帯が占める範囲 `[from, to)`。`from == to` は一時点。
///
/// 終わりが未定・書かれていない時間帯は、始まりの時点 (日付だけならその日) だけにする。
fn occupied(window: &Window) -> Option<TimeRange> {
    match bounds(window) {
        (Bound::At { at: start, .. }, Bound::At { at: end, .. }) => Some((start, end.max(start))),
        (Bound::At { at: start, .. }, Bound::Day { to, .. }) => Some((start, to.max(start))),
        (Bound::At { at, .. }, Bound::Undecided | Bound::Unspecified) => Some((at, at)),
        (Bound::Day { from, to, .. }, Bound::At { at, .. }) => {
            Some((from, if at > from { at } else { to }))
        }
        (Bound::Day { from, to, .. }, Bound::Day { to: end, .. }) => Some((from, end.max(to))),
        (Bound::Day { from, to, .. }, Bound::Undecided | Bound::Unspecified) => Some((from, to)),
        (Bound::Undecided | Bound::Unspecified, Bound::At { at, .. }) => Some((at, at)),
        (Bound::Undecided | Bound::Unspecified, Bound::Day { from, to, .. }) => Some((from, to)),
        _ => None,
    }
}

/// 範囲 (一時点を含む) が日と重なるか。
fn overlaps(range: TimeRange, day: &Range<DateTime<Utc>>) -> bool {
    let (from, to) = range;
    if from == to {
        day.contains(&from)
    } else {
        from < day.end && day.start < to
    }
}

/// 時間帯に関わる時刻の範囲 (検索用)。予定・実際・実施中の報告・延長と日程変更の前後のすべてを含み、
/// 終わりの分からない時間帯は、開始 (開始がなければ実施中の報告の時点) から [`OPEN_FOR`] だけ先まで
/// 伸ばす。日時が 1 つもなければ `None`。
pub fn span(detail: &AgendaDetail) -> Option<TimeRange> {
    let mut windows: Vec<&Window> = Vec::new();
    let mut bounds_seen: Vec<Bound> = Vec::new();
    let changes = match detail {
        AgendaDetail::Maintenance(maintenance) => {
            windows.extend([&maintenance.planned, &maintenance.actual]);
            bounds_seen.extend(maintenance.ongoing_bound());
            &maintenance.changes
        }
        AgendaDetail::Period(period) => {
            windows.push(&period.period.window);
            &period.period.changes
        }
    };
    for change in changes {
        match change {
            Change::Extended { from, end, .. } => {
                bounds_seen.extend([Bound::of(from), Bound::of(end)])
            }
            Change::Rescheduled { from, to, .. } => windows.extend([from, to]),
        }
    }
    for window in &windows {
        let (start, end) = bounds(window);
        bounds_seen.extend([start, end]);
    }
    let points: Vec<DateTime<Utc>> = bounds_seen
        .iter()
        .flat_map(|bound| [bound.first(), bound.last()])
        .flatten()
        .collect();
    let from = *points.iter().min()?;
    let mut to = *points.iter().max()?;
    for window in &windows {
        if let (start, Bound::Undecided | Bound::Unspecified) = bounds(window)
            && let Some(start) = start.first()
        {
            to = to.max(start + OPEN_FOR);
        }
    }
    // 開始の時刻がなく実施中と報告されたメンテナンスは、報告の時点から終わりまで (終わりが分からなければ
    // OPEN_FOR) が、今メンテ中かを決める範囲になる。
    if let AgendaDetail::Maintenance(maintenance) = detail
        && let Some(at) = maintenance.ongoing_at()
        && matches!(
            bounds(maintenance.window()).1,
            Bound::Undecided | Bound::Unspecified
        )
    {
        to = to.max(at + OPEN_FOR);
    }
    Some((from, to))
}

// ---- 今の状態 ----

/// メンテナンスの、今の時刻での状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MaintenanceState {
    /// 予定の時間内 (完了の報告はまだない)。開始の時刻がなくても、実施中の報告の時点から終わりまでを含む。
    InWindow,
    /// 開始の時刻はないが実施中と報告され、終わりは未定か書かれていない (報告の時点から [`OPEN_FOR`] のあいだ)。
    ReportedOngoing,
    /// 始まったが、終了は未定 (延長で未定になったものを含む)。
    EndUndecided,
    /// 終了予定を過ぎたが、完了の報告がない。
    Overdue,
    /// 開始は書かれていないが、終了の予定がこれから。
    UntilEnd,
    /// 始まったが、終了が書かれていない。
    EndUnknown,
    /// 日付だけの予定の日 (時刻が分からないので「メンテ中」とはしない)。
    OnDate,
    /// これから行う。
    Upcoming,
    /// 記述が食い違うなどで決められない。
    Unresolved,
    /// 延期した。
    Postponed,
    /// 終わった (完了の報告がある)。
    Completed,
    /// 中止した。
    Cancelled,
    /// 予定の時刻を過ぎてしばらくたった (完了の報告はないまま)。
    Past,
    /// 時刻の分からない予定 (日時未定・記載なし)。
    Undated,
}

impl MaintenanceState {
    /// 一覧に 1 つだけ出すときの順 (小さいほど先)。宣言の順。
    fn rank(self) -> u8 {
        self as u8
    }
}

/// メンテナンスの今の状態。
pub fn maintenance_state(
    maintenance: &ResolvedMaintenance,
    now: DateTime<Utc>,
) -> MaintenanceState {
    match maintenance.disposition {
        Disposition::Completed => MaintenanceState::Completed,
        Disposition::Cancelled => MaintenanceState::Cancelled,
        Disposition::Postponed => MaintenanceState::Postponed,
        Disposition::Unresolved => MaintenanceState::Unresolved,
        Disposition::Scheduled => timing(maintenance, now),
    }
}

/// 時刻から決まる状態 (結果は見ない)。
///
/// 予定どおりのメンテナンスで開始の時刻がなく、時刻まで分かる実施中の報告があれば、報告の時点から
/// 実施中とみなす。終わりが書かれていれば終わりまで「予定の時間内」、未定・記載なしなら報告の時点から
/// [`OPEN_FOR`] のあいだ「実施中と報告されている」。開始の時刻があれば報告は使わない。
fn timing(maintenance: &ResolvedMaintenance, now: DateTime<Utc>) -> MaintenanceState {
    let window = maintenance.window();
    let state = time_state(window, now);
    let (start, end) = bounds(window);
    if matches!(start, Bound::At { .. }) {
        return state;
    }
    let Some(as_of) = maintenance.ongoing_at().filter(|as_of| now >= *as_of) else {
        return state;
    };
    match end {
        Bound::At { at: end, .. } | Bound::Day { to: end, .. } => {
            if now < end {
                MaintenanceState::InWindow
            } else {
                ended_state(end, now)
            }
        }
        Bound::Undecided | Bound::Unspecified => {
            if now < as_of + OPEN_FOR {
                MaintenanceState::ReportedOngoing
            } else {
                MaintenanceState::Past
            }
        }
    }
}

/// 終わったはずの時刻を過ぎた予定 (完了の報告はない)。しばらくは「終了予定を過ぎています」。
fn ended_state(end: DateTime<Utc>, now: DateTime<Utc>) -> MaintenanceState {
    if now < end + OVERDUE_FOR {
        MaintenanceState::Overdue
    } else {
        MaintenanceState::Past
    }
}

/// 予定どおりのメンテナンスの、時間帯だけから決まる状態。
fn time_state(window: &Window, now: DateTime<Utc>) -> MaintenanceState {
    let ended = |end: DateTime<Utc>| ended_state(end, now);
    match bounds(window) {
        (Bound::At { at: start, .. }, end) => {
            if now < start {
                return MaintenanceState::Upcoming;
            }
            match end {
                Bound::At { at: end, .. } if end > start => {
                    if now < end {
                        MaintenanceState::InWindow
                    } else {
                        ended(end)
                    }
                }
                Bound::Day { to, .. } if to > start => {
                    if now < to {
                        MaintenanceState::InWindow
                    } else {
                        ended(to)
                    }
                }
                Bound::Undecided if now < start + OPEN_FOR => MaintenanceState::EndUndecided,
                // 終わりが書かれていないか、始まりより前 (書き間違いの可能性) なら、終わりは分からない。
                Bound::Unspecified | Bound::At { .. } | Bound::Day { .. }
                    if now < start + OPEN_FOR =>
                {
                    MaintenanceState::EndUnknown
                }
                _ => MaintenanceState::Past,
            }
        }
        (Bound::Day { from, to, .. }, end) => {
            let last = match end {
                Bound::Day { to: end, .. } => end.max(to),
                Bound::At { at, .. } if at > from => at,
                _ => to,
            };
            if now < from {
                MaintenanceState::Upcoming
            } else if now < last {
                MaintenanceState::OnDate
            } else {
                MaintenanceState::Past
            }
        }
        (Bound::Undecided | Bound::Unspecified, Bound::At { at: end, .. }) => {
            if now < end {
                MaintenanceState::UntilEnd
            } else {
                ended(end)
            }
        }
        (Bound::Undecided | Bound::Unspecified, Bound::Day { from, to, .. }) => {
            if now < from {
                MaintenanceState::Upcoming
            } else if now < to {
                MaintenanceState::OnDate
            } else {
                MaintenanceState::Past
            }
        }
        (Bound::Undecided | Bound::Unspecified, _) => MaintenanceState::Undated,
    }
}

/// 本文を取り直して、追記 (完了・延長・中止など) を確かめたいメンテナンスか。
///
/// これから行う・予定の時間内・延長・終了未定・決められないメンテナンス (結果が予定どおりか、
/// 決められない) で、開始の [`RECHECK_AHEAD`] 前から、終わりの [`RECHECK_AFTER`] 後まで。
/// 終わりが未定・書かれていなければ、開始の [`OPEN_FOR`] 後を終わりとみなす。開始が書かれていなければ、
/// 実施中の報告の時点を開始の代わりにする。
/// 終わった・中止・延期 (新しい日時は別の件になる) は取り直さない。
pub fn needs_recheck(maintenance: &ResolvedMaintenance, now: DateTime<Utc>) -> bool {
    if !matches!(
        maintenance.disposition,
        Disposition::Scheduled | Disposition::Unresolved
    ) {
        return false;
    }
    let (start, end) = bounds(maintenance.window());
    let first = start
        .first()
        .or_else(|| maintenance.ongoing_bound().and_then(Bound::first))
        .or(end.first());
    let last = end
        .last()
        .filter(|last| first.is_none_or(|first| *last >= first))
        .or(first.map(|first| first + OPEN_FOR));
    match (first, last) {
        (Some(first), Some(last)) => now >= first - RECHECK_AHEAD && now < last + RECHECK_AFTER,
        _ => false,
    }
}

// ---- 今日・明日 ----

/// 表示の地域時刻の今日と明日。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Days {
    pub today: Range<DateTime<Utc>>,
    pub tomorrow: Range<DateTime<Utc>>,
}

impl Days {
    pub fn at<Z: TimeZone>(now: DateTime<Utc>, tz: &Z) -> Self {
        let today = now.with_timezone(tz).date_naive();
        let start = local_midnight(tz, today);
        let next = local_midnight(tz, today + Duration::days(1));
        let after = local_midnight(tz, today + Duration::days(2));
        Self {
            today: start..next,
            tomorrow: next..after,
        }
    }
}

/// 「今日・明日の予定を上に」の並び。
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Priority {
    Today,
    Tomorrow,
    #[default]
    Normal,
}

/// 期間の種類のうち、「今日・明日の予定」に数えるもの。
///
/// イベント・ガチャ・キャンペーンは始まりと終わりの節目を数える。販売・受け取り (補填を含む)・
/// そのほかの期限は、メンテナンスの記事にも多く書かれ、数えると一覧の上が埋まるので数えない。
pub fn counts_for_agenda(kind: PeriodKind) -> bool {
    matches!(
        kind,
        PeriodKind::Event | PeriodKind::Gacha | PeriodKind::Campaign
    )
}

/// 期間の節目 (始まりと終わり)。一時点か、日付だけならその日の範囲。
/// 終わりは `[開始, 終了)` の最後の時点 (`終了 - 1 秒`) にする。0 時ちょうどの終わりは前の日の節目。
fn milestones(window: &Window) -> Vec<(Milestone, TimeRange)> {
    let mut points = Vec::new();
    match Bound::of(&window.start) {
        Bound::At { at, .. } => points.push((Milestone::Start, (at, at))),
        Bound::Day { from, to, .. } => points.push((Milestone::Start, (from, to))),
        Bound::Undecided | Bound::Unspecified => {}
    }
    match Bound::of(&window.end) {
        Bound::At { at, .. } => {
            let last = at - Duration::seconds(1);
            points.push((Milestone::End, (last, last)));
        }
        Bound::Day { from, to, .. } => points.push((Milestone::End, (from, to))),
        Bound::Undecided | Bound::Unspecified => {}
    }
    points
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Milestone {
    Start,
    End,
}

/// まだ意味のある節目。終わりの節目は、終わってしまったら数えない (今日 12:59 に終わったイベントを、
/// 15:00 に「今日の予定」として上に出さない)。始まりの節目は、始まったあとも今日の分は数える
/// (今日から始まったことを知らせる)。
fn live_milestones(window: &Window, now: DateTime<Utc>) -> Vec<(Milestone, TimeRange)> {
    milestones(window)
        .into_iter()
        .filter(|(milestone, (from, to))| {
            *milestone == Milestone::Start || (if from == to { now <= *from } else { now < *to })
        })
        .collect()
}

/// 行の「今日・明日の予定」としての並び。
fn item_priority(item: &AgendaItem, now: DateTime<Utc>, days: &Days) -> Priority {
    let classify = |range: TimeRange| {
        if overlaps(range, &days.today) {
            Priority::Today
        } else if overlaps(range, &days.tomorrow) {
            Priority::Tomorrow
        } else {
            Priority::Normal
        }
    };
    match &item.detail {
        AgendaDetail::Maintenance(maintenance) => {
            if !matches!(
                maintenance.disposition,
                Disposition::Scheduled | Disposition::Unresolved
            ) {
                return Priority::Normal;
            }
            // 決められないものも、時刻の上では予定どおりのものと同じに扱う (実施中の報告は予定どおりのものだけ)。
            match timing(maintenance, now) {
                // 今まさに続いているかもしれない (予定の時間内・実施中の報告・終了未定・終了予定を
                // 過ぎたばかり) なら今日。
                MaintenanceState::InWindow
                | MaintenanceState::ReportedOngoing
                | MaintenanceState::EndUndecided
                | MaintenanceState::EndUnknown
                | MaintenanceState::UntilEnd
                | MaintenanceState::Overdue => Priority::Today,
                MaintenanceState::Upcoming | MaintenanceState::OnDate => {
                    occupied(maintenance.window()).map_or(Priority::Normal, classify)
                }
                // 終わってしばらくたった予定・日時の分からない予定は数えない。
                _ => Priority::Normal,
            }
        }
        AgendaDetail::Period(period) => {
            if !counts_for_agenda(period.period.kind) {
                return Priority::Normal;
            }
            live_milestones(&period.period.window, now)
                .into_iter()
                .map(|(_, range)| classify(range))
                .min()
                .unwrap_or(Priority::Normal)
        }
    }
}

// ---- バッジ ----

/// バッジの色合い。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Tone {
    /// 予定の時間内 (メンテ中)。
    Active,
    /// これからの予定。
    Scheduled,
    /// 今日・明日に始まる期間。
    Starting,
    /// 今日・明日に終わる期間。
    Ending,
    /// 確かめたほうがよい (終了予定を過ぎた・終了未定・延期・決められない)。
    Attention,
    /// 終わった。
    Done,
    /// 過ぎた予定・中止など。
    Muted,
}

/// 予定のバッジ。一覧の行と記事の見出しで同じものを出す。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Badge {
    pub text: String,
    pub tone: Tone,
    /// 詳しい説明 (ツールチップ)。
    pub detail: String,
}

/// 日時を表示の地域時刻で書く。
struct Clock<'a, Z: TimeZone> {
    tz: &'a Z,
    /// 今日 (表示の地域時刻)。年を省くかと、日付を省くかの判断に使う。
    today: NaiveDate,
}

impl<'a, Z: TimeZone> Clock<'a, Z> {
    fn new(now: DateTime<Utc>, tz: &'a Z) -> Self {
        Self {
            tz,
            today: now.with_timezone(tz).date_naive(),
        }
    }

    /// `11/12(木)`。今年でなければ `2027/1/5(火)`。
    fn date(&self, date: NaiveDate) -> String {
        const WEEKDAYS: [&str; 7] = ["月", "火", "水", "木", "金", "土", "日"];
        let weekday = WEEKDAYS[date.weekday().num_days_from_monday() as usize];
        if date.year() == self.today.year() {
            format!("{}/{}({weekday})", date.month(), date.day())
        } else {
            format!("{}/{}/{}({weekday})", date.year(), date.month(), date.day())
        }
    }

    fn local(&self, at: DateTime<Utc>) -> (NaiveDate, String) {
        let local = at.with_timezone(self.tz);
        (
            local.date_naive(),
            format!("{:02}:{:02}", local.hour(), local.minute()),
        )
    }

    /// `11/12(木) 14:00`。
    fn instant(&self, at: DateTime<Utc>, approximate: bool) -> String {
        let (date, time) = self.local(at);
        format!("{} {time}{}", self.date(date), about(approximate))
    }

    /// 時刻だけ (今日なら `17:00`、ほかの日なら `11/13(金) 02:00`)。
    fn time_or_instant(&self, at: DateTime<Utc>, approximate: bool, base: NaiveDate) -> String {
        let (date, time) = self.local(at);
        if date == base {
            format!("{time}{}", about(approximate))
        } else {
            self.instant(at, approximate)
        }
    }

    /// 時間帯の表記。日時が 1 つもなければ `None`。
    fn window(&self, window: &Window) -> Option<String> {
        Some(match bounds(window) {
            (
                Bound::At {
                    at: start,
                    approximate,
                },
                end,
            ) => {
                let (start_date, _) = self.local(start);
                let head = self.instant(start, approximate);
                match end {
                    Bound::At {
                        at: end,
                        approximate: end_approximate,
                    } => format!(
                        "{head}–{}",
                        self.time_or_instant(end, end_approximate, start_date)
                    ),
                    Bound::Day { date, .. } => format!("{head}–{}", self.date(date)),
                    Bound::Undecided => format!("{head}– (終了未定)"),
                    Bound::Unspecified => format!("{head}–"),
                }
            }
            (Bound::Day { date, .. }, end) => match end {
                Bound::Day { date: last, .. } if last != date => {
                    format!("{}–{}", self.date(date), self.date(last))
                }
                Bound::At { at, approximate } => {
                    format!("{}–{}", self.date(date), self.instant(at, approximate))
                }
                _ => self.date(date),
            },
            (Bound::Undecided | Bound::Unspecified, Bound::At { at, approximate }) => {
                format!("{} まで", self.instant(at, approximate))
            }
            (Bound::Undecided | Bound::Unspecified, Bound::Day { date, .. }) => {
                format!("{} まで", self.date(date))
            }
            (Bound::Undecided, _) => "日時未定".to_string(),
            (Bound::Unspecified, Bound::Undecided) => "終了未定".to_string(),
            (Bound::Unspecified, Bound::Unspecified) => return None,
        })
    }

    /// 「17:00 まで」の部分 (今日でなければ日付も付ける)。
    fn until(&self, end: Bound) -> Option<String> {
        match end {
            Bound::At { at, approximate } => {
                Some(self.time_or_instant(at, approximate, self.today))
            }
            Bound::Day { date, .. } => Some(self.date(date)),
            Bound::Undecided | Bound::Unspecified => None,
        }
    }

    /// 1 つの日時の表記。`base` と同じ日の時刻なら、日付を省く。
    fn point(&self, bound: Bound, base: Option<NaiveDate>) -> String {
        match bound {
            Bound::At { at, approximate } => match base {
                Some(base) => self.time_or_instant(at, approximate, base),
                None => self.instant(at, approximate),
            },
            Bound::Day { date, .. } => self.date(date),
            Bound::Undecided => "未定".to_string(),
            Bound::Unspecified => "記載なし".to_string(),
        }
    }

    /// 延長の表記 (「延長 11/12(木) 17:00 → 19:00」)。延長する前の終わりが分からなければ、新しい終わりだけ。
    fn extension(&self, from: Bound, end: Bound) -> String {
        let base = match from {
            Bound::At { at, .. } => Some(self.local(at).0),
            Bound::Day { date, .. } => Some(date),
            Bound::Undecided | Bound::Unspecified => None,
        };
        match (base, end) {
            (Some(_), Bound::Undecided) => format!("延長 {} → 未定", self.point(from, None)),
            (Some(_), Bound::Unspecified) => format!("延長 ({} から)", self.point(from, None)),
            (Some(_), end) => format!(
                "延長 {} → {}",
                self.point(from, None),
                self.point(end, base)
            ),
            (None, Bound::Undecided) => "延長 (終了未定)".to_string(),
            (None, Bound::Unspecified) => "延長".to_string(),
            (None, end) => format!("延長 ({} まで)", self.point(end, None)),
        }
    }
}

fn about(approximate: bool) -> &'static str {
    if approximate { "頃" } else { "" }
}

/// 時間帯の表記 (記事の詳細の欄に使う)。日時が 1 つもなければ `None`。
pub fn window_text<Z: TimeZone>(window: &Window, now: DateTime<Utc>, tz: &Z) -> Option<String> {
    Clock::new(now, tz).window(window)
}

/// 開始・終了の 1 つの値の表記。
pub fn slot_text<Z: TimeZone>(slot: &Slot<TimeValue>, now: DateTime<Utc>, tz: &Z) -> String {
    let clock = Clock::new(now, tz);
    match Bound::of(slot) {
        Bound::At { at, approximate } => clock.instant(at, approximate),
        Bound::Day { date, .. } => clock.date(date),
        Bound::Undecided => "未定".to_string(),
        Bound::Unspecified => "記載なし".to_string(),
    }
}

/// メンテナンスの対象の表記。書かれていなければ `None`。
pub fn scope_text(scope: &Scope) -> Option<&str> {
    match scope {
        Scope::Unknown => None,
        Scope::All => Some("全体"),
        Scope::Named(name) => Some(name),
    }
}

/// 結果の表記。
pub fn disposition_text(disposition: Disposition) -> &'static str {
    match disposition {
        Disposition::Scheduled => "予定どおり (完了の報告はまだない)",
        Disposition::Completed => "終了",
        Disposition::Cancelled => "中止",
        Disposition::Postponed => "延期",
        Disposition::Unresolved => "記述が食い違うため決められない",
    }
}

/// 期間の種類の表記。
pub fn period_kind_text(kind: PeriodKind) -> &'static str {
    match kind {
        PeriodKind::Event => "イベント",
        PeriodKind::Gacha => "ガチャ",
        PeriodKind::Campaign => "キャンペーン",
        PeriodKind::Sale => "販売",
        PeriodKind::Claim => "受け取り",
        PeriodKind::Other => "期間",
    }
}

/// 延長・日程変更の表記 (記事の詳細とツールチップに出す)。
///
/// 延長は「延長 11/12(木) 17:00 → 19:00」、日程変更は変更ごとに
/// 「日程変更 11/10(火) 14:00–17:00 → 11/12(木) 14:00–17:00」。前か後が書かれていなければ、分かるほうだけを出す。
pub fn change_text<Z: TimeZone>(change: &Change, now: DateTime<Utc>, tz: &Z) -> String {
    let clock = Clock::new(now, tz);
    match change {
        Change::Extended { from, end, .. } => clock.extension(Bound::of(from), Bound::of(end)),
        Change::Rescheduled { from, to, .. } => match (clock.window(from), clock.window(to)) {
            (Some(from), Some(to)) => format!("日程変更 {from} → {to}"),
            (Some(from), None) => format!("日程変更 (変更前: {from})"),
            (None, Some(to)) => format!("日程変更 (変更後: {to})"),
            (None, None) => "日程変更".to_string(),
        },
    }
}

/// 実施中の報告の表記 (「11/12(木) 15:00 の時点で実施中」)。
pub fn ongoing_text<Z: TimeZone>(ongoing: &Ongoing, now: DateTime<Utc>, tz: &Z) -> String {
    let clock = Clock::new(now, tz);
    format!(
        "{} の時点で実施中",
        clock.point(Bound::of_value(&ongoing.as_of), None)
    )
}

/// メンテナンスのバッジの中身 (対象の名前を付ける前)。
#[derive(Debug, Clone)]
struct MaintenanceParts {
    state: MaintenanceState,
    /// 対象の名前を付ける前の文言 (「メンテ中 (17:00 まで予定)」)。
    text: String,
    tone: Tone,
    /// 説明の 2 行目から (1 行目は対象の名前)。
    lines: Vec<String>,
}

fn maintenance_parts<Z: TimeZone>(
    maintenance: &ResolvedMaintenance,
    now: DateTime<Utc>,
    tz: &Z,
) -> MaintenanceParts {
    let clock = Clock::new(now, tz);
    let state = maintenance_state(maintenance, now);
    let window = maintenance.window();
    let (_, end) = bounds(window);
    let when = clock.window(window);
    let extended = maintenance.extended();
    let running = if extended {
        "メンテ延長中"
    } else {
        "メンテ中"
    };
    let with_when = |prefix: &str| match &when {
        Some(when) => format!("{prefix} {when}"),
        None => prefix.to_string(),
    };
    let (text, tone) = match state {
        MaintenanceState::InWindow => (
            match clock.until(end) {
                Some(until) => format!("{running} ({until} まで予定)"),
                None => running.to_string(),
            },
            Tone::Active,
        ),
        MaintenanceState::ReportedOngoing => (format!("{running} (終了未定)"), Tone::Active),
        MaintenanceState::EndUndecided => (
            if extended {
                "メンテ延長 (終了未定)"
            } else {
                "メンテ終了未定"
            }
            .to_string(),
            Tone::Attention,
        ),
        MaintenanceState::Overdue => ("終了予定を過ぎています".to_string(), Tone::Attention),
        MaintenanceState::UntilEnd => (
            match clock.until(end) {
                Some(until) => format!("メンテ ({until} まで予定)"),
                None => with_when("メンテ"),
            },
            Tone::Scheduled,
        ),
        MaintenanceState::EndUnknown => (with_when("メンテ"), Tone::Attention),
        MaintenanceState::OnDate | MaintenanceState::Upcoming => {
            (with_when("メンテ"), Tone::Scheduled)
        }
        MaintenanceState::Unresolved => ("メンテ 要確認".to_string(), Tone::Attention),
        MaintenanceState::Postponed => ("メンテ延期".to_string(), Tone::Attention),
        MaintenanceState::Completed => ("メンテ終了".to_string(), Tone::Done),
        MaintenanceState::Cancelled => ("メンテ中止".to_string(), Tone::Muted),
        MaintenanceState::Past => (with_when("メンテ"), Tone::Muted),
        MaintenanceState::Undated => (
            if matches!(window.start, Slot::Undecided) {
                "メンテ 日時未定"
            } else {
                "メンテ"
            }
            .to_string(),
            Tone::Muted,
        ),
    };
    MaintenanceParts {
        state,
        text,
        tone,
        lines: maintenance_lines(maintenance, state, &clock, now, tz),
    }
}

/// メンテナンスのバッジの説明 (対象の名前の行を除く)。
fn maintenance_lines<Z: TimeZone>(
    maintenance: &ResolvedMaintenance,
    state: MaintenanceState,
    clock: &Clock<'_, Z>,
    now: DateTime<Utc>,
    tz: &Z,
) -> Vec<String> {
    let mut lines = Vec::new();
    if let Some(planned) = clock.window(&maintenance.planned) {
        lines.push(format!("予定: {planned}"));
    }
    if let Some(actual) = clock.window(&maintenance.actual) {
        lines.push(format!("実際: {actual}"));
    }
    if let Some(ongoing) = maintenance.ongoing_report() {
        lines.push(ongoing_text(ongoing, now, tz));
    }
    lines.extend(
        maintenance
            .changes
            .iter()
            .map(|change| change_text(change, now, tz)),
    );
    // 開始の時刻がなく、実施中の報告から「メンテ中」としているか。
    let reported = maintenance.ongoing_at().is_some()
        && !matches!(bounds(maintenance.window()).0, Bound::At { .. });
    lines.push(match state {
        MaintenanceState::InWindow if reported => {
            "実施中と報告されています (完了の報告はまだありません)".to_string()
        }
        MaintenanceState::InWindow => "予定の時間内です (完了の報告はまだありません)".to_string(),
        MaintenanceState::ReportedOngoing => match maintenance.window().end {
            Slot::Undecided => "実施中と報告されています (終了の時刻は未定です)".to_string(),
            _ => "実施中と報告されています (終了の時刻は書かれていません)".to_string(),
        },
        MaintenanceState::Overdue => {
            "終了予定の時刻を過ぎましたが、完了の報告はまだありません".to_string()
        }
        MaintenanceState::EndUndecided => "終了の時刻は未定です".to_string(),
        MaintenanceState::EndUnknown => "終了の時刻は書かれていません".to_string(),
        MaintenanceState::OnDate => "日付だけの予定です (時刻は書かれていません)".to_string(),
        _ => format!("結果: {}", disposition_text(maintenance.disposition)),
    });
    if maintenance.resolved_by.is_some() {
        lines.push("結果は別の記事の報告によります".to_string());
    }
    lines
}

/// 対象の名前を付けたバッジ。`names` はまとめた対象の名前 (書かれた順)。3 つ以上なら
/// 「PC版ほか 2 件」と短くし、説明には全部の名前を出す。
fn scoped_badge(parts: MaintenanceParts, scope: &Scope, names: &[String]) -> Badge {
    let prefix = match names {
        [] => None,
        [one] => Some(one.clone()),
        [first, second] => Some(format!("{first}・{second}")),
        [first, rest @ ..] => Some(format!("{first}ほか {} 件", rest.len())),
    };
    let title = if names.is_empty() {
        match scope_text(scope) {
            Some(scope) => format!("メンテナンス ({scope})"),
            None => "メンテナンス".to_string(),
        }
    } else {
        format!("メンテナンス ({})", names.join("・"))
    };
    Badge {
        text: match prefix {
            Some(prefix) => format!("{prefix} {}", parts.text),
            None => parts.text,
        },
        tone: parts.tone,
        detail: std::iter::once(title)
            .chain(parts.lines)
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

/// メンテナンス 1 件のバッジ。
pub fn maintenance_badge<Z: TimeZone>(
    maintenance: &ResolvedMaintenance,
    now: DateTime<Utc>,
    tz: &Z,
) -> Badge {
    let names: Vec<String> = match &maintenance.scope {
        Scope::Named(name) => vec![name.clone()],
        Scope::Unknown | Scope::All => Vec::new(),
    };
    scoped_badge(
        maintenance_parts(maintenance, now, tz),
        &maintenance.scope,
        &names,
    )
}

/// 期間の節目が今日・明日にあれば、そのバッジ (数える種類だけ。終わってしまった終わりの節目は除く)。
///
/// 今日・明日かは延長・日程変更の後の期間で決め、延長されていれば「(延長)」を付ける。
pub fn period_badge<Z: TimeZone>(
    period: &Period,
    now: DateTime<Utc>,
    days: &Days,
    tz: &Z,
) -> Option<Badge> {
    if !counts_for_agenda(period.kind) {
        return None;
    }
    let clock = Clock::new(now, tz);
    let found = live_milestones(&period.window, now)
        .into_iter()
        .filter_map(|(milestone, range)| {
            let (day, label) = if overlaps(range, &days.today) {
                (0, "今日")
            } else if overlaps(range, &days.tomorrow) {
                (1, "明日")
            } else {
                return None;
            };
            Some((day, milestone, range, label))
        })
        .min_by_key(|(day, milestone, _, _)| (*day, *milestone == Milestone::End));
    let (_, milestone, range, day) = found?;
    Some(milestone_badge(
        period, milestone, range, day, &clock, now, tz,
    ))
}

/// 期間の節目 1 つのバッジ (「ガチャ 今日 23:59 まで」)。`day` は「今日」「明日」。
fn milestone_badge<Z: TimeZone>(
    period: &Period,
    milestone: Milestone,
    (from, to): TimeRange,
    day: &str,
    clock: &Clock<'_, Z>,
    now: DateTime<Utc>,
    tz: &Z,
) -> Badge {
    let kind = period_kind_text(period.kind);
    let edge = match milestone {
        Milestone::Start => "から",
        Milestone::End => "まで",
    };
    let mut text = if from == to {
        // 終わりは 1 秒前にしているので、表示は元の時刻に戻す。
        let at = if milestone == Milestone::End {
            from + Duration::seconds(1)
        } else {
            from
        };
        format!("{kind} {day} {} {edge}", clock.local(at).1)
    } else {
        format!("{kind} {day}{edge}")
    };
    if is_extended(&period.changes) {
        text.push_str(" (延長)");
    }
    let mut detail = vec![match &period.label {
        Some(name) => format!("{kind}: {name}"),
        None => kind.to_string(),
    }];
    if let Some(window) = clock.window(&period.window) {
        detail.push(format!("期間: {window}"));
    }
    detail.extend(
        period
            .changes
            .iter()
            .map(|change| change_text(change, now, tz)),
    );
    Badge {
        text,
        tone: match milestone {
            Milestone::Start => Tone::Starting,
            Milestone::End => Tone::Ending,
        },
        detail: detail.join("\n"),
    }
}

// ---- 今日・明日の予定の画面 ----

/// 「今日の予定」の画面で見る日。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Day {
    Today,
    Tomorrow,
}

impl Day {
    fn range(self, days: &Days) -> &Range<DateTime<Utc>> {
        match self {
            Day::Today => &days.today,
            Day::Tomorrow => &days.tomorrow,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Day::Today => "今日",
            Day::Tomorrow => "明日",
        }
    }
}

/// その日の予定のまとまり。宣言の順に画面に並べる。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DayGroup {
    /// 今確かめたいメンテナンス (予定の時間内・実施中の報告・終了未定・終了予定を過ぎた・決められない)。
    /// 今日だけ。
    Now,
    /// その日の時刻の決まった予定 (メンテナンスの時間帯と、数える種類の期間の始まり・終わりの節目)。
    Timed,
    /// 日付だけで、時刻の書かれていない予定。
    DateOnly,
    /// その日のメンテナンスのうち、終わった (完了の報告・時刻を過ぎた)・中止・延期したもの。
    /// 件数には数えない。
    Finished,
}

/// その日の予定の 1 件。同じ記事の、時間帯と状態が同じで対象だけが違うメンテナンスは 1 件にまとめる。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DayEntry {
    pub entry_id: i64,
    pub source_id: String,
    pub group: DayGroup,
    /// 並べる時刻。メンテナンスは始まり (なければ実施中の報告の時点)、期間はその日の節目。
    pub at: Option<DateTime<Utc>>,
    /// 予定の名前 (「メンテナンス (PC版・iOS版)」「ガチャ (〇〇ピックアップ)」)。
    pub title: String,
    pub badge: Badge,
    /// 時間帯 (延長・日程変更の後のもの)。日時が書かれていなければ `None`。
    pub when: Option<String>,
    pub maintenance: bool,
}

impl DayEntry {
    /// 件数に数えるか (終わった・中止・延期を除く)。
    pub fn counts(&self) -> bool {
        self.group != DayGroup::Finished
    }
}

/// メンテナンスのまとまりを、その日のどこに置くか。その日に関係しなければ `None`。
fn maintenance_day_group(
    maintenance: &ResolvedMaintenance,
    state: MaintenanceState,
    day: Day,
    days: &Days,
) -> Option<DayGroup> {
    let range = day.range(days);
    let on_day = occupied(maintenance.window()).is_some_and(|occupied| overlaps(occupied, range))
        || maintenance
            .ongoing_bound()
            .and_then(|bound| Some((bound.first()?, bound.last()?)))
            .is_some_and(|reported| overlaps(reported, range));
    let date_only = matches!(
        bounds(maintenance.window()),
        (Bound::Day { .. }, _) | (Bound::Undecided | Bound::Unspecified, Bound::Day { .. })
    );
    match state {
        MaintenanceState::Undated => None,
        MaintenanceState::Completed
        | MaintenanceState::Cancelled
        | MaintenanceState::Postponed
        | MaintenanceState::Past => on_day.then_some(DayGroup::Finished),
        MaintenanceState::InWindow
        | MaintenanceState::ReportedOngoing
        | MaintenanceState::EndUndecided
        | MaintenanceState::Overdue
        | MaintenanceState::EndUnknown
        | MaintenanceState::UntilEnd
            if day == Day::Today =>
        {
            Some(DayGroup::Now)
        }
        MaintenanceState::Unresolved if day == Day::Today && on_day => Some(DayGroup::Now),
        _ if on_day && date_only => Some(DayGroup::DateOnly),
        _ if on_day => Some(DayGroup::Timed),
        _ => None,
    }
}

/// 報告の対象が予告の対象を含むか (`gnn_schedule` の突き合わせと同じ決まり)。
fn report_covers(report: &Scope, plan: &Scope) -> bool {
    match (report, plan) {
        (Scope::Unknown | Scope::All, _) => true,
        (Scope::Named(_), Scope::Unknown | Scope::All) => false,
        (Scope::Named(report), Scope::Named(plan)) => report == plan,
    }
}

impl Agenda {
    /// 今日・明日の予定 (「今日の予定」の画面)。まとまりの順、まとまりの中は時刻の順 (時刻のないものは後)。
    ///
    /// メンテナンスはその日に時間帯が重なるもの (今日は、今確かめたいものも)、期間は数える種類
    /// (イベント・ガチャ・キャンペーン) の節目がその日にあるもの。終わった節目は数えない
    /// ([`live_milestones`])。同じ期間の節目がその日に 2 つあれば、始まりを出す。
    ///
    /// 同じメンテナンスは記事をまたいで 1 件にする: 予告の記事に取り込まれた報告の記事の件 (予告の
    /// `resolved_by` がその記事で、対象が合うもの) は出さず、同じゲーム・同じ対象・同じ開始の件 (予告と
    /// 再告知、開始を書いた延長の報告) は、いちばん古い記事 (記事の番号の小さいもの) を代表にする。
    pub fn day_entries<Z: TimeZone>(
        &self,
        day: Day,
        now: DateTime<Utc>,
        days: &Days,
        tz: &Z,
    ) -> Vec<DayEntry> {
        let clock = Clock::new(now, tz);
        let range = day.range(days);
        // 報告を取り込んだ予告 (ソース・報告の記事・予告の対象)。
        let folded: Vec<(&str, i64, &Scope)> = self
            .items
            .iter()
            .filter_map(|item| match &item.detail {
                AgendaDetail::Maintenance(maintenance) => maintenance
                    .resolved_by
                    .filter(|by| *by != item.entry_id)
                    .map(|by| (item.source_id.as_str(), by, &maintenance.scope)),
                AgendaDetail::Period(_) => None,
            })
            .collect();
        let is_folded = |item: &AgendaItem, scope: &Scope| {
            folded.iter().any(|(source, by, plan)| {
                *source == item.source_id && *by == item.entry_id && report_covers(scope, plan)
            })
        };
        let mut maintenance_entries = Vec::new();
        let mut visited = HashSet::new();
        for item in &self.items {
            if !visited.insert(item.entry_id) {
                continue;
            }
            for group in maintenance_groups(self.of_entry(item.entry_id), now, tz) {
                if is_folded(group.item, &group.maintenance.scope) {
                    continue;
                }
                let Some(day_group) =
                    maintenance_day_group(group.maintenance, group.parts.state, day, days)
                else {
                    continue;
                };
                let title = if group.names.is_empty() {
                    match scope_text(&group.maintenance.scope) {
                        Some(scope) => format!("メンテナンス ({scope})"),
                        None => "メンテナンス".to_string(),
                    }
                } else {
                    format!("メンテナンス ({})", group.names.join("・"))
                };
                maintenance_entries.push(DayEntry {
                    entry_id: group.item.entry_id,
                    source_id: group.item.source_id.clone(),
                    group: day_group,
                    at: group.start,
                    title,
                    when: clock.window(group.maintenance.window()),
                    maintenance: true,
                    badge: group.badge(),
                });
            }
        }
        // 同じゲーム・同じ対象・同じ開始の件は、いちばん古い記事のものを残す。
        maintenance_entries.sort_by_key(|entry| entry.entry_id);
        let mut seen = HashSet::new();
        let mut entries: Vec<DayEntry> = maintenance_entries
            .into_iter()
            .filter(|entry| {
                entry.at.is_none()
                    || seen.insert((entry.source_id.clone(), entry.title.clone(), entry.at))
            })
            .collect();
        for item in &self.items {
            let AgendaDetail::Period(period) = &item.detail else {
                continue;
            };
            let period = &period.period;
            if !counts_for_agenda(period.kind) {
                continue;
            }
            let Some((milestone, milestone_range)) = live_milestones(&period.window, now)
                .into_iter()
                .filter(|(_, milestone_range)| overlaps(*milestone_range, range))
                .min_by_key(|(milestone, _)| *milestone == Milestone::End)
            else {
                continue;
            };
            let (from, to) = milestone_range;
            let kind = period_kind_text(period.kind);
            entries.push(DayEntry {
                entry_id: item.entry_id,
                source_id: item.source_id.clone(),
                group: if from == to {
                    DayGroup::Timed
                } else {
                    DayGroup::DateOnly
                },
                // 終わりの節目は 1 秒前にしているので、並べる時刻は元の時刻に戻す。
                at: Some(if from == to && milestone == Milestone::End {
                    from + Duration::seconds(1)
                } else {
                    from
                }),
                title: match &period.label {
                    Some(label) => format!("{kind} ({label})"),
                    None => kind.to_string(),
                },
                badge: milestone_badge(
                    period,
                    milestone,
                    milestone_range,
                    day.label(),
                    &clock,
                    now,
                    tz,
                ),
                when: clock.window(&period.window),
                maintenance: false,
            });
        }
        entries.sort_by(|a, b| {
            (a.group, a.at.is_none(), a.at, a.entry_id).cmp(&(
                b.group,
                b.at.is_none(),
                b.at,
                b.entry_id,
            ))
        });
        entries
    }

    /// 今日に関係する予定の件数 (終わった・中止・延期を除く)。サイドバーの「今日の予定」に出す。
    pub fn today_count<Z: TimeZone>(&self, now: DateTime<Utc>, days: &Days, tz: &Z) -> usize {
        self.day_entries(Day::Today, now, days, tz)
            .iter()
            .filter(|entry| entry.counts())
            .count()
    }

    /// 予定の条件に合う記事 (記事の番号の昇順)。お知らせの一覧の「予定で絞る」に使う。
    /// 規則は [`ScheduleFilter`]。記事どうしはまとめない (予告と報告の記事のどちらも合う)。
    pub fn filter_entries(
        &self,
        filter: ScheduleFilter,
        now: DateTime<Utc>,
        days: &Days,
    ) -> Vec<i64> {
        let mut ids: Vec<i64> = self
            .items
            .iter()
            .filter(|item| item_matches(item, filter, now, days))
            .map(|item| item.entry_id)
            .collect();
        ids.sort_unstable();
        ids.dedup();
        ids
    }
}

// ---- 予定で絞る ----

/// お知らせの一覧を予定で絞る条件。メンテナンスと、数える種類の期間 (イベント・ガチャ・キャンペーン。
/// 販売・受け取りなどはバッジと同じく数えない) を見る。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ScheduleFilter {
    /// その日に関係する予定がある。「今日の予定」の画面に出るもの (終わった・中止・延期したメンテナンスを含む)。
    Related(Day),
    /// その日に始まる予定がある。メンテナンスの開始 (中止・延期したものを除く。始まったあとも含む) と、
    /// 期間の始まり。
    Starts(Day),
    /// その日に終わる予定がある。まだ終わっていないものだけ: メンテナンスは終了予定がその日で、完了・中止・
    /// 延期の報告がなく、終了予定を過ぎてしばらくたっていないもの (過ぎたばかりで完了の報告がないものは含む)。
    /// 期間は終わりの節目がその日で、まだ過ぎていないもの。0 時ちょうどの終わりは前の日に数える。
    Ends(Day),
}

impl ScheduleFilter {
    pub fn day(self) -> Day {
        match self {
            ScheduleFilter::Related(day)
            | ScheduleFilter::Starts(day)
            | ScheduleFilter::Ends(day) => day,
        }
    }
}

/// 開始・終了の値がその日にあるか。時刻は `[0 時, 翌日 0 時)` に入るか、日付だけならその日か。
/// `end` なら、0 時ちょうどの終わりを前の日に数える (期間の節目と同じ)。
fn bound_on(bound: Bound, day: &Range<DateTime<Utc>>, end: bool) -> bool {
    match bound {
        Bound::At { at, .. } => {
            let at = if end { at - Duration::seconds(1) } else { at };
            day.contains(&at)
        }
        Bound::Day { from, to, .. } => overlaps((from, to), day),
        Bound::Undecided | Bound::Unspecified => false,
    }
}

/// 予定の表の 1 行が条件に合うか。
fn item_matches(
    item: &AgendaItem,
    filter: ScheduleFilter,
    now: DateTime<Utc>,
    days: &Days,
) -> bool {
    let day = filter.day();
    let range = day.range(days);
    match &item.detail {
        AgendaDetail::Maintenance(maintenance) => {
            let state = maintenance_state(maintenance, now);
            let (start, end) = bounds(maintenance.window());
            match filter {
                ScheduleFilter::Related(_) => {
                    maintenance_day_group(maintenance, state, day, days).is_some()
                }
                ScheduleFilter::Starts(_) => {
                    !matches!(
                        maintenance.disposition,
                        Disposition::Cancelled | Disposition::Postponed
                    ) && bound_on(start, range, false)
                }
                ScheduleFilter::Ends(_) => {
                    matches!(
                        maintenance.disposition,
                        Disposition::Scheduled | Disposition::Unresolved
                    ) && state != MaintenanceState::Past
                        && bound_on(end, range, true)
                }
            }
        }
        AgendaDetail::Period(period) => {
            counts_for_agenda(period.period.kind)
                && live_milestones(&period.period.window, now).into_iter().any(
                    |(milestone, milestone_range)| {
                        overlaps(milestone_range, range)
                            && match filter {
                                ScheduleFilter::Related(_) => true,
                                ScheduleFilter::Starts(_) => milestone == Milestone::Start,
                                ScheduleFilter::Ends(_) => milestone == Milestone::End,
                            }
                    },
                )
        }
    }
}

// ---- サイドバーの印 ----

/// サイドバーのゲームの横に出す印の強さ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum MarkLevel {
    /// 今メンテナンスの予定の時間内。
    Now,
    /// 今日メンテナンスがある (これから・終了未定・終了予定を過ぎた、を含む)。
    Today,
}

/// サイドバーの印。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceMark {
    pub level: MarkLevel,
    /// 印の元になったメンテナンスのバッジ (ツールチップに使う)。
    pub badge: Badge,
}

// ---- まとめ ----

/// 読み込んだ予定の表。記事ごとに引ける。
#[derive(Debug, Clone, Default)]
pub struct Agenda {
    items: Vec<AgendaItem>,
    by_entry: HashMap<i64, Vec<usize>>,
    /// 状態が変わりうる時刻 (昇順)。
    boundaries: Vec<DateTime<Utc>>,
}

impl Agenda {
    pub fn new(items: Vec<AgendaItem>) -> Self {
        let mut by_entry: HashMap<i64, Vec<usize>> = HashMap::new();
        let mut boundaries = Vec::new();
        for (index, item) in items.iter().enumerate() {
            by_entry.entry(item.entry_id).or_default().push(index);
            match &item.detail {
                AgendaDetail::Maintenance(maintenance) => {
                    boundaries.extend(maintenance_boundaries(maintenance));
                }
                // 数える種類の期間は、終わりを過ぎるとバッジ・今日明日の並び・「今日終わる」の絞り込みから
                // 外れる ([`live_milestones`])。始まりは過ぎても外れないので数えない。
                AgendaDetail::Period(period) if counts_for_agenda(period.period.kind) => {
                    boundaries.extend(
                        milestones(&period.period.window)
                            .into_iter()
                            .filter(|(milestone, _)| *milestone == Milestone::End)
                            .map(|(_, (from, to))| {
                                if from == to {
                                    from + Duration::seconds(1)
                                } else {
                                    to
                                }
                            }),
                    );
                }
                AgendaDetail::Period(_) => {}
            }
        }
        boundaries.sort();
        boundaries.dedup();
        Self {
            items,
            by_entry,
            boundaries,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn items(&self) -> &[AgendaItem] {
        &self.items
    }

    /// 記事の行。
    pub fn of_entry(&self, entry_id: i64) -> impl Iterator<Item = &AgendaItem> + '_ {
        self.by_entry
            .get(&entry_id)
            .into_iter()
            .flatten()
            .map(|index| &self.items[*index])
    }

    /// 記事の「今日・明日の予定」としての並び。
    pub fn priority(&self, entry_id: i64, now: DateTime<Utc>, days: &Days) -> Priority {
        self.of_entry(entry_id)
            .map(|item| item_priority(item, now, days))
            .min()
            .unwrap_or_default()
    }

    /// 今日・明日の予定がある記事。
    pub fn prioritized(&self, now: DateTime<Utc>, days: &Days) -> Vec<i64> {
        let mut ids: Vec<i64> = self
            .by_entry
            .keys()
            .copied()
            .filter(|id| self.priority(*id, now, days) != Priority::Normal)
            .collect();
        ids.sort_unstable();
        ids
    }

    /// 記事のバッジ。メンテナンス (今に近い状態から) のあとに、今日・明日に節目のある期間 (今日の分から)。
    ///
    /// 同じ時間帯・同じ状態で対象だけが違うメンテナンスは 1 つにまとめる (「Android版・iOS版 メンテ中 …」)。
    /// 同じ文言のバッジも 1 つにまとめる (同じ日に始まるガチャが 2 つ書かれている、など)。
    pub fn badges<Z: TimeZone>(
        &self,
        entry_id: i64,
        now: DateTime<Utc>,
        days: &Days,
        tz: &Z,
    ) -> Vec<Badge> {
        let mut maintenance: Vec<(u8, Option<DateTime<Utc>>, Badge)> =
            maintenance_groups(self.of_entry(entry_id), now, tz)
                .into_iter()
                .map(|group| (group.parts.state.rank(), group.start, group.badge()))
                .collect();
        let mut periods: Vec<(bool, Badge)> = Vec::new();
        for item in self.of_entry(entry_id) {
            if let AgendaDetail::Period(period) = &item.detail
                && let Some(badge) = period_badge(&period.period, now, days, tz)
            {
                let tomorrow = item_priority(item, now, days) != Priority::Today;
                periods.push((tomorrow, badge));
            }
        }
        maintenance.sort_by_key(|(rank, start, _)| (*rank, *start));
        periods.sort_by_key(|(tomorrow, _)| *tomorrow);
        let mut seen = HashSet::new();
        maintenance
            .into_iter()
            .map(|(_, _, badge)| badge)
            .chain(periods.into_iter().map(|(_, badge)| badge))
            .filter(|badge| seen.insert(badge.text.clone()))
            .collect()
    }

    /// ソースごとのサイドバーの印。今メンテナンスの予定の時間内か実施中と報告されていれば「今」、
    /// 今日メンテナンスがあれば「今日」。印の説明には、対象をまとめたバッジを使う。
    pub fn marks<Z: TimeZone>(
        &self,
        now: DateTime<Utc>,
        days: &Days,
        tz: &Z,
    ) -> HashMap<String, SourceMark> {
        let mut marks: HashMap<String, (MarkLevel, u8, SourceMark)> = HashMap::new();
        let mut visited = HashSet::new();
        for item in &self.items {
            if !visited.insert(item.entry_id) {
                continue;
            }
            for group in maintenance_groups(self.of_entry(item.entry_id), now, tz) {
                let state = group.parts.state;
                let level = match state {
                    MaintenanceState::InWindow | MaintenanceState::ReportedOngoing => {
                        MarkLevel::Now
                    }
                    MaintenanceState::EndUndecided
                    | MaintenanceState::EndUnknown
                    | MaintenanceState::Overdue => MarkLevel::Today,
                    _ if item_priority(group.item, now, days) == Priority::Today => {
                        MarkLevel::Today
                    }
                    _ => continue,
                };
                let source_id = group.item.source_id.clone();
                let better = marks
                    .get(&source_id)
                    .is_none_or(|(best, rank, _)| (level, state.rank()) < (*best, *rank));
                if better {
                    let mark = SourceMark {
                        level,
                        badge: group.badge(),
                    };
                    marks.insert(source_id, (level, state.rank(), mark));
                }
            }
        }
        marks
            .into_iter()
            .map(|(source_id, (_, _, mark))| (source_id, mark))
            .collect()
    }

    /// `after` より後で、表示の状態が変わりうる最初の時刻 (日付の変わり目は含まない)。
    pub fn next_boundary(&self, after: DateTime<Utc>) -> Option<DateTime<Utc>> {
        let index = self.boundaries.partition_point(|at| *at <= after);
        self.boundaries.get(index).copied()
    }
}

/// 同じ記事の、同じ時間帯・同じ状態で対象だけが違うメンテナンスのまとまり。
struct MaintenanceGroup<'a> {
    /// まとまりの最初の行 (並びと印の判断に使う)。
    item: &'a AgendaItem,
    maintenance: &'a ResolvedMaintenance,
    /// 並べる順の時刻 (開始。なければ実施中の報告の時点)。
    start: Option<DateTime<Utc>>,
    parts: MaintenanceParts,
    /// まとめた対象の名前 (書かれた順)。名前のない対象 (書かれていない・全体) はまとめない。
    names: Vec<String>,
}

impl MaintenanceGroup<'_> {
    fn badge(self) -> Badge {
        scoped_badge(self.parts, &self.maintenance.scope, &self.names)
    }
}

/// 記事のメンテナンスを、対象だけが違うものでまとめる。状態や時間帯が違うもの (一方だけ完了など) は
/// まとめない。
fn maintenance_groups<'a, Z: TimeZone>(
    items: impl IntoIterator<Item = &'a AgendaItem>,
    now: DateTime<Utc>,
    tz: &Z,
) -> Vec<MaintenanceGroup<'a>> {
    let mut groups: Vec<MaintenanceGroup<'a>> = Vec::new();
    for item in items {
        let AgendaDetail::Maintenance(maintenance) = &item.detail else {
            continue;
        };
        let maintenance: &ResolvedMaintenance = maintenance;
        let parts = maintenance_parts(maintenance, now, tz);
        if let Scope::Named(name) = &maintenance.scope
            && let Some(group) = groups.iter_mut().find(|group| {
                !group.names.is_empty()
                    && group.parts.state == parts.state
                    && group.parts.text == parts.text
                    && group.maintenance.window() == maintenance.window()
            })
        {
            if !group.names.contains(name) {
                group.names.push(name.clone());
            }
            continue;
        }
        groups.push(MaintenanceGroup {
            item,
            maintenance,
            start: bounds(maintenance.window())
                .0
                .first()
                .or(maintenance.ongoing_at()),
            names: match &maintenance.scope {
                Scope::Named(name) => vec![name.clone()],
                Scope::Unknown | Scope::All => Vec::new(),
            },
            parts,
        });
    }
    groups
}

/// メンテナンスの状態が変わりうる時刻。
fn maintenance_boundaries(maintenance: &ResolvedMaintenance) -> Vec<DateTime<Utc>> {
    let mut points = Vec::new();
    for window in [&maintenance.planned, &maintenance.actual] {
        let (start, end) = bounds(window);
        points.extend(start.first());
        points.extend(start.last());
        if let Some(end) = end.last() {
            points.extend([end, end + OVERDUE_FOR]);
        }
        if let (Some(start), Bound::Undecided | Bound::Unspecified) = (start.first(), end) {
            points.push(start + OPEN_FOR);
        }
    }
    // 実施中の報告から「メンテ中」になる時刻と、終わりが分からないときに外れる時刻。
    if let Some(at) = maintenance.ongoing_at() {
        points.extend([at, at + OPEN_FOR]);
    }
    points
}

/// 旧形式の `time_range` に書く日付 (`tz` の暦日)。予定どおり・決められないメンテナンスの
/// 時間帯が重なる日と実施中と報告された日、数える種類の期間の節目の日。
pub fn legacy_dates<'a, Z: TimeZone>(
    items: impl IntoIterator<Item = &'a AgendaItem>,
    tz: &Z,
) -> Vec<NaiveDate> {
    let local = |at: DateTime<Utc>| at.with_timezone(tz).date_naive();
    let mut dates = Vec::new();
    let mut push_range = |(from, to): TimeRange| {
        let first = local(from);
        // 終わりは含まないので、1 秒前の日までにする。
        let last = if to > from {
            local(to - Duration::seconds(1))
        } else {
            first
        };
        let mut date = first;
        let mut count = 0;
        while date <= last && count < LEGACY_MAX_DAYS {
            dates.push(date);
            date += Duration::days(1);
            count += 1;
        }
    };
    for item in items {
        match &item.detail {
            AgendaDetail::Maintenance(maintenance) => {
                if !matches!(
                    maintenance.disposition,
                    Disposition::Scheduled | Disposition::Unresolved
                ) {
                    continue;
                }
                if let Some(range) = occupied(maintenance.window()) {
                    push_range(range);
                }
                // 実施中と報告された日も入れる (開始の時刻が書かれていない実施中の告知など)。
                if let Some(bound) = maintenance.ongoing_bound()
                    && let (Some(from), Some(to)) = (bound.first(), bound.last())
                {
                    push_range((from, to));
                }
            }
            AgendaDetail::Period(period) => {
                if counts_for_agenda(period.period.kind) {
                    for (_, range) in milestones(&period.period.window) {
                        push_range(range);
                    }
                }
            }
        }
    }
    dates.sort();
    dates.dedup();
    dates
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schedule::{Evidence, Field, Maintenance};
    use pretty_assertions::assert_eq;

    fn tokyo() -> chrono_tz::Tz {
        chrono_tz::Asia::Tokyo
    }

    /// 日本時間の日時。
    fn jst(month: u32, day: u32, hour: u32, minute: u32) -> DateTime<Utc> {
        tokyo()
            .with_ymd_and_hms(2026, month, day, hour, minute, 0)
            .unwrap()
            .with_timezone(&Utc)
    }

    fn at(month: u32, day: u32, hour: u32, minute: u32) -> Slot<TimeValue> {
        value(
            When::Instant {
                at: jst(month, day, hour, minute),
                zone: Zone::Named("Asia/Tokyo".into()),
            },
            Certainty::Explicit,
        )
    }

    fn about_at(month: u32, day: u32, hour: u32, minute: u32) -> Slot<TimeValue> {
        value(
            When::Instant {
                at: jst(month, day, hour, minute),
                zone: Zone::Named("Asia/Tokyo".into()),
            },
            Certainty::Approximate,
        )
    }

    fn date(month: u32, day: u32) -> Slot<TimeValue> {
        value(
            When::Date {
                date: NaiveDate::from_ymd_opt(2026, month, day).unwrap(),
                zone: Zone::Named("Asia/Tokyo".into()),
            },
            Certainty::Explicit,
        )
    }

    fn value(when: When, certainty: Certainty) -> Slot<TimeValue> {
        Slot::Value(TimeValue {
            when,
            certainty,
            evidence: Vec::new(),
        })
    }

    fn window(start: Slot<TimeValue>, end: Slot<TimeValue>) -> Window {
        Window { start, end }
    }

    /// `month` 月 `day` 日 `hour`:`minute` (日本時間) の時点で実施中という報告。
    fn ongoing_at(month: u32, day: u32, hour: u32, minute: u32) -> Option<Ongoing> {
        let Slot::Value(as_of) = at(month, day, hour, minute) else {
            unreachable!()
        };
        Some(Ongoing {
            as_of,
            evidence: vec![Evidence {
                field: Field::Body,
                text: "現在メンテナンスを実施しております".into(),
            }],
        })
    }

    /// 日付だけの「実施中」の報告。
    fn ongoing_on(month: u32, day: u32) -> Option<Ongoing> {
        let Slot::Value(as_of) = date(month, day) else {
            unreachable!()
        };
        Some(Ongoing {
            as_of,
            evidence: Vec::new(),
        })
    }

    fn extension(from: Slot<TimeValue>, end: Slot<TimeValue>) -> Change {
        Change::Extended {
            from,
            end,
            evidence: Vec::new(),
        }
    }

    fn maintenance(planned: Window, disposition: Disposition) -> ResolvedMaintenance {
        ResolvedMaintenance {
            index: 0,
            scope: Scope::Unknown,
            disposition,
            planned,
            actual: Window::default(),
            changes: Vec::new(),
            ongoing: None,
            resolved_by: None,
        }
    }

    fn item(entry_id: i64, source_id: &str, detail: AgendaDetail) -> AgendaItem {
        AgendaItem {
            entry_id,
            source_id: source_id.into(),
            detail,
        }
    }

    fn period(kind: PeriodKind, start: Slot<TimeValue>, end: Slot<TimeValue>) -> AgendaDetail {
        AgendaDetail::from(AgendaPeriod {
            index: 0,
            period: Period {
                kind,
                label: None,
                window: window(start, end),
                changes: Vec::new(),
                evidence: Vec::new(),
            },
        })
    }

    fn text(maintenance: &ResolvedMaintenance, now: DateTime<Utc>) -> (String, Tone) {
        let badge = maintenance_badge(maintenance, now, &tokyo());
        (badge.text, badge.tone)
    }

    #[test]
    fn badges_follow_the_time_without_claiming_completion() {
        let m = maintenance(
            window(at(11, 12, 14, 0), at(11, 12, 17, 0)),
            Disposition::Scheduled,
        );
        assert_eq!(
            text(&m, jst(11, 10, 9, 0)),
            ("メンテ 11/12(木) 14:00–17:00".into(), Tone::Scheduled)
        );
        // 開始ちょうどから終了の直前までが予定の時間内 ([開始, 終了))。
        assert_eq!(
            maintenance_state(&m, jst(11, 12, 13, 59)),
            MaintenanceState::Upcoming
        );
        assert_eq!(
            text(&m, jst(11, 12, 14, 0)),
            ("メンテ中 (17:00 まで予定)".into(), Tone::Active)
        );
        // 終了予定を過ぎても完了にはしない。
        assert_eq!(
            text(&m, jst(11, 12, 17, 0)),
            ("終了予定を過ぎています".into(), Tone::Attention)
        );
        // しばらくたったら、予定の時間帯だけを出す。
        assert_eq!(
            text(&m, jst(11, 13, 6, 0)),
            ("メンテ 11/12(木) 14:00–17:00".into(), Tone::Muted)
        );

        let mut done = m.clone();
        done.disposition = Disposition::Completed;
        assert_eq!(
            text(&done, jst(11, 12, 15, 0)),
            ("メンテ終了".into(), Tone::Done)
        );
        done.disposition = Disposition::Cancelled;
        assert_eq!(
            text(&done, jst(11, 12, 15, 0)),
            ("メンテ中止".into(), Tone::Muted)
        );
        done.disposition = Disposition::Postponed;
        assert_eq!(
            text(&done, jst(11, 12, 15, 0)),
            ("メンテ延期".into(), Tone::Attention)
        );
        done.disposition = Disposition::Unresolved;
        assert_eq!(
            text(&done, jst(11, 12, 15, 0)),
            ("メンテ 要確認".into(), Tone::Attention)
        );

        // 来年の予定は年を付ける。日をまたぐ時間帯は終わりにも日付を付ける。
        let overnight = maintenance(
            window(at(11, 12, 23, 0), at(11, 13, 2, 0)),
            Disposition::Scheduled,
        );
        assert_eq!(
            text(&overnight, jst(11, 1, 0, 0)).0,
            "メンテ 11/12(木) 23:00–11/13(金) 02:00"
        );
        assert_eq!(
            text(&overnight, jst(11, 12, 23, 30)).0,
            "メンテ中 (11/13(金) 02:00 まで予定)"
        );
        assert_eq!(
            text(&m, Utc.with_ymd_and_hms(2025, 12, 1, 0, 0, 0).unwrap()).0,
            "メンテ 2026/11/12(木) 14:00–17:00"
        );
    }

    #[test]
    fn approximate_and_undecided_ends_are_marked() {
        // 「07:00 から約 5 時間」の終わりは概算なので「頃」を付ける。
        let m = maintenance(
            window(at(9, 28, 7, 0), about_at(9, 28, 12, 0)),
            Disposition::Scheduled,
        );
        assert_eq!(
            text(&m, jst(9, 27, 0, 0)).0,
            "メンテ 9/28(月) 07:00–12:00頃"
        );
        assert_eq!(text(&m, jst(9, 28, 8, 0)).0, "メンテ中 (12:00頃 まで予定)");

        // 延長して終了未定。終わりのない時間帯にはしない。
        let mut extended = maintenance(
            window(at(11, 12, 14, 0), Slot::Undecided),
            Disposition::Scheduled,
        );
        extended.changes.push(Change::Extended {
            from: Slot::Unspecified,
            end: Slot::Undecided,
            evidence: Vec::new(),
        });
        assert_eq!(
            text(&extended, jst(11, 12, 18, 0)),
            ("メンテ延長 (終了未定)".into(), Tone::Attention)
        );
        assert_eq!(
            maintenance_state(&extended, jst(11, 13, 14, 0)),
            MaintenanceState::Past
        );
        extended.changes.clear();
        assert_eq!(text(&extended, jst(11, 12, 18, 0)).0, "メンテ終了未定");
        assert_eq!(
            text(&extended, jst(11, 11, 18, 0)).0,
            "メンテ 11/12(木) 14:00– (終了未定)"
        );

        // 延長して新しい終わりが決まった。
        let mut longer = maintenance(
            window(at(11, 12, 14, 0), at(11, 12, 19, 0)),
            Disposition::Scheduled,
        );
        longer.changes.push(Change::Extended {
            from: Slot::Unspecified,
            end: at(11, 12, 19, 0),
            evidence: Vec::new(),
        });
        assert_eq!(
            text(&longer, jst(11, 12, 18, 0)).0,
            "メンテ延長中 (19:00 まで予定)"
        );

        // 開始が書かれていない「17:00 終了予定」は、予定の時間内とはしない。
        let until = maintenance(
            window(Slot::Unspecified, at(11, 12, 17, 0)),
            Disposition::Scheduled,
        );
        assert_eq!(
            text(&until, jst(11, 12, 15, 0)),
            ("メンテ (17:00 まで予定)".into(), Tone::Scheduled)
        );
    }

    #[test]
    fn date_only_schedules_are_never_in_progress() {
        let m = maintenance(
            window(date(11, 12), Slot::Unspecified),
            Disposition::Scheduled,
        );
        assert_eq!(text(&m, jst(11, 10, 0, 0)).0, "メンテ 11/12(木)");
        let state = maintenance_state(&m, jst(11, 12, 15, 0));
        assert_eq!(state, MaintenanceState::OnDate);
        assert_eq!(
            text(&m, jst(11, 12, 15, 0)),
            ("メンテ 11/12(木)".into(), Tone::Scheduled)
        );
        assert_eq!(
            maintenance_state(&m, jst(11, 13, 0, 0)),
            MaintenanceState::Past
        );

        let undated = maintenance(Window::default(), Disposition::Scheduled);
        assert_eq!(
            text(&undated, jst(11, 12, 15, 0)),
            ("メンテ".into(), Tone::Muted)
        );
        let undecided = maintenance(
            window(Slot::Undecided, Slot::Unspecified),
            Disposition::Scheduled,
        );
        assert_eq!(text(&undecided, jst(11, 12, 15, 0)).0, "メンテ 日時未定");

        // 対象が書かれていれば、バッジの先頭に出す。
        let mut pc = maintenance(
            window(at(11, 12, 14, 0), at(11, 12, 17, 0)),
            Disposition::Scheduled,
        );
        pc.scope = Scope::Named("PC版".into());
        assert_eq!(
            text(&pc, jst(11, 12, 15, 0)).0,
            "PC版 メンテ中 (17:00 まで予定)"
        );
        assert!(
            maintenance_badge(&pc, jst(11, 12, 15, 0), &tokyo())
                .detail
                .contains("メンテナンス (PC版)")
        );
    }

    #[test]
    fn today_and_tomorrow_use_local_calendar_days() {
        let now = jst(11, 12, 13, 0);
        let days = Days::at(now, &tokyo());
        assert_eq!(days.today, jst(11, 12, 0, 0)..jst(11, 13, 0, 0));
        assert_eq!(days.tomorrow, jst(11, 13, 0, 0)..jst(11, 14, 0, 0));
        let priority = |detail: AgendaDetail| item_priority(&item(1, "a", detail), now, &days);
        let scheduled = |start, end| {
            AgendaDetail::from(maintenance(window(start, end), Disposition::Scheduled))
        };
        let cases = [
            (
                "今日の予定",
                at(11, 12, 14, 0),
                at(11, 12, 17, 0),
                Priority::Today,
            ),
            (
                "明日の予定",
                at(11, 13, 14, 0),
                at(11, 13, 17, 0),
                Priority::Tomorrow,
            ),
            (
                "明後日より先",
                at(11, 15, 14, 0),
                at(11, 15, 17, 0),
                Priority::Normal,
            ),
            // 今日の夜から明日にかけての時間帯は今日に重なる。
            (
                "日をまたぐ",
                at(11, 12, 23, 0),
                at(11, 13, 2, 0),
                Priority::Today,
            ),
            // 明日の 0 時ちょうどに始まるものは明日。明後日の 0 時ちょうどに始まるものは明日に重ならない。
            (
                "明日の 0 時から",
                at(11, 13, 0, 0),
                at(11, 13, 3, 0),
                Priority::Tomorrow,
            ),
            (
                "明後日の 0 時から",
                at(11, 14, 0, 0),
                at(11, 14, 3, 0),
                Priority::Normal,
            ),
            (
                "明後日の 0 時まで",
                at(11, 13, 22, 0),
                at(11, 14, 0, 0),
                Priority::Tomorrow,
            ),
            // 日付だけの予定は、その日に数える。
            (
                "日付だけ",
                date(11, 13),
                Slot::Unspecified,
                Priority::Tomorrow,
            ),
            // 終わってしばらくたった予定は、今日の時間帯でも数えない (完了の報告がなくても)。
            (
                "今日の未明に終わった",
                at(11, 12, 0, 0),
                at(11, 12, 0, 30),
                Priority::Normal,
            ),
            // 終了未定は、始まりから OPEN_FOR を過ぎたら数えない。
            (
                "終了未定",
                at(11, 10, 14, 0),
                Slot::Undecided,
                Priority::Normal,
            ),
        ];
        for (label, start, end, expected) in cases {
            assert_eq!(priority(scheduled(start, end)), expected, "{label}");
        }
        // 終わったメンテナンス・中止したメンテナンスは数えない。
        let mut done = maintenance(
            window(at(11, 12, 14, 0), at(11, 12, 17, 0)),
            Disposition::Completed,
        );
        assert_eq!(priority(AgendaDetail::from(done.clone())), Priority::Normal);
        done.disposition = Disposition::Cancelled;
        assert_eq!(priority(AgendaDetail::from(done)), Priority::Normal);
        // 決められないものも、時刻の上では同じに数える。
        let unresolved = maintenance(
            window(at(11, 13, 14, 0), at(11, 13, 17, 0)),
            Disposition::Unresolved,
        );
        assert_eq!(priority(AgendaDetail::from(unresolved)), Priority::Tomorrow);

        // 期間は種類ごとに数えるかを決め、始まりと終わりの節目だけを数える。
        let periods = [
            (
                "明日終わる",
                PeriodKind::Event,
                at(11, 1, 15, 0),
                at(11, 13, 13, 59),
                Priority::Tomorrow,
            ),
            (
                "節目がない",
                PeriodKind::Event,
                at(11, 1, 15, 0),
                at(11, 30, 13, 59),
                Priority::Normal,
            ),
            // 始まりの節目は、始まったあとも今日のうちは数える。
            (
                "今日始まった",
                PeriodKind::Gacha,
                at(11, 12, 12, 0),
                at(11, 30, 0, 0),
                Priority::Today,
            ),
            // 終わりの節目は、終わってしまったら数えない。
            (
                "今日終わった",
                PeriodKind::Event,
                at(11, 1, 15, 0),
                at(11, 12, 12, 59),
                Priority::Normal,
            ),
            // 0 時ちょうどの終わりは前の日の節目。
            (
                "今夜 0 時まで",
                PeriodKind::Campaign,
                at(11, 1, 0, 0),
                at(11, 13, 0, 0),
                Priority::Today,
            ),
            // 補填の受け取り期限や販売の期限は数えない。
            (
                "受け取り期限",
                PeriodKind::Claim,
                Slot::Unspecified,
                at(11, 12, 23, 59),
                Priority::Normal,
            ),
            (
                "販売の期限",
                PeriodKind::Sale,
                Slot::Unspecified,
                at(11, 12, 23, 59),
                Priority::Normal,
            ),
        ];
        for (label, kind, start, end, expected) in periods {
            assert_eq!(priority(period(kind, start, end)), expected, "{label}");
        }

        // 表示の地域時刻が違えば、今日・明日の区切りも変わる。日本時間 11/13 8:00–9:00 は、
        // UTC では 11/12 23:00–24:00 なので、UTC で見ると今日、日本時間で見ると明日。
        let morning = jst(11, 12, 10, 0);
        let tomorrow_morning = item(1, "a", scheduled(at(11, 13, 8, 0), at(11, 13, 9, 0)));
        let utc_days = Days::at(morning, &Utc);
        assert_eq!(
            utc_days.today.start,
            Utc.with_ymd_and_hms(2026, 11, 12, 0, 0, 0).unwrap()
        );
        assert_eq!(
            item_priority(&tomorrow_morning, morning, &utc_days),
            Priority::Today
        );
        assert_eq!(
            item_priority(&tomorrow_morning, morning, &Days::at(morning, &tokyo())),
            Priority::Tomorrow
        );
    }

    #[test]
    fn running_or_overdue_maintenance_counts_as_today() {
        // 前の日に始まって終了未定のまま。始まりの日は昨日だが、続いているかもしれないので今日に出す。
        let now = jst(11, 13, 1, 0);
        let days = Days::at(now, &tokyo());
        let open = item(
            1,
            "a",
            AgendaDetail::from(maintenance(
                window(at(11, 12, 22, 0), Slot::Undecided),
                Disposition::Scheduled,
            )),
        );
        assert_eq!(item_priority(&open, now, &days), Priority::Today);
        // 終了予定を過ぎたばかりで、完了の報告がない。
        let overdue = item(
            2,
            "a",
            AgendaDetail::from(maintenance(
                window(at(11, 12, 20, 0), at(11, 12, 23, 0)),
                Disposition::Scheduled,
            )),
        );
        assert_eq!(item_priority(&overdue, now, &days), Priority::Today);
    }

    #[test]
    fn period_badges_show_milestones_only() {
        let now = jst(11, 12, 10, 0);
        let days = Days::at(now, &tokyo());
        let badge = |detail: AgendaDetail| {
            let AgendaDetail::Period(period) = detail else {
                unreachable!()
            };
            period_badge(&period.period, now, &days, &tokyo()).map(|badge| badge.text)
        };
        assert_eq!(
            badge(period(
                PeriodKind::Event,
                at(11, 12, 15, 0),
                at(11, 26, 13, 59)
            ))
            .as_deref(),
            Some("イベント 今日 15:00 から")
        );
        assert_eq!(
            badge(period(
                PeriodKind::Gacha,
                at(11, 1, 15, 0),
                at(11, 13, 13, 59)
            ))
            .as_deref(),
            Some("ガチャ 明日 13:59 まで")
        );
        assert_eq!(
            badge(period(PeriodKind::Event, date(11, 13), date(11, 20))).as_deref(),
            Some("イベント 明日から")
        );
        assert_eq!(
            badge(period(
                PeriodKind::Event,
                at(11, 1, 15, 0),
                at(11, 30, 13, 59)
            )),
            None
        );
        assert_eq!(
            badge(period(
                PeriodKind::Claim,
                Slot::Unspecified,
                at(11, 12, 23, 59)
            )),
            None
        );
        // 今日の朝に終わった期間は出さない。今夜終わる期間は出す。
        assert_eq!(
            badge(period(
                PeriodKind::Gacha,
                at(11, 1, 15, 0),
                at(11, 12, 9, 59)
            )),
            None
        );
        assert_eq!(
            badge(period(
                PeriodKind::Gacha,
                at(11, 1, 15, 0),
                at(11, 12, 23, 59)
            ))
            .as_deref(),
            Some("ガチャ 今日 23:59 まで")
        );

        // 同じ記事に同じ文言の期間が並んでも、バッジは 1 つにまとめる。明日の分は今日の分の後ろ。
        let agenda = Agenda::new(vec![
            item(
                1,
                "a",
                period(PeriodKind::Event, at(11, 13, 15, 0), at(11, 30, 0, 0)),
            ),
            item(
                1,
                "a",
                period(PeriodKind::Gacha, at(11, 12, 15, 0), at(11, 30, 0, 0)),
            ),
            item(
                1,
                "a",
                period(PeriodKind::Gacha, at(11, 12, 15, 0), at(11, 26, 0, 0)),
            ),
        ]);
        let texts: Vec<String> = agenda
            .badges(1, now, &days, &tokyo())
            .into_iter()
            .map(|badge| badge.text)
            .collect();
        assert_eq!(
            texts,
            vec!["ガチャ 今日 15:00 から", "イベント 明日 15:00 から"]
        );
    }

    #[test]
    fn agenda_orders_badges_marks_sources_and_finds_boundaries() {
        let now = jst(11, 12, 15, 0);
        let days = Days::at(now, &tokyo());
        let running = maintenance(
            window(at(11, 12, 14, 0), at(11, 12, 17, 0)),
            Disposition::Scheduled,
        );
        let mut pc = maintenance(
            window(at(11, 20, 14, 0), at(11, 20, 17, 0)),
            Disposition::Scheduled,
        );
        pc.index = 1;
        let later = maintenance(
            window(at(11, 12, 20, 0), at(11, 12, 22, 0)),
            Disposition::Scheduled,
        );
        let agenda = Agenda::new(vec![
            item(1, "game", AgendaDetail::from(pc)),
            item(1, "game", AgendaDetail::from(running)),
            item(
                1,
                "game",
                period(PeriodKind::Event, at(11, 13, 15, 0), at(11, 26, 13, 59)),
            ),
            item(2, "other", AgendaDetail::from(later)),
            item(
                3,
                "quiet",
                period(PeriodKind::Claim, Slot::Unspecified, at(11, 12, 23, 59)),
            ),
        ]);
        let texts: Vec<String> = agenda
            .badges(1, now, &days, &tokyo())
            .into_iter()
            .map(|badge| badge.text)
            .collect();
        assert_eq!(
            texts,
            vec![
                "メンテ中 (17:00 まで予定)",
                "メンテ 11/20(金) 14:00–17:00",
                "イベント 明日 15:00 から"
            ]
        );
        assert_eq!(agenda.priority(1, now, &days), Priority::Today);
        assert_eq!(agenda.priority(3, now, &days), Priority::Normal);
        assert_eq!(agenda.priority(99, now, &days), Priority::Normal);
        assert_eq!(agenda.prioritized(now, &days), vec![1, 2]);

        let marks = agenda.marks(now, &days, &tokyo());
        assert_eq!(marks["game"].level, MarkLevel::Now);
        assert_eq!(marks["game"].badge.text, "メンテ中 (17:00 まで予定)");
        assert_eq!(marks["other"].level, MarkLevel::Today);
        assert!(!marks.contains_key("quiet"));

        // 状態が変わりうる時刻: 開始・終了・終了予定を過ぎてからの区切り。
        assert_eq!(agenda.next_boundary(now), Some(jst(11, 12, 17, 0)));
        assert_eq!(
            agenda.next_boundary(jst(11, 12, 17, 0)),
            Some(jst(11, 12, 20, 0))
        );
        assert_eq!(
            agenda.next_boundary(jst(11, 12, 22, 0)),
            Some(jst(11, 13, 5, 0))
        );
        // 数える種類の期間の終わりも区切りにする (始まり・数えない種類の期間は区切りにしない)。
        assert_eq!(
            agenda.next_boundary(jst(11, 21, 5, 0)),
            Some(jst(11, 26, 13, 59))
        );
        assert_eq!(agenda.next_boundary(jst(12, 31, 0, 0)), None);
    }

    #[test]
    fn schedule_filters_find_articles_by_day_start_and_end() {
        let now = jst(11, 13, 12, 0);
        let days = Days::at(now, &tokyo());
        let planned = |entry_id: i64, start, end, disposition| {
            item(
                entry_id,
                "game",
                AgendaDetail::from(maintenance(window(start, end), disposition)),
            )
        };
        let agenda = Agenda::new(vec![
            // 予定の時間内。
            planned(
                1,
                at(11, 13, 11, 0),
                at(11, 13, 14, 0),
                Disposition::Scheduled,
            ),
            // 今日の夜から明日にかけて。
            planned(
                2,
                at(11, 13, 23, 0),
                at(11, 14, 2, 0),
                Disposition::Scheduled,
            ),
            // 0 時ちょうどに終わる (今日の終わりに数える)。
            planned(
                3,
                at(11, 13, 22, 0),
                at(11, 14, 0, 0),
                Disposition::Scheduled,
            ),
            // 完了の報告がある (始まりには数え、終わりには数えない)。
            planned(
                4,
                at(11, 13, 4, 0),
                at(11, 13, 6, 0),
                Disposition::Completed,
            ),
            // 中止 (今日に関係するが、始まりにも終わりにも数えない)。
            planned(
                5,
                at(11, 13, 15, 0),
                at(11, 13, 16, 0),
                Disposition::Cancelled,
            ),
            // 今日終わるガチャ・明日始まるイベント。
            item(
                6,
                "game",
                period(PeriodKind::Gacha, at(11, 1, 12, 0), at(11, 13, 23, 59)),
            ),
            item(
                7,
                "game",
                period(PeriodKind::Event, at(11, 14, 15, 0), at(11, 20, 23, 59)),
            ),
            // 今日のうちに終わったキャンペーンは、もう今日終わるものに数えない。
            item(
                8,
                "game",
                period(PeriodKind::Campaign, at(11, 12, 0, 0), at(11, 13, 11, 59)),
            ),
            // 販売はバッジと同じく数えない。
            item(
                9,
                "game",
                period(PeriodKind::Sale, at(11, 13, 12, 0), at(11, 13, 20, 0)),
            ),
            // 日付だけの始まり。
            item(
                10,
                "game",
                period(PeriodKind::Event, date(11, 13), Slot::Unspecified),
            ),
        ]);
        let ids = |filter| agenda.filter_entries(filter, now, &days);
        assert_eq!(
            ids(ScheduleFilter::Related(Day::Today)),
            vec![1, 2, 3, 4, 5, 6, 10]
        );
        assert_eq!(ids(ScheduleFilter::Related(Day::Tomorrow)), vec![2, 7]);
        assert_eq!(
            ids(ScheduleFilter::Starts(Day::Today)),
            vec![1, 2, 3, 4, 10]
        );
        assert_eq!(ids(ScheduleFilter::Starts(Day::Tomorrow)), vec![7]);
        assert_eq!(ids(ScheduleFilter::Ends(Day::Today)), vec![1, 3, 6]);
        assert_eq!(ids(ScheduleFilter::Ends(Day::Tomorrow)), vec![2]);
        // 期間は終わりを過ぎると、今日終わるものから外れる (6)。メンテナンスは、終了予定を過ぎても
        // 完了の報告がなければしばらく残す (1)。
        assert_eq!(
            agenda.filter_entries(ScheduleFilter::Ends(Day::Today), jst(11, 13, 23, 59), &days),
            vec![1, 3]
        );
    }

    #[test]
    fn recheck_window_covers_the_schedule_and_a_little_after() {
        let m = maintenance(
            window(at(11, 12, 14, 0), at(11, 12, 17, 0)),
            Disposition::Scheduled,
        );
        assert!(!needs_recheck(&m, jst(11, 4, 0, 0)));
        assert!(needs_recheck(&m, jst(11, 6, 0, 0)));
        assert!(needs_recheck(&m, jst(11, 12, 15, 0)));
        assert!(needs_recheck(&m, jst(11, 13, 4, 59)));
        assert!(!needs_recheck(&m, jst(11, 13, 5, 0)));

        let mut done = m.clone();
        done.disposition = Disposition::Completed;
        assert!(!needs_recheck(&done, jst(11, 12, 15, 0)));
        done.disposition = Disposition::Cancelled;
        assert!(!needs_recheck(&done, jst(11, 12, 15, 0)));
        let mut unresolved = m.clone();
        unresolved.disposition = Disposition::Unresolved;
        assert!(needs_recheck(&unresolved, jst(11, 12, 15, 0)));

        // 終了未定は、始まりから OPEN_FOR を終わりとみなす。
        let open = maintenance(
            window(at(11, 12, 14, 0), Slot::Undecided),
            Disposition::Scheduled,
        );
        assert!(needs_recheck(&open, jst(11, 13, 13, 0)));
        assert!(!needs_recheck(&open, jst(11, 14, 3, 0)));
        // 日時がなければ取り直さない。
        assert!(!needs_recheck(
            &maintenance(Window::default(), Disposition::Scheduled),
            jst(11, 12, 15, 0)
        ));
    }

    #[test]
    fn builds_items_from_schedules_and_resolutions() {
        let evidence = vec![Evidence {
            field: Field::Body,
            text: "11/12(木) 14:00～17:00".into(),
        }];
        let schedule = Schedule {
            maintenance: vec![Maintenance {
                scope: Scope::All,
                planned: window(at(11, 12, 14, 0), at(11, 12, 17, 0)),
                actual: Window::default(),
                disposition: Disposition::Scheduled,
                ongoing: None,
                changes: Vec::new(),
                evidence: evidence.clone(),
            }],
            periods: vec![Period {
                kind: PeriodKind::Claim,
                label: Some("補填".into()),
                window: window(Slot::Unspecified, at(11, 19, 23, 59)),
                changes: Vec::new(),
                evidence,
            }],
            notes: Vec::new(),
        };
        let articles = vec![SourceSchedule {
            entry_id: 7,
            published: None,
            first_seen_at: jst(11, 10, 12, 0),
            schedule,
        }];
        // 別の記事 (8) が延長と完了を報告した。
        let extended = Change::Extended {
            from: at(11, 12, 17, 0),
            end: at(11, 12, 18, 0),
            evidence: Vec::new(),
        };
        let resolutions = vec![Resolution {
            entry_id: 7,
            index: 0,
            disposition: Disposition::Completed,
            planned: window(at(11, 12, 14, 0), at(11, 12, 17, 0)),
            actual: window(at(11, 12, 14, 0), at(11, 12, 16, 30)),
            changes: vec![extended.clone()],
            ongoing: None,
            resolved_by: Some(8),
        }];
        let items = build_items("game", &articles, &resolutions);
        assert_eq!(items.len(), 2);
        let AgendaDetail::Maintenance(resolved) = &items[0].detail else {
            panic!("メンテナンス")
        };
        assert_eq!(resolved.disposition, Disposition::Completed);
        assert_eq!(resolved.resolved_by, Some(8));
        assert_eq!(resolved.scope, Scope::All);
        // 別の記事で報告された延長も、予告の記事のメンテナンスに付く。
        assert_eq!(resolved.changes, vec![extended]);
        assert!(matches!(items[1].detail, AgendaDetail::Period(_)));
        // 突き合わせの結果がなければ記事の記述のまま。
        let plain = build_items("game", &articles, &[]);
        let AgendaDetail::Maintenance(resolved) = &plain[0].detail else {
            panic!("メンテナンス")
        };
        assert_eq!(resolved.disposition, Disposition::Scheduled);
        assert_eq!(resolved.resolved_by, None);
        assert!(resolved.changes.is_empty());

        // 別の記事で報告された延長 (18:00 まで) も、表を引く範囲に入る。
        assert_eq!(
            span(&items[0].detail),
            Some((jst(11, 12, 14, 0), jst(11, 12, 18, 0)))
        );
        assert_eq!(
            span(&plain[0].detail),
            Some((jst(11, 12, 14, 0), jst(11, 12, 17, 0)))
        );
        assert_eq!(
            span(&items[1].detail),
            Some((jst(11, 19, 23, 59), jst(11, 19, 23, 59)))
        );
        let open = AgendaDetail::from(maintenance(
            window(at(11, 12, 14, 0), Slot::Undecided),
            Disposition::Scheduled,
        ));
        assert_eq!(span(&open), Some((jst(11, 12, 14, 0), jst(11, 13, 14, 0))));
        assert_eq!(
            span(&AgendaDetail::from(maintenance(
                Window::default(),
                Disposition::Scheduled
            ))),
            None
        );
        // 日付だけの値は、その地域の暦日の範囲。
        assert_eq!(
            span(&AgendaDetail::from(maintenance(
                window(date(11, 12), Slot::Unspecified),
                Disposition::Scheduled
            ))),
            Some((jst(11, 12, 0, 0), jst(11, 13, 0, 0)))
        );
    }

    #[test]
    fn legacy_dates_list_local_days() {
        let items = vec![
            item(
                1,
                "a",
                AgendaDetail::from(maintenance(
                    window(at(11, 12, 23, 0), at(11, 13, 2, 0)),
                    Disposition::Scheduled,
                )),
            ),
            item(
                1,
                "a",
                AgendaDetail::from(maintenance(
                    window(at(11, 20, 14, 0), at(11, 20, 17, 0)),
                    Disposition::Completed,
                )),
            ),
            item(
                1,
                "a",
                period(PeriodKind::Event, at(11, 14, 15, 0), at(11, 26, 0, 0)),
            ),
            item(
                1,
                "a",
                period(PeriodKind::Claim, Slot::Unspecified, at(11, 30, 23, 59)),
            ),
        ];
        let day = |d| NaiveDate::from_ymd_opt(2026, 11, d).unwrap();
        assert_eq!(
            legacy_dates(&items, &tokyo()),
            vec![day(12), day(13), day(14), day(25)]
        );
    }

    #[test]
    fn ongoing_reports_without_a_start_mean_in_progress() {
        // 「現在メンテナンスを実施しております。17:00 終了予定です。」(11/6 15:00 の追記。開始は書かれていない)
        let mut until_five = maintenance(
            window(Slot::Unspecified, at(11, 6, 17, 0)),
            Disposition::Scheduled,
        );
        until_five.ongoing = ongoing_at(11, 6, 15, 0);
        // 報告の時点より前は、終わりだけの予定のまま。
        assert_eq!(
            text(&until_five, jst(11, 6, 14, 0)),
            ("メンテ (17:00 まで予定)".into(), Tone::Scheduled)
        );
        assert_eq!(
            text(&until_five, jst(11, 6, 15, 30)),
            ("メンテ中 (17:00 まで予定)".into(), Tone::Active)
        );
        let detail = maintenance_badge(&until_five, jst(11, 6, 15, 30), &tokyo()).detail;
        assert!(detail.contains("11/6(金) 15:00 の時点で実施中"), "{detail}");
        assert!(detail.contains("実施中と報告されています"), "{detail}");
        // 終わりを過ぎても完了にはしない。
        assert_eq!(
            text(&until_five, jst(11, 6, 17, 0)).0,
            "終了予定を過ぎています"
        );

        // 「終了時間は未定です」。報告の時点から OPEN_FOR のあいだだけ「メンテ中 (終了未定)」。
        let mut open = maintenance(
            window(Slot::Unspecified, Slot::Undecided),
            Disposition::Scheduled,
        );
        open.ongoing = ongoing_at(11, 6, 15, 0);
        assert_eq!(
            text(&open, jst(11, 6, 16, 0)),
            ("メンテ中 (終了未定)".into(), Tone::Active)
        );
        assert_eq!(
            maintenance_state(&open, jst(11, 7, 14, 59)),
            MaintenanceState::ReportedOngoing
        );
        assert_eq!(
            maintenance_state(&open, jst(11, 7, 15, 0)),
            MaintenanceState::Past
        );
        let detail = maintenance_badge(&open, jst(11, 6, 16, 0), &tokyo()).detail;
        assert!(detail.contains("終了の時刻は未定です"), "{detail}");
        // 終わりが書かれていなくても同じ。
        let mut unwritten = maintenance(Window::default(), Disposition::Scheduled);
        unwritten.ongoing = ongoing_at(11, 6, 15, 0);
        assert_eq!(text(&unwritten, jst(11, 6, 16, 0)).0, "メンテ中 (終了未定)");
        // 延長して終了未定になった。
        open.changes
            .push(extension(at(11, 6, 17, 0), Slot::Undecided));
        assert_eq!(text(&open, jst(11, 6, 18, 0)).0, "メンテ延長中 (終了未定)");

        // 日付だけの報告からは「メンテ中」にしない (説明には出す)。
        let mut dated = maintenance(
            window(Slot::Unspecified, Slot::Undecided),
            Disposition::Scheduled,
        );
        dated.ongoing = ongoing_on(11, 6);
        assert_ne!(
            maintenance_state(&dated, jst(11, 6, 16, 0)),
            MaintenanceState::ReportedOngoing
        );
        assert_ne!(text(&dated, jst(11, 6, 16, 0)).1, Tone::Active);
        let detail = maintenance_badge(&dated, jst(11, 6, 16, 0), &tokyo()).detail;
        assert!(detail.contains("11/6(金) の時点で実施中"), "{detail}");

        // 開始の時刻があれば、報告は使わない。
        let mut started = maintenance(
            window(at(11, 6, 18, 0), at(11, 6, 20, 0)),
            Disposition::Scheduled,
        );
        started.ongoing = ongoing_at(11, 6, 15, 0);
        assert_eq!(
            maintenance_state(&started, jst(11, 6, 16, 0)),
            MaintenanceState::Upcoming
        );

        // 終わったメンテナンスに残った報告も使わない。
        let mut done = until_five.clone();
        done.disposition = Disposition::Completed;
        let badge = maintenance_badge(&done, jst(11, 6, 15, 30), &tokyo());
        assert_eq!(badge.text, "メンテ終了");
        assert!(!badge.detail.contains("実施中"), "{}", badge.detail);
    }

    #[test]
    fn ongoing_reports_set_ranges_rechecks_order_marks_and_legacy_dates() {
        let mut open = maintenance(
            window(Slot::Unspecified, Slot::Undecided),
            Disposition::Scheduled,
        );
        open.ongoing = ongoing_at(11, 6, 15, 0);
        let detail = AgendaDetail::from(open.clone());
        // 表を引く範囲は、報告の時点から OPEN_FOR。
        assert_eq!(span(&detail), Some((jst(11, 6, 15, 0), jst(11, 7, 15, 0))));
        // 本文の取り直しも、報告の時点を開始の代わりにする (OPEN_FOR を終わりとみなし、その RECHECK_AFTER 後まで)。
        assert!(needs_recheck(&open, jst(11, 7, 14, 0)));
        assert!(needs_recheck(&open, jst(11, 8, 2, 59)));
        assert!(!needs_recheck(&open, jst(11, 8, 3, 0)));

        let now = jst(11, 6, 16, 0);
        let days = Days::at(now, &tokyo());
        let agenda = Agenda::new(vec![item(1, "game", detail)]);
        // 今日の予定として上に出し、サイドバーは「今」の印。
        assert_eq!(agenda.priority(1, now, &days), Priority::Today);
        let marks = agenda.marks(now, &days, &tokyo());
        assert_eq!(marks["game"].level, MarkLevel::Now);
        assert_eq!(marks["game"].badge.text, "メンテ中 (終了未定)");
        // 報告の時点と、そこから OPEN_FOR たった時点で状態が変わる。
        assert_eq!(
            agenda.next_boundary(jst(11, 6, 14, 0)),
            Some(jst(11, 6, 15, 0))
        );
        assert_eq!(agenda.next_boundary(now), Some(jst(11, 7, 15, 0)));
        // 旧形式の日付にも、実施中と報告された日を入れる。
        assert_eq!(
            legacy_dates(agenda.items(), &tokyo()),
            vec![NaiveDate::from_ymd_opt(2026, 11, 6).unwrap()]
        );
    }

    #[test]
    fn changes_show_before_and_after() {
        let now = jst(11, 12, 10, 0);
        let tz = tokyo();
        let cases = [
            (
                extension(at(11, 12, 17, 0), at(11, 12, 19, 0)),
                "延長 11/12(木) 17:00 → 19:00",
            ),
            (
                extension(at(11, 12, 23, 0), at(11, 13, 2, 0)),
                "延長 11/12(木) 23:00 → 11/13(金) 02:00",
            ),
            (
                extension(at(11, 12, 17, 0), Slot::Undecided),
                "延長 11/12(木) 17:00 → 未定",
            ),
            (
                extension(Slot::Unspecified, at(11, 12, 19, 0)),
                "延長 (11/12(木) 19:00 まで)",
            ),
            (
                extension(Slot::Unspecified, Slot::Undecided),
                "延長 (終了未定)",
            ),
            (
                Change::Rescheduled {
                    from: window(at(11, 10, 14, 0), at(11, 10, 17, 0)),
                    to: window(at(11, 12, 14, 0), at(11, 12, 17, 0)),
                    evidence: Vec::new(),
                },
                "日程変更 11/10(火) 14:00–17:00 → 11/12(木) 14:00–17:00",
            ),
            // 変更の後が書かれていない (変更の後を持つ前の読み取り結果)。
            (
                Change::Rescheduled {
                    from: window(at(11, 10, 14, 0), at(11, 10, 17, 0)),
                    to: Window::default(),
                    evidence: Vec::new(),
                },
                "日程変更 (変更前: 11/10(火) 14:00–17:00)",
            ),
        ];
        for (change, expected) in cases {
            assert_eq!(change_text(&change, now, &tz), expected);
        }

        // 延長はバッジの説明にも出す。
        let mut longer = maintenance(
            window(at(11, 12, 14, 0), at(11, 12, 19, 0)),
            Disposition::Scheduled,
        );
        longer
            .changes
            .push(extension(at(11, 12, 17, 0), at(11, 12, 19, 0)));
        let badge = maintenance_badge(&longer, jst(11, 12, 18, 0), &tz);
        assert_eq!(badge.text, "メンテ延長中 (19:00 まで予定)");
        assert!(
            badge.detail.contains("延長 11/12(木) 17:00 → 19:00"),
            "{}",
            badge.detail
        );
    }

    #[test]
    fn period_changes_mark_extensions_and_use_the_final_window() {
        let now = jst(11, 12, 10, 0);
        let days = Days::at(now, &tokyo());
        // 今日 13:59 までだったイベントを、明日 13:59 まで延長した (期間は延長の後のもの)。
        let mut extended = Period {
            kind: PeriodKind::Event,
            label: Some("ハロウィン".into()),
            window: window(at(11, 1, 15, 0), at(11, 13, 13, 59)),
            changes: vec![extension(at(11, 12, 13, 59), at(11, 13, 13, 59))],
            evidence: Vec::new(),
        };
        let badge = period_badge(&extended, now, &days, &tokyo()).unwrap();
        assert_eq!(badge.text, "イベント 明日 13:59 まで (延長)");
        assert!(
            badge
                .detail
                .contains("延長 11/12(木) 13:59 → 11/13(金) 13:59"),
            "{}",
            badge.detail
        );
        // 今日・明日は最後の期間で決める (延長する前の今日の終わりでは数えない)。
        let row = |period: &Period| {
            item(
                1,
                "a",
                AgendaDetail::from(AgendaPeriod {
                    index: 0,
                    period: period.clone(),
                }),
            )
        };
        assert_eq!(
            item_priority(&row(&extended), now, &days),
            Priority::Tomorrow
        );
        // 来週まで延びたなら、今日・明日のどちらでもない。
        extended.window.end = at(11, 20, 13, 59);
        extended.changes = vec![extension(at(11, 12, 13, 59), at(11, 20, 13, 59))];
        assert_eq!(period_badge(&extended, now, &days, &tokyo()), None);
        assert_eq!(item_priority(&row(&extended), now, &days), Priority::Normal);

        // 日程変更はバッジの説明に出す。表を引く範囲には変更の前の期間も入る。
        let rescheduled = Period {
            kind: PeriodKind::Gacha,
            label: None,
            window: window(at(11, 13, 15, 0), at(11, 30, 0, 0)),
            changes: vec![Change::Rescheduled {
                from: window(at(11, 12, 15, 0), at(11, 30, 0, 0)),
                to: window(at(11, 13, 15, 0), at(11, 30, 0, 0)),
                evidence: Vec::new(),
            }],
            evidence: Vec::new(),
        };
        let badge = period_badge(&rescheduled, now, &days, &tokyo()).unwrap();
        assert_eq!(badge.text, "ガチャ 明日 15:00 から");
        assert!(
            badge.detail.contains(
                "日程変更 11/12(木) 15:00–11/30(月) 00:00 → 11/13(金) 15:00–11/30(月) 00:00"
            ),
            "{}",
            badge.detail
        );
        assert_eq!(
            span(&row(&rescheduled).detail),
            Some((jst(11, 12, 15, 0), jst(11, 30, 0, 0)))
        );
    }

    #[test]
    fn maintenance_for_several_scopes_share_one_badge() {
        let now = jst(11, 12, 15, 0);
        let days = Days::at(now, &tokyo());
        let scoped = |index: usize, name: &str, disposition: Disposition| {
            let mut m = maintenance(window(at(11, 12, 14, 0), at(11, 12, 17, 0)), disposition);
            m.index = index;
            m.scope = Scope::Named(name.into());
            item(1, "game", AgendaDetail::from(m))
        };
        let texts = |agenda: &Agenda| -> Vec<String> {
            agenda
                .badges(1, now, &days, &tokyo())
                .into_iter()
                .map(|badge| badge.text)
                .collect()
        };

        // 同じ時間帯・同じ状態で、対象だけが違う。
        let two = Agenda::new(vec![
            scoped(0, "Android版", Disposition::Scheduled),
            scoped(1, "iOS版", Disposition::Scheduled),
        ]);
        assert_eq!(
            texts(&two),
            vec!["Android版・iOS版 メンテ中 (17:00 まで予定)"]
        );
        let badge = two.badges(1, now, &days, &tokyo()).remove(0);
        assert!(
            badge.detail.starts_with("メンテナンス (Android版・iOS版)"),
            "{}",
            badge.detail
        );
        // サイドバーの印の説明も、まとめたもの。
        assert_eq!(
            two.marks(now, &days, &tokyo())["game"].badge.text,
            "Android版・iOS版 メンテ中 (17:00 まで予定)"
        );

        // 3 つ以上は短くし、説明には全部の名前を出す。
        let three = Agenda::new(vec![
            scoped(0, "PC版", Disposition::Scheduled),
            scoped(1, "Android版", Disposition::Scheduled),
            scoped(2, "iOS版", Disposition::Scheduled),
        ]);
        let badge = three.badges(1, now, &days, &tokyo()).remove(0);
        assert_eq!(badge.text, "PC版ほか 2 件 メンテ中 (17:00 まで予定)");
        assert!(
            badge
                .detail
                .starts_with("メンテナンス (PC版・Android版・iOS版)"),
            "{}",
            badge.detail
        );

        // 状態が違う (一方だけ完了) ものはまとめない。
        let split = Agenda::new(vec![
            scoped(0, "PC版", Disposition::Completed),
            scoped(1, "Android版", Disposition::Scheduled),
        ]);
        assert_eq!(
            texts(&split),
            vec!["Android版 メンテ中 (17:00 まで予定)", "PC版 メンテ終了"]
        );

        // 時間帯が違うものもまとめない。
        let mut later = maintenance(
            window(at(11, 12, 14, 0), at(11, 12, 19, 0)),
            Disposition::Scheduled,
        );
        later.index = 1;
        later.scope = Scope::Named("iOS版".into());
        let different = Agenda::new(vec![
            scoped(0, "Android版", Disposition::Scheduled),
            item(1, "game", AgendaDetail::from(later)),
        ]);
        assert_eq!(
            texts(&different),
            vec![
                "Android版 メンテ中 (17:00 まで予定)",
                "iOS版 メンテ中 (19:00 まで予定)"
            ]
        );
    }

    #[test]
    fn day_entries_group_the_schedule_of_today_and_tomorrow() {
        let now = jst(11, 13, 12, 0);
        let days = Days::at(now, &tokyo());
        let scoped = |entry_id: i64, index: usize, scope: &str| {
            let mut m = maintenance(
                window(at(11, 13, 15, 0), at(11, 13, 16, 0)),
                Disposition::Scheduled,
            );
            m.index = index;
            m.scope = Scope::Named(scope.into());
            item(entry_id, "d", AgendaDetail::from(m))
        };
        let agenda = Agenda::new(vec![
            // 予定の時間内。
            item(
                1,
                "a",
                AgendaDetail::from(maintenance(
                    window(at(11, 13, 11, 0), at(11, 13, 14, 0)),
                    Disposition::Scheduled,
                )),
            ),
            // 今日これから。
            item(
                2,
                "a",
                AgendaDetail::from(maintenance(
                    window(at(11, 13, 18, 0), at(11, 13, 19, 0)),
                    Disposition::Scheduled,
                )),
            ),
            // 日付だけ。
            item(
                3,
                "b",
                AgendaDetail::from(maintenance(
                    window(date(11, 13), Slot::Unspecified),
                    Disposition::Scheduled,
                )),
            ),
            // 完了の報告がある (数えない)。
            item(
                4,
                "b",
                AgendaDetail::from(maintenance(
                    window(at(11, 13, 4, 0), at(11, 13, 6, 0)),
                    Disposition::Completed,
                )),
            ),
            // 明日。
            item(
                5,
                "c",
                AgendaDetail::from(maintenance(
                    window(at(11, 14, 14, 0), at(11, 14, 17, 0)),
                    Disposition::Scheduled,
                )),
            ),
            // 今日終わるガチャ・今日始まったイベント (始まったあとも今日の節目に数える)。
            item(
                6,
                "c",
                period(PeriodKind::Gacha, at(11, 1, 12, 0), at(11, 13, 23, 59)),
            ),
            item(
                7,
                "c",
                period(PeriodKind::Event, at(11, 13, 0, 0), at(11, 20, 23, 59)),
            ),
            // 販売は数えない。終わってしまった終わりの節目も数えない。
            item(
                8,
                "c",
                period(PeriodKind::Sale, at(11, 13, 12, 0), at(11, 13, 20, 0)),
            ),
            item(
                9,
                "c",
                period(PeriodKind::Campaign, at(11, 12, 0, 0), at(11, 13, 11, 59)),
            ),
            // 対象だけが違うメンテナンスは 1 件にまとめる。
            scoped(10, 0, "Android版"),
            scoped(10, 1, "iOS版"),
        ]);
        let summary = |day: Day| {
            agenda
                .day_entries(day, now, &days, &tokyo())
                .into_iter()
                .map(|entry| (entry.entry_id, entry.group, entry.title, entry.badge.text))
                .collect::<Vec<_>>()
        };
        let entry = |id: i64, group: DayGroup, title: &str, badge: &str| {
            (id, group, title.to_string(), badge.to_string())
        };
        assert_eq!(
            summary(Day::Today),
            vec![
                entry(
                    1,
                    DayGroup::Now,
                    "メンテナンス",
                    "メンテ中 (14:00 まで予定)"
                ),
                entry(7, DayGroup::Timed, "イベント", "イベント 今日 00:00 から"),
                entry(
                    10,
                    DayGroup::Timed,
                    "メンテナンス (Android版・iOS版)",
                    "Android版・iOS版 メンテ 11/13(金) 15:00–16:00"
                ),
                entry(
                    2,
                    DayGroup::Timed,
                    "メンテナンス",
                    "メンテ 11/13(金) 18:00–19:00"
                ),
                entry(6, DayGroup::Timed, "ガチャ", "ガチャ 今日 23:59 まで"),
                entry(3, DayGroup::DateOnly, "メンテナンス", "メンテ 11/13(金)"),
                entry(4, DayGroup::Finished, "メンテナンス", "メンテ終了"),
            ]
        );
        assert_eq!(agenda.today_count(now, &days, &tokyo()), 6);
        assert_eq!(
            summary(Day::Tomorrow),
            vec![entry(
                5,
                DayGroup::Timed,
                "メンテナンス",
                "メンテ 11/14(土) 14:00–17:00"
            )]
        );
        // 終了予定を過ぎたメンテナンスは、今日の「今確かめたい」に出す (完了とはしない)。
        let overdue = Agenda::new(vec![item(
            1,
            "a",
            AgendaDetail::from(maintenance(
                window(at(11, 13, 9, 0), at(11, 13, 11, 30)),
                Disposition::Scheduled,
            )),
        )]);
        let entries = overdue.day_entries(Day::Today, now, &days, &tokyo());
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].group, DayGroup::Now);
        assert_eq!(entries[0].badge.tone, Tone::Attention);
        assert!(entries[0].counts());
    }

    #[test]
    fn day_entries_show_one_maintenance_across_its_articles() {
        let now = jst(11, 13, 16, 0);
        let days = Days::at(now, &tokyo());
        // 予告 (記事 1) と再告知 (記事 2) は、完了の報告 (記事 3) を取り込んで完了になっている。
        let resolved = |entry_id: i64| {
            let mut m = maintenance(
                window(at(11, 13, 13, 0), at(11, 13, 15, 0)),
                Disposition::Completed,
            );
            m.resolved_by = Some(3);
            item(entry_id, "a", AgendaDetail::from(m))
        };
        // 完了の報告そのもの (開始は書かれていない)。
        let report = maintenance(
            window(Slot::Unspecified, Slot::Unspecified),
            Disposition::Completed,
        );
        let report = ResolvedMaintenance {
            actual: window(Slot::Unspecified, at(11, 13, 14, 30)),
            ..report
        };
        // 別のゲームの、同じ時間帯のメンテナンスはまとめない (完了の報告がないので終了予定を過ぎている)。
        let other = maintenance(
            window(at(11, 13, 13, 0), at(11, 13, 15, 0)),
            Disposition::Scheduled,
        );
        let agenda = Agenda::new(vec![
            resolved(2),
            resolved(1),
            item(3, "a", AgendaDetail::from(report)),
            item(4, "b", AgendaDetail::from(other)),
        ]);
        let entries = agenda.day_entries(Day::Today, now, &days, &tokyo());
        let summary: Vec<(i64, &str, DayGroup)> = entries
            .iter()
            .map(|entry| (entry.entry_id, entry.source_id.as_str(), entry.group))
            .collect();
        assert_eq!(
            summary,
            vec![(4, "b", DayGroup::Now), (1, "a", DayGroup::Finished)]
        );
        assert_eq!(agenda.today_count(now, &days, &tokyo()), 1);
        // 対象の名前のある報告は、同じ名前の予告にだけ取り込まれる。
        let mut named_plan = maintenance(
            window(at(11, 13, 13, 0), at(11, 13, 15, 0)),
            Disposition::Completed,
        );
        named_plan.scope = Scope::Named("PC版".into());
        named_plan.resolved_by = Some(6);
        let mut named_report = maintenance(
            window(at(11, 13, 13, 0), at(11, 13, 15, 0)),
            Disposition::Completed,
        );
        named_report.scope = Scope::Named("iOS版".into());
        let agenda = Agenda::new(vec![
            item(5, "a", AgendaDetail::from(named_plan)),
            item(6, "a", AgendaDetail::from(named_report)),
        ]);
        let ids: Vec<i64> = agenda
            .day_entries(Day::Today, now, &days, &tokyo())
            .iter()
            .map(|entry| entry.entry_id)
            .collect();
        assert_eq!(ids, vec![5, 6]);
    }
}
