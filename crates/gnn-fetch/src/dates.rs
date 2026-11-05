//! 投稿日時の解釈。
//!
//! 本文に書かれたメンテナンスやイベントの日時は `gnn-schedule` が読む (未来の日付を前年に寄せる・
//! 0 時を日付だけとみなす、といった投稿日時向けの規則は予定には使わない)。

use std::sync::LazyLock;

use chrono::{
    DateTime, Datelike as _, Duration, NaiveDate, NaiveDateTime, NaiveTime, TimeZone as _, Utc,
};
use chrono_tz::Tz;
use gnn_core::Published;
use regex::Regex;
use unicode_normalization::UnicodeNormalization as _;

/// 日時を解釈するときの前提。
#[derive(Debug, Clone, Copy)]
pub struct DateContext {
    /// タイムゾーンのない日時をどの地域時刻として読むか。
    pub tz: Tz,
    /// 年のない日付の年を決める基準。
    pub now: DateTime<Utc>,
}

/// JSON の数値を UNIX 時刻として読む。13 桁以上はミリ秒とみなす。
pub fn from_unix_number(value: f64) -> Option<Published> {
    if !value.is_finite() || value <= 0.0 {
        return None;
    }
    let seconds = if value >= 1e12 { value / 1000.0 } else { value };
    DateTime::from_timestamp(seconds as i64, 0).map(Published::datetime)
}

/// 文字列の日時を読む。
///
/// タイムゾーン付きならその時刻、なければ `ctx.tz` の地域時刻とみなす。
/// 時刻がないもの、およびタイムゾーンなしで 0 時 0 分 0 秒のものは日付だけの投稿とする
/// (日付しか持たない配信元が 0 時を埋めていることが多いため)。
pub fn parse(text: &str, ctx: DateContext) -> Option<Published> {
    let text: String = text.nfkc().collect();
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    if text.bytes().all(|b| b.is_ascii_digit()) && text.len() >= 9 {
        return text.parse::<f64>().ok().and_then(from_unix_number);
    }
    if let Ok(at) = DateTime::parse_from_rfc3339(text) {
        return Some(Published::datetime(at.with_timezone(&Utc)));
    }
    if let Ok(at) = DateTime::parse_from_rfc2822(text) {
        return Some(Published::datetime(at.with_timezone(&Utc)));
    }
    for format in OFFSET_FORMATS {
        if let Ok(at) = DateTime::parse_from_str(text, format) {
            return Some(Published::datetime(at.with_timezone(&Utc)));
        }
    }
    for format in NAIVE_DATETIME_FORMATS {
        if let Ok(naive) = NaiveDateTime::parse_from_str(text, format) {
            return Some(local(naive, ctx.tz));
        }
    }
    for format in DATE_FORMATS {
        if let Ok(date) = NaiveDate::parse_from_str(text, format) {
            return Some(Published::date(date, ctx.tz));
        }
    }
    search_japanese(text, ctx)
}

const OFFSET_FORMATS: &[&str] = &[
    "%Y-%m-%d %H:%M:%S%z",
    "%Y-%m-%d %H:%M:%S %z",
    "%Y/%m/%d %H:%M:%S %z",
];

const NAIVE_DATETIME_FORMATS: &[&str] = &[
    "%Y-%m-%dT%H:%M:%S%.f",
    "%Y-%m-%dT%H:%M:%S",
    "%Y-%m-%dT%H:%M",
    "%Y-%m-%d %H:%M:%S%.f",
    "%Y-%m-%d %H:%M:%S",
    "%Y-%m-%d %H:%M",
    "%Y/%m/%d %H:%M:%S",
    "%Y/%m/%d %H:%M",
    "%Y.%m.%d %H:%M:%S",
    "%Y.%m.%d %H:%M",
];

const DATE_FORMATS: &[&str] = &[
    "%Y-%m-%d",
    "%Y%m%d",
    "%Y/%m/%d",
    "%Y.%m.%d",
    "%m/%d/%Y",
    "%B %d, %Y",
    "%b %d, %Y",
    "%d %b %Y",
];

fn local(naive: NaiveDateTime, tz: Tz) -> Published {
    if naive.time() == NaiveTime::MIN {
        return Published::date(naive.date(), tz);
    }
    let at = tz
        .from_local_datetime(&naive)
        .earliest()
        .unwrap_or_else(|| tz.from_utc_datetime(&naive))
        .with_timezone(&Utc);
    Published::datetime(at)
}

static FULL_DATE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(
        r"(\d{4})\s*[年/.\-]\s*(\d{1,2})\s*[月/.\-]\s*(\d{1,2})\s*日?(?:\s*[(（][^)）]{1,4}[)）])?(?:\s*(\d{1,2})\s*[:時]\s*(\d{2}))?",
    )
    .expect("regex")
});

static MONTH_DAY: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(\d{1,2})\s*[月/]\s*(\d{1,2})\s*日?(?:\s*[(（][^)）]{1,4}[)）])?(?:\s*(\d{1,2})\s*[:時]\s*(\d{2}))?")
        .expect("regex")
});

/// 「2026年11月6日(木) 12:00」「11月6日」のような書き方を文中から探す。
fn search_japanese(text: &str, ctx: DateContext) -> Option<Published> {
    if let Some(caps) = FULL_DATE.captures(text) {
        let date = NaiveDate::from_ymd_opt(
            caps[1].parse().ok()?,
            caps[2].parse().ok()?,
            caps[3].parse().ok()?,
        )?;
        return Some(with_time(date, caps.get(4).zip(caps.get(5)), ctx.tz));
    }
    if let Some(caps) = MONTH_DAY.captures(text) {
        let month: u32 = caps[1].parse().ok()?;
        let day: u32 = caps[2].parse().ok()?;
        let date = infer_year(month, day, ctx)?;
        return Some(with_time(date, caps.get(3).zip(caps.get(4)), ctx.tz));
    }
    None
}

fn with_time(
    date: NaiveDate,
    time: Option<(regex::Match<'_>, regex::Match<'_>)>,
    tz: Tz,
) -> Published {
    let time = time.and_then(|(hour, minute)| {
        NaiveTime::from_hms_opt(
            hour.as_str().parse().ok()?,
            minute.as_str().parse().ok()?,
            0,
        )
    });
    match time {
        Some(time) => {
            let naive = date.and_time(time);
            let at = tz
                .from_local_datetime(&naive)
                .earliest()
                .unwrap_or_else(|| tz.from_utc_datetime(&naive))
                .with_timezone(&Utc);
            Published::datetime(at)
        }
        None => Published::date(date, tz),
    }
}

/// 年のない日付の年を決める。今日より 31 日以上先になるなら前年とみなす。
fn infer_year(month: u32, day: u32, ctx: DateContext) -> Option<NaiveDate> {
    let today = ctx.now.with_timezone(&ctx.tz).date_naive();
    let date = NaiveDate::from_ymd_opt(today.year(), month, day)?;
    if date > today + Duration::days(31) {
        NaiveDate::from_ymd_opt(today.year() - 1, month, day)
    } else {
        Some(date)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gnn_core::TimePrecision;
    use pretty_assertions::assert_eq;

    fn ctx() -> DateContext {
        DateContext {
            tz: chrono_tz::Asia::Tokyo,
            now: Utc.with_ymd_and_hms(2026, 11, 6, 3, 0, 0).unwrap(),
        }
    }

    fn utc(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap()
    }

    #[test]
    fn keeps_explicit_offsets() {
        // 元の Python 実装はタイムゾーンを捨てていたため 9 時間ずれていた。
        let published = parse("2026-11-04T05:18:00+00:00", ctx()).unwrap();
        assert_eq!(published, Published::datetime(utc(2026, 11, 4, 5, 18)));
        let published = parse("2026-09-03T09:08:00.000Z", ctx()).unwrap();
        assert_eq!(published.at, utc(2026, 9, 3, 9, 8));
        let published = parse("Fri, 06 Nov 2026 10:00:00 +0900", ctx()).unwrap();
        assert_eq!(published.at, utc(2026, 11, 6, 1, 0));
    }

    #[test]
    fn reads_naive_values_in_source_timezone() {
        assert_eq!(
            parse("2026-11-04 14:00:00", ctx()).unwrap().at,
            utc(2026, 11, 4, 5, 0)
        );
        assert_eq!(
            parse("2026/11/04 14:00", ctx()).unwrap().at,
            utc(2026, 11, 4, 5, 0)
        );
        assert_eq!(
            parse("２０２６年１１月４日 14:30", ctx()).unwrap().at,
            utc(2026, 11, 4, 5, 30)
        );
        assert_eq!(
            parse("2026.11.04(水) 9時05", ctx()).unwrap().at,
            utc(2026, 11, 4, 0, 5)
        );
    }

    #[test]
    fn date_only_values_have_date_precision() {
        for text in [
            "2026/10/27",
            "2026-11-06T00:00:00",
            "2026-11-05 00:00",
            "2026年10月27日",
        ] {
            let published = parse(text, ctx()).unwrap();
            assert_eq!(published.precision, TimePrecision::Date, "{text}");
        }
        assert_eq!(
            parse("2026/10/27", ctx()).unwrap().at,
            utc(2026, 10, 26, 15, 0)
        );
    }

    #[test]
    fn infers_year_for_month_day() {
        assert_eq!(parse("11月5日", ctx()).unwrap().at, utc(2026, 11, 4, 15, 0));
        // 今日より 1 か月以上先の日付は前年の投稿とみなす。
        assert_eq!(
            parse("12月25日", ctx()).unwrap().at,
            utc(2025, 12, 24, 15, 0)
        );
        assert_eq!(
            parse("お知らせ 11/02(月) 12:00 更新", ctx()).unwrap().at,
            utc(2026, 11, 2, 3, 0)
        );
    }

    #[test]
    fn reads_unix_numbers() {
        assert_eq!(
            parse("1792983616", ctx()).unwrap().at.timestamp(),
            1_792_983_616
        );
        assert_eq!(
            from_unix_number(1_792_983_616_000.0)
                .unwrap()
                .at
                .timestamp(),
            1_792_983_616
        );
        assert_eq!(from_unix_number(0.0), None);
        assert_eq!(parse("not a date", ctx()), None);
        assert_eq!(parse("", ctx()), None);
    }
}
