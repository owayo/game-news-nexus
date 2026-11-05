//! 作文の例 (メンテナンスのお知らせの書き方をまねた文。実際の記事の文ではない) で、埋め込んだ IPAdic を
//! 使って読み取りを確かめる。対象・開始と終了の役割・予定と実際・状態・確かさまで見る。
//!
//! 投稿日時は特記がなければ 2026-11-05 17:00 (Asia/Tokyo)、ソースの地域時刻は Asia/Tokyo。

mod common;

use common::*;
use gnn_core::schedule::{Certainty, Disposition, PeriodKind, Scope, Slot, When, Zone};
use pretty_assertions::assert_eq;

#[test]
fn case01_heading_and_value_line_with_unrelated_dates() {
    let schedule = read(
        "メンテナンスのお知らせ",
        "下記の日程でメンテナンスを行います。\n▼メンテナンス日程\n・2026/11/12(木) 14:00～17:00 終了予定\n※時間は前後する場合があります\n----\n■11/8(日) 23:59に終了\n▼ショップ\n・限定パック",
    );
    let maintenance = only(&schedule);
    assert_eq!(maintenance.disposition, Disposition::Scheduled);
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2026-11-12 14:00"))
    );
    assert_eq!(
        instant(&maintenance.planned.end),
        Some(jst("2026-11-12 17:00"))
    );
    assert_eq!(
        certainty(&maintenance.planned.start),
        Some(Certainty::Explicit)
    );
    // 時刻だけの終わりの日付は始まりから補う。
    assert_eq!(
        certainty(&maintenance.planned.end),
        Some(Certainty::Inferred)
    );
    assert_eq!(maintenance.actual.start, Slot::Unspecified);
    // 11/8 はメンテナンスではなく、ショップの販売の終わり。
    let sales = periods(&schedule, PeriodKind::Sale);
    assert_eq!(sales.len(), 1, "{}", describe(&schedule));
    assert_eq!(instant(&sales[0].window.end), Some(jst("2026-11-08 23:59")));
}

#[test]
fn case02_year_less_date_with_weekday() {
    let schedule = read(
        "メンテナンスのお知らせ",
        "◆メンテナンス時間\n11月6日(金)11:00 ～ 14:00 終了予定",
    );
    let maintenance = only(&schedule);
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2026-11-06 11:00"))
    );
    assert_eq!(
        instant(&maintenance.planned.end),
        Some(jst("2026-11-06 14:00"))
    );
    assert_eq!(
        certainty(&maintenance.planned.start),
        Some(Certainty::Inferred)
    );
    assert!(schedule.notes.is_empty(), "{}", describe(&schedule));
}

#[test]
fn case03_headings_packed_in_one_line() {
    let schedule = read(
        "【予告】メンテナンスのお知らせ",
        "以下の日時でメンテナンスを実施させていただきます。 【メンテナンス実施日時】 2026/11/06（金）14:00 ～ 17:00 【注意事項】 ・メンテナンス中はログインできません。",
    );
    let maintenance = only(&schedule);
    assert_eq!(maintenance.disposition, Disposition::Scheduled);
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2026-11-06 14:00"))
    );
    assert_eq!(
        instant(&maintenance.planned.end),
        Some(jst("2026-11-06 17:00"))
    );
}

#[test]
fn case04_both_ends_have_dates() {
    let schedule = read(
        "定期メンテナンス実施のお知らせ",
        "▼実施期間\n2026年11月4日（水）12:00 ～ 2026年11月4日（水）16:00",
    );
    let maintenance = only(&schedule);
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2026-11-04 12:00"))
    );
    assert_eq!(
        instant(&maintenance.planned.end),
        Some(jst("2026-11-04 16:00"))
    );
    assert_eq!(
        certainty(&maintenance.planned.end),
        Some(Certainty::Explicit)
    );
}

#[test]
fn case05_sentence_with_start_only() {
    let schedule = read(
        "お知らせ",
        "2026年11月4日（水）12:00より定期メンテナンスを実施いたします。",
    );
    let maintenance = only(&schedule);
    assert_eq!(maintenance.disposition, Disposition::Scheduled);
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2026-11-04 12:00"))
    );
    assert_eq!(maintenance.planned.end, Slot::Unspecified);
}

#[test]
fn case06_completion_report_and_claim_deadline() {
    let schedule = read(
        "定期メンテナンス終了のお知らせ",
        "2026年11月4日（水）12:00より実施していた定期メンテナンスは、2026年11月4日（水）16:00に終了いたしました。\n▼補填内容\n・アイテム×100\n【お受け取り期間】11月11日（水）23:59まで",
    );
    let maintenance = only(&schedule);
    assert_eq!(maintenance.disposition, Disposition::Completed);
    assert_eq!(
        instant(&maintenance.actual.start),
        Some(jst("2026-11-04 12:00"))
    );
    assert_eq!(
        instant(&maintenance.actual.end),
        Some(jst("2026-11-04 16:00"))
    );
    assert_eq!(maintenance.planned.start, Slot::Unspecified);
    let claims = periods(&schedule, PeriodKind::Claim);
    assert_eq!(claims.len(), 1, "{}", describe(&schedule));
    assert_eq!(
        instant(&claims[0].window.end),
        Some(jst("2026-11-11 23:59"))
    );
}

#[test]
fn case07_time_only_end_of_completion() {
    let schedule = read(
        "お知らせ",
        "11月2日（月）10:30より行っておりましたメンテナンスが16:30に完了いたしました。",
    );
    let maintenance = only(&schedule);
    assert_eq!(maintenance.disposition, Disposition::Completed);
    assert_eq!(
        instant(&maintenance.actual.start),
        Some(jst("2026-11-02 10:30"))
    );
    assert_eq!(
        instant(&maintenance.actual.end),
        Some(jst("2026-11-02 16:30"))
    );
    assert_eq!(
        certainty(&maintenance.actual.end),
        Some(Certainty::Inferred)
    );
}

#[test]
fn case08_extension_with_undecided_end_is_not_cancellation() {
    let schedule = read(
        "メンテナンス延長のお知らせ",
        "現在実施中のメンテナンスにつきまして、作業延長のためメンテナンスを延長いたします。\n▼終了時間\n未定",
    );
    let maintenance = only(&schedule);
    assert_eq!(maintenance.disposition, Disposition::Scheduled);
    assert_eq!(maintenance.planned.end, Slot::Undecided);
    assert!(has_extended(maintenance));
    assert!(matches!(
        &maintenance.changes[0],
        gnn_core::schedule::Change::Extended {
            end: Slot::Undecided,
            ..
        }
    ));
}

#[test]
fn case09_appended_completion_at_the_top() {
    let schedule = read(
        "メンテナンスのお知らせ",
        "(11/5 18:50追記)\n本日14:00より行っておりましたメンテナンスにおきまして、先ほどメンテナンスが完了いたしました。\n----\n現在行っておりますメンテナンスを延長いたします。",
    );
    let maintenance = only(&schedule);
    assert_eq!(maintenance.disposition, Disposition::Completed);
    // 「本日」は追記の日 (11/5)。
    assert_eq!(
        instant(&maintenance.actual.start),
        Some(jst("2026-11-05 14:00"))
    );
    assert_eq!(
        certainty(&maintenance.actual.start),
        Some(Certainty::Inferred)
    );
    // 延長は完了とは別に残す。
    assert!(has_extended(maintenance));
}

#[test]
fn case10_cancellation_of_planned_maintenance() {
    let schedule = read(
        "お知らせ",
        "11月10日(火)に予定しておりましたメンテナンスは、中止させていただきます。",
    );
    let maintenance = only(&schedule);
    assert_eq!(maintenance.disposition, Disposition::Cancelled);
    assert_eq!(date(&maintenance.planned.start), Some(ymd("2026-11-10")));
}

#[test]
fn case11_postponed_to_a_new_window() {
    let schedule = read(
        "お知らせ",
        "11月10日に予定していたメンテナンスを、11月12日(木)14:00～17:00に延期いたします。",
    );
    let maintenance = only(&schedule);
    assert_eq!(maintenance.disposition, Disposition::Scheduled);
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2026-11-12 14:00"))
    );
    assert_eq!(
        instant(&maintenance.planned.end),
        Some(jst("2026-11-12 17:00"))
    );
    let from = rescheduled_from(maintenance).expect("日程変更の前");
    assert_eq!(date(&from.start), Some(ymd("2026-11-10")));
}

#[test]
fn case12_negated_execution_is_cancellation() {
    let schedule = read("お知らせ", "11月10日のメンテナンスは実施いたしません。");
    let maintenance = only(&schedule);
    assert_eq!(maintenance.disposition, Disposition::Cancelled);
    assert_eq!(date(&maintenance.planned.start), Some(ymd("2026-11-10")));
}

#[test]
fn case13_duration_and_explicit_zone_differing_from_source() {
    let mut context = default_context();
    context.timezone = chrono_tz::Asia::Shanghai;
    let schedule = read_with(
        "お知らせ",
        "■ メンテナンス日時\n2026/09/28 07:00 (JST)からおよそ5時間を予定。",
        context,
    );
    let maintenance = only(&schedule);
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2026-09-28 07:00"))
    );
    assert_eq!(
        instant(&maintenance.planned.end),
        Some(jst("2026-09-28 12:00"))
    );
    assert_eq!(
        certainty(&maintenance.planned.end),
        Some(Certainty::Approximate)
    );
    let Slot::Value(start) = &maintenance.planned.start else {
        panic!()
    };
    assert!(
        matches!(&start.when, When::Instant { zone: Zone::Named(name), .. } if name == "Asia/Tokyo")
    );
}

#[test]
fn case14_update_with_duration_counts_as_downtime() {
    let schedule = read(
        "Ver.3.3アップデート詳細",
        "【アップデート開始日時】\n2026/10/21 07:00（JST）\nアップデートの所要時間は約5時間と予想されます。",
    );
    let maintenance = only(&schedule);
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2026-10-21 07:00"))
    );
    assert_eq!(
        instant(&maintenance.planned.end),
        Some(jst("2026-10-21 12:00"))
    );
    assert_eq!(
        certainty(&maintenance.planned.end),
        Some(Certainty::Approximate)
    );
    assert!(!maintenance.evidence.is_empty());
}

#[test]
fn case15_window_only_in_title() {
    let schedule = read(
        "11/12(木) 14:00～17:00 メンテナンスのお知らせ",
        "メンテナンスを実施いたします。詳しくはゲーム内のお知らせをご覧ください。",
    );
    let maintenance = only(&schedule);
    assert_eq!(maintenance.disposition, Disposition::Scheduled);
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2026-11-12 14:00"))
    );
    assert_eq!(
        instant(&maintenance.planned.end),
        Some(jst("2026-11-12 17:00"))
    );
}

#[test]
fn case16_title_date_of_completion_is_not_the_start() {
    let schedule = read_with(
        "10/30 メンテナンス完了のお知らせ",
        "メンテナンスが完了いたしました。",
        context("2026-10-30 18:00"),
    );
    let maintenance = only(&schedule);
    assert_eq!(maintenance.disposition, Disposition::Completed);
    assert_eq!(maintenance.actual.start, Slot::Unspecified);
    assert_eq!(maintenance.planned.start, Slot::Unspecified);
    assert_eq!(date(&maintenance.actual.end), Some(ymd("2026-10-30")));
}

#[test]
fn case17_reference_to_past_maintenance() {
    let schedule = read(
        "9月9日メンテナンス後からのバトルの挙動について",
        "9月9日のメンテナンス以降、一部の敵の行動が変わっております。",
    );
    assert!(schedule.maintenance.is_empty(), "{}", describe(&schedule));
}

#[test]
fn case18_compensation_condition_and_24_59() {
    let schedule = read(
        "お知らせ",
        "▼補償対象\n2026/11/20 24:59（JST）までにログインした方",
    );
    assert!(schedule.maintenance.is_empty(), "{}", describe(&schedule));
    // 補償の対象の条件は期間ではない。
    assert!(schedule.periods.is_empty(), "{}", describe(&schedule));
    let schedule = read(
        "お知らせ",
        "補償は2026/11/20 24:59（JST）までにログインしてお受け取りください。",
    );
    let claims = periods(&schedule, PeriodKind::Claim);
    assert_eq!(claims.len(), 1, "{}", describe(&schedule));
    // 24:59 は翌日の 0:59。
    assert_eq!(
        instant(&claims[0].window.end),
        Some(jst("2026-11-21 00:59"))
    );
}

#[test]
fn case19_separate_windows_per_platform() {
    let schedule = read(
        "メンテナンスのお知らせ",
        "■メンテナンス日時\n・Android版：11月6日(金) 10:00～12:00\n・iOS版：11月6日(金) 13:00～15:00",
    );
    assert_eq!(schedule.maintenance.len(), 2, "{}", describe(&schedule));
    let android = &schedule.maintenance[0];
    assert_eq!(android.scope, named("Android版"));
    assert_eq!(
        instant(&android.planned.start),
        Some(jst("2026-11-06 10:00"))
    );
    assert_eq!(instant(&android.planned.end), Some(jst("2026-11-06 12:00")));
    let ios = &schedule.maintenance[1];
    assert_eq!(ios.scope, named("iOS版"));
    assert_eq!(instant(&ios.planned.start), Some(jst("2026-11-06 13:00")));
}

#[test]
fn case20_crossing_midnight() {
    for body in [
        "■メンテナンス日時\n11月6日(金) 23:00 ～ 11月7日(土) 2:00",
        "■メンテナンス日時\n11月6日(金) 23:00～翌2:00",
    ] {
        let schedule = read("メンテナンスのお知らせ", body);
        let maintenance = only(&schedule);
        assert_eq!(
            instant(&maintenance.planned.start),
            Some(jst("2026-11-06 23:00"))
        );
        assert_eq!(
            instant(&maintenance.planned.end),
            Some(jst("2026-11-07 02:00"))
        );
    }
}

#[test]
fn case21_today_and_end_in_the_next_sentence() {
    let schedule = read_with(
        "お知らせ",
        "本日15:00より臨時メンテナンスを実施いたします。終了は17:00を予定しております。",
        context("2026-11-06 09:00"),
    );
    let maintenance = only(&schedule);
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2026-11-06 15:00"))
    );
    assert_eq!(
        instant(&maintenance.planned.end),
        Some(jst("2026-11-06 17:00"))
    );
}

#[test]
fn case22_ongoing_without_start_and_undecided_end() {
    let schedule = read(
        "【緊急】ログインできない不具合について",
        "現在、緊急メンテナンスを実施しております。終了時間は未定です。",
    );
    let maintenance = only(&schedule);
    assert_eq!(maintenance.disposition, Disposition::Scheduled);
    assert_eq!(maintenance.planned.start, Slot::Unspecified);
    assert_eq!(maintenance.actual.start, Slot::Unspecified);
    assert_eq!(maintenance.planned.end, Slot::Undecided);
}

#[test]
fn case23_event_only() {
    let schedule = read(
        "ハロウィンイベント開催のお知らせ",
        "開催期間：10月30日(金) 15:00 ～ 11月6日(金) 13:59",
    );
    assert!(schedule.maintenance.is_empty(), "{}", describe(&schedule));
    let events = periods(&schedule, PeriodKind::Event);
    assert_eq!(events.len(), 1, "{}", describe(&schedule));
    assert_eq!(
        instant(&events[0].window.start),
        Some(jst("2026-10-30 15:00"))
    );
    assert_eq!(
        instant(&events[0].window.end),
        Some(jst("2026-11-06 13:59"))
    );
    assert_eq!(events[0].label.as_deref(), Some("ハロウィンイベント"));
}

#[test]
fn case24_after_maintenance_is_not_maintenance() {
    let schedule = read(
        "お知らせ",
        "メンテナンス明けより、ピックアップガチャを開催いたします。",
    );
    assert!(schedule.maintenance.is_empty(), "{}", describe(&schedule));
}

#[test]
fn case25_full_width_digits() {
    let schedule = read(
        "メンテナンスのお知らせ",
        "■メンテナンス日時\n２０２６年１１月１２日（木）１４：００～１７：００",
    );
    let maintenance = only(&schedule);
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2026-11-12 14:00"))
    );
    assert_eq!(
        instant(&maintenance.planned.end),
        Some(jst("2026-11-12 17:00"))
    );
    // 根拠は正規化する前の原文のまま。
    let Slot::Value(start) = &maintenance.planned.start else {
        panic!()
    };
    assert_eq!(
        start.evidence[0].text,
        "２０２６年１１月１２日（木）１４：００～１７：００"
    );
}

#[test]
fn case26_year_boundary() {
    let schedule = read_with(
        "お知らせ",
        "1月5日(火) 14:00～17:00 メンテナンス",
        context("2026-12-28 12:00"),
    );
    let maintenance = only(&schedule);
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2027-01-05 14:00"))
    );
    assert_eq!(
        instant(&maintenance.planned.end),
        Some(jst("2027-01-05 17:00"))
    );
}

#[test]
fn case27_caution_note_is_not_ongoing() {
    let schedule = read(
        "お知らせ",
        "※メンテナンス中はゲームをプレイすることができません。",
    );
    assert!(schedule.maintenance.is_empty(), "{}", describe(&schedule));
}

#[test]
fn case28_conditions_are_not_completion() {
    for body in [
        "メンテナンスが終了した後に配布します。",
        "終了したらお知らせします",
    ] {
        let schedule = read("お知らせ", body);
        assert!(
            schedule
                .maintenance
                .iter()
                .all(|maintenance| maintenance.disposition != Disposition::Completed),
            "{body}: {}",
            describe(&schedule)
        );
    }
}

#[test]
fn case29_scope_of_negation() {
    let schedule = read("メンテナンスのお知らせ", "メンテナンスは終了していません。");
    assert_eq!(disposition(&schedule), Some(Disposition::Scheduled));
    let schedule = read("メンテナンスのお知らせ", "延長はいたしません。");
    let maintenance = only(&schedule);
    assert!(!has_extended(maintenance), "{}", describe(&schedule));
}

#[test]
fn case30_possibility_is_not_cancellation() {
    let schedule = read(
        "メンテナンスのお知らせ",
        "状況により中止する場合があります。",
    );
    assert_eq!(disposition(&schedule), Some(Disposition::Scheduled));
}

#[test]
fn case31_before_and_after_in_any_order() {
    for body in [
        "【変更前】11月10日(火) 14:00～17:00\n【変更後】11月12日(木) 14:00～17:00",
        "【変更後】11月12日(木) 14:00～17:00\n【変更前】11月10日(火) 14:00～17:00",
    ] {
        let schedule = read("メンテナンス日程変更のお知らせ", body);
        let maintenance = only(&schedule);
        assert_eq!(
            instant(&maintenance.planned.start),
            Some(jst("2026-11-12 14:00"))
        );
        assert_eq!(
            instant(&maintenance.planned.end),
            Some(jst("2026-11-12 17:00"))
        );
        let from = rescheduled_from(maintenance).expect("変更前");
        assert_eq!(instant(&from.start), Some(jst("2026-11-10 14:00")));
    }
}

#[test]
fn case32_reserve_day_is_kept_out() {
    let schedule = read(
        "メンテナンスのお知らせ",
        "■メンテナンス日時\n11月12日(木) 14:00～17:00\n予備日：11月13日(金)",
    );
    let maintenance = only(&schedule);
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2026-11-12 14:00"))
    );
    assert_eq!(
        instant(&maintenance.planned.end),
        Some(jst("2026-11-12 17:00"))
    );
    assert!(
        has_note(&schedule, "reserve_day"),
        "{}",
        describe(&schedule)
    );
}

#[test]
fn case33_reversed_times_are_not_rolled_over() {
    let schedule = read(
        "メンテナンスのお知らせ",
        "■メンテナンス日時\n11月6日 14:00～11:00",
    );
    let maintenance = only(&schedule);
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2026-11-06 14:00"))
    );
    assert_eq!(maintenance.planned.end, Slot::Unspecified);
    assert!(
        has_note(&schedule, "reversed_range"),
        "{}",
        describe(&schedule)
    );
}

#[test]
fn case34_midnight_is_a_time() {
    let schedule = read(
        "メンテナンスのお知らせ",
        "■メンテナンス日時\n11月7日(土) 0:00～6:00",
    );
    let maintenance = only(&schedule);
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2026-11-07 00:00"))
    );
    assert_eq!(
        instant(&maintenance.planned.end),
        Some(jst("2026-11-07 06:00"))
    );
}

#[test]
fn case35_explicit_year_wins_over_wrong_weekday() {
    let schedule = read(
        "メンテナンスのお知らせ",
        "■メンテナンス日時\n2026年11月12日(金) 14:00～",
    );
    let maintenance = only(&schedule);
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2026-11-12 14:00"))
    );
    assert_eq!(
        certainty(&maintenance.planned.start),
        Some(Certainty::Explicit)
    );
    assert!(
        has_note(&schedule, "weekday_mismatch"),
        "{}",
        describe(&schedule)
    );
}

// 最小の対: 文言を少しだけ変えて結果が変わるか。

#[test]
fn pair_end_statements() {
    let completed = read(
        "お知らせ",
        "11月5日14:00より行っておりましたメンテナンスが終了しました。",
    );
    assert_eq!(disposition(&completed), Some(Disposition::Completed));
    let planned = read(
        "お知らせ",
        "11月5日14:00より行っておりますメンテナンスは17:00に終了予定です。",
    );
    assert_eq!(disposition(&planned), Some(Disposition::Scheduled));
    assert_eq!(
        instant(&planned.maintenance[0].planned.end),
        Some(jst("2026-11-05 17:00")),
        "{}",
        describe(&planned)
    );
    let not_yet = read(
        "お知らせ",
        "11月5日14:00より行っておりますメンテナンスは終了していません。",
    );
    assert_eq!(disposition(&not_yet), Some(Disposition::Scheduled));
    let condition = read(
        "お知らせ",
        "11月5日14:00より行っておりますメンテナンスが終了したら、お知らせします。",
    );
    assert_ne!(disposition(&condition), Some(Disposition::Completed));
}

#[test]
fn pair_target_changes_from_maintenance_to_sale() {
    let maintenance = read("お知らせ", "11/12からメンテを実施します。");
    assert_eq!(
        date(&only(&maintenance).planned.start),
        Some(ymd("2026-11-12"))
    );
    let sale = read("お知らせ", "11/12からショップの販売を実施します。");
    assert!(sale.maintenance.is_empty(), "{}", describe(&sale));
    let sales = periods(&sale, PeriodKind::Sale);
    assert_eq!(sales.len(), 1, "{}", describe(&sale));
    assert_eq!(date(&sales[0].window.start), Some(ymd("2026-11-12")));
}

#[test]
fn pair_adding_a_claim_deadline_does_not_change_the_window() {
    let base = "▼メンテナンス日程\n・2026/11/12(木) 14:00～17:00 終了予定";
    let plain = read("メンテナンスのお知らせ", base);
    let with_claim = read(
        "メンテナンスのお知らせ",
        &format!("{base}\n▼補填の受け取り期限\n11月19日(木)23:59まで"),
    );
    assert_eq!(plain.maintenance, with_claim.maintenance);
    assert_eq!(periods(&with_claim, PeriodKind::Claim).len(), 1);
}

#[test]
fn pair_extension_of_one_platform_keeps_the_rest() {
    let base = "■メンテナンス日時\n11月12日(木) 14:00～17:00";
    let plain = read("メンテナンスのお知らせ", base);
    assert_eq!(plain.maintenance.len(), 1);
    let schedule = read(
        "メンテナンスのお知らせ",
        &format!("{base}\n大変申し訳ございませんが、PC版のみメンテナンスを延長いたします。"),
    );
    assert_eq!(schedule.maintenance.len(), 2, "{}", describe(&schedule));
    let rest = schedule
        .maintenance
        .iter()
        .find(|maintenance| maintenance.scope == Scope::Unknown)
        .expect("対象の書かれていない件");
    assert!(!has_extended(rest));
    assert_eq!(instant(&rest.planned.end), Some(jst("2026-11-12 17:00")));
    let pc = schedule
        .maintenance
        .iter()
        .find(|maintenance| maintenance.scope == named("PC版"))
        .expect("PC版");
    assert!(has_extended(pc));
    // PC版の延長で、全体を完了にも延長にもしない。
    let schedule = read(
        "メンテナンスのお知らせ",
        &format!("{base}\nPC版のみメンテナンスが完了いたしました。"),
    );
    let rest = schedule
        .maintenance
        .iter()
        .find(|maintenance| maintenance.scope == Scope::Unknown)
        .expect("対象の書かれていない件");
    assert_eq!(
        rest.disposition,
        Disposition::Scheduled,
        "{}",
        describe(&schedule)
    );
}

#[test]
fn pair_title_window_and_completion_without_time() {
    let title = "11/12(木) 14:00～17:00 メンテナンスのお知らせ";
    let plain = read(title, "メンテナンスを実施いたします。");
    assert_eq!(disposition(&plain), Some(Disposition::Scheduled));
    let done = read(
        title,
        "先ほどメンテナンスが完了いたしました。\n----\nメンテナンスを実施いたします。",
    );
    let maintenance = only(&done);
    assert_eq!(maintenance.disposition, Disposition::Completed);
    // 題名の時間帯は予定として残る。
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2026-11-12 14:00"))
    );
}

#[test]
fn struck_through_schedule_is_the_old_one() {
    let html =
        "<p>▼メンテナンス日時<br><s>11月10日(火) 14:00～17:00</s><br>11月12日(木) 14:00～17:00</p>";
    let schedule = read_html_with("メンテナンス日程変更のお知らせ", html, default_context());
    let maintenance = only(&schedule);
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2026-11-12 14:00"))
    );
    let from = rescheduled_from(maintenance).expect("取り消し線の日程");
    assert_eq!(instant(&from.start), Some(jst("2026-11-10 14:00")));
}

#[test]
fn table_cells_and_plain_text_bodies_read_alike() {
    let html =
        "<table><tr><th>メンテナンス日時</th><td>11月12日(木) 14:00～17:00</td></tr></table>";
    let from_html = read_html_with("メンテナンスのお知らせ", html, default_context());
    assert_eq!(
        instant(&only(&from_html).planned.start),
        Some(jst("2026-11-12 14:00"))
    );
    let mut extractor = gnn_schedule::Extractor::new();
    let from_text = extractor.extract(&gnn_schedule::Article {
        title: "メンテナンスのお知らせ",
        body_html: "",
        body_text: "▼メンテナンス日時\n11月12日(木) 14:00～17:00",
        context: default_context(),
    });
    assert_eq!(
        instant(&only(&from_text).planned.end),
        Some(jst("2026-11-12 17:00"))
    );
}

#[test]
fn server_time_needs_the_server_zone() {
    let body = "イベント期間：2026/10/21 12:00 (サーバー時間) ～ 2026/11/11 04:59 (JST)";
    let schedule = read("イベントのお知らせ", body);
    // サーバーの地域時刻が分からなければ、始まりは決めない。
    let events = periods(&schedule, PeriodKind::Event);
    assert_eq!(events.len(), 1, "{}", describe(&schedule));
    assert_eq!(events[0].window.start, Slot::Unspecified);
    assert!(has_note(&schedule, "server_time_unknown"));
    let mut context = default_context();
    context.server_timezone = Some(chrono_tz::Asia::Shanghai);
    let schedule = read_with("イベントのお知らせ", body, context);
    let events = periods(&schedule, PeriodKind::Event);
    assert_eq!(
        instant(&events[0].window.start),
        Some(jst("2026-10-21 13:00"))
    );
    assert_eq!(
        instant(&events[0].window.end),
        Some(jst("2026-11-11 04:59"))
    );
}

#[test]
fn daylight_saving_gaps_are_not_guessed() {
    let mut context = default_context();
    context.timezone = chrono_tz::America::New_York;
    let schedule = read_with(
        "メンテナンスのお知らせ",
        "■メンテナンス日時\n2026年3月8日 2:30～4:00",
        context,
    );
    let maintenance = only(&schedule);
    assert_eq!(maintenance.planned.start, Slot::Unspecified);
    assert!(has_note(&schedule, "dst_gap"), "{}", describe(&schedule));
}

#[test]
fn leap_day_without_year() {
    let schedule = read_with(
        "メンテナンスのお知らせ",
        "■メンテナンス日時\n2月29日(火) 14:00～17:00",
        context("2028-02-20 12:00"),
    );
    let maintenance = only(&schedule);
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2028-02-29 14:00"))
    );
}

#[test]
fn morning_and_afternoon_words() {
    let schedule = read(
        "メンテナンスのお知らせ",
        "11月12日(木)午後2時より午後5時までメンテナンスを実施いたします。",
    );
    let maintenance = only(&schedule);
    assert_eq!(
        instant(&maintenance.planned.start),
        Some(jst("2026-11-12 14:00"))
    );
    assert_eq!(
        instant(&maintenance.planned.end),
        Some(jst("2026-11-12 17:00"))
    );
}
