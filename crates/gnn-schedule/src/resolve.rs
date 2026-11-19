//! 日時の確定。年のない日付の年、相対の日付 (本日・翌日・同日)、時刻だけの値の日付、地域時刻を決める。
//!
//! - 年は、明記があればそれ。なければ基準の日 (投稿日・追記の日) に最も近い年。曜日で遠い年へは
//!   移さない (曜日が合わなければ `weekday_mismatch` を残す)。範囲の終わりは始まりの年を使い、
//!   始まりより前になるなら次の年にする (年越し)。
//! - `本日` `明日` は基準の日から、`翌日` `同日` は直前に書かれた日付から。
//! - 時刻だけの値は、直前に書かれた日付 (なければ基準の日) の時刻。`24:59` は翌日の `0:59`。
//!   範囲の終わりが始まりより前の時刻で `翌` もないときは決めない (`reversed_range`)。
//! - 地域時刻は、日時に付いた明記 → 範囲のもう一方の明記 → 文脈の明記 → ソースの既定 の順。
//!   `サーバー時間` はソースの `server_timezone` (なければ決めない)。夏時間の重複・欠落の時刻は決めない。

use chrono::{
    DateTime, Datelike, Duration, FixedOffset, LocalResult, NaiveDate, NaiveDateTime, NaiveTime,
    TimeZone, Utc,
};
use chrono_tz::Tz;
use gnn_core::schedule::{Certainty, Evidence, Note, When, Zone};

use crate::temporal::{DateSpec, Point, RangeEnd, RelativeDay, TimeSpec, ZoneSpec};

/// 地域時刻。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ZoneRef {
    Named(Tz),
    Offset(i32),
}

impl ZoneRef {
    fn zone(self) -> Zone {
        match self {
            ZoneRef::Named(tz) => Zone::Named(tz.name().to_string()),
            ZoneRef::Offset(seconds) => Zone::Offset(seconds),
        }
    }
}

/// 地域の日時 (UTC に直す前)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Local {
    pub date: NaiveDate,
    /// `None` は日付だけ。
    pub time: Option<NaiveTime>,
    pub zone: ZoneRef,
    pub certainty: Certainty,
}

impl Local {
    /// 比べるための値 (日付だけは 0 時)。
    pub fn naive(&self) -> NaiveDateTime {
        self.date.and_time(self.time.unwrap_or(NaiveTime::MIN))
    }

    /// 時刻を足す (所要時間から終わりを求める)。
    pub fn plus_minutes(&self, minutes: u32) -> Option<Local> {
        let time = self.time?;
        let naive = self.date.and_time(time) + Duration::minutes(minutes as i64);
        Some(Local {
            date: naive.date(),
            time: Some(naive.time()),
            zone: self.zone,
            certainty: Certainty::Approximate,
        })
    }
}

/// 日時を決める文脈。
pub(crate) struct Resolver {
    /// ソースの既定の地域時刻。
    pub default_zone: Tz,
    pub server_zone: Option<Tz>,
    /// 投稿日 (なければ初めて見つけた日) の地域の日付。投稿日が分からないときは、初めて見つけた日
    /// (保存済みの値なので、読み直しても変わらない) を使う。
    pub reference: NaiveDate,
    /// 記事の投稿日時。
    pub published: Option<gnn_core::Published>,
    /// 初めて見つけた日時。
    pub first_seen_at: DateTime<Utc>,
    pub notes: Vec<Note>,
}

impl Resolver {
    pub fn new(context: &crate::Context) -> Self {
        let at = context
            .published
            .map_or(context.first_seen_at, |published| published.at);
        Resolver {
            default_zone: context.timezone,
            server_zone: context.server_timezone,
            reference: at.with_timezone(&context.timezone).date_naive(),
            published: context.published,
            first_seen_at: context.first_seen_at,
            notes: Vec::new(),
        }
    }

    /// 記事の日時 (投稿日時、なければ初めて見つけた日時)。日付だけの投稿日は日付だけの値にする。
    pub fn article_moment(&self) -> When {
        let zone = Zone::Named(self.default_zone.name().to_string());
        match self.published {
            Some(gnn_core::Published {
                at,
                precision: gnn_core::TimePrecision::Date,
            }) => When::Date {
                date: at.with_timezone(&self.default_zone).date_naive(),
                zone,
            },
            Some(published) => When::Instant {
                at: published.at,
                zone,
            },
            None => When::Instant {
                at: self.first_seen_at,
                zone,
            },
        }
    }

    fn note(&mut self, code: &str, message: String, evidence: &[Evidence]) {
        if self
            .notes
            .iter()
            .any(|note| note.code == code && note.evidence == evidence)
        {
            return;
        }
        self.notes.push(Note {
            code: code.to_string(),
            message,
            evidence: evidence.to_vec(),
        });
    }

    /// 地域時刻を決める。`spec` は日時に付いた明記、`fallback` は範囲のもう一方・文脈の明記。
    pub fn zone(
        &mut self,
        spec: Option<ZoneSpec>,
        fallback: Option<ZoneSpec>,
        evidence: &[Evidence],
    ) -> Option<ZoneRef> {
        match spec.or(fallback) {
            None => Some(ZoneRef::Named(self.default_zone)),
            Some(ZoneSpec::Japan) => Some(ZoneRef::Named(chrono_tz::Asia::Tokyo)),
            Some(ZoneSpec::Offset(seconds)) => Some(ZoneRef::Offset(seconds)),
            Some(ZoneSpec::Server) => match self.server_zone {
                Some(tz) => Some(ZoneRef::Named(tz)),
                None => {
                    self.note(
                        "server_time_unknown",
                        format!(
                            "{}は「サーバー時間」で書かれていますが、サーバーの地域時刻が分からないため使っていません",
                            quote(evidence)
                        ),
                        evidence,
                    );
                    None
                }
            },
        }
    }

    /// 日付を決める。`anchor` は直前に書かれた日付、`reference` は区画の基準の日。
    /// `not_before` は範囲の始まり (終わりの年を決めるのに使う)。
    pub fn date(
        &mut self,
        spec: &DateSpec,
        anchor: Option<NaiveDate>,
        reference: NaiveDate,
        not_before: Option<NaiveDate>,
        evidence: &[Evidence],
    ) -> Option<(NaiveDate, Certainty)> {
        match *spec {
            DateSpec::Absolute {
                year,
                month,
                day,
                weekday,
            } => {
                let (date, certainty) = match year {
                    Some(year) => (
                        NaiveDate::from_ymd_opt(year, month, day)?,
                        Certainty::Explicit,
                    ),
                    None => (
                        infer_year(month, day, reference, not_before)?,
                        Certainty::Inferred,
                    ),
                };
                if let Some(weekday) = weekday
                    && date.weekday() != weekday
                {
                    self.note(
                        "weekday_mismatch",
                        format!(
                            "{}月{}日の曜日の表記 ({}) が暦 ({}) と合いません。書かれた日付を使っています",
                            month,
                            day,
                            weekday_short(weekday),
                            weekday_ja(date.weekday())
                        ),
                        evidence,
                    );
                }
                Some((date, certainty))
            }
            DateSpec::Relative(relative) => {
                let base = match relative {
                    RelativeDay::NextDay | RelativeDay::SameDay | RelativeDay::PreviousDay => {
                        anchor.unwrap_or(reference)
                    }
                    _ => reference,
                };
                let date = match relative {
                    RelativeDay::Today | RelativeDay::SameDay => base,
                    RelativeDay::Tomorrow | RelativeDay::NextDay => base.succ_opt()?,
                    RelativeDay::DayAfterTomorrow => base.succ_opt()?.succ_opt()?,
                    RelativeDay::Yesterday | RelativeDay::PreviousDay => base.pred_opt()?,
                };
                Some((date, Certainty::Inferred))
            }
        }
    }

    /// 点を地域の日時にする。
    pub fn point(
        &mut self,
        point: &Point,
        anchor: Option<NaiveDate>,
        reference: NaiveDate,
        zone_fallback: Option<ZoneSpec>,
        evidence: &[Evidence],
    ) -> Option<Local> {
        let zone = self.zone(point.zone, zone_fallback, evidence)?;
        let (date, mut certainty) = match &point.date {
            Some(spec) => self.date(spec, anchor, reference, None, evidence)?,
            None => (anchor.unwrap_or(reference), Certainty::Inferred),
        };
        if point.date.is_none() && point.time.is_none() {
            return None;
        }
        let (date, time) = match point.time {
            Some(time) => {
                let (date, time) = clock(date, time)?;
                (date, Some(time))
            }
            None => (date, None),
        };
        if point.approx {
            certainty = Certainty::Approximate;
        }
        Some(Local {
            date,
            time,
            zone,
            certainty,
        })
    }

    /// 範囲を地域の日時の組にする。終わりは `None` (書かれていない・決められない) か値。
    pub fn range(
        &mut self,
        start: Option<&Point>,
        end: &RangeEnd,
        anchor: Option<NaiveDate>,
        reference: NaiveDate,
        zone_fallback: Option<ZoneSpec>,
        evidence: &[Evidence],
    ) -> (Option<Local>, Option<Local>) {
        let end_point = match end {
            RangeEnd::Point(point) => Some(point),
            _ => None,
        };
        // 片方にだけ明記された地域時刻は、もう片方にも使う。
        let start_zone = start
            .and_then(|point| point.zone)
            .or(end_point.and_then(|point| point.zone))
            .or(zone_fallback);
        let end_zone = end_point
            .and_then(|point| point.zone)
            .or(start.and_then(|point| point.zone))
            .or(zone_fallback);
        let start_local = start.and_then(|point| {
            if point.date.is_none() && point.time.is_none() {
                return None;
            }
            self.point(point, anchor, reference, start_zone, evidence)
        });
        let Some(end_point) = end_point else {
            return (start_local, None);
        };
        let end_local = match (&start_local, &end_point.date) {
            (Some(start), None) => {
                // 時刻だけの終わりは、始まりの日の時刻 (`翌` は次の日)。
                let zone = self.zone(end_point.zone, end_zone, evidence);
                match (zone, end_point.time) {
                    (Some(zone), Some(time)) => {
                        let base = if time.next_day {
                            start.date.succ_opt()
                        } else {
                            Some(start.date)
                        };
                        base.and_then(|base| clock(base, time)).and_then(|(date, time)| {
                            let local = Local {
                                date,
                                time: Some(time),
                                zone,
                                certainty: Certainty::Inferred,
                            };
                            if start.time.is_some() && local.naive() <= start.naive() {
                                self.note(
                                    "reversed_range",
                                    format!(
                                        "{}は終わりの時刻が始まりより前のため、終わりの時刻を決めていません",
                                        quote(evidence)
                                    ),
                                    evidence,
                                );
                                None
                            } else {
                                Some(local)
                            }
                        })
                    }
                    _ => None,
                }
            }
            (Some(start), Some(spec)) => {
                let zone = self.zone(end_point.zone, end_zone, evidence);
                let date = self.date(
                    spec,
                    Some(start.date),
                    reference,
                    Some(start.date),
                    evidence,
                );
                match (zone, date) {
                    (Some(zone), Some((date, certainty))) => {
                        let time = end_point.time.and_then(|time| clock(date, time));
                        let local = match time {
                            Some((date, time)) => Local {
                                date,
                                time: Some(time),
                                zone,
                                certainty,
                            },
                            None => Local {
                                date,
                                time: None,
                                zone,
                                certainty,
                            },
                        };
                        if local.naive() < start.naive() {
                            self.note(
                                "reversed_range",
                                format!(
                                    "{}は終わりが始まりより前のため、終わりを決めていません",
                                    quote(evidence)
                                ),
                                evidence,
                            );
                            None
                        } else {
                            Some(local)
                        }
                    }
                    _ => None,
                }
            }
            (None, _) => self.point(end_point, anchor, reference, end_zone, evidence),
        };
        (start_local, end_local)
    }

    /// 地域の日時を `When` にする。夏時間の重複・欠落は決めない。
    pub fn when(&mut self, local: &Local, evidence: &[Evidence]) -> Option<When> {
        let Some(time) = local.time else {
            return Some(When::Date {
                date: local.date,
                zone: local.zone.zone(),
            });
        };
        let naive = local.date.and_time(time);
        let at: Option<DateTime<Utc>> = match local.zone {
            ZoneRef::Named(tz) => match tz.from_local_datetime(&naive) {
                LocalResult::Single(at) => Some(at.with_timezone(&Utc)),
                LocalResult::Ambiguous(_, _) => {
                    self.note(
                        "dst_ambiguous",
                        format!(
                            "{}は夏時間の切り替えで 2 度ある時刻のため、日時を決めていません",
                            quote(evidence)
                        ),
                        evidence,
                    );
                    None
                }
                LocalResult::None => {
                    self.note(
                        "dst_gap",
                        format!(
                            "{}は夏時間の切り替えで存在しない時刻のため、日時を決めていません",
                            quote(evidence)
                        ),
                        evidence,
                    );
                    None
                }
            },
            ZoneRef::Offset(seconds) => FixedOffset::east_opt(seconds)
                .and_then(|offset| offset.from_local_datetime(&naive).single())
                .map(|at| at.with_timezone(&Utc)),
        };
        at.map(|at| When::Instant {
            at,
            zone: local.zone.zone(),
        })
    }
}

/// 年のない月日に年を付ける。基準の日に最も近い年 (範囲の終わりは始まり以後で最も近い年)。
fn infer_year(
    month: u32,
    day: u32,
    reference: NaiveDate,
    not_before: Option<NaiveDate>,
) -> Option<NaiveDate> {
    if let Some(start) = not_before {
        return [start.year(), start.year() + 1]
            .into_iter()
            .filter_map(|year| NaiveDate::from_ymd_opt(year, month, day))
            .find(|date| *date >= start);
    }
    [reference.year() - 1, reference.year(), reference.year() + 1]
        .into_iter()
        .filter_map(|year| NaiveDate::from_ymd_opt(year, month, day))
        .min_by_key(|date| (*date - reference).num_days().abs())
}

/// 時刻を日付に付ける。24 時以降は次の日にする。
fn clock(date: NaiveDate, time: TimeSpec) -> Option<(NaiveDate, NaiveTime)> {
    let days = time.hour / 24;
    let hour = time.hour % 24;
    let date = date + Duration::days(days as i64);
    let time = NaiveTime::from_hms_opt(hour, time.minute, time.second)?;
    Some((date, time))
}

/// 根拠の表記を「」で囲む (根拠がなければ「この日時」)。
fn quote(evidence: &[Evidence]) -> String {
    match evidence.first() {
        Some(evidence) => format!("「{}」", evidence.text),
        None => "この日時".to_string(),
    }
}

fn weekday_short(weekday: chrono::Weekday) -> &'static str {
    match weekday {
        chrono::Weekday::Mon => "月",
        chrono::Weekday::Tue => "火",
        chrono::Weekday::Wed => "水",
        chrono::Weekday::Thu => "木",
        chrono::Weekday::Fri => "金",
        chrono::Weekday::Sat => "土",
        chrono::Weekday::Sun => "日",
    }
}

fn weekday_ja(weekday: chrono::Weekday) -> &'static str {
    match weekday {
        chrono::Weekday::Mon => "月曜日",
        chrono::Weekday::Tue => "火曜日",
        chrono::Weekday::Wed => "水曜日",
        chrono::Weekday::Thu => "木曜日",
        chrono::Weekday::Fri => "金曜日",
        chrono::Weekday::Sat => "土曜日",
        chrono::Weekday::Sun => "日曜日",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn infers_years_near_reference() {
        let reference = NaiveDate::from_ymd_opt(2026, 12, 28).unwrap();
        assert_eq!(
            infer_year(1, 5, reference, None),
            NaiveDate::from_ymd_opt(2027, 1, 5)
        );
        let reference = NaiveDate::from_ymd_opt(2026, 11, 5).unwrap();
        assert_eq!(
            infer_year(11, 12, reference, None),
            NaiveDate::from_ymd_opt(2026, 11, 12)
        );
        let start = NaiveDate::from_ymd_opt(2026, 12, 28).unwrap();
        assert_eq!(
            infer_year(1, 5, reference, Some(start)),
            NaiveDate::from_ymd_opt(2027, 1, 5)
        );
        assert_eq!(
            infer_year(2, 29, NaiveDate::from_ymd_opt(2028, 2, 1).unwrap(), None),
            NaiveDate::from_ymd_opt(2028, 2, 29)
        );
    }

    #[test]
    fn rolls_hours_past_midnight() {
        let date = NaiveDate::from_ymd_opt(2026, 11, 20).unwrap();
        let (date, time) = clock(
            date,
            TimeSpec {
                hour: 24,
                minute: 59,
                second: 0,
                next_day: false,
            },
        )
        .unwrap();
        assert_eq!(date, NaiveDate::from_ymd_opt(2026, 11, 21).unwrap());
        assert_eq!(time, NaiveTime::from_hms_opt(0, 59, 0).unwrap());
    }
}
