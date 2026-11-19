//! 日時などの表示用の書式。

use chrono::{DateTime, Datelike as _, Local, NaiveDate, TimeZone, Utc};
use gnn_core::{Published, TimePrecision};

/// 一覧に出す日時。今日と昨日は「今日 05:00」「昨日 15:40」、それより前は日付だけ (今年なら
/// 「11/06」、前の年なら「2025/11/06」)。日付だけの記事に時刻を補わず、投稿日時が分からない記事は
/// 取得した日を「取得 11/13」と出す。
pub fn list_time(
    published: Option<Published>,
    first_seen_at: DateTime<Utc>,
    now: DateTime<Utc>,
) -> String {
    list_time_in(published, first_seen_at, now, &Local)
}

fn list_time_in<Z: TimeZone>(
    published: Option<Published>,
    first_seen_at: DateTime<Utc>,
    now: DateTime<Utc>,
    tz: &Z,
) -> String
where
    Z::Offset: std::fmt::Display,
{
    let today = now.with_timezone(tz).date_naive();
    let day = |date: NaiveDate| -> Option<&'static str> {
        if date == today {
            Some("今日")
        } else if Some(date) == today.pred_opt() {
            Some("昨日")
        } else {
            None
        }
    };
    let date_text = |date: NaiveDate| -> String {
        if date.year() == today.year() {
            format!("{:02}/{:02}", date.month(), date.day())
        } else {
            format!("{}/{:02}/{:02}", date.year(), date.month(), date.day())
        }
    };
    match published {
        Some(Published { at, precision }) => {
            let local = at.with_timezone(tz);
            let date = local.date_naive();
            match (day(date), precision) {
                (Some(day), TimePrecision::Date) => day.to_string(),
                (Some(day), _) => format!("{day} {}", local.format("%H:%M")),
                (None, _) => date_text(date),
            }
        }
        None => {
            let date = first_seen_at.with_timezone(tz).date_naive();
            format!(
                "取得 {}",
                day(date).map_or_else(|| date_text(date), str::to_string)
            )
        }
    }
}

/// 本文の見出しに出す日時。
pub fn full_time(published: Option<Published>, first_seen_at: DateTime<Utc>) -> String {
    match published {
        Some(Published {
            at,
            precision: TimePrecision::Date,
        }) => at.with_timezone(&Local).format("%Y年%m月%d日").to_string(),
        Some(Published { at, .. }) => at
            .with_timezone(&Local)
            .format("%Y年%m月%d日 %H:%M")
            .to_string(),
        None => format!(
            "{} に取得",
            first_seen_at
                .with_timezone(&Local)
                .format("%Y年%m月%d日 %H:%M")
        ),
    }
}

/// 「3 分前」「昨日 12:34」のような相対表記。
pub fn relative_time(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let elapsed = now - at;
    if elapsed.num_seconds() < 60 {
        return "たった今".to_string();
    }
    if elapsed.num_minutes() < 60 {
        return format!("{} 分前", elapsed.num_minutes());
    }
    let local = at.with_timezone(&Local);
    let today = now.with_timezone(&Local).date_naive();
    if local.date_naive() == today {
        return local.format("%H:%M").to_string();
    }
    if Some(local.date_naive()) == today.pred_opt() {
        return local.format("昨日 %H:%M").to_string();
    }
    if local.year() == today.year() {
        return local.format("%m/%d %H:%M").to_string();
    }
    local.format("%Y/%m/%d").to_string()
}

/// 未来の時刻を「あと 12 分」のように出す。今日でなければ「明日 08:13」「11/15 08:13」。
pub fn until(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let remaining = at - now;
    if remaining.num_seconds() <= 60 {
        return "まもなく".to_string();
    }
    if remaining.num_minutes() < 60 {
        return format!("{} 分後", remaining.num_minutes());
    }
    let local = at.with_timezone(&Local);
    let today = now.with_timezone(&Local).date_naive();
    if local.date_naive() == today {
        local.format("%H:%M").to_string()
    } else if Some(local.date_naive()) == today.succ_opt() {
        local.format("明日 %H:%M").to_string()
    } else {
        local.format("%m/%d %H:%M").to_string()
    }
}

/// 取得の間隔 (「30 分ごと」「24 時間ごと」「90 分ごと」)。
pub fn interval(minutes: i64) -> String {
    if minutes >= 60 && minutes % 60 == 0 {
        format!("{} 時間ごと", minutes / 60)
    } else {
        format!("{minutes} 分ごと")
    }
}

/// 「11月13日（金）」。
pub fn day_heading(date: NaiveDate) -> String {
    const WEEKDAYS: [&str; 7] = ["月", "火", "水", "木", "金", "土", "日"];
    format!(
        "{}月{}日（{}）",
        date.month(),
        date.day(),
        WEEKDAYS[date.weekday().num_days_from_monday() as usize]
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{Duration, FixedOffset};

    #[test]
    fn formats_relative_times() {
        let now = Utc::now();
        assert_eq!(relative_time(now - Duration::seconds(5), now), "たった今");
        assert_eq!(relative_time(now - Duration::minutes(12), now), "12 分前");
        assert_eq!(until(now + Duration::minutes(30), now), "30 分後");
        assert_eq!(until(now + Duration::seconds(30), now), "まもなく");
    }

    #[test]
    fn list_times_show_the_time_only_for_today_and_yesterday() {
        let jst = FixedOffset::east_opt(9 * 3600).unwrap();
        let at = |y: i32, mo: u32, d: u32, h: u32, mi: u32| {
            jst.with_ymd_and_hms(y, mo, d, h, mi, 0)
                .unwrap()
                .with_timezone(&Utc)
        };
        let now = at(2026, 11, 13, 12, 0);
        let datetime = |at| Some(Published::datetime(at));
        let date = |at: DateTime<Utc>| {
            Some(Published {
                at,
                precision: TimePrecision::Date,
            })
        };
        let time = |published| list_time_in(published, now, now, &jst);
        assert_eq!(time(datetime(at(2026, 11, 13, 5, 0))), "今日 05:00");
        assert_eq!(time(datetime(at(2026, 11, 12, 15, 40))), "昨日 15:40");
        assert_eq!(time(datetime(at(2026, 11, 6, 9, 0))), "11/06");
        assert_eq!(time(datetime(at(2025, 11, 6, 9, 0))), "2025/11/06");
        assert_eq!(time(date(at(2026, 11, 13, 0, 0))), "今日");
        assert_eq!(time(date(at(2026, 11, 6, 0, 0))), "11/06");
        assert_eq!(
            list_time_in(None, at(2026, 11, 10, 8, 0), now, &jst),
            "取得 11/10"
        );
        assert_eq!(
            day_heading(NaiveDate::from_ymd_opt(2026, 11, 13).unwrap()),
            "11月13日（金）"
        );
        assert_eq!(interval(30), "30 分ごと");
        assert_eq!(interval(90), "90 分ごと");
        assert_eq!(interval(1440), "24 時間ごと");
    }
}
