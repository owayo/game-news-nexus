//! 統合テストの共通の部品。作文の記事を読む。

#![allow(dead_code)]

use chrono::{DateTime, NaiveDate, NaiveDateTime, TimeZone, Utc};
use chrono_tz::Tz;
use gnn_core::Published;
use gnn_core::schedule::{
    Certainty, Change, Disposition, Maintenance, Period, PeriodKind, Schedule, Scope, Slot,
    TimeValue, When,
};
use gnn_schedule::{Article, Context, Extractor};

pub const TOKYO: Tz = chrono_tz::Asia::Tokyo;

/// 日本時間の日時。`"2026-11-12 14:00"`。
pub fn jst(text: &str) -> DateTime<Utc> {
    let naive = NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").expect("日時");
    TOKYO
        .from_local_datetime(&naive)
        .single()
        .unwrap()
        .with_timezone(&Utc)
}

pub fn ymd(text: &str) -> NaiveDate {
    NaiveDate::parse_from_str(text, "%Y-%m-%d").expect("日付")
}

/// 投稿日時 (日本時間) とソースの地域時刻の文脈。
pub fn context(published: &str) -> Context {
    Context {
        published: Some(Published::datetime(jst(published))),
        first_seen_at: jst(published),
        timezone: TOKYO,
        server_timezone: None,
    }
}

/// 既定の文脈 (投稿 2026-11-05 17:00、Asia/Tokyo)。
pub fn default_context() -> Context {
    context("2026-11-05 17:00")
}

/// 本文を `<br>` でつないだ HTML として読む (多くのソースの形)。
pub fn read(title: &str, body: &str) -> Schedule {
    read_with(title, body, default_context())
}

pub fn read_with(title: &str, body: &str, context: Context) -> Schedule {
    let html = body
        .lines()
        .map(gnn_core::text::escape_html)
        .collect::<Vec<_>>()
        .join("<br>");
    read_html_with(title, &html, context)
}

pub fn read_html_with(title: &str, html: &str, context: Context) -> Schedule {
    let mut extractor = Extractor::new();
    let text = gnn_core::text::html_to_text(html);
    extractor.extract(&Article {
        title,
        body_html: html,
        body_text: &text,
        context,
    })
}

/// 形態素解析を使わずに読む (比べるため)。
pub fn read_baseline(title: &str, body: &str) -> Schedule {
    let html = body
        .lines()
        .map(gnn_core::text::escape_html)
        .collect::<Vec<_>>()
        .join("<br>");
    let mut extractor = Extractor::new();
    let text = gnn_core::text::html_to_text(&html);
    extractor.extract_without_morphology(&Article {
        title,
        body_html: &html,
        body_text: &text,
        context: default_context(),
    })
}

pub fn instant(slot: &Slot<TimeValue>) -> Option<DateTime<Utc>> {
    match slot.value()?.when {
        When::Instant { at, .. } => Some(at),
        When::Date { .. } => None,
    }
}

pub fn date(slot: &Slot<TimeValue>) -> Option<NaiveDate> {
    match slot.value()?.when {
        When::Date { date, .. } => Some(date),
        When::Instant { .. } => None,
    }
}

pub fn certainty(slot: &Slot<TimeValue>) -> Option<Certainty> {
    slot.value().map(|value| value.certainty)
}

/// メンテナンスがちょうど 1 件。
pub fn only(schedule: &Schedule) -> &Maintenance {
    assert_eq!(
        schedule.maintenance.len(),
        1,
        "メンテナンスが 1 件ではない: {}",
        describe(schedule)
    );
    &schedule.maintenance[0]
}

pub fn has_extended(maintenance: &Maintenance) -> bool {
    maintenance
        .changes
        .iter()
        .any(|change| matches!(change, Change::Extended { .. }))
}

pub fn rescheduled_from(maintenance: &Maintenance) -> Option<&gnn_core::schedule::Window> {
    maintenance.changes.iter().find_map(|change| match change {
        Change::Rescheduled { from, .. } => Some(from),
        _ => None,
    })
}

pub fn periods(schedule: &Schedule, kind: PeriodKind) -> Vec<&Period> {
    schedule
        .periods
        .iter()
        .filter(|period| period.kind == kind)
        .collect()
}

pub fn has_note(schedule: &Schedule, code: &str) -> bool {
    schedule.notes.iter().any(|note| note.code == code)
}

pub fn named(name: &str) -> Scope {
    Scope::Named(name.to_string())
}

/// 人が読む形 (失敗の調査用)。
pub fn describe(schedule: &Schedule) -> String {
    let mut out = String::new();
    for maintenance in &schedule.maintenance {
        out.push_str(&format!(
            "\n  メンテ {:?} {:?} 予定[{} - {}] 実際[{} - {}]{} 変更{}",
            maintenance.scope,
            maintenance.disposition,
            slot(&maintenance.planned.start),
            slot(&maintenance.planned.end),
            slot(&maintenance.actual.start),
            slot(&maintenance.actual.end),
            maintenance
                .ongoing
                .as_ref()
                .map(|ongoing| format!(" 実施中({})", slot(&Slot::Value(ongoing.as_of.clone()))))
                .unwrap_or_default(),
            changes(&maintenance.changes)
        ));
    }
    for period in &schedule.periods {
        out.push_str(&format!(
            "\n  期間 {:?} {:?} [{} - {}] 変更{}",
            period.kind,
            period.label,
            slot(&period.window.start),
            slot(&period.window.end),
            changes(&period.changes)
        ));
    }
    for note in &schedule.notes {
        out.push_str(&format!("\n  記録 {} {}", note.code, note.message));
    }
    out
}

/// 延長・日程変更を人が読む形にする。
pub fn changes(changes: &[Change]) -> String {
    changes
        .iter()
        .map(|change| match change {
            Change::Extended { from, end, .. } => format!(" 延長({} → {})", slot(from), slot(end)),
            Change::Rescheduled { from, to, .. } => format!(
                " 変更({} - {} → {} - {})",
                slot(&from.start),
                slot(&from.end),
                slot(&to.start),
                slot(&to.end)
            ),
        })
        .collect()
}

pub fn slot(slot: &Slot<TimeValue>) -> String {
    match slot {
        Slot::Unspecified => "-".to_string(),
        Slot::Undecided => "未定".to_string(),
        Slot::Value(value) => {
            let certainty = match value.certainty {
                Certainty::Explicit => "",
                Certainty::Approximate => "≈",
                Certainty::Inferred => "?",
            };
            match &value.when {
                When::Date { date, .. } => format!("{date}{certainty}"),
                When::Instant { at, .. } => format!(
                    "{}{certainty}",
                    at.with_timezone(&TOKYO).format("%Y-%m-%d %H:%M")
                ),
            }
        }
    }
}

pub fn disposition(schedule: &Schedule) -> Option<Disposition> {
    schedule
        .maintenance
        .first()
        .map(|maintenance| maintenance.disposition)
}
