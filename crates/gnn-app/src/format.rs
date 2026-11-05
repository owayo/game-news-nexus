//! 日時などの表示用の書式。

use chrono::{DateTime, Datelike as _, Local, Utc};
use gnn_core::{Published, TimePrecision};

/// 一覧に出す日時。日付だけの記事は日付だけ出す。
pub fn list_time(published: Option<Published>, sort_at: DateTime<Utc>) -> String {
    match published {
        Some(Published {
            at,
            precision: TimePrecision::Date,
        }) => at.with_timezone(&Local).format("%m/%d").to_string(),
        Some(Published { at, .. }) => at.with_timezone(&Local).format("%m/%d %H:%M").to_string(),
        None => sort_at
            .with_timezone(&Local)
            .format("%m/%d %H:%M")
            .to_string(),
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

/// 未来の時刻を「あと 12 分」のように出す。
pub fn until(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let remaining = at - now;
    if remaining.num_seconds() <= 60 {
        return "まもなく".to_string();
    }
    if remaining.num_minutes() < 60 {
        return format!("{} 分後", remaining.num_minutes());
    }
    at.with_timezone(&Local).format("%H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn formats_relative_times() {
        let now = Utc::now();
        assert_eq!(relative_time(now - Duration::seconds(5), now), "たった今");
        assert_eq!(relative_time(now - Duration::minutes(12), now), "12 分前");
        assert_eq!(until(now + Duration::minutes(30), now), "30 分後");
        assert_eq!(until(now + Duration::seconds(30), now), "まもなく");
    }
}
