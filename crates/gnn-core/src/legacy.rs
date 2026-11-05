//! dmm-games-info が書き出していた `game_info.jsonl` の読み書き。
//!
//! 1 行 1 記事で `{"game", "time", "title", "body", "time_range"}` を持つ。
//! 元の記事 ID や URL は持たないため、取り込んだ記事は `legacy:` で始まる ID になる。
//! `time_range` (メンテナンスやイベントの日付) は読み捨て、予定は取り込んだ本文から読み直す。

use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead as _, BufReader, BufWriter, Write as _};
use std::path::Path;

use anyhow::{Context as _, Result};
use chrono::{DateTime, Datelike as _, NaiveDate};
use serde::{Deserialize, Serialize};

use crate::entry::{Entry, EntryDraft, Published, list_signature};

/// 読む行。`time_range` などほかの項目は読み捨てる。
#[derive(Debug, Deserialize)]
struct LegacyRow {
    game: String,
    time: i64,
    title: String,
    #[serde(default)]
    body: String,
}

/// 書く行。旧アプリは `time_range` があることを前提に読むので、空でも必ず書く。
#[derive(Debug, Serialize)]
struct LegacyOutput<'a> {
    game: &'a str,
    time: i64,
    title: &'a str,
    body: &'a str,
    time_range: Vec<(i32, u32, u32)>,
}

/// 旧形式から読んだ記事。
#[derive(Debug, Default)]
pub struct LegacyImport {
    /// ソース ID と記事。
    pub rows: Vec<(String, EntryDraft)>,
    /// 読めなかった行の番号 (1 始まり)。
    pub skipped: Vec<usize>,
}

/// 旧形式を読む。読めない行は飛ばし、その行番号を残す。
pub fn read_jsonl(path: &Path) -> Result<LegacyImport> {
    let file = File::open(path).with_context(|| format!("{} を開けません", path.display()))?;
    let mut rows = Vec::new();
    let mut skipped = Vec::new();
    for (index, line) in BufReader::new(file).lines().enumerate() {
        let line = line?;
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        let Ok(row) = serde_json::from_str::<LegacyRow>(line) else {
            skipped.push(index + 1);
            continue;
        };
        let Some(at) = DateTime::from_timestamp(row.time, 0) else {
            skipped.push(index + 1);
            continue;
        };
        let external_id = format!(
            "legacy:{}",
            list_signature(&[Some(&row.title), Some(&row.time.to_string())])
        );
        rows.push((
            row.game,
            EntryDraft {
                external_id,
                title: row.title.clone(),
                url: None,
                published: Some(Published::datetime(at)),
                body_html: Some(row.body.clone()),
                image_url: None,
                note: None,
                list_signature: String::new(),
            },
        ));
    }
    Ok(LegacyImport { rows, skipped })
}

/// 旧形式で書き出す。新しい順に並べた記事と、記事ごとの `time_range` の日付
/// (`agenda::legacy_dates` で予定の表から作る) を受け取る。
pub fn write_jsonl(
    path: &Path,
    rows: &[(String, Entry)],
    dates: &HashMap<i64, Vec<NaiveDate>>,
) -> Result<()> {
    let mut bytes = Vec::new();
    {
        let mut writer = BufWriter::new(&mut bytes);
        for (source_id, entry) in rows {
            let row = LegacyOutput {
                game: source_id,
                time: entry.summary.sort_at.timestamp(),
                title: &entry.summary.title,
                body: &entry.body_html,
                time_range: dates
                    .get(&entry.summary.id)
                    .into_iter()
                    .flatten()
                    .map(|date| (date.year(), date.month(), date.day()))
                    .collect(),
            };
            serde_json::to_writer(&mut writer, &row)?;
            writer.write_all(b"\n")?;
        }
        writer.flush()?;
    }
    crate::config::write_atomic(path, &bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Channel;
    use crate::store::{EntryQuery, Store};
    use chrono::Utc;

    #[test]
    fn imports_and_exports_legacy_rows() {
        let dir = tempfile::tempdir().unwrap();
        let input = dir.path().join("game_info.jsonl");
        std::fs::write(
            &input,
            concat!(
                r#"{"game": "edens", "time": 1770908400, "title": "臨時メンテナンス終了のお知らせ", "body": "<p><img src=\"http://asset.localhost/C:/temp/images/a.png\"></p>", "time_range": []}"#,
                "\n",
                "not json\n",
                r#"{"game": "tenkei", "time": 1770822000, "title": "アップデートのお知らせ", "body": "<p>本文</p>", "time_range": [[2026, 2, 12]]}"#,
                "\n"
            ),
        )
        .unwrap();

        let LegacyImport { rows, skipped } = read_jsonl(&input).unwrap();
        assert_eq!(skipped, vec![2]);
        assert_eq!(rows.len(), 2);
        assert!(
            rows[0]
                .1
                .body_html
                .as_deref()
                .unwrap()
                .contains("http://asset.localhost/C:/temp/images/a.png")
        );
        assert_eq!(rows[1].1.title, "アップデートのお知らせ");

        let mut store = Store::open_in_memory().unwrap();
        assert_eq!(store.import_legacy(&rows, Utc::now()).unwrap(), 2);
        let entries = store.query(&EntryQuery::channel(Channel::News)).unwrap();
        assert_eq!(entries.len(), 2);

        // 旧形式の `time_range` は読み捨て、書き出すときは予定の表から作った日付を書く。
        let tenkei = entries
            .iter()
            .find(|entry| entry.source_id == "tenkei")
            .unwrap()
            .id;
        let dates = HashMap::from([(tenkei, vec![NaiveDate::from_ymd_opt(2026, 2, 13).unwrap()])]);
        let output = dir.path().join("out.jsonl");
        write_jsonl(&output, &store.legacy_rows().unwrap(), &dates).unwrap();
        let text = std::fs::read_to_string(&output).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].contains("\"game\":\"edens\""));
        assert!(lines[0].contains("\"time_range\":[]"), "{}", lines[0]);
        assert!(lines[1].contains("[[2026,2,13]]"), "{}", lines[1]);
    }
}
