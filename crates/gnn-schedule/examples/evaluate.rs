//! 手元の DB の記事で読み取りを評価する (読むだけ。DB は書き換えない)。
//!
//! ```text
//! cargo run -p gnn-schedule --release --example evaluate -- <nexus.db> [--gold <正解.json>]
//!     [--ids 1,2,3] [--all] [--baseline] [--quiet]
//! ```
//!
//! - 既定ではメンテナンスに関係する記事 (題名か本文に「メンテ」を含む) を読み、記事ごとの結果と集計を出す。
//!   `--all` はすべての記事。`--ids` は指定した記事だけ。
//! - `--gold` を渡すと正解と比べ、時間帯の一致率・補填などの期限をメンテナンスと取り違えた率・状態の誤り・
//!   実施中の時点の一致率・保留の率を、正解の `split` (`tune` 調整に使った記事、`holdout` 使っていない記事)
//!   ごとに出す。形態素解析を使わない読み (`extract_without_morphology`) の数字も並べる。
//! - `--baseline` は記事ごとの結果を形態素解析を使わない読みで出す。
//! - HoYoverse のソース (原神・崩壊：スターレイル・ゼンレスゾーンゼロ) は、本文の既定の地域時刻を
//!   Asia/Tokyo、サーバー時間を Asia/Shanghai として読む。ほかは Asia/Tokyo。
//!
//! 正解のファイル (JSON の配列):
//!
//! ```json
//! [{"id": 3, "split": "tune",
//!   "maintenance": [{"scope": null, "status": "scheduled",
//!                    "planned": ["2026-11-05 14:00", "2026-11-05 17:00"], "actual": [null, null],
//!                    "extended": false, "ongoing": "2026-11-05 15:30"}],
//!   "not_maintenance": ["2026-10-31 23:59"]}]
//! ```
//!
//! 時刻は日本時間の `YYYY-MM-DD HH:MM`、日付だけは `YYYY-MM-DD`、未定は `"undecided"`、書かれていないは `null`。
//! `scope` は 1 つの対象の名前 (複数の対象を挙げた記事は対象ごとに 1 件)。`ongoing` は「実施中」の時点
//! (`Ongoing::as_of`。実施中の報告がなければ書かない)。

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use chrono::{DateTime, TimeZone, Utc};
use chrono_tz::Tz;
use gnn_core::schedule::{
    Change, Disposition, Maintenance, Ongoing, Schedule, Scope, Slot, TimeValue, When,
};
use gnn_core::{Published, TimePrecision};
use gnn_schedule::{Article, Context, Extractor, SourceArticle};
use rusqlite::{Connection, OpenFlags};
use serde::Deserialize;

const TOKYO: Tz = chrono_tz::Asia::Tokyo;

struct Row {
    id: i64,
    source_id: String,
    title: String,
    body_html: String,
    body_text: String,
    published: Option<Published>,
    first_seen_at: DateTime<Utc>,
}

#[derive(Debug, Deserialize)]
struct Gold {
    id: i64,
    #[serde(default = "default_split")]
    split: String,
    #[serde(default)]
    maintenance: Vec<GoldMaintenance>,
    #[serde(default)]
    not_maintenance: Vec<String>,
}

fn default_split() -> String {
    "tune".to_string()
}

#[derive(Debug, Deserialize)]
struct GoldMaintenance {
    #[serde(default)]
    scope: Option<String>,
    status: String,
    #[serde(default)]
    planned: [Option<String>; 2],
    #[serde(default)]
    actual: [Option<String>; 2],
    #[serde(default)]
    extended: bool,
    /// 実施中の時点 (`Ongoing::as_of`)。
    #[serde(default)]
    ongoing: Option<String>,
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(db) = args.first() else {
        eprintln!(
            "使い方: evaluate <nexus.db> [--gold <正解.json>] [--ids 1,2] [--all] [--baseline] [--quiet]"
        );
        std::process::exit(2);
    };
    let flag = |name: &str| args.iter().any(|arg| arg == name);
    let value = |name: &str| {
        args.iter()
            .position(|arg| arg == name)
            .and_then(|index| args.get(index + 1))
            .cloned()
    };
    let ids: Option<Vec<i64>> = value("--ids").map(|ids| {
        ids.split(',')
            .filter_map(|id| id.trim().parse().ok())
            .collect()
    });
    let gold: Vec<Gold> = value("--gold")
        .map(|path| {
            let text = std::fs::read_to_string(&path)
                .unwrap_or_else(|e| panic!("{path} を読めません: {e}"));
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("{path} の書き方: {e}"))
        })
        .unwrap_or_default();

    let rows = load(db);
    let selected: Vec<&Row> = rows
        .iter()
        .filter(|row| match &ids {
            Some(ids) => ids.contains(&row.id),
            None => {
                flag("--all")
                    || row.title.contains("メンテ")
                    || row.body_text.contains("メンテ")
                    || gold.iter().any(|gold| gold.id == row.id)
            }
        })
        .collect();

    let mut extractor = Extractor::new();
    // 1 回目は辞書のページを読み込むので、時間の計測から外す。
    if let Some(row) = selected.first() {
        let _ = extractor.extract(&article(row));
    }
    let mut schedules: BTreeMap<i64, Schedule> = BTreeMap::new();
    let mut baseline: BTreeMap<i64, Schedule> = BTreeMap::new();
    let mut total = Duration::ZERO;
    let mut slowest = (Duration::ZERO, 0);
    for row in &selected {
        let started = Instant::now();
        let schedule = extractor.extract(&article(row));
        let elapsed = started.elapsed();
        total += elapsed;
        if elapsed > slowest.0 {
            slowest = (elapsed, row.id);
        }
        schedules.insert(row.id, schedule);
        baseline.insert(row.id, extractor.extract_without_morphology(&article(row)));
    }

    if flag("--debug") {
        for row in &selected {
            println!("##### {} {}", row.id, row.title);
            println!("{}", extractor.debug(&article(row)));
        }
        return;
    }
    if !flag("--quiet") {
        for row in &selected {
            let schedule = if flag("--baseline") {
                &baseline[&row.id]
            } else {
                &schedules[&row.id]
            };
            print_article(row, schedule);
        }
        print_reconcile(&selected, &schedules);
    }

    println!("\n# 集計");
    println!("記事: {} 件", selected.len());
    let with_maintenance = schedules
        .values()
        .filter(|schedule| !schedule.maintenance.is_empty())
        .count();
    let with_periods = schedules
        .values()
        .filter(|schedule| !schedule.periods.is_empty())
        .count();
    println!(
        "メンテナンスを読んだ記事: {with_maintenance} 件、期間を読んだ記事: {with_periods} 件"
    );
    let mut dispositions: BTreeMap<String, usize> = BTreeMap::new();
    for schedule in schedules.values() {
        for maintenance in &schedule.maintenance {
            *dispositions
                .entry(format!("{:?}", maintenance.disposition))
                .or_default() += 1;
        }
    }
    println!("状態: {dispositions:?}");
    let mut notes: BTreeMap<String, usize> = BTreeMap::new();
    for schedule in schedules.values() {
        for note in &schedule.notes {
            *notes.entry(note.code.clone()).or_default() += 1;
        }
    }
    println!("記録: {notes:?}");
    if !selected.is_empty() {
        println!(
            "時間: 合計 {:.1} ms、1 記事あたり {:.2} ms、最も遅い記事 {} ({:.2} ms)",
            total.as_secs_f64() * 1000.0,
            total.as_secs_f64() * 1000.0 / selected.len() as f64,
            slowest.1,
            slowest.0.as_secs_f64() * 1000.0
        );
    }

    // 突き合わせの時間 (ソースごとに、記事を古い順に渡す)。
    let mut by_source: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for row in &selected {
        by_source.entry(&row.source_id).or_default().push(row);
    }
    let started = Instant::now();
    let mut resolved_count = 0;
    let mut linked = 0;
    for rows in by_source.values() {
        let articles: Vec<SourceArticle<'_>> = rows
            .iter()
            .map(|row| SourceArticle {
                entry_id: row.id,
                published: row.published,
                first_seen_at: row.first_seen_at,
                schedule: &schedules[&row.id],
            })
            .collect();
        let resolved = gnn_schedule::reconcile(&articles);
        resolved_count += resolved.len();
        linked += resolved
            .iter()
            .filter(|item| item.resolved_by.is_some())
            .count();
    }
    println!(
        "突き合わせ: メンテナンス {resolved_count} 件のうち {linked} 件に別の記事の報告を反映 ({:.2} ms)",
        started.elapsed().as_secs_f64() * 1000.0
    );

    if !gold.is_empty() {
        for split in ["tune", "holdout"] {
            let golds: Vec<&Gold> = gold.iter().filter(|gold| gold.split == split).collect();
            if golds.is_empty() {
                continue;
            }
            println!("\n## 正解との比較: {split} ({} 記事)", golds.len());
            let morphology = score(&golds, &schedules, flag("--verbose"));
            let regex = score(&golds, &baseline, false);
            println!("| 指標 | 形態素解析 | 正規表現と構造だけ |");
            println!("| --- | --- | --- |");
            for (name, a, b) in [
                ("時間帯の一致率", &morphology.windows, &regex.windows),
                ("開始の一致率", &morphology.starts, &regex.starts),
                (
                    "状態の誤り",
                    &morphology.status_errors,
                    &regex.status_errors,
                ),
                ("実施中の一致率", &morphology.ongoing, &regex.ongoing),
                (
                    "延長の誤り",
                    &morphology.extended_errors,
                    &regex.extended_errors,
                ),
                (
                    "期限などの取り違え",
                    &morphology.confusions,
                    &regex.confusions,
                ),
                ("余分なメンテナンス", &morphology.spurious, &regex.spurious),
                ("読み落とし", &morphology.missed, &regex.missed),
                ("保留", &morphology.abstained, &regex.abstained),
            ] {
                println!("| {name} | {} | {} |", a.show(), b.show());
            }
        }
    }
}

fn load(path: &str) -> Vec<Row> {
    let connection = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap_or_else(|e| panic!("{path} を開けません: {e}"));
    let mut statement = connection
        .prepare(
            "SELECT id, source_id, title, body_html, body_text, published_at, published_precision, \
             first_seen_at FROM entries ORDER BY source_id, COALESCE(published_at, first_seen_at), id",
        )
        .expect("SQL");
    statement
        .query_map([], |row| {
            let published_at: Option<i64> = row.get(5)?;
            let precision: Option<String> = row.get(6)?;
            let first_seen: i64 = row.get(7)?;
            Ok(Row {
                id: row.get(0)?,
                source_id: row.get(1)?,
                title: row.get(2)?,
                body_html: row.get(3)?,
                body_text: row.get(4)?,
                published: published_at.map(|at| Published {
                    at: Utc.timestamp_opt(at, 0).single().unwrap_or_default(),
                    precision: precision
                        .as_deref()
                        .and_then(TimePrecision::parse)
                        .unwrap_or(TimePrecision::DateTime),
                }),
                first_seen_at: Utc
                    .timestamp_opt(first_seen, 0)
                    .single()
                    .unwrap_or_default(),
            })
        })
        .expect("SQL")
        .filter_map(Result::ok)
        .collect()
}

fn context(row: &Row) -> Context {
    let hoyoverse = matches!(
        row.source_id.as_str(),
        "genshin_impact" | "honkai_star_rail" | "zenless_zone_zero"
    );
    Context {
        published: row.published,
        first_seen_at: row.first_seen_at,
        timezone: TOKYO,
        server_timezone: hoyoverse.then_some(chrono_tz::Asia::Shanghai),
    }
}

fn article(row: &Row) -> Article<'_> {
    Article {
        title: &row.title,
        body_html: &row.body_html,
        body_text: &row.body_text,
        context: context(row),
    }
}

fn show_slot(slot: &Slot<TimeValue>) -> String {
    match slot {
        Slot::Unspecified => "-".to_string(),
        Slot::Undecided => "未定".to_string(),
        Slot::Value(value) => {
            let mark = match value.certainty {
                gnn_core::schedule::Certainty::Explicit => "",
                gnn_core::schedule::Certainty::Approximate => "≈",
                gnn_core::schedule::Certainty::Inferred => "?",
            };
            match &value.when {
                When::Date { date, .. } => format!("{date}{mark}"),
                When::Instant { at, .. } => {
                    format!(
                        "{}{mark}",
                        at.with_timezone(&TOKYO).format("%Y-%m-%d %H:%M")
                    )
                }
            }
        }
    }
}

fn print_article(row: &Row, schedule: &Schedule) {
    let published = row.published.map_or("-".to_string(), |published| {
        published
            .at
            .with_timezone(&TOKYO)
            .format("%Y-%m-%d %H:%M")
            .to_string()
    });
    println!(
        "\n### {} {} [{published}] {}",
        row.id, row.source_id, row.title
    );
    for maintenance in &schedule.maintenance {
        println!(
            "  メンテ {} {:?} 予定[{} - {}] 実際[{} - {}]{}{}",
            scope_text(&maintenance.scope),
            maintenance.disposition,
            show_slot(&maintenance.planned.start),
            show_slot(&maintenance.planned.end),
            show_slot(&maintenance.actual.start),
            show_slot(&maintenance.actual.end),
            ongoing_text(maintenance.ongoing.as_ref()),
            changes_text(&maintenance.changes)
        );
        for evidence in maintenance.evidence.iter().take(2) {
            println!(
                "    根拠 {:?}: {}",
                evidence.field,
                evidence.text.replace('\n', " / ")
            );
        }
    }
    for period in &schedule.periods {
        println!(
            "  期間 {:?} {} [{} - {}]{}",
            period.kind,
            period.label.as_deref().unwrap_or("-"),
            show_slot(&period.window.start),
            show_slot(&period.window.end),
            changes_text(&period.changes)
        );
    }
    for note in &schedule.notes {
        println!("  記録 {} {}", note.code, note.message);
    }
}

fn scope_text(scope: &Scope) -> String {
    match scope {
        Scope::Unknown => "対象不明".to_string(),
        Scope::All => "全体".to_string(),
        Scope::Named(name) => name.clone(),
    }
}

fn changes_text(changes: &[Change]) -> String {
    changes
        .iter()
        .map(|change| match change {
            Change::Extended { from, end, .. } => {
                format!(" 延長({} → {})", show_slot(from), show_slot(end))
            }
            Change::Rescheduled { from, to, .. } => format!(
                " 日程変更({} - {} → {} - {})",
                show_slot(&from.start),
                show_slot(&from.end),
                show_slot(&to.start),
                show_slot(&to.end)
            ),
        })
        .collect()
}

fn ongoing_text(ongoing: Option<&Ongoing>) -> String {
    ongoing.map_or(String::new(), |ongoing| {
        format!(
            " 実施中({} 時点)",
            show_slot(&Slot::Value(ongoing.as_of.clone()))
        )
    })
}

fn print_reconcile(rows: &[&Row], schedules: &BTreeMap<i64, Schedule>) {
    println!("\n# 記事どうしの突き合わせ");
    let mut by_source: BTreeMap<&str, Vec<&Row>> = BTreeMap::new();
    for row in rows {
        by_source.entry(&row.source_id).or_default().push(row);
    }
    for (source, rows) in by_source {
        let articles: Vec<SourceArticle<'_>> = rows
            .iter()
            .map(|row| SourceArticle {
                entry_id: row.id,
                published: row.published,
                first_seen_at: row.first_seen_at,
                schedule: &schedules[&row.id],
            })
            .collect();
        for resolved in gnn_schedule::reconcile(&articles) {
            if let Some(by) = resolved.resolved_by {
                println!(
                    "  {source} 記事 {} の {} 件目 → {:?} 予定[{} - {}] 実際[{} - {}]{}{} (記事 {by} の報告)",
                    resolved.entry_id,
                    resolved.index + 1,
                    resolved.disposition,
                    show_slot(&resolved.planned.start),
                    show_slot(&resolved.planned.end),
                    show_slot(&resolved.actual.start),
                    show_slot(&resolved.actual.end),
                    ongoing_text(resolved.ongoing.as_ref()),
                    changes_text(&resolved.changes),
                );
            }
        }
    }
}

/// 割合の指標。
#[derive(Default)]
struct Rate {
    hit: usize,
    total: usize,
}

impl Rate {
    fn add(&mut self, hit: bool) {
        self.total += 1;
        if hit {
            self.hit += 1;
        }
    }

    fn show(&self) -> String {
        if self.total == 0 {
            return "-".to_string();
        }
        format!(
            "{}/{} ({:.0}%)",
            self.hit,
            self.total,
            self.hit as f64 * 100.0 / self.total as f64
        )
    }
}

#[derive(Default)]
struct Score {
    /// 正解のメンテナンスのうち、予定・実際の開始と終了がすべて合った割合。
    windows: Rate,
    /// 開始 (予定か実際) が合った割合。
    starts: Rate,
    /// 対応の付いたメンテナンスのうち、状態が違った割合。
    status_errors: Rate,
    /// 正解か読みのどちらかが実施中としたメンテナンスのうち、実施中の時点が合った割合。
    ongoing: Rate,
    extended_errors: Rate,
    /// 期限などの日時をメンテナンスの時として読んだ記事の割合 (期限などのある記事のうち)。
    confusions: Rate,
    /// 正解にないメンテナンスを読んだ記事の割合。
    spurious: Rate,
    /// 正解のメンテナンスを読み落とした割合。
    missed: Rate,
    /// 決められないもの (`Unresolved` か記録) を残した記事の割合。
    abstained: Rate,
}

fn score(golds: &[&Gold], schedules: &BTreeMap<i64, Schedule>, verbose: bool) -> Score {
    let mut score = Score::default();
    for gold in golds {
        let Some(schedule) = schedules.get(&gold.id) else {
            continue;
        };
        let mut used = vec![false; schedule.maintenance.len()];
        for expected in &gold.maintenance {
            // 対象と開始で対応を付ける。
            let found = schedule
                .maintenance
                .iter()
                .enumerate()
                .filter(|(index, _)| !used[*index])
                .max_by_key(|(_, maintenance)| similarity(expected, maintenance))
                .filter(|(_, maintenance)| similarity(expected, maintenance) > 0);
            score.missed.add(found.is_none());
            let Some((index, maintenance)) = found else {
                score.windows.add(false);
                score.starts.add(false);
                if verbose {
                    println!("  読み落とし: 記事 {} {:?}", gold.id, expected);
                }
                continue;
            };
            used[index] = true;
            let window_ok = slot_eq(&expected.planned[0], &maintenance.planned.start)
                && slot_eq(&expected.planned[1], &maintenance.planned.end)
                && slot_eq(&expected.actual[0], &maintenance.actual.start)
                && slot_eq(&expected.actual[1], &maintenance.actual.end);
            score.windows.add(window_ok);
            let start_ok = (expected.planned[0].is_some()
                && slot_eq(&expected.planned[0], &maintenance.planned.start))
                || (expected.actual[0].is_some()
                    && slot_eq(&expected.actual[0], &maintenance.actual.start))
                || (expected.planned[0].is_none() && expected.actual[0].is_none());
            score.starts.add(start_ok);
            let status_ok = status_name(maintenance.disposition) == expected.status;
            score.status_errors.add(!status_ok);
            let ongoing_ok = match &maintenance.ongoing {
                Some(ongoing) => slot_eq(&expected.ongoing, &Slot::Value(ongoing.as_of.clone())),
                None => expected.ongoing.is_none(),
            };
            if expected.ongoing.is_some() || maintenance.ongoing.is_some() {
                score.ongoing.add(ongoing_ok);
            }
            let extended = maintenance
                .changes
                .iter()
                .any(|change| matches!(change, gnn_core::schedule::Change::Extended { .. }));
            score.extended_errors.add(extended != expected.extended);
            if verbose && (!window_ok || !status_ok || extended != expected.extended || !ongoing_ok)
            {
                println!(
                    "  違い: 記事 {} 正解 {:?} / 読み {:?} 予定[{} - {}] 実際[{} - {}] 延長 {extended}{}",
                    gold.id,
                    expected,
                    maintenance.disposition,
                    show_slot(&maintenance.planned.start),
                    show_slot(&maintenance.planned.end),
                    show_slot(&maintenance.actual.start),
                    show_slot(&maintenance.actual.end),
                    ongoing_text(maintenance.ongoing.as_ref()),
                );
            }
        }
        let spurious = used.iter().any(|used| !used);
        score.spurious.add(spurious);
        if verbose && spurious {
            println!("  余分: 記事 {}", gold.id);
        }
        if !gold.not_maintenance.is_empty() {
            let confused = schedule.maintenance.iter().any(|maintenance| {
                [
                    &maintenance.planned.start,
                    &maintenance.planned.end,
                    &maintenance.actual.start,
                    &maintenance.actual.end,
                ]
                .into_iter()
                .any(|slot| {
                    gold.not_maintenance
                        .iter()
                        .any(|text| slot_eq(&Some(text.clone()), slot))
                })
            });
            score.confusions.add(confused);
            if verbose && confused {
                println!("  取り違え: 記事 {}", gold.id);
            }
        }
        let abstained = schedule
            .maintenance
            .iter()
            .any(|maintenance| maintenance.disposition == Disposition::Unresolved)
            || !schedule.notes.is_empty();
        score.abstained.add(abstained);
    }
    score
}

/// 正解と読みの近さ (対象が合えば 1、開始の日が合えばさらに足す)。対象の名前は空白を除いて比べる。
fn similarity(expected: &GoldMaintenance, maintenance: &Maintenance) -> u32 {
    let compact = |name: &str| name.split_whitespace().collect::<String>();
    let scope_ok = match (&expected.scope, &maintenance.scope) {
        (None, Scope::Unknown | Scope::All) => true,
        (Some(name), Scope::Named(found)) => compact(name) == compact(found),
        (Some(name), Scope::All) => name == "全体",
        _ => false,
    };
    let mut points = u32::from(scope_ok);
    for (want, slot) in [
        (&expected.planned[0], &maintenance.planned.start),
        (&expected.actual[0], &maintenance.actual.start),
        (&expected.planned[1], &maintenance.planned.end),
        (&expected.actual[1], &maintenance.actual.end),
    ] {
        if want.is_some() && slot_eq(want, slot) {
            points += 2;
        }
    }
    if points == 0
        && expected
            .planned
            .iter()
            .chain(&expected.actual)
            .all(Option::is_none)
    {
        points = 1;
    }
    points
}

fn slot_eq(expected: &Option<String>, slot: &Slot<TimeValue>) -> bool {
    match (expected.as_deref(), slot) {
        (None, Slot::Unspecified) => true,
        (Some("undecided"), Slot::Undecided) => true,
        (Some(text), Slot::Value(value)) => match &value.when {
            When::Instant { at, .. } => {
                at.with_timezone(&TOKYO)
                    .format("%Y-%m-%d %H:%M")
                    .to_string()
                    == text
            }
            When::Date { date, .. } => date.to_string() == text,
        },
        _ => false,
    }
}

fn status_name(disposition: Disposition) -> &'static str {
    match disposition {
        Disposition::Scheduled => "scheduled",
        Disposition::Completed => "completed",
        Disposition::Cancelled => "cancelled",
        Disposition::Postponed => "postponed",
        Disposition::Unresolved => "unresolved",
    }
}
