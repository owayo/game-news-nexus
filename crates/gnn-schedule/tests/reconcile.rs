//! 記事どうしの突き合わせ。

mod common;

use chrono::{DateTime, Utc};
use common::*;
use gnn_core::Published;
use gnn_core::schedule::{Change, Disposition, Schedule};
use gnn_schedule::{SourceArticle, reconcile};

fn article<'a>(id: i64, published: &str, schedule: &'a Schedule) -> SourceArticle<'a> {
    let at: DateTime<Utc> = jst(published);
    SourceArticle {
        entry_id: id,
        published: Some(Published::datetime(at)),
        first_seen_at: at,
        schedule,
    }
}

#[test]
fn completion_in_another_article_resolves_the_notice() {
    let notice = read_with(
        "11/12(木) 14:00～17:00 メンテナンスのお知らせ",
        "下記の日程でメンテナンスを実施いたします。",
        context("2026-11-10 15:00"),
    );
    let reminder = read_with(
        "【再告知】11/12(木) 14:00～17:00 メンテナンスのお知らせ",
        "下記の日程でメンテナンスを実施いたします。",
        context("2026-11-11 15:00"),
    );
    let report = read_with(
        "メンテナンス完了のお知らせ",
        "11月12日(木)14:00より行っておりましたメンテナンスが17:30に完了いたしました。",
        context("2026-11-12 17:30"),
    );
    let resolved = reconcile(&[
        article(1, "2026-11-10 15:00", &notice),
        article(2, "2026-11-11 15:00", &reminder),
        article(3, "2026-11-12 17:30", &report),
    ]);
    assert_eq!(resolved.len(), 3);
    for item in &resolved[..2] {
        assert_eq!(item.disposition, Disposition::Completed);
        assert_eq!(item.resolved_by, Some(3));
        assert_eq!(instant(&item.actual.end), Some(jst("2026-11-12 17:30")));
        assert_eq!(instant(&item.planned.end), Some(jst("2026-11-12 17:00")));
    }
    assert_eq!(resolved[2].resolved_by, None);
}

#[test]
fn extension_in_another_article_adds_the_change() {
    let notice = read_with(
        "11/12(木) 14:00～17:00 メンテナンスのお知らせ",
        "メンテナンスを実施いたします。",
        context("2026-11-10 15:00"),
    );
    let extension = read_with(
        "メンテナンス延長のお知らせ",
        "11月12日(木)14:00より行っておりますメンテナンスを延長いたします。\n▼終了時間\n未定",
        context("2026-11-12 16:50"),
    );
    let resolved = reconcile(&[
        article(1, "2026-11-10 15:00", &notice),
        article(2, "2026-11-12 16:50", &extension),
    ]);
    assert_eq!(resolved[0].disposition, Disposition::Scheduled);
    assert_eq!(resolved[0].resolved_by, Some(2));
    assert!(
        resolved[0]
            .changes
            .iter()
            .any(|change| matches!(change, Change::Extended { .. }))
    );
    assert_eq!(resolved[0].planned.end, gnn_core::schedule::Slot::Undecided);
}

#[test]
fn platform_report_does_not_resolve_the_whole() {
    let notice = read_with(
        "11/12(木) 14:00～17:00 メンテナンスのお知らせ",
        "メンテナンスを実施いたします。",
        context("2026-11-10 15:00"),
    );
    let report = read_with(
        "PC版メンテナンス終了のお知らせ",
        "11月12日(木)14:00より行っておりましたPC版のメンテナンスが終了いたしました。",
        context("2026-11-12 18:00"),
    );
    let resolved = reconcile(&[
        article(1, "2026-11-10 15:00", &notice),
        article(2, "2026-11-12 18:00", &report),
    ]);
    assert_eq!(resolved[0].disposition, Disposition::Scheduled);
    assert_eq!(resolved[0].resolved_by, None);
}

#[test]
fn nearby_dates_are_not_linked() {
    let notice = read_with(
        "11/12(木) 14:00～17:00 メンテナンスのお知らせ",
        "メンテナンスを実施いたします。",
        context("2026-11-10 15:00"),
    );
    let report = read_with(
        "メンテナンス完了のお知らせ",
        "11月13日(金)14:00より行っておりましたメンテナンスが終了いたしました。",
        context("2026-11-13 18:00"),
    );
    let resolved = reconcile(&[
        article(1, "2026-11-10 15:00", &notice),
        article(2, "2026-11-13 18:00", &report),
    ]);
    assert_eq!(resolved[0].disposition, Disposition::Scheduled);
    assert_eq!(resolved[0].resolved_by, None);
}

#[test]
fn ambiguous_reports_are_not_linked() {
    // 同じ日に対象の違う予告が 2 つあり、対象を書かない日付だけの報告はどちらか決められない。
    let android = read_with(
        "Android版メンテナンスのお知らせ",
        "■メンテナンス日時\n11月12日(木) 10:00～12:00",
        context("2026-11-10 15:00"),
    );
    let ios = read_with(
        "iOS版メンテナンスのお知らせ",
        "■メンテナンス日時\n11月12日(木) 13:00～15:00",
        context("2026-11-10 15:10"),
    );
    let report = read_with(
        "11/12 メンテナンス完了のお知らせ",
        "メンテナンスが完了いたしました。",
        context("2026-11-12 18:00"),
    );
    let resolved = reconcile(&[
        article(1, "2026-11-10 15:00", &android),
        article(2, "2026-11-10 15:10", &ios),
        article(3, "2026-11-12 18:00", &report),
    ]);
    assert_eq!(resolved[0].resolved_by, None);
    assert_eq!(resolved[1].resolved_by, None);
}

#[test]
fn many_articles_stay_fast() {
    // 数百件でも記事の数の 2 乗にならない (索引で候補を引く)。
    let schedules: Vec<Schedule> = (0..300)
        .map(|day| {
            let date = ymd("2026-01-01") + chrono::Duration::days(day);
            read_with(
                "メンテナンスのお知らせ",
                &format!(
                    "■メンテナンス日時\n{}年{}月{}日 14:00～17:00",
                    chrono::Datelike::year(&date),
                    chrono::Datelike::month(&date),
                    chrono::Datelike::day(&date)
                ),
                context(&format!("{} 09:00", date.format("%Y-%m-%d"))),
            )
        })
        .collect();
    let articles: Vec<SourceArticle<'_>> = schedules
        .iter()
        .enumerate()
        .map(|(index, schedule)| {
            let date = ymd("2026-01-01") + chrono::Duration::days(index as i64);
            article(
                index as i64,
                &format!("{} 09:00", date.format("%Y-%m-%d")),
                schedule,
            )
        })
        .collect();
    let started = std::time::Instant::now();
    let resolved = reconcile(&articles);
    assert_eq!(resolved.len(), 300);
    assert!(started.elapsed() < std::time::Duration::from_millis(200));
}
