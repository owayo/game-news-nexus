//! 形態素解析が効く箇所。正規表現と文書の構造だけの読み (`extract_without_morphology`、日時から前向きに
//! 最初の動作の語を探す読み) と並べ、形態素解析の読みだけが正しい作文を確かめる。

mod common;

use common::*;
use gnn_core::schedule::{Disposition, Schedule};

/// 形態素解析の読みが `check` を満たし、正規表現だけの読みは満たさない。
fn morphology_wins(title: &str, body: &str, check: impl Fn(&Schedule) -> bool) {
    let morphology = read(title, body);
    let baseline = read_baseline(title, body);
    assert!(
        check(&morphology),
        "形態素解析の読みが違う: {body}{}",
        describe(&morphology)
    );
    assert!(
        !check(&baseline),
        "正規表現だけでも読めた (比べる例にならない): {body}{}",
        describe(&baseline)
    );
}

#[test]
fn relative_clause_before_cancellation() {
    // 「予定していた」は修飾節。主節は中止。
    morphology_wins(
        "お知らせ",
        "11/12に予定していたメンテナンスを中止します。",
        |schedule| disposition(schedule) == Some(Disposition::Cancelled),
    );
}

#[test]
fn negation_belongs_to_another_predicate() {
    // 否定の作用先は「解消」。メンテナンスは終わった。
    morphology_wins(
        "お知らせ",
        "メンテナンスは終了いたしましたが、一部の不具合はまだ解消していません。",
        |schedule| disposition(schedule) == Some(Disposition::Completed),
    );
}

#[test]
fn conditional_end_is_not_completion() {
    morphology_wins(
        "お知らせ",
        "11月5日14:00より行っておりますメンテナンスが終了したら、お知らせします。",
        |schedule| {
            schedule
                .maintenance
                .iter()
                .all(|maintenance| maintenance.disposition != Disposition::Completed)
                && !schedule.maintenance.is_empty()
        },
    );
}

#[test]
fn sale_end_and_next_day_maintenance() {
    // 11/12 は販売の終わり。メンテナンスは翌日。
    morphology_wins(
        "お知らせ",
        "11/12に販売が終了するため、翌日メンテナンスを行います。",
        |schedule| {
            schedule.maintenance.len() == 1
                && date(&schedule.maintenance[0].planned.start) == Some(ymd("2026-11-13"))
        },
    );
}

#[test]
fn caution_note_during_maintenance() {
    morphology_wins(
        "お知らせ",
        "※メンテナンス中は、11月5日以降に追加されたクエストを実施することができません。",
        |schedule| schedule.maintenance.is_empty(),
    );
}

#[test]
fn maintenance_as_an_adjunct() {
    // 「メンテナンスにて」は時の参照。メンテナンスの予告ではない。
    morphology_wins(
        "不具合について",
        "本不具合は11月9日に予定しているメンテナンスにて修正いたします。",
        |schedule| schedule.maintenance.is_empty(),
    );
}

#[test]
fn update_without_maintenance() {
    morphology_wins(
        "データ更新の実施について",
        "11月2日 15:30より、メンテナンスを伴わないデータ更新を実施いたします。",
        |schedule| schedule.maintenance.is_empty(),
    );
}

#[test]
fn possibility_of_extension() {
    morphology_wins(
        "メンテナンスのお知らせ",
        "11月12日14:00よりメンテナンスを実施いたします。メンテナンスは作業状況により延長する場合がございます。",
        |schedule| schedule.maintenance.len() == 1 && !has_extended(&schedule.maintenance[0]),
    );
}
