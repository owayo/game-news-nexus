//! 実施中の報告・延長・日程変更・期間の変更・複数の対象。作文の記事で確かめる。

mod common;

use chrono::{DateTime, Utc};
use common::*;
use gnn_core::Published;
use gnn_core::schedule::{
    Certainty, Change, Disposition, Maintenance, PeriodKind, Schedule, Slot, TimeValue, When,
    Window,
};
use gnn_schedule::{Context, SourceArticle, reconcile};

fn ongoing_as_of(maintenance: &Maintenance) -> &TimeValue {
    &maintenance
        .ongoing
        .as_ref()
        .expect("実施中の報告がない")
        .as_of
}

fn extended(changes: &[Change]) -> Vec<(&Slot<TimeValue>, &Slot<TimeValue>)> {
    changes
        .iter()
        .filter_map(|change| match change {
            Change::Extended { from, end, .. } => Some((from, end)),
            _ => None,
        })
        .collect()
}

fn rescheduled(changes: &[Change]) -> Vec<(&Window, &Window)> {
    changes
        .iter()
        .filter_map(|change| match change {
            Change::Rescheduled { from, to, .. } => Some((from, to)),
            _ => None,
        })
        .collect()
}

fn find<'a>(schedule: &'a Schedule, name: &str) -> &'a Maintenance {
    schedule
        .maintenance
        .iter()
        .find(|maintenance| maintenance.scope == named(name))
        .unwrap_or_else(|| panic!("{name} の件がない: {}", describe(schedule)))
}

// ---- 実施中 ----

#[test]
fn ongoing_without_time_takes_the_appended_datetime() {
    let schedule = read_with(
        "緊急メンテナンスのお知らせ",
        "(11/5 15:30追記)\n現在、緊急メンテナンスを実施しております。終了時間は未定です。\n----\n一部の機能で不具合が発生しております。",
        context("2026-11-05 14:00"),
    );
    let maintenance = only(&schedule);
    assert_eq!(
        maintenance.disposition,
        Disposition::Scheduled,
        "{}",
        describe(&schedule)
    );
    let as_of = ongoing_as_of(maintenance);
    assert!(
        matches!(as_of.when, When::Instant { at, .. } if at == jst("2026-11-05 15:30")),
        "{}",
        describe(&schedule)
    );
    assert_eq!(as_of.certainty, Certainty::Explicit);
    // 時点は開始の時刻にしない。
    assert_eq!(maintenance.planned.start, Slot::Unspecified);
    assert_eq!(maintenance.planned.end, Slot::Undecided);
}

#[test]
fn ongoing_with_a_date_only_append_is_a_date() {
    let schedule = read_with(
        "緊急メンテナンスのお知らせ",
        "(11/5追記)\n現在、緊急メンテナンスを実施しております。",
        context("2026-11-05 14:00"),
    );
    let as_of = ongoing_as_of(only(&schedule));
    assert!(
        matches!(as_of.when, When::Date { date, .. } if date == ymd("2026-11-05")),
        "{}",
        describe(&schedule)
    );
    assert_eq!(as_of.certainty, Certainty::Explicit);
}

#[test]
fn ongoing_without_append_takes_the_article_time() {
    let body = "現在、緊急メンテナンスを実施しております。終了時間は未定です。";
    // 投稿日時。
    let schedule = read_with(
        "緊急メンテナンスのお知らせ",
        body,
        context("2026-11-05 14:10"),
    );
    let maintenance = only(&schedule);
    let as_of = ongoing_as_of(maintenance);
    assert!(matches!(as_of.when, When::Instant { at, .. } if at == jst("2026-11-05 14:10")));
    assert_eq!(as_of.certainty, Certainty::Inferred);
    assert_eq!(maintenance.planned.start, Slot::Unspecified);
    // 日付だけの投稿日。
    let date_only = Context {
        published: Some(Published {
            at: jst("2026-11-05 00:00"),
            precision: gnn_core::TimePrecision::Date,
        }),
        ..context("2026-11-05 14:10")
    };
    let schedule = read_with("緊急メンテナンスのお知らせ", body, date_only);
    let as_of = ongoing_as_of(only(&schedule));
    assert!(matches!(as_of.when, When::Date { date, .. } if date == ymd("2026-11-05")));
    assert_eq!(as_of.certainty, Certainty::Inferred);
    // 投稿日時がなければ初めて見つけた日時。
    let unknown = Context {
        published: None,
        first_seen_at: jst("2026-11-05 14:25"),
        ..context("2026-11-05 14:10")
    };
    let schedule = read_with("緊急メンテナンスのお知らせ", body, unknown);
    let as_of = ongoing_as_of(only(&schedule));
    assert!(matches!(as_of.when, When::Instant { at, .. } if at == jst("2026-11-05 14:25")));
    assert_eq!(as_of.certainty, Certainty::Inferred);
}

#[test]
fn ongoing_with_start_time_keeps_the_written_start() {
    let schedule = read_with(
        "メンテナンスのお知らせ",
        "本日14:00より行っておりますメンテナンスにつきまして、現在も作業を続けております。",
        context("2026-11-05 15:00"),
    );
    let maintenance = only(&schedule);
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2026-11-05 14:00")),
        "{}",
        describe(&schedule)
    );
    assert!(maintenance.ongoing.is_some(), "{}", describe(&schedule));
}

#[test]
fn completed_maintenance_has_no_ongoing_report() {
    let schedule = read_with(
        "緊急メンテナンスのお知らせ",
        "(11/5 18:50追記)\nメンテナンスが完了いたしました。\n----\n現在、緊急メンテナンスを実施しております。",
        context("2026-11-05 14:00"),
    );
    let maintenance = only(&schedule);
    assert_eq!(maintenance.disposition, Disposition::Completed);
    assert_eq!(maintenance.ongoing, None, "{}", describe(&schedule));
}

#[test]
fn current_maintenance_in_an_append_is_ongoing() {
    let schedule = read_with(
        "臨時メンテナンスのお知らせ",
        // 文の途中の改行 (`…につきまして` の次の行) はつないで読む。
        "(11/12 20:45追記)\n現在の臨時メンテナンスにつきまして\n本日21:00に終了を予定しております。\n----\n11月12日(木)18:00より臨時メンテナンスを実施いたします。",
        context("2026-11-12 17:00"),
    );
    let maintenance = only(&schedule);
    assert_eq!(maintenance.disposition, Disposition::Scheduled);
    let as_of = ongoing_as_of(maintenance);
    assert!(
        matches!(as_of.when, When::Instant { at, .. } if at == jst("2026-11-12 20:45")),
        "{}",
        describe(&schedule)
    );
    assert_eq!(
        instant(&maintenance.planned.end),
        Some(jst("2026-11-12 21:00"))
    );
}

#[test]
fn extension_announced_before_the_start_is_not_ongoing() {
    let schedule = read_with(
        "メンテナンス時間延長のお知らせ",
        "11月12日(木)14:00～17:00に予定しておりますメンテナンスは、終了時間を19:00に延長いたします。",
        context("2026-11-10 12:00"),
    );
    let maintenance = only(&schedule);
    assert_eq!(maintenance.ongoing, None, "{}", describe(&schedule));
    let changes = extended(&maintenance.changes);
    assert_eq!(changes.len(), 1, "{}", describe(&schedule));
    assert_eq!(instant(changes[0].0), Some(jst("2026-11-12 17:00")));
    assert_eq!(instant(changes[0].1), Some(jst("2026-11-12 19:00")));
    assert_eq!(
        instant(&maintenance.planned.end),
        Some(jst("2026-11-12 19:00"))
    );
}

// ---- 延長 ----

#[test]
fn extension_with_written_start() {
    let schedule = read_with(
        "メンテナンス延長のお知らせ",
        "本日14:00より実施しておりますメンテナンスを17:00より延長いたします。終了は19:00を予定しております。",
        context("2026-11-12 16:50"),
    );
    let maintenance = only(&schedule);
    let changes = extended(&maintenance.changes);
    assert_eq!(changes.len(), 1, "{}", describe(&schedule));
    let (from, end) = changes[0];
    assert_eq!(instant(from), Some(jst("2026-11-12 17:00")));
    assert_eq!(certainty(from), Some(Certainty::Inferred), "日付は本日から");
    assert_eq!(
        instant(end),
        Some(jst("2026-11-12 19:00")),
        "{}",
        describe(&schedule)
    );
    assert_eq!(
        instant(&maintenance.planned.end),
        Some(jst("2026-11-12 19:00"))
    );
}

#[test]
fn extension_without_start_takes_the_previous_end() {
    let schedule = read_with(
        "メンテナンスのお知らせ",
        "(11/12 16:30追記)\nメンテナンスを19:00まで延長いたします。\n----\n■メンテナンス日時\n11月12日(木) 14:00～17:00",
        context("2026-11-11 15:00"),
    );
    let maintenance = only(&schedule);
    let changes = extended(&maintenance.changes);
    assert_eq!(changes.len(), 1, "{}", describe(&schedule));
    let (from, end) = changes[0];
    assert_eq!(
        instant(from),
        Some(jst("2026-11-12 17:00")),
        "{}",
        describe(&schedule)
    );
    assert_eq!(certainty(from), Some(Certainty::Inferred));
    assert_eq!(instant(end), Some(jst("2026-11-12 19:00")));
    assert_eq!(
        instant(&maintenance.planned.end),
        Some(jst("2026-11-12 19:00"))
    );
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2026-11-12 14:00"))
    );
    // 延長の記述は「実施中」の報告でもある (時点は追記の日時)。
    assert!(matches!(
        ongoing_as_of(maintenance).when,
        When::Instant { at, .. } if at == jst("2026-11-12 16:30")
    ));
}

#[test]
fn extension_with_nothing_known_before_is_unspecified() {
    let schedule = read(
        "メンテナンス延長のお知らせ",
        "現在実施中のメンテナンスにつきまして、作業延長のためメンテナンスを延長いたします。\n▼終了時間\n未定",
    );
    let maintenance = only(&schedule);
    let changes = extended(&maintenance.changes);
    assert_eq!(changes.len(), 1, "{}", describe(&schedule));
    assert_eq!(*changes[0].0, Slot::Unspecified);
    assert_eq!(*changes[0].1, Slot::Undecided);
}

// ---- 日程変更 ----

#[test]
fn two_reschedules_keep_each_step() {
    let schedule = read_with(
        "メンテナンス日程変更のお知らせ",
        "(11/9 12:00追記)\n【変更前】11月12日(木) 14:00～17:00\n【変更後】11月19日(木) 14:00～17:00\n----\n【変更前】11月10日(火) 14:00～17:00\n【変更後】11月12日(木) 14:00～17:00",
        context("2026-11-08 12:00"),
    );
    let maintenance = only(&schedule);
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2026-11-19 14:00")),
        "{}",
        describe(&schedule)
    );
    let changes = rescheduled(&maintenance.changes);
    assert_eq!(changes.len(), 2, "{}", describe(&schedule));
    assert_eq!(instant(&changes[0].0.start), Some(jst("2026-11-10 14:00")));
    assert_eq!(instant(&changes[0].1.start), Some(jst("2026-11-12 14:00")));
    assert_eq!(instant(&changes[1].0.start), Some(jst("2026-11-12 14:00")));
    assert_eq!(instant(&changes[1].1.start), Some(jst("2026-11-19 14:00")));
    assert_eq!(instant(&changes[1].1.end), Some(jst("2026-11-19 17:00")));
}

#[test]
fn struck_through_steps_are_two_reschedules() {
    let html = "<p>▼メンテナンス日時<br><s>11月10日(火) 14:00～17:00</s><br><s>11月12日(木) 14:00～17:00</s><br>11月19日(木) 14:00～17:00</p>";
    let schedule = read_html_with(
        "メンテナンス日程変更のお知らせ",
        html,
        context("2026-11-09 12:00"),
    );
    let maintenance = only(&schedule);
    let changes = rescheduled(&maintenance.changes);
    assert_eq!(changes.len(), 2, "{}", describe(&schedule));
    assert_eq!(instant(&changes[0].1.start), Some(jst("2026-11-12 14:00")));
    assert_eq!(instant(&changes[1].1.start), Some(jst("2026-11-19 14:00")));
}

#[test]
fn postponement_has_the_new_window_after() {
    let schedule = read(
        "お知らせ",
        "11月10日に予定していたメンテナンスを、11月12日(木)14:00～17:00に延期いたします。",
    );
    let maintenance = only(&schedule);
    let changes = rescheduled(&maintenance.changes);
    assert_eq!(changes.len(), 1, "{}", describe(&schedule));
    assert_eq!(date(&changes[0].0.start), Some(ymd("2026-11-10")));
    assert_eq!(instant(&changes[0].1.start), Some(jst("2026-11-12 14:00")));
    assert_eq!(instant(&changes[0].1.end), Some(jst("2026-11-12 17:00")));
}

// ---- 期間の変更 ----

#[test]
fn period_extension_from_and_to() {
    let schedule = read_with(
        "イベント開催期間延長のお知らせ",
        "▼ハロウィンイベント\n・開催期間を『～2026年10月22日4:59』から『～2026年10月25日4:59』へ延長しました。",
        context("2026-10-21 18:00"),
    );
    let events = periods(&schedule, PeriodKind::Event);
    assert_eq!(events.len(), 1, "{}", describe(&schedule));
    let period = events[0];
    assert_eq!(instant(&period.window.end), Some(jst("2026-10-25 04:59")));
    let changes = extended(&period.changes);
    assert_eq!(changes.len(), 1, "{}", describe(&schedule));
    assert_eq!(instant(changes[0].0), Some(jst("2026-10-22 04:59")));
    assert_eq!(instant(changes[0].1), Some(jst("2026-10-25 04:59")));
}

#[test]
fn period_extension_with_before_and_after_labels() {
    let schedule = read_with(
        "イベント開催期間延長のお知らせ",
        "総力戦イベントの開催期間を1日延長いたします。\n変更前：2026年10月30日(金) 23:59まで\n変更後：2026年10月31日(土) 23:59まで",
        context("2026-10-29 18:00"),
    );
    let events = periods(&schedule, PeriodKind::Event);
    assert_eq!(events.len(), 1, "{}", describe(&schedule));
    let period = events[0];
    assert_eq!(instant(&period.window.end), Some(jst("2026-10-31 23:59")));
    let changes = extended(&period.changes);
    assert_eq!(changes.len(), 1, "{}", describe(&schedule));
    assert_eq!(instant(changes[0].0), Some(jst("2026-10-30 23:59")));
}

#[test]
fn period_extension_without_before_takes_the_earlier_end() {
    let schedule = read_with(
        "パック販売期間延長のお知らせ",
        "▼パック販売期間\n2026年10月24日10:00 ～ 2026年11月2日4:59\n販売期間を2026年11月5日4:59まで延長いたします。",
        context("2026-10-30 18:00"),
    );
    let sales: Vec<_> = schedule.periods.iter().collect();
    assert_eq!(sales.len(), 1, "{}", describe(&schedule));
    let period = sales[0];
    assert_eq!(instant(&period.window.start), Some(jst("2026-10-24 10:00")));
    assert_eq!(instant(&period.window.end), Some(jst("2026-11-05 04:59")));
    let changes = extended(&period.changes);
    assert_eq!(changes.len(), 1, "{}", describe(&schedule));
    assert_eq!(instant(changes[0].0), Some(jst("2026-11-02 04:59")));
    assert_eq!(certainty(changes[0].0), Some(Certainty::Inferred));
}

#[test]
fn period_reschedule_keeps_before_and_after() {
    let schedule = read_with(
        "「ランキング戦」開催日程の変更について",
        "【変更前の開催期間】\n11月1日(日) 2:00 ～ 11月5日(木) 1:59\n【変更後の開催期間】\n11月15日(日) 2:00 ～ 11月19日(木) 1:59",
        context("2026-10-30 18:00"),
    );
    assert_eq!(schedule.periods.len(), 1, "{}", describe(&schedule));
    let period = &schedule.periods[0];
    assert_eq!(instant(&period.window.start), Some(jst("2026-11-15 02:00")));
    let changes = rescheduled(&period.changes);
    assert_eq!(changes.len(), 1, "{}", describe(&schedule));
    assert_eq!(instant(&changes[0].0.start), Some(jst("2026-11-01 02:00")));
    assert_eq!(instant(&changes[0].0.end), Some(jst("2026-11-05 01:59")));
    assert_eq!(*changes[0].1, period.window);
}

// ---- 複数の対象 ----

#[test]
fn several_scopes_are_split_and_partial_completion_stays_with_its_scope() {
    let base = "iOS版・Android版のメンテナンスを11月12日(木)14:00～17:00に実施いたします。";
    let schedule = read_with("メンテナンスのお知らせ", base, context("2026-11-11 12:00"));
    assert_eq!(schedule.maintenance.len(), 2, "{}", describe(&schedule));
    for name in ["iOS版", "Android版"] {
        let maintenance = find(&schedule, name);
        assert_eq!(maintenance.disposition, Disposition::Scheduled);
        assert_eq!(
            instant(&maintenance.planned.start),
            Some(jst("2026-11-12 14:00"))
        );
        assert_eq!(
            instant(&maintenance.planned.end),
            Some(jst("2026-11-12 17:00"))
        );
        assert!(!maintenance.evidence.is_empty());
    }
    let schedule = read_with(
        "メンテナンスのお知らせ",
        &format!(
            "(11/12 16:40追記)\niOS版のみメンテナンスが完了いたしました。Android版は引き続きメンテナンスを実施しております。\n----\n{base}"
        ),
        context("2026-11-11 12:00"),
    );
    assert_eq!(schedule.maintenance.len(), 2, "{}", describe(&schedule));
    let ios = find(&schedule, "iOS版");
    assert_eq!(
        ios.disposition,
        Disposition::Completed,
        "{}",
        describe(&schedule)
    );
    assert_eq!(ios.ongoing, None);
    let android = find(&schedule, "Android版");
    assert_eq!(
        android.disposition,
        Disposition::Scheduled,
        "{}",
        describe(&schedule)
    );
    assert!(matches!(
        ongoing_as_of(android).when,
        When::Instant { at, .. } if at == jst("2026-11-12 16:40")
    ));
}

#[test]
fn scope_list_is_split_per_scope() {
    let schedule = read_with(
        "メンテナンスのお知らせ",
        "◼メンテナンス対象\n・iOS版\n・Android版\n・PCブラウザ版\n◼メンテナンス日時\n11月12日(木) 14:00～17:00",
        context("2026-11-11 12:00"),
    );
    assert_eq!(schedule.maintenance.len(), 3, "{}", describe(&schedule));
    for name in ["iOS版", "Android版", "PCブラウザ版"] {
        let maintenance = find(&schedule, name);
        assert_eq!(
            instant(&maintenance.planned.end),
            Some(jst("2026-11-12 17:00"))
        );
    }
}

#[test]
fn partial_extension_applies_to_its_scope_only() {
    let schedule = read_with(
        "メンテナンスのお知らせ",
        "(11/12 16:50追記)\nAndroid版のみメンテナンスを19:00まで延長いたします。\n----\niOS版・Android版のメンテナンスを11月12日(木)14:00～17:00に実施いたします。",
        context("2026-11-11 12:00"),
    );
    let ios = find(&schedule, "iOS版");
    assert!(extended(&ios.changes).is_empty(), "{}", describe(&schedule));
    assert_eq!(instant(&ios.planned.end), Some(jst("2026-11-12 17:00")));
    let android = find(&schedule, "Android版");
    let changes = extended(&android.changes);
    assert_eq!(changes.len(), 1, "{}", describe(&schedule));
    assert_eq!(instant(changes[0].0), Some(jst("2026-11-12 17:00")));
    assert_eq!(instant(&android.planned.end), Some(jst("2026-11-12 19:00")));
}

// ---- 記事どうし ----

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
fn reconcile_takes_the_newest_ongoing_report_and_fills_the_extension_start() {
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
    let plan = &resolved[0];
    assert_eq!(plan.disposition, Disposition::Scheduled);
    let ongoing = plan.ongoing.as_ref().expect("実施中の報告");
    assert!(
        matches!(ongoing.as_of.when, When::Instant { at, .. } if at == jst("2026-11-12 16:50"))
    );
    let changes = extended(&plan.changes);
    assert_eq!(changes.len(), 1);
    // 延長の記事に前の終わりがなければ、予告の予定の終わりを推定で入れる。
    assert_eq!(instant(changes[0].0), Some(jst("2026-11-12 17:00")));
    assert_eq!(certainty(changes[0].0), Some(Certainty::Inferred));
    assert_eq!(*changes[0].1, Slot::Undecided);

    // 完了の報告が来れば、実施中の報告は消える。
    let report = read_with(
        "メンテナンス完了のお知らせ",
        "11月12日(木)14:00より行っておりましたメンテナンスが20:10に完了いたしました。",
        context("2026-11-12 20:15"),
    );
    let resolved = reconcile(&[
        article(1, "2026-11-10 15:00", &notice),
        article(2, "2026-11-12 16:50", &extension),
        article(3, "2026-11-12 20:15", &report),
    ]);
    assert_eq!(resolved[0].disposition, Disposition::Completed);
    assert_eq!(resolved[0].ongoing, None);
    assert_eq!(resolved[1].ongoing, None);
}

#[test]
fn unscoped_report_reaches_every_split_scope() {
    let notice = read_with(
        "メンテナンスのお知らせ",
        "iOS版・Android版のメンテナンスを11月12日(木)14:00～17:00に実施いたします。",
        context("2026-11-10 15:00"),
    );
    assert_eq!(notice.maintenance.len(), 2, "{}", describe(&notice));
    let report = read_with(
        "メンテナンス完了のお知らせ",
        "11月12日(木)14:00より行っておりましたメンテナンスが17:00に完了いたしました。",
        context("2026-11-12 17:05"),
    );
    let resolved = reconcile(&[
        article(1, "2026-11-10 15:00", &notice),
        article(2, "2026-11-12 17:05", &report),
    ]);
    for item in &resolved[..2] {
        assert_eq!(item.disposition, Disposition::Completed);
        assert_eq!(item.resolved_by, Some(2));
    }
    // 名前のある報告は、同じ名前の件にだけ当てる。
    let partial = read_with(
        "メンテナンス完了のお知らせ",
        "11月12日(木)14:00より行っておりましたiOS版のメンテナンスが17:00に完了いたしました。",
        context("2026-11-12 17:05"),
    );
    let resolved = reconcile(&[
        article(1, "2026-11-10 15:00", &notice),
        article(2, "2026-11-12 17:05", &partial),
    ]);
    let ios = resolved
        .iter()
        .zip(&notice.maintenance)
        .find(|(_, maintenance)| maintenance.scope == named("iOS版"))
        .map(|(item, _)| item)
        .expect("iOS版");
    assert_eq!(ios.disposition, Disposition::Completed);
    let android = resolved
        .iter()
        .zip(&notice.maintenance)
        .find(|(_, maintenance)| maintenance.scope == named("Android版"))
        .map(|(item, _)| item)
        .expect("Android版");
    assert_eq!(android.disposition, Disposition::Scheduled);
}
