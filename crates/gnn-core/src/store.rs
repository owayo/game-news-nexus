//! 記事と取得状況、予定の読み取り結果を保存する SQLite ストア。

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::Path;

use anyhow::{Context as _, Result};
use chrono::{DateTime, Utc};
use rusqlite::{Connection, OptionalExtension as _, Row, Transaction, params, params_from_iter};
use sha2::{Digest, Sha256};

use crate::agenda::{self, AgendaDetail, AgendaItem, Bound, Resolution, SourceSchedule};
use crate::entry::{Entry, EntryDraft, EntrySummary, Published, TimePrecision, sort_time};
use crate::image_ref::{self, ImageIdentity, ImageObservation};
use crate::schedule::{Disposition, PeriodKind, Schedule, Scope, Slot, TimeValue};
use crate::source::Channel;
use crate::text::{html_to_text, normalize};

/// 1: 初版。2: 旧形式から引き継いだ記事の元の ID (`legacy_id`) を持つ。
/// 3: 記事の版 (`revision`)・本文を確かめた時刻 (`body_checked_at`)・予定の読み取り結果
///    (`schedule_analysis`)・予定の表 (`schedule_items`) を持つ。本文の日付の列 (`notable_dates`) をなくす。
/// 4: 取得の定義の `url_identity.images` に合う画像の取得 URL (`entry_images`) を持つ。
const DATABASE_VERSION: i32 = 4;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS entries (
    id INTEGER PRIMARY KEY,
    source_id TEXT NOT NULL,
    channel TEXT NOT NULL,
    external_id TEXT NOT NULL,
    origin TEXT NOT NULL DEFAULT 'fetch',
    title TEXT NOT NULL,
    url TEXT,
    image_url TEXT,
    note TEXT,
    published_at INTEGER,
    published_precision TEXT,
    first_seen_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    sort_at INTEGER NOT NULL,
    body_html TEXT NOT NULL DEFAULT '',
    body_text TEXT NOT NULL DEFAULT '',
    content_hash TEXT NOT NULL DEFAULT '',
    list_signature TEXT NOT NULL DEFAULT '',
    read_at INTEGER,
    legacy_id TEXT,
    revision INTEGER NOT NULL DEFAULT 1,
    body_checked_at INTEGER,
    UNIQUE (source_id, external_id)
);
CREATE INDEX IF NOT EXISTS entries_channel_sort ON entries (channel, sort_at DESC);
CREATE INDEX IF NOT EXISTS entries_source_sort ON entries (source_id, sort_at DESC);
CREATE INDEX IF NOT EXISTS entries_legacy ON entries (source_id, legacy_id);
CREATE VIRTUAL TABLE IF NOT EXISTS entries_fts USING fts5 (search, tokenize = 'trigram');

CREATE TABLE IF NOT EXISTS source_state (
    source_id TEXT PRIMARY KEY,
    last_attempt_at INTEGER,
    last_success_at INTEGER,
    last_status TEXT,
    last_message TEXT,
    consecutive_failures INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS fetch_log (
    id INTEGER PRIMARY KEY,
    source_id TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    finished_at INTEGER NOT NULL,
    status TEXT NOT NULL,
    listed INTEGER NOT NULL,
    added INTEGER NOT NULL,
    updated INTEGER NOT NULL,
    message TEXT
);
CREATE INDEX IF NOT EXISTS fetch_log_finished ON fetch_log (finished_at DESC);

-- 予定の読み取り結果 (記事ごと)。記事から作り直せる派生データ。
-- status: found (予定あり)・nothing (該当なし)・failed (失敗)。行がなければ未解析。
CREATE TABLE IF NOT EXISTS schedule_analysis (
    entry_id INTEGER PRIMARY KEY REFERENCES entries (id) ON DELETE CASCADE,
    extractor TEXT NOT NULL,
    revision INTEGER NOT NULL,
    context TEXT NOT NULL,
    status TEXT NOT NULL,
    analyzed_at INTEGER NOT NULL,
    result TEXT,
    error TEXT
);

-- 検索用の予定の表。同じソースの読み取り結果を突き合わせたもので、ソースごとに作り直す。
-- 時刻は UTC の秒。日付だけの値は、開始ならその日の 0 時、終了なら翌日の 0 時 (含まない)。
CREATE TABLE IF NOT EXISTS schedule_items (
    id INTEGER PRIMARY KEY,
    entry_id INTEGER NOT NULL REFERENCES entries (id) ON DELETE CASCADE,
    source_id TEXT NOT NULL,
    kind TEXT NOT NULL,
    item_index INTEGER NOT NULL,
    scope_kind TEXT,
    scope_name TEXT,
    disposition TEXT,
    resolved_by INTEGER,
    period_kind TEXT,
    label TEXT,
    starts_at INTEGER,
    start_kind TEXT NOT NULL,
    ends_at INTEGER,
    end_kind TEXT NOT NULL,
    span_from INTEGER,
    span_to INTEGER,
    payload TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS schedule_items_entry ON schedule_items (entry_id);
CREATE INDEX IF NOT EXISTS schedule_items_source ON schedule_items (source_id, span_to);
CREATE INDEX IF NOT EXISTS schedule_items_span ON schedule_items (span_to, span_from);

-- 予定の表を作り直す必要があるソース (記事を消した・中身が変わったなど)。
CREATE TABLE IF NOT EXISTS schedule_stale_sources (
    source_id TEXT PRIMARY KEY
);

-- 取得の定義の url_identity.images に合う画像。記事の本文と画像 (image_url) には、取得のたびに
-- 変わらない参照 `https://source-image.invalid/<source_id>/<key>` を入れ、取りに行くときは最後に見た
-- 配信元の URL (fetch_url。署名や期限が付いたまま) を使う。記事ごとに持ち、記事と一緒に消える。
CREATE TABLE IF NOT EXISTS entry_images (
    entry_id INTEGER NOT NULL REFERENCES entries (id) ON DELETE CASCADE,
    source_id TEXT NOT NULL,
    key TEXT NOT NULL,
    fetch_url TEXT NOT NULL,
    seen_at INTEGER NOT NULL,
    PRIMARY KEY (entry_id, key)
);
CREATE INDEX IF NOT EXISTS entry_images_key ON entry_images (source_id, key, seen_at);
"#;

/// 取得ログを残す件数。
const FETCH_LOG_LIMIT: i64 = 1000;

/// 記事の出どころ。旧形式から取り込んだ記事は、同じタイトルの記事を取得したときに引き継ぐ。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Origin {
    Fetch,
    Legacy,
}

impl Origin {
    fn as_str(self) -> &'static str {
        match self {
            Origin::Fetch => "fetch",
            Origin::Legacy => "legacy",
        }
    }
}

/// 取得済みの記事。詳細を取り直すかの判断と、取り直さないときの値に使う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KnownItem {
    pub list_signature: String,
    pub first_seen_at: DateTime<Utc>,
    pub has_body: bool,
    pub title: String,
    pub url: Option<String>,
    pub image_url: Option<String>,
    pub note: Option<String>,
    pub published: Option<Published>,
}

/// 画像の参照を持つ記事 1 件。取得 URL の期限が切れた画像を取り直すときに、詳細を取り直す記事を選ぶのに使う。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageHolder {
    pub external_id: String,
    /// 記事の画像 (`image_url`) がこの参照か。違えば本文にだけある。
    pub is_image_url: bool,
    /// この記事で取得 URL を最後に記録した時刻。
    pub seen_at: DateTime<Utc>,
}

/// 取得結果を保存した結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyReport {
    pub added: Vec<NewEntry>,
    pub updated: usize,
    pub removed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewEntry {
    pub id: i64,
    pub title: String,
}

/// 1 回の取得の状態。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunStatus {
    /// すべて取れた。
    Ok,
    /// 一部の記事で失敗した。
    Partial,
    /// 一覧から取れなかった。
    Failed,
}

impl RunStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RunStatus::Ok => "ok",
            RunStatus::Partial => "partial",
            RunStatus::Failed => "failed",
        }
    }

    pub fn parse(value: &str) -> Self {
        match value {
            "ok" => RunStatus::Ok,
            "partial" => RunStatus::Partial,
            _ => RunStatus::Failed,
        }
    }
}

/// 取得ログの 1 行。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FetchRun {
    pub source_id: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: DateTime<Utc>,
    pub status: RunStatus,
    pub listed: usize,
    pub added: usize,
    pub updated: usize,
    pub message: Option<String>,
}

/// ソースごとの最新の取得状況。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceState {
    pub source_id: String,
    pub last_attempt_at: Option<DateTime<Utc>>,
    pub last_success_at: Option<DateTime<Utc>>,
    pub last_status: Option<RunStatus>,
    pub last_message: Option<String>,
    pub consecutive_failures: u32,
}

/// 一覧の絞り込み条件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EntryQuery {
    pub channel: Channel,
    /// `None` はすべてのソース。
    pub source_ids: Option<Vec<String>>,
    pub search: Option<String>,
    pub unread_only: bool,
    /// `None` はすべての記事。`Some` ならその ID の記事だけ (ほかの条件も満たすもの)。
    pub ids: Option<Vec<i64>>,
    pub limit: usize,
}

impl EntryQuery {
    pub fn channel(channel: Channel) -> Self {
        Self {
            channel,
            source_ids: None,
            search: None,
            unread_only: false,
            ids: None,
            limit: 5000,
        }
    }
}

/// 予定を読み取る記事 1 件 (保存済みの値)。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnalysisInput {
    pub entry_id: i64,
    pub source_id: String,
    /// 読んだときの記事の版。結果を保存するときに今の版と照合する。
    pub revision: i64,
    pub title: String,
    pub body_html: String,
    pub body_text: String,
    pub published: Option<Published>,
    pub first_seen_at: DateTime<Utc>,
}

/// 予定の読み取りの結果 1 件。
#[derive(Debug, Clone, PartialEq)]
pub struct AnalysisResult {
    pub entry_id: i64,
    /// 読んだ記事の版 ([`AnalysisInput::revision`])。
    pub revision: i64,
    /// 抽出器の識別子 (`gnn_schedule::extractor_id`)。
    pub extractor: String,
    /// 読んだときの文脈 (本文の既定の地域時刻など) の指紋。
    pub context: String,
    pub analyzed_at: DateTime<Utc>,
    /// 読み取った予定。失敗したならその理由。
    pub outcome: Result<Schedule, String>,
}

/// 保存した読み取り結果の状態。結果がなければ未解析。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AnalysisStatus {
    /// 予定を読み取れた (メンテナンスか期間が 1 件以上)。
    Found,
    /// 読んだが、予定は書かれていなかった。
    Nothing,
    /// 読み取りに失敗した。
    Failed,
}

impl AnalysisStatus {
    fn as_str(self) -> &'static str {
        match self {
            AnalysisStatus::Found => "found",
            AnalysisStatus::Nothing => "nothing",
            AnalysisStatus::Failed => "failed",
        }
    }

    fn parse(value: &str) -> Self {
        match value {
            "found" => AnalysisStatus::Found,
            "nothing" => AnalysisStatus::Nothing,
            _ => AnalysisStatus::Failed,
        }
    }
}

/// 読み取り結果を保存した結果。
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SavedAnalyses {
    pub saved: usize,
    /// 読んでいるあいだに中身が変わった・消えた記事の結果 (捨てた)。
    pub discarded: usize,
    /// 予定の表を作り直したソース。
    pub rebuilt: Vec<String>,
}

/// 記事の予定 (詳細の表示用)。
#[derive(Debug, Clone, PartialEq)]
pub struct EntrySchedule {
    pub status: AnalysisStatus,
    pub analyzed_at: DateTime<Utc>,
    /// 読んだのが今の中身か。違えば読み直しを待っている。
    pub current: bool,
    pub schedule: Option<Schedule>,
    pub error: Option<String>,
    /// 予定の表の行 (同じソースの記事と突き合わせたあと)。
    pub items: Vec<AgendaItem>,
    /// 結果を報告した別の記事の題名。
    pub resolvers: HashMap<i64, String>,
}

pub struct Store {
    conn: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn =
            Connection::open(path).with_context(|| format!("{} を開けません", path.display()))?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    /// 閉じる。最後の接続が閉じるとき、SQLite は WAL の中身をデータベースに書き戻してから
    /// WAL を消す (落とすだけと違い、閉じられなかったことが分かる)。
    pub fn close(self) -> Result<()> {
        self.conn
            .close()
            .map_err(|(_, err)| anyhow::Error::new(err).context("データベースを閉じられません"))
    }

    fn init(mut conn: Connection) -> Result<Self> {
        // 予定の表は、記事を消すと一緒に消える (ON DELETE CASCADE)。接続ごとに有効にする。
        conn.pragma_update(None, "foreign_keys", true)?;
        let tx = conn.transaction()?;
        let version: i32 = tx.pragma_query_value(None, "user_version", |row| row.get(0))?;
        if version > DATABASE_VERSION {
            anyhow::bail!("データベースの版 ({version}) がこのアプリより新しいため開けません");
        }
        if version < DATABASE_VERSION {
            migrate(&tx).context("データベースを新しい版に移せません")?;
        }
        tx.execute_batch(SCHEMA)
            .context("データベースの初期化に失敗しました")?;
        tx.pragma_update(None, "user_version", DATABASE_VERSION)?;
        tx.commit()?;
        Ok(Self { conn })
    }

    /// ソースの取得済み記事。
    pub fn known_items(&self, source_id: &str) -> Result<HashMap<String, KnownItem>> {
        let mut statement = self.conn.prepare_cached(
            "SELECT external_id, list_signature, first_seen_at, body_html != '', title, url, image_url, note,
                published_at, published_precision
             FROM entries WHERE source_id = ?1",
        )?;
        let rows = statement.query_map([source_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                KnownItem {
                    list_signature: row.get(1)?,
                    first_seen_at: from_unix(row.get(2)?),
                    has_body: row.get(3)?,
                    title: row.get(4)?,
                    url: row.get(5)?,
                    image_url: row.get(6)?,
                    note: row.get(7)?,
                    published: published_from(row.get(8)?, row.get(9)?),
                },
            ))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 取得結果を保存する。
    ///
    /// `images` はそのソースの `url_identity.images`。合う画像は、本文と `image_url` を参照にして
    /// 保存し、配信元の URL (取得 URL) を `entry_images` に記録する。記事が変わっていなくても、
    /// 取得 URL は今回のものに差し替える (署名の期限が切れた URL を使わないため)。
    ///
    /// `keep` を渡すと、そこにも `drafts` にもない記事を消す (新作・近日リリースのように
    /// 一覧そのものを置き換えるソース向け)。`keep` には、上限や期間で読まなかった分も含めた
    /// 一覧のすべての記事の ID を渡す。ソースの出し先が変わっていれば既存の記事も移す。
    pub fn apply_fetch(
        &mut self,
        source_id: &str,
        channel: Channel,
        images: &ImageIdentity,
        drafts: &[EntryDraft],
        keep: Option<&[String]>,
        now: DateTime<Utc>,
    ) -> Result<ApplyReport> {
        let tx = self.conn.transaction()?;
        let mut report = ApplyReport::default();
        let mut seen = HashSet::new();

        report.updated += tx.execute(
            "UPDATE entries SET channel = ?2 WHERE source_id = ?1 AND channel != ?2",
            params![source_id, channel.as_str()],
        )?;

        for draft in drafts {
            if !seen.insert(draft.external_id.as_str()) {
                continue;
            }
            match upsert(&tx, source_id, channel, images, draft, Origin::Fetch, now)? {
                Upsert::Added(id) => report.added.push(NewEntry {
                    id,
                    title: draft.title.clone(),
                }),
                Upsert::Updated => report.updated += 1,
                Upsert::Unchanged => {}
            }
        }

        if let Some(keep) = keep {
            seen.extend(keep.iter().map(String::as_str));
            let stale: Vec<i64> = {
                let mut statement =
                    tx.prepare("SELECT id, external_id FROM entries WHERE source_id = ?1")?;
                let rows = statement.query_map([source_id], |row| {
                    Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
                })?;
                rows.filter_map(|row| row.ok())
                    .filter(|(_, external_id)| !seen.contains(external_id.as_str()))
                    .map(|(id, _)| id)
                    .collect()
            };
            for id in &stale {
                delete_entry(&tx, *id)?;
            }
            report.removed = stale.len();
        }

        tx.commit()?;
        Ok(report)
    }

    /// 旧形式から記事を取り込む。同じ記事がすでにあれば (取得した記事に引き継いだ後も) 何もしない。
    pub fn import_legacy(
        &mut self,
        rows: &[(String, EntryDraft)],
        now: DateTime<Utc>,
    ) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut added = 0;
        // 旧形式の記事には画像の同一性の決まりを当てない。取り込んだ記事は書き換えず、取得した記事の
        // ように取得 URL を差し替えることもないため。
        let images = ImageIdentity::default();
        for (source_id, draft) in rows {
            if let Upsert::Added(_) = upsert(
                &tx,
                source_id,
                Channel::News,
                &images,
                draft,
                Origin::Legacy,
                now,
            )? {
                added += 1;
            }
        }
        tx.commit()?;
        Ok(added)
    }

    /// 取得ログを記録し、ソースの状態を更新する。
    pub fn record_run(&mut self, run: &FetchRun) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO fetch_log (source_id, started_at, finished_at, status, listed, added, updated, message)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                run.source_id,
                run.started_at.timestamp(),
                run.finished_at.timestamp(),
                run.status.as_str(),
                run.listed as i64,
                run.added as i64,
                run.updated as i64,
                run.message,
            ],
        )?;
        let succeeded = run.status != RunStatus::Failed;
        tx.execute(
            "INSERT INTO source_state (source_id, last_attempt_at, last_success_at, last_status, last_message, consecutive_failures)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (source_id) DO UPDATE SET
                last_attempt_at = excluded.last_attempt_at,
                last_success_at = COALESCE(excluded.last_success_at, source_state.last_success_at),
                last_status = excluded.last_status,
                last_message = excluded.last_message,
                consecutive_failures = CASE WHEN ?7 THEN 0 ELSE source_state.consecutive_failures + 1 END",
            params![
                run.source_id,
                run.finished_at.timestamp(),
                succeeded.then(|| run.finished_at.timestamp()),
                run.status.as_str(),
                run.message,
                if succeeded { 0 } else { 1 },
                succeeded,
            ],
        )?;
        tx.execute(
            "DELETE FROM fetch_log WHERE id <= (SELECT id FROM fetch_log ORDER BY id DESC LIMIT 1 OFFSET ?1)",
            [FETCH_LOG_LIMIT],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn source_states(&self) -> Result<HashMap<String, SourceState>> {
        let mut statement = self.conn.prepare(
            "SELECT source_id, last_attempt_at, last_success_at, last_status, last_message, consecutive_failures FROM source_state",
        )?;
        let rows = statement.query_map([], |row| {
            Ok(SourceState {
                source_id: row.get(0)?,
                last_attempt_at: row.get::<_, Option<i64>>(1)?.map(from_unix),
                last_success_at: row.get::<_, Option<i64>>(2)?.map(from_unix),
                last_status: row
                    .get::<_, Option<String>>(3)?
                    .map(|status| RunStatus::parse(&status)),
                last_message: row.get(4)?,
                consecutive_failures: row.get(5)?,
            })
        })?;
        rows.map(|row| {
            row.map(|state| (state.source_id.clone(), state))
                .map_err(Into::into)
        })
        .collect()
    }

    pub fn recent_runs(&self, limit: usize) -> Result<Vec<FetchRun>> {
        let mut statement = self.conn.prepare(
            "SELECT source_id, started_at, finished_at, status, listed, added, updated, message
             FROM fetch_log ORDER BY id DESC LIMIT ?1",
        )?;
        let rows = statement.query_map([limit as i64], |row| {
            Ok(FetchRun {
                source_id: row.get(0)?,
                started_at: from_unix(row.get(1)?),
                finished_at: from_unix(row.get(2)?),
                status: RunStatus::parse(&row.get::<_, String>(3)?),
                listed: row.get::<_, i64>(4)? as usize,
                added: row.get::<_, i64>(5)? as usize,
                updated: row.get::<_, i64>(6)? as usize,
                message: row.get(7)?,
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 一覧用の要約を新しい順に返す。
    pub fn query(&self, query: &EntryQuery) -> Result<Vec<EntrySummary>> {
        let mut sql = format!("SELECT {SUMMARY_COLUMNS} FROM entries WHERE channel = ?");
        let mut values: Vec<rusqlite::types::Value> =
            vec![query.channel.as_str().to_string().into()];

        if let Some(source_ids) = &query.source_ids {
            if source_ids.is_empty() {
                return Ok(Vec::new());
            }
            sql.push_str(&format!(
                " AND source_id IN ({})",
                vec!["?"; source_ids.len()].join(",")
            ));
            values.extend(source_ids.iter().map(|id| id.clone().into()));
        }
        if let Some(ids) = &query.ids {
            if ids.is_empty() {
                return Ok(Vec::new());
            }
            sql.push_str(&format!(" AND id IN ({})", vec!["?"; ids.len()].join(",")));
            values.extend(ids.iter().map(|id| (*id).into()));
        }
        if query.unread_only {
            sql.push_str(" AND read_at IS NULL");
        }
        if let Some(search) = query
            .search
            .as_deref()
            .map(normalize)
            .filter(|search| !search.trim().is_empty())
        {
            let search = search.trim().to_string();
            if search.chars().count() >= 3 {
                sql.push_str(
                    " AND id IN (SELECT rowid FROM entries_fts WHERE entries_fts MATCH ?)",
                );
                values.push(format!("\"{}\"", search.replace('"', "\"\"")).into());
            } else {
                sql.push_str(
                    " AND id IN (SELECT rowid FROM entries_fts WHERE instr(search, ?) > 0)",
                );
                values.push(search.into());
            }
        }
        sql.push_str(" ORDER BY sort_at DESC, id DESC LIMIT ?");
        values.push((query.limit as i64).into());

        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values), summary_from_row)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn entry(&self, id: i64) -> Result<Option<Entry>> {
        let sql = format!(
            "SELECT {SUMMARY_COLUMNS}, body_html, body_text, updated_at, body_checked_at FROM entries WHERE id = ?1"
        );
        Ok(self
            .conn
            .query_row(&sql, [id], |row| {
                Ok(Entry {
                    summary: summary_from_row(row)?,
                    body_html: row.get(SUMMARY_COLUMN_COUNT)?,
                    body_text: row.get(SUMMARY_COLUMN_COUNT + 1)?,
                    updated_at: from_unix(row.get(SUMMARY_COLUMN_COUNT + 2)?),
                    body_checked_at: row
                        .get::<_, Option<i64>>(SUMMARY_COLUMN_COUNT + 3)?
                        .map(from_unix),
                })
            })
            .optional()?)
    }

    pub fn set_read(&mut self, id: i64, read: bool, now: DateTime<Utc>) -> Result<()> {
        self.conn.execute(
            "UPDATE entries SET read_at = ?2 WHERE id = ?1",
            params![id, read.then(|| now.timestamp())],
        )?;
        Ok(())
    }

    /// 指定した記事を既読にする。画面に出ている記事だけを既読にするときに使う。
    pub fn mark_read_ids(&mut self, ids: &[i64], now: DateTime<Utc>) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let mut changed = 0;
        {
            let mut statement =
                tx.prepare("UPDATE entries SET read_at = ?2 WHERE id = ?1 AND read_at IS NULL")?;
            for id in ids {
                changed += statement.execute(params![id, now.timestamp()])?;
            }
        }
        tx.commit()?;
        Ok(changed)
    }

    /// 条件に合う未読の記事をすべて既読にする。
    pub fn mark_all_read(
        &mut self,
        channel: Channel,
        source_ids: Option<&[String]>,
        now: DateTime<Utc>,
    ) -> Result<usize> {
        let mut sql =
            "UPDATE entries SET read_at = ? WHERE channel = ? AND read_at IS NULL".to_string();
        let mut values: Vec<rusqlite::types::Value> =
            vec![now.timestamp().into(), channel.as_str().to_string().into()];
        if let Some(source_ids) = source_ids {
            if source_ids.is_empty() {
                return Ok(0);
            }
            sql.push_str(&format!(
                " AND source_id IN ({})",
                vec!["?"; source_ids.len()].join(",")
            ));
            values.extend(source_ids.iter().map(|id| id.clone().into()));
        }
        Ok(self.conn.execute(&sql, params_from_iter(values))?)
    }

    /// ソースごとの未読数。
    pub fn unread_counts(&self, channel: Channel) -> Result<HashMap<String, usize>> {
        let mut statement = self.conn.prepare(
            "SELECT source_id, COUNT(*) FROM entries WHERE channel = ?1 AND read_at IS NULL GROUP BY source_id",
        )?;
        let rows = statement.query_map([channel.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)? as usize))
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 保存期間を過ぎたお知らせを消す。
    pub fn prune(&mut self, older_than: DateTime<Utc>) -> Result<usize> {
        let tx = self.conn.transaction()?;
        let ids: Vec<i64> = {
            let mut statement =
                tx.prepare("SELECT id FROM entries WHERE channel = 'news' AND sort_at < ?1")?;
            let rows = statement.query_map([older_than.timestamp()], |row| row.get(0))?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        for id in &ids {
            delete_entry(&tx, *id)?;
        }
        tx.commit()?;
        Ok(ids.len())
    }

    /// ソースの記事と状態をすべて消す。
    pub fn delete_source(&mut self, source_id: &str) -> Result<usize> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "DELETE FROM entries_fts WHERE rowid IN (SELECT id FROM entries WHERE source_id = ?1)",
            [source_id],
        )?;
        // 読み取り結果と予定の表は、記事と一緒に消える。
        let removed = tx.execute("DELETE FROM entries WHERE source_id = ?1", [source_id])?;
        tx.execute(
            "DELETE FROM schedule_stale_sources WHERE source_id = ?1",
            [source_id],
        )?;
        tx.execute("DELETE FROM source_state WHERE source_id = ?1", [source_id])?;
        tx.commit()?;
        Ok(removed)
    }

    /// 旧形式で書き出すための全お知らせ。新しい順。
    pub fn legacy_rows(&self) -> Result<Vec<(String, Entry)>> {
        let mut statement = self
            .conn
            .prepare("SELECT id FROM entries WHERE channel = 'news' ORDER BY sort_at DESC")?;
        let ids: Vec<i64> = statement
            .query_map([], |row| row.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        let mut rows = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(mut entry) = self.entry(id)? {
                // 画像の参照はこのアプリの中でしか使えないので、最後に見た配信元の URL に戻す。
                let expanded = image_ref::expand_references(&entry.body_html, |source_id, key| {
                    fetch_url_of(&self.conn, Some(id), source_id, key)
                        .ok()
                        .flatten()
                })
                .into_owned();
                entry.body_html = expanded;
                rows.push((entry.summary.source_id.clone(), entry));
            }
        }
        Ok(rows)
    }

    /// 画像の参照 (`https://source-image.invalid/<source_id>/<key>`) を取りに行く URL。
    /// 最後に見た配信元の URL を返す。記録がなければ `None`。
    pub fn image_fetch_url(&self, source_id: &str, key: &str) -> Result<Option<String>> {
        fetch_url_of(&self.conn, None, source_id, key)
    }

    /// 画像の参照を持つ記事。取得 URL を新しく記録した順。
    pub fn image_holders(&self, source_id: &str, key: &str) -> Result<Vec<ImageHolder>> {
        let reference = image_ref::reference_url(source_id, key);
        let mut statement = self.conn.prepare_cached(
            "SELECT e.external_id, COALESCE(e.image_url = ?3, 0), i.seen_at
             FROM entry_images i JOIN entries e ON e.id = i.entry_id
             WHERE i.source_id = ?1 AND i.key = ?2
             ORDER BY i.seen_at DESC, i.entry_id DESC",
        )?;
        let rows = statement.query_map(params![source_id, key, reference], |row| {
            Ok(ImageHolder {
                external_id: row.get(0)?,
                is_image_url: row.get(1)?,
                seen_at: from_unix(row.get(2)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }
}

// ---- 予定の読み取り結果と予定の表 ----

impl Store {
    /// 予定を読み直す必要があるお知らせ (結果がない・抽出器の識別子が違う・記事の版が違う・
    /// 文脈が違う)。新しい順に `limit` 件まで返す。
    ///
    /// `contexts` はソースごとの文脈の指紋 `(ソース ID, 指紋)`。ここにないソースの記事は読まない
    /// (使っていないソース)。失敗した結果も、抽出器・版・文脈が同じなら読み直さない
    /// (同じ入力では同じように失敗するため。アプリを更新すれば抽出器の識別子が変わる)。
    pub fn pending_analyses(
        &self,
        extractor: &str,
        contexts: &[(String, String)],
        limit: usize,
    ) -> Result<Vec<AnalysisInput>> {
        if contexts.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }
        let sql = format!(
            "WITH ctx (source_id, context) AS (VALUES {})
             SELECT e.id, e.source_id, e.revision, e.title, e.body_html, e.body_text,
                e.published_at, e.published_precision, e.first_seen_at
             FROM entries e
             JOIN ctx ON ctx.source_id = e.source_id
             LEFT JOIN schedule_analysis a ON a.entry_id = e.id
             WHERE e.channel = 'news'
               AND (a.entry_id IS NULL OR a.extractor != ? OR a.revision != e.revision
                    OR a.context != ctx.context)
             ORDER BY e.sort_at DESC, e.id DESC
             LIMIT ?",
            vec!["(?, ?)"; contexts.len()].join(", ")
        );
        let mut values: Vec<rusqlite::types::Value> = Vec::new();
        for (source_id, context) in contexts {
            values.push(source_id.clone().into());
            values.push(context.clone().into());
        }
        values.push(extractor.to_string().into());
        values.push((limit as i64).into());
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values), |row| {
            Ok(AnalysisInput {
                entry_id: row.get(0)?,
                source_id: row.get(1)?,
                revision: row.get(2)?,
                title: row.get(3)?,
                body_html: row.get(4)?,
                body_text: row.get(5)?,
                published: published_from(row.get(6)?, row.get(7)?),
                first_seen_at: from_unix(row.get(8)?),
            })
        })?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// 予定の表を作り直す必要があるソースがあるか。
    pub fn has_stale_schedules(&self) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS (SELECT 1 FROM schedule_stale_sources)",
            [],
            |row| row.get(0),
        )?)
    }

    /// 読み取り結果を保存し、予定が変わりうるソース (と作り直し待ちのソース) の予定の表を作り直す。
    /// 保存と作り直しは 1 つのトランザクションで行う。
    ///
    /// 結果は記事の版を照合してから保存する。読んでいるあいだに中身が変わった記事・消えた記事の
    /// 結果は捨てる (古い本文の結果で上書きしない・消した記事の行を作らない)。記事の既読・
    /// 更新時刻・並びの時刻は変えない。
    ///
    /// `reconcile` は、同じソースの記事の読み取り結果 (古い順) を突き合わせる (`gnn_schedule::reconcile`)。
    /// 突き合わせの結果がないメンテナンスは、記事の記述のまま表に入れる。
    pub fn save_analyses(
        &mut self,
        results: &[AnalysisResult],
        reconcile: &mut dyn FnMut(&[SourceSchedule]) -> Vec<Resolution>,
    ) -> Result<SavedAnalyses> {
        let tx = self.conn.transaction()?;
        let mut report = SavedAnalyses::default();
        let mut sources: BTreeSet<String> = BTreeSet::new();
        for result in results {
            let current: Option<(String, i64, bool)> = tx
                .query_row(
                    "SELECT source_id, revision, EXISTS (SELECT 1 FROM schedule_items WHERE entry_id = ?1)
                     FROM entries WHERE id = ?1",
                    [result.entry_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()?;
            let Some((source_id, _, had_items)) =
                current.filter(|(_, revision, _)| *revision == result.revision)
            else {
                report.discarded += 1;
                continue;
            };
            let (status, json, error) = match &result.outcome {
                Ok(schedule) if schedule.is_empty() => (
                    AnalysisStatus::Nothing,
                    Some(serde_json::to_string(schedule)?),
                    None,
                ),
                Ok(schedule) => (
                    AnalysisStatus::Found,
                    Some(serde_json::to_string(schedule)?),
                    None,
                ),
                Err(message) => (AnalysisStatus::Failed, None, Some(message.as_str())),
            };
            tx.execute(
                "INSERT INTO schedule_analysis (entry_id, extractor, revision, context, status, analyzed_at, result, error)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                 ON CONFLICT (entry_id) DO UPDATE SET
                    extractor = excluded.extractor,
                    revision = excluded.revision,
                    context = excluded.context,
                    status = excluded.status,
                    analyzed_at = excluded.analyzed_at,
                    result = excluded.result,
                    error = excluded.error",
                params![
                    result.entry_id,
                    result.extractor,
                    result.revision,
                    result.context,
                    status.as_str(),
                    result.analyzed_at.timestamp(),
                    json,
                    error,
                ],
            )?;
            report.saved += 1;
            // 予定がなかった記事が、また予定なしになっただけなら表は変わらない。
            if had_items || status == AnalysisStatus::Found {
                sources.insert(source_id);
            }
        }
        {
            let mut statement = tx.prepare("SELECT source_id FROM schedule_stale_sources")?;
            let stale = statement.query_map([], |row| row.get::<_, String>(0))?;
            for source_id in stale {
                sources.insert(source_id?);
            }
        }
        for source_id in &sources {
            rebuild_source(&tx, source_id, reconcile)?;
        }
        tx.commit()?;
        report.rebuilt = sources.into_iter().collect();
        Ok(report)
    }

    /// 予定の表の行。`source_ids` (`None` はすべて) のお知らせのうち、関わる時刻の範囲が
    /// `[from, to)` と重なるもの。日時が 1 つもない行は返さない。
    pub fn agenda(
        &self,
        source_ids: Option<&[String]>,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<AgendaItem>> {
        let mut sql = "SELECT i.entry_id, i.source_id, i.kind, i.payload
             FROM schedule_items i JOIN entries e ON e.id = i.entry_id
             WHERE e.channel = 'news' AND i.span_from < ? AND i.span_to >= ?"
            .to_string();
        let mut values: Vec<rusqlite::types::Value> =
            vec![to.timestamp().into(), from.timestamp().into()];
        if let Some(source_ids) = source_ids {
            if source_ids.is_empty() {
                return Ok(Vec::new());
            }
            sql.push_str(&format!(
                " AND i.source_id IN ({})",
                vec!["?"; source_ids.len()].join(",")
            ));
            values.extend(source_ids.iter().map(|id| id.clone().into()));
        }
        sql.push_str(" ORDER BY i.entry_id, i.kind, i.item_index");
        let mut statement = self.conn.prepare(&sql)?;
        let rows = statement.query_map(params_from_iter(values), item_columns)?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter_map(parse_item)
            .collect())
    }

    /// 本文を取り直す候補。ソースのメンテナンスの行のうち関わる範囲が `[from, to)` と重なるものと、
    /// その記事の外部 ID。
    pub fn recheck_candidates(
        &self,
        source_id: &str,
        from: DateTime<Utc>,
        to: DateTime<Utc>,
    ) -> Result<Vec<(String, AgendaItem)>> {
        let mut statement = self.conn.prepare_cached(
            "SELECT i.entry_id, i.source_id, i.kind, i.payload, e.external_id
             FROM schedule_items i JOIN entries e ON e.id = i.entry_id
             WHERE i.source_id = ?1 AND i.kind = 'maintenance' AND e.channel = 'news'
               AND i.span_from < ?3 AND i.span_to >= ?2
             ORDER BY i.entry_id, i.item_index",
        )?;
        let rows = statement.query_map(
            params![source_id, from.timestamp(), to.timestamp()],
            |row| Ok((item_columns(row)?, row.get::<_, String>(4)?)),
        )?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .filter_map(|(columns, external_id)| Some((external_id, parse_item(columns)?)))
            .collect())
    }

    /// 記事の予定 (詳細の表示用)。まだ読み取っていなければ `None`。
    pub fn entry_schedule(&self, entry_id: i64) -> Result<Option<EntrySchedule>> {
        let found = self
            .conn
            .query_row(
                "SELECT a.status, a.analyzed_at, a.revision = e.revision, a.result, a.error
                 FROM schedule_analysis a JOIN entries e ON e.id = a.entry_id
                 WHERE a.entry_id = ?1",
                [entry_id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, bool>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()?;
        let Some((status, analyzed_at, current, result, error)) = found else {
            return Ok(None);
        };
        let items: Vec<AgendaItem> = {
            let mut statement = self.conn.prepare_cached(
                "SELECT entry_id, source_id, kind, payload FROM schedule_items
                 WHERE entry_id = ?1 ORDER BY kind, item_index",
            )?;
            let rows = statement.query_map([entry_id], item_columns)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
                .into_iter()
                .filter_map(parse_item)
                .collect()
        };
        let mut resolvers = HashMap::new();
        for item in &items {
            if let AgendaDetail::Maintenance(maintenance) = &item.detail
                && let Some(id) = maintenance.resolved_by
                && !resolvers.contains_key(&id)
                && let Some(title) = self
                    .conn
                    .query_row("SELECT title FROM entries WHERE id = ?1", [id], |row| {
                        row.get::<_, String>(0)
                    })
                    .optional()?
            {
                resolvers.insert(id, title);
            }
        }
        Ok(Some(EntrySchedule {
            status: AnalysisStatus::parse(&status),
            analyzed_at: from_unix(analyzed_at),
            current,
            schedule: result.and_then(|json| serde_json::from_str(&json).ok()),
            error,
            items,
            resolvers,
        }))
    }
}

/// 前の版のデータベースを今の版の形にする。表と索引を作る (`SCHEMA`) 前に、同じトランザクションで行う。
/// 列の有無を見て足し引きするので、途中の版から何度呼んでも同じ形になる。
fn migrate(tx: &Transaction<'_>) -> Result<()> {
    let columns: HashSet<String> = {
        let mut statement = tx.prepare("SELECT name FROM pragma_table_info('entries')")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    if columns.is_empty() {
        // 新しいデータベース。
        return Ok(());
    }
    if !columns.contains("legacy_id") {
        tx.execute_batch("ALTER TABLE entries ADD COLUMN legacy_id TEXT;")?;
    }
    if !columns.contains("revision") {
        tx.execute_batch("ALTER TABLE entries ADD COLUMN revision INTEGER NOT NULL DEFAULT 1;")?;
    }
    if !columns.contains("body_checked_at") {
        tx.execute_batch("ALTER TABLE entries ADD COLUMN body_checked_at INTEGER;")?;
    }
    if columns.contains("notable_dates") {
        // 予定は新しい読み取りで作り直すので、本文の日付は移さずに消す。
        tx.execute_batch("ALTER TABLE entries DROP COLUMN notable_dates;")?;
    }
    Ok(())
}

const SUMMARY_COLUMNS: &str = "id, source_id, channel, title, url, image_url, note, published_at, published_precision, first_seen_at, sort_at, read_at";
const SUMMARY_COLUMN_COUNT: usize = 12;

fn summary_from_row(row: &Row<'_>) -> rusqlite::Result<EntrySummary> {
    Ok(EntrySummary {
        id: row.get(0)?,
        source_id: row.get(1)?,
        channel: Channel::parse(&row.get::<_, String>(2)?).unwrap_or_default(),
        title: row.get(3)?,
        url: row.get(4)?,
        image_url: row.get(5)?,
        note: row.get(6)?,
        published: published_from(row.get(7)?, row.get(8)?),
        first_seen_at: from_unix(row.get(9)?),
        sort_at: from_unix(row.get(10)?),
        read: row.get::<_, Option<i64>>(11)?.is_some(),
    })
}

/// 保存した投稿日時の列 (UNIX 秒と精度) を読む。
fn published_from(at: Option<i64>, precision: Option<String>) -> Option<Published> {
    match (at, precision.as_deref().and_then(TimePrecision::parse)) {
        (Some(at), Some(precision)) => Some(Published {
            at: from_unix(at),
            precision,
        }),
        _ => None,
    }
}

enum Upsert {
    Added(i64),
    Updated,
    Unchanged,
}

struct Existing {
    id: i64,
    title: String,
    url: Option<String>,
    image_url: Option<String>,
    note: Option<String>,
    published_at: Option<i64>,
    published_precision: Option<String>,
    first_seen_at: i64,
    content_hash: String,
    list_signature: String,
}

/// 旧形式の記事を取得した記事として引き継ぐときに、投稿日時のずれを許す幅 (秒)。
const LEGACY_MATCH_WINDOW: i64 = 36 * 3600;

/// 状態の決まった記事を探す。
///
/// 取得した記事なら、見つからないときに旧形式から取り込んだ同じ記事を引き継ぐ。旧形式は
/// 元の ID を持たないため、タイトルが同じで投稿日時が近いものを同じ記事とみなし、
/// 元の ID は `legacy_id` に残す (同じ旧形式をもう一度取り込んでも重ならないように)。
/// 旧形式の記事なら、すでに取得した記事に引き継がれていないかも調べる。
fn find_existing(
    tx: &Transaction<'_>,
    source_id: &str,
    draft: &EntryDraft,
    origin: Origin,
) -> Result<Option<Existing>> {
    const COLUMNS: &str = "id, title, url, image_url, note, published_at, published_precision, first_seen_at, content_hash, list_signature";
    let select = format!("SELECT {COLUMNS} FROM entries");
    let map = |row: &Row<'_>| {
        Ok(Existing {
            id: row.get(0)?,
            title: row.get(1)?,
            url: row.get(2)?,
            image_url: row.get(3)?,
            note: row.get(4)?,
            published_at: row.get(5)?,
            published_precision: row.get(6)?,
            first_seen_at: row.get(7)?,
            content_hash: row.get(8)?,
            list_signature: row.get(9)?,
        })
    };
    let found = tx
        .query_row(
            &format!("{select} WHERE source_id = ?1 AND external_id = ?2"),
            params![source_id, draft.external_id],
            map,
        )
        .optional()?;
    if found.is_some() {
        return Ok(found);
    }
    if origin == Origin::Legacy {
        return Ok(tx
            .query_row(
                &format!("{select} WHERE source_id = ?1 AND legacy_id = ?2"),
                params![source_id, draft.external_id],
                map,
            )
            .optional()?);
    }

    let candidates: Vec<(Existing, String)> = {
        let mut statement = tx.prepare(&format!(
            "SELECT {COLUMNS}, external_id FROM entries WHERE source_id = ?1 AND origin = 'legacy' AND title = ?2"
        ))?;
        let rows = statement.query_map(params![source_id, draft.title], |row| {
            Ok((map(row)?, row.get::<_, String>(10)?))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let adopted = match draft.published {
        Some(published) => candidates
            .into_iter()
            .filter_map(|(existing, legacy_id)| {
                let distance = (existing.published_at? - published.at.timestamp()).abs();
                (distance <= LEGACY_MATCH_WINDOW).then_some((distance, existing, legacy_id))
            })
            .min_by_key(|(distance, _, _)| *distance)
            .map(|(_, existing, legacy_id)| (existing, legacy_id)),
        // 投稿日時が分からなければ、同じタイトルの旧記事が 1 件だけのときに限る。
        None if candidates.len() == 1 => candidates.into_iter().next(),
        None => None,
    };
    let Some((existing, legacy_id)) = adopted else {
        return Ok(None);
    };
    tx.execute(
        "UPDATE entries SET external_id = ?2, origin = 'fetch', legacy_id = ?3 WHERE id = ?1",
        params![existing.id, draft.external_id, legacy_id],
    )?;
    Ok(Some(existing))
}

fn upsert(
    tx: &Transaction<'_>,
    source_id: &str,
    channel: Channel,
    images: &ImageIdentity,
    draft: &EntryDraft,
    origin: Origin,
    now: DateTime<Utc>,
) -> Result<Upsert> {
    let published_at = draft.published.map(|published| published.at.timestamp());
    let published_precision = draft
        .published
        .map(|published| published.precision.as_str().to_string());
    // 本文を取ってきた時刻。旧形式から取り込んだ本文は配信元で確かめたものではない。
    let body_checked_at =
        (origin == Origin::Fetch && draft.body_html.is_some()).then(|| now.timestamp());

    // 宣言 (`url_identity.images`) に合う画像は参照にして保存し、配信元の URL は観測として残す。
    let mut observed: Vec<ImageObservation> = Vec::new();
    let image_url = draft
        .image_url
        .as_deref()
        .map(|url| match images.observe(url) {
            Some(observation) => {
                let reference = observation.reference.clone();
                observed.push(observation);
                reference
            }
            None => url.to_string(),
        });
    let body_html = draft.body_html.as_deref().map(|body| {
        let (html, found) = images.rewrite_html(body);
        observed.extend(found);
        html.into_owned()
    });
    // 本文があれば、記事が参照する画像はこの本文と画像で決まる (ほかの行は消してよい)。
    let referenced = body_html
        .as_deref()
        .map(|body| referenced_keys(source_id, body, image_url.as_deref()));

    let Some(existing) = find_existing(tx, source_id, draft, origin)? else {
        let body_html = body_html.unwrap_or_default();
        let body_text = html_to_text(&body_html);
        let sort_at = sort_time(draft.published, now).timestamp();
        tx.execute(
            "INSERT INTO entries (source_id, channel, external_id, origin, title, url, image_url, note,
                published_at, published_precision, first_seen_at, updated_at, sort_at,
                body_html, body_text, content_hash, list_signature, body_checked_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
            params![
                source_id,
                channel.as_str(),
                draft.external_id,
                origin.as_str(),
                draft.title,
                draft.url,
                image_url,
                draft.note,
                published_at,
                published_precision,
                now.timestamp(),
                sort_at,
                body_html,
                body_text,
                content_hash(&body_html),
                draft.list_signature,
                body_checked_at,
            ],
        )?;
        let id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO entries_fts (rowid, search) VALUES (?1, ?2)",
            params![
                id,
                search_text(&draft.title, &body_text, draft.note.as_deref())
            ],
        )?;
        record_images(tx, id, source_id, &observed, referenced.as_deref(), now)?;
        return Ok(Upsert::Added(id));
    };

    // 旧形式を取り込み直したときは、すでにある記事を書き換えない。取得で引き継いだあとの
    // 記事 (追記された本文や URL・画像) を、旧形式の古い内容で戻さないため。
    if origin == Origin::Legacy {
        return Ok(Upsert::Unchanged);
    }

    // 保存済みの画像と本文は、記録した取得 URL に戻してから今の決まりで直して比べる。宣言を足す前に
    // 保存した値 (配信元の URL のまま) や、前の決まりで作った参照が残っていることがあるため。
    let stored_image = existing
        .image_url
        .as_deref()
        .map(|url| current_form_of_url(tx, existing.id, url, images))
        .transpose()?;
    let image_changed = stored_image != image_url;
    let new_hash = body_html.as_deref().map(content_hash);
    let body_changed = match &new_hash {
        Some(hash) if *hash != existing.content_hash => {
            let stored: String = tx.query_row(
                "SELECT body_html FROM entries WHERE id = ?1",
                [existing.id],
                |row| row.get(0),
            )?;
            content_hash(&current_form_of_html(tx, existing.id, &stored, images)?) != *hash
        }
        _ => false,
    };
    // 今回の値に差し替える本文 (中身が変わったか、画像の参照だけが直ったもの)。
    let body_to_write = body_html
        .as_deref()
        .zip(new_hash.as_deref())
        .filter(|(_, hash)| *hash != existing.content_hash);

    let published_changed = existing.published_at != published_at
        || existing.published_precision != published_precision;
    let metadata_changed = existing.title != draft.title
        || existing.url != draft.url
        || image_changed
        || existing.note != draft.note
        || published_changed;
    if !metadata_changed && !body_changed {
        // 記事は変わっていない。取り直しの判断に使う一覧の署名と、本文を確かめた時刻を残す。
        // 画像の参照だけが直った値も差し替える。本文の文字は変わらないので、版・既読・検索は
        // そのまま。設定の指紋が変わって一覧の署名だけが変わったときも、更新には数えない。
        let image_repaired = existing.image_url != image_url;
        if existing.list_signature != draft.list_signature
            || body_checked_at.is_some()
            || image_repaired
            || body_to_write.is_some()
        {
            tx.execute(
                "UPDATE entries SET list_signature = ?2, body_checked_at = COALESCE(?3, body_checked_at),
                    image_url = CASE WHEN ?4 THEN ?5 ELSE image_url END,
                    body_html = COALESCE(?6, body_html), content_hash = COALESCE(?7, content_hash)
                 WHERE id = ?1",
                params![
                    existing.id,
                    draft.list_signature,
                    body_checked_at,
                    image_repaired,
                    image_url,
                    body_to_write.map(|(body, _)| body),
                    body_to_write.map(|(_, hash)| hash),
                ],
            )?;
        }
        record_images(
            tx,
            existing.id,
            source_id,
            &observed,
            referenced.as_deref(),
            now,
        )?;
        return Ok(Upsert::Unchanged);
    }
    // 予定の読み取りに使う中身 (題名・本文・投稿日時) が変わったら、記事の版を上げる。
    let content_changed = existing.title != draft.title || published_changed || body_changed;

    let first_seen_at = from_unix(existing.first_seen_at);
    let sort_at = sort_time(draft.published, first_seen_at).timestamp();
    tx.execute(
        "UPDATE entries SET title = ?2, url = ?3, image_url = ?4, note = ?5, published_at = ?6,
            published_precision = ?7, sort_at = ?8, list_signature = ?9, updated_at = ?10,
            revision = revision + ?11, body_checked_at = COALESCE(?12, body_checked_at),
            read_at = CASE WHEN title != ?2 THEN NULL ELSE read_at END
         WHERE id = ?1",
        params![
            existing.id,
            draft.title,
            draft.url,
            image_url,
            draft.note,
            published_at,
            published_precision,
            sort_at,
            draft.list_signature,
            now.timestamp(),
            i64::from(content_changed),
            body_checked_at,
        ],
    )?;
    if content_changed {
        invalidate_schedule(tx, existing.id)?;
    }

    let body_text = match body_to_write {
        Some((body_html, hash)) => {
            let body_text = html_to_text(body_html);
            tx.execute(
                "UPDATE entries SET body_html = ?2, body_text = ?3, content_hash = ?4 WHERE id = ?1",
                params![existing.id, body_html, body_text, hash],
            )?;
            body_text
        }
        None => tx.query_row(
            "SELECT body_text FROM entries WHERE id = ?1",
            [existing.id],
            |row| row.get(0),
        )?,
    };
    tx.execute(
        "UPDATE entries_fts SET search = ?2 WHERE rowid = ?1",
        params![
            existing.id,
            search_text(&draft.title, &body_text, draft.note.as_deref())
        ],
    )?;
    record_images(
        tx,
        existing.id,
        source_id,
        &observed,
        referenced.as_deref(),
        now,
    )?;
    Ok(Upsert::Updated)
}

/// 本文と画像が参照する画像の鍵 (このソースのもの)。
fn referenced_keys(source_id: &str, body_html: &str, image_url: Option<&str>) -> Vec<String> {
    let mut keys: Vec<String> = image_ref::references_in(body_html)
        .into_iter()
        .filter(|(source, _)| source == source_id)
        .map(|(_, key)| key)
        .collect();
    if let Some((source, key)) = image_url.and_then(image_ref::parse_reference)
        && source == source_id
        && !keys.iter().any(|known| known == key)
    {
        keys.push(key.to_string());
    }
    keys
}

/// 記事の画像の取得 URL を記録する。`referenced` (本文を取り直したときの、記事が参照する画像の鍵) が
/// あれば、そこにない画像の行を消す。
fn record_images(
    tx: &Transaction<'_>,
    entry_id: i64,
    source_id: &str,
    observed: &[ImageObservation],
    referenced: Option<&[String]>,
    now: DateTime<Utc>,
) -> Result<()> {
    if let Some(referenced) = referenced {
        let stale: Vec<String> = {
            let mut statement =
                tx.prepare_cached("SELECT key FROM entry_images WHERE entry_id = ?1")?;
            let rows = statement.query_map([entry_id], |row| row.get::<_, String>(0))?;
            rows.filter_map(|row| row.ok())
                .filter(|key| !referenced.contains(key))
                .collect()
        };
        for key in stale {
            tx.execute(
                "DELETE FROM entry_images WHERE entry_id = ?1 AND key = ?2",
                params![entry_id, key],
            )?;
        }
    }
    for observation in observed {
        let Some((source, key)) = image_ref::parse_reference(&observation.reference) else {
            continue;
        };
        if source != source_id {
            continue;
        }
        tx.execute(
            "INSERT INTO entry_images (entry_id, source_id, key, fetch_url, seen_at)
             VALUES (?1, ?2, ?3, ?4, ?5)
             ON CONFLICT (entry_id, key) DO UPDATE SET
                fetch_url = excluded.fetch_url, seen_at = excluded.seen_at",
            params![
                entry_id,
                source_id,
                key,
                observation.fetch_url,
                now.timestamp()
            ],
        )?;
    }
    Ok(())
}

/// 画像の参照の取得 URL。記事 `entry_id` の観測を先に、なければ同じソースの最新の観測を使う。
fn fetch_url_of(
    tx: &Connection,
    entry_id: Option<i64>,
    source_id: &str,
    key: &str,
) -> Result<Option<String>> {
    Ok(tx
        .prepare_cached(
            "SELECT fetch_url FROM entry_images WHERE source_id = ?1 AND key = ?2
             ORDER BY entry_id IS ?3 DESC, seen_at DESC, entry_id DESC LIMIT 1",
        )?
        .query_row(params![source_id, key, entry_id], |row| row.get(0))
        .optional()?)
}

/// 保存済みの画像の URL を、今の決まりで直した形にする。参照は記録した取得 URL に戻してから直す。
fn current_form_of_url(
    tx: &Transaction<'_>,
    entry_id: i64,
    url: &str,
    images: &ImageIdentity,
) -> Result<String> {
    let raw = match image_ref::parse_reference(url) {
        Some((source_id, key)) => fetch_url_of(tx, Some(entry_id), source_id, key)?,
        None => None,
    };
    Ok(images.identify(raw.as_deref().unwrap_or(url)).into_owned())
}

/// 保存済みの本文を、今の決まりで直した形にする。参照は記録した取得 URL に戻してから直す。
fn current_form_of_html(
    tx: &Transaction<'_>,
    entry_id: i64,
    html: &str,
    images: &ImageIdentity,
) -> Result<String> {
    let mut failure = None;
    let expanded = image_ref::expand_references(html, |source_id, key| {
        match fetch_url_of(tx, Some(entry_id), source_id, key) {
            Ok(url) => url,
            Err(err) => {
                failure.get_or_insert(err);
                None
            }
        }
    });
    if let Some(err) = failure {
        return Err(err);
    }
    Ok(images.rewrite_html(&expanded).0.into_owned())
}

/// 記事の予定の表の行を外し、そのソースを作り直し待ちにする。
///
/// 中身が変わった記事の古い読み取り結果は、読み直すまで今の予定として出さない。同じソースのほかの
/// 記事との突き合わせ (この記事の報告で決まった結果など) も作り直す。
fn invalidate_schedule(tx: &Transaction<'_>, entry_id: i64) -> Result<()> {
    tx.execute(
        "INSERT OR IGNORE INTO schedule_stale_sources (source_id)
         SELECT DISTINCT source_id FROM schedule_items WHERE entry_id = ?1",
        [entry_id],
    )?;
    tx.execute("DELETE FROM schedule_items WHERE entry_id = ?1", [entry_id])?;
    Ok(())
}

fn delete_entry(tx: &Transaction<'_>, id: i64) -> Result<()> {
    invalidate_schedule(tx, id)?;
    tx.execute("DELETE FROM entries_fts WHERE rowid = ?1", [id])?;
    // 読み取り結果は記事と一緒に消える (ON DELETE CASCADE)。
    tx.execute("DELETE FROM entries WHERE id = ?1", [id])?;
    Ok(())
}

/// 突き合わせに使う読み取り結果の行 (`id, published_at, published_precision, first_seen_at, result`)。
type FoundRow = (i64, Option<i64>, Option<String>, i64, Option<String>);

/// ソースの予定の表を、今の中身を読んだ結果 (予定ありのもの) を突き合わせて作り直す。
fn rebuild_source(
    tx: &Transaction<'_>,
    source_id: &str,
    reconcile: &mut dyn FnMut(&[SourceSchedule]) -> Vec<Resolution>,
) -> Result<()> {
    let rows: Vec<FoundRow> = {
        let mut statement = tx.prepare_cached(
            "SELECT e.id, e.published_at, e.published_precision, e.first_seen_at, a.result
             FROM schedule_analysis a JOIN entries e ON e.id = a.entry_id
             WHERE e.source_id = ?1 AND e.channel = 'news' AND a.status = 'found'
               AND a.revision = e.revision
             ORDER BY e.sort_at, e.id",
        )?;
        let rows = statement.query_map([source_id], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut articles = Vec::with_capacity(rows.len());
    for (entry_id, published_at, precision, first_seen_at, result) in rows {
        match result.as_deref().map(serde_json::from_str::<Schedule>) {
            Some(Ok(schedule)) => articles.push(SourceSchedule {
                entry_id,
                published: published_from(published_at, precision),
                first_seen_at: from_unix(first_seen_at),
                schedule,
            }),
            // 読めない結果 (型が変わったなど) は消して、読み直させる。
            other => {
                tracing::warn!(
                    entry_id,
                    "保存した予定を読めないため読み直します: {:?}",
                    other.map(|result| result.err())
                );
                tx.execute(
                    "DELETE FROM schedule_analysis WHERE entry_id = ?1",
                    [entry_id],
                )?;
            }
        }
    }
    let resolutions = if articles.is_empty() {
        Vec::new()
    } else {
        reconcile(&articles)
    };
    let items = agenda::build_items(source_id, &articles, &resolutions);
    tx.execute(
        "DELETE FROM schedule_items WHERE source_id = ?1",
        [source_id],
    )?;
    for item in &items {
        insert_item(tx, item)?;
    }
    tx.execute(
        "DELETE FROM schedule_stale_sources WHERE source_id = ?1",
        [source_id],
    )?;
    Ok(())
}

/// 予定の表の 1 行を書く。検索用の列と、行の値の JSON (`payload`) を持つ。
fn insert_item(tx: &Transaction<'_>, item: &AgendaItem) -> Result<()> {
    struct Columns<'a> {
        kind: &'static str,
        index: usize,
        scope: Option<(&'static str, Option<&'a str>)>,
        disposition: Option<&'static str>,
        resolved_by: Option<i64>,
        period_kind: Option<&'static str>,
        label: Option<&'a str>,
        window: &'a crate::schedule::Window,
        payload: String,
    }
    let columns = match &item.detail {
        AgendaDetail::Maintenance(maintenance) => Columns {
            kind: "maintenance",
            index: maintenance.index,
            scope: Some(match &maintenance.scope {
                Scope::Unknown => ("unknown", None),
                Scope::All => ("all", None),
                Scope::Named(name) => ("named", Some(name.as_str())),
            }),
            disposition: Some(disposition_str(maintenance.disposition)),
            resolved_by: maintenance.resolved_by,
            period_kind: None,
            label: None,
            window: maintenance.window(),
            payload: serde_json::to_string(maintenance)?,
        },
        AgendaDetail::Period(period) => Columns {
            kind: "period",
            index: period.index,
            scope: None,
            disposition: None,
            resolved_by: None,
            period_kind: Some(period_kind_str(period.period.kind)),
            label: period.period.label.as_deref(),
            window: &period.period.window,
            payload: serde_json::to_string(period)?,
        },
    };
    let (starts_at, start_kind) = slot_columns(&columns.window.start, false);
    let (ends_at, end_kind) = slot_columns(&columns.window.end, true);
    let span = agenda::span(&item.detail);
    tx.prepare_cached(
        "INSERT INTO schedule_items (entry_id, source_id, kind, item_index, scope_kind, scope_name,
            disposition, resolved_by, period_kind, label, starts_at, start_kind, ends_at, end_kind,
            span_from, span_to, payload)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)",
    )?
    .execute(params![
        item.entry_id,
        item.source_id,
        columns.kind,
        columns.index as i64,
        columns.scope.map(|(kind, _)| kind),
        columns.scope.and_then(|(_, name)| name),
        columns.disposition,
        columns.resolved_by,
        columns.period_kind,
        columns.label,
        starts_at,
        start_kind,
        ends_at,
        end_kind,
        span.map(|(from, _)| from.timestamp()),
        span.map(|(_, to)| to.timestamp()),
        columns.payload,
    ])?;
    Ok(())
}

/// 開始・終了の列 (UTC 秒と種類)。日付だけの値は、開始ならその日の 0 時、終了なら翌日の 0 時。
fn slot_columns(slot: &Slot<TimeValue>, end: bool) -> (Option<i64>, &'static str) {
    match Bound::of(slot) {
        Bound::At { at, .. } => (Some(at.timestamp()), "instant"),
        Bound::Day { from, to, .. } => (Some(if end { to } else { from }.timestamp()), "date"),
        Bound::Undecided => (None, "undecided"),
        Bound::Unspecified => (None, "unspecified"),
    }
}

fn disposition_str(disposition: Disposition) -> &'static str {
    match disposition {
        Disposition::Scheduled => "scheduled",
        Disposition::Completed => "completed",
        Disposition::Cancelled => "cancelled",
        Disposition::Postponed => "postponed",
        Disposition::Unresolved => "unresolved",
    }
}

fn period_kind_str(kind: PeriodKind) -> &'static str {
    match kind {
        PeriodKind::Event => "event",
        PeriodKind::Gacha => "gacha",
        PeriodKind::Campaign => "campaign",
        PeriodKind::Sale => "sale",
        PeriodKind::Claim => "claim",
        PeriodKind::Other => "other",
    }
}

type ItemColumns = (i64, String, String, String);

/// 予定の表の行の `entry_id, source_id, kind, payload`。
fn item_columns(row: &Row<'_>) -> rusqlite::Result<ItemColumns> {
    Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
}

fn parse_item((entry_id, source_id, kind, payload): ItemColumns) -> Option<AgendaItem> {
    let detail = match kind.as_str() {
        "maintenance" => {
            serde_json::from_str::<agenda::ResolvedMaintenance>(&payload).map(AgendaDetail::from)
        }
        "period" => serde_json::from_str::<agenda::AgendaPeriod>(&payload).map(AgendaDetail::from),
        _ => return None,
    };
    match detail {
        Ok(detail) => Some(AgendaItem {
            entry_id,
            source_id,
            detail,
        }),
        Err(err) => {
            tracing::warn!(entry_id, "予定の表の行を読めません: {err}");
            None
        }
    }
}

fn search_text(title: &str, body_text: &str, note: Option<&str>) -> String {
    normalize(&format!("{title}\n{}\n{body_text}", note.unwrap_or("")))
}

fn content_hash(body_html: &str) -> String {
    hex::encode(&Sha256::digest(body_html.as_bytes())[..16])
}

fn from_unix(seconds: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(seconds, 0).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agenda::ResolvedMaintenance;
    use crate::entry::list_signature;
    use crate::schedule::{Certainty, Change, Maintenance, Ongoing, Period, When, Window, Zone};
    use chrono::{Duration, TimeZone};
    use pretty_assertions::assert_eq;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 6, 3, 0, 0).unwrap()
    }

    fn draft(id: &str, title: &str, body: &str) -> EntryDraft {
        EntryDraft {
            external_id: id.into(),
            title: title.into(),
            url: Some(format!("https://example.com/{id}")),
            published: Some(Published::datetime(now() - Duration::hours(1))),
            body_html: Some(body.into()),
            image_url: None,
            note: None,
            list_signature: list_signature(&[Some(title)]),
        }
    }

    fn revision(store: &Store, id: i64) -> i64 {
        store
            .conn
            .query_row("SELECT revision FROM entries WHERE id = ?1", [id], |row| {
                row.get(0)
            })
            .unwrap()
    }

    fn column_names(store: &Store) -> Vec<String> {
        let mut statement = store
            .conn
            .prepare("SELECT name FROM pragma_table_info('entries')")
            .unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<rusqlite::Result<_>>()
            .unwrap()
    }

    /// 日本時間の日時の値。
    fn at(day: u32, hour: u32) -> Slot<TimeValue> {
        Slot::Value(TimeValue {
            when: When::Instant {
                at: chrono_tz::Asia::Tokyo
                    .with_ymd_and_hms(2026, 11, day, hour, 0, 0)
                    .unwrap()
                    .with_timezone(&Utc),
                zone: Zone::Named("Asia/Tokyo".into()),
            },
            certainty: Certainty::Explicit,
            evidence: Vec::new(),
        })
    }

    /// 11 月 `day` 日 `start`–`end` 時 (日本時間) のメンテナンスの予定。
    fn maintenance_on(day: u32, start: u32, end: u32, disposition: Disposition) -> Schedule {
        Schedule {
            maintenance: vec![Maintenance {
                scope: Scope::Unknown,
                planned: Window {
                    start: at(day, start),
                    end: at(day, end),
                },
                actual: Window::default(),
                disposition,
                ongoing: None,
                changes: Vec::new(),
                evidence: Vec::new(),
            }],
            ..Schedule::default()
        }
    }

    fn result(input: &AnalysisInput, outcome: Result<Schedule, String>) -> AnalysisResult {
        AnalysisResult {
            entry_id: input.entry_id,
            revision: input.revision,
            extractor: "ext/1".into(),
            context: "tz=Asia/Tokyo".into(),
            analyzed_at: now(),
            outcome,
        }
    }

    fn contexts(ids: &[&str]) -> Vec<(String, String)> {
        ids.iter()
            .map(|id| (id.to_string(), "tz=Asia/Tokyo".to_string()))
            .collect()
    }

    /// 突き合わせない (記事の記述のまま)。
    fn as_written(_: &[SourceSchedule]) -> Vec<Resolution> {
        Vec::new()
    }

    fn wide(store: &Store, sources: Option<&[String]>) -> Vec<AgendaItem> {
        store
            .agenda(
                sources,
                now() - Duration::days(30),
                now() + Duration::days(30),
            )
            .unwrap()
    }

    #[test]
    fn inserts_updates_and_skips_unchanged() {
        let mut store = Store::open_in_memory().unwrap();
        let report = store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[draft("1", "メンテナンスのお知らせ", "<p>本文</p>")],
                None,
                now(),
            )
            .unwrap();
        assert_eq!(report.added.len(), 1);

        let again = store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[draft("1", "メンテナンスのお知らせ", "<p>本文</p>")],
                None,
                now(),
            )
            .unwrap();
        assert_eq!(again, ApplyReport::default());

        let id = report.added[0].id;
        store.set_read(id, true, now()).unwrap();
        let changed = store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[draft("1", "メンテナンスのお知らせ", "<p>追記</p>")],
                None,
                now(),
            )
            .unwrap();
        assert_eq!(changed.updated, 1);
        let entry = store.entry(id).unwrap().unwrap();
        assert_eq!(entry.body_text, "追記");
        // 本文の追記だけなら既読のまま。タイトルが変わったら未読に戻す。
        assert!(entry.summary.read);
        store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[draft("1", "【追記】メンテナンスのお知らせ", "<p>追記</p>")],
                None,
                now(),
            )
            .unwrap();
        assert!(!store.entry(id).unwrap().unwrap().summary.read);
    }

    #[test]
    fn keeps_body_when_detail_was_skipped() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[draft("1", "告知", "<p>本文</p>")],
                None,
                now(),
            )
            .unwrap();
        let mut skipped = draft("1", "告知", "");
        skipped.body_html = None;
        store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[skipped],
                None,
                now(),
            )
            .unwrap();
        let known = store.known_items("game").unwrap();
        assert!(known["1"].has_body);
        let entries = store.query(&EntryQuery::channel(Channel::News)).unwrap();
        assert_eq!(
            store.entry(entries[0].id).unwrap().unwrap().body_html,
            "<p>本文</p>"
        );
    }

    #[test]
    fn revision_counts_content_changes_only() {
        let mut store = Store::open_in_memory().unwrap();
        let mut base = draft("1", "告知", "<p>本文</p>");
        let id = store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[base.clone()],
                None,
                now(),
            )
            .unwrap()
            .added[0]
            .id;
        assert_eq!(revision(&store, id), 1);
        assert_eq!(
            store.entry(id).unwrap().unwrap().body_checked_at,
            Some(now())
        );

        // 本文を取り直しても中身が同じなら版は変わらない。確かめた時刻だけ進む。
        let later = now() + Duration::hours(1);
        store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[base.clone()],
                None,
                later,
            )
            .unwrap();
        assert_eq!(revision(&store, id), 1);
        let entry = store.entry(id).unwrap().unwrap();
        assert_eq!(entry.body_checked_at, Some(later));
        assert_eq!(entry.updated_at, now());

        // 本文を取り直さなかった (詳細を読まなかった) だけなら変わらない。
        let mut skipped = base.clone();
        skipped.body_html = None;
        store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[skipped],
                None,
                later,
            )
            .unwrap();
        assert_eq!(revision(&store, id), 1);

        // URL・画像・補足・一覧の署名は読み取りに使わないので、版は上げない。
        base.url = Some("https://example.com/moved".into());
        base.image_url = Some("https://example.com/a.png".into());
        base.note = Some("お知らせ".into());
        base.list_signature = "changed".into();
        store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[base.clone()],
                None,
                later,
            )
            .unwrap();
        assert_eq!(revision(&store, id), 1);

        // 本文・題名・投稿日時が変わったら上げる。
        base.body_html = Some("<p>追記: 完了しました</p>".into());
        store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[base.clone()],
                None,
                later,
            )
            .unwrap();
        assert_eq!(revision(&store, id), 2);
        base.title = "【完了】告知".into();
        store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[base.clone()],
                None,
                later,
            )
            .unwrap();
        assert_eq!(revision(&store, id), 3);
        base.published = Some(Published::datetime(now()));
        store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[base.clone()],
                None,
                later,
            )
            .unwrap();
        assert_eq!(revision(&store, id), 4);

        // 旧形式から取り込んだ本文は、配信元で確かめた時刻を持たない。
        let legacy_id = {
            store
                .import_legacy(
                    &[("game".into(), draft("legacy:x", "旧", "<p>旧</p>"))],
                    now(),
                )
                .unwrap();
            store
                .query(&EntryQuery::channel(Channel::News))
                .unwrap()
                .into_iter()
                .find(|entry| entry.title == "旧")
                .unwrap()
                .id
        };
        assert_eq!(
            store.entry(legacy_id).unwrap().unwrap().body_checked_at,
            None
        );
        assert_eq!(revision(&store, legacy_id), 1);
    }

    /// `https://cdn.example.com/secure/` の画像の署名 (`s`) と期限 (`t`) を見ない決まり。`also` も見ない。
    fn signed_images(source_id: &str, also: &[&str]) -> ImageIdentity {
        let mut ignore_query = vec!["s".to_string(), "t".to_string()];
        ignore_query.extend(also.iter().map(|name| name.to_string()));
        ImageIdentity::new(
            source_id,
            &crate::source::UrlIdentity {
                images: vec![crate::source::UrlIdentityRule {
                    origin: "https://cdn.example.com".into(),
                    path_prefix: Some("/secure/".into()),
                    ignore_query,
                }],
            },
        )
    }

    /// 署名 `sig` と期限 `t` の付いた画像の URL。
    fn signed(name: &str, sig: &str, t: u32) -> String {
        format!("https://cdn.example.com/secure/img/{name}.png?s={sig}&t={t}")
    }

    /// 一覧の画像を本文の先頭にも入れ、本文にも署名付きの画像がある記事 (girls_creation の形)。
    fn signed_draft(sig: &str, t: u32) -> EntryDraft {
        let mut draft = draft("1", "イベント開催", "");
        draft.image_url = Some(signed("banner", sig, t));
        draft.body_html = Some(format!(
            r#"<p><img src="{}"></p><p>開催します。<img src="{}" width="70%"></p>"#,
            crate::text::escape_html(&signed("banner", sig, t)),
            crate::text::escape_html(&signed("detail", &format!("{sig}2"), t)),
        ));
        draft
    }

    fn image_key(reference: &str) -> String {
        image_ref::parse_reference(reference).unwrap().1.to_string()
    }

    #[test]
    fn lists_entries_holding_an_image_reference() {
        let mut store = Store::open_in_memory().unwrap();
        let images = signed_images("game", &[]);
        // 記事 1 は一覧の画像 (image_url) と本文の画像を、記事 2 は同じ本文の画像だけを持つ。
        store
            .apply_fetch(
                "game",
                Channel::News,
                &images,
                &[signed_draft("AAA", 100)],
                None,
                now(),
            )
            .unwrap();
        let second = draft(
            "2",
            "続報",
            &format!(
                r#"<p><img src="{}"></p>"#,
                crate::text::escape_html(&signed("detail", "BBB2", 200))
            ),
        );
        let later = now() + Duration::hours(1);
        store
            .apply_fetch("game", Channel::News, &images, &[second], None, later)
            .unwrap();
        let banner = image_key(&images.reference(&signed("banner", "x", 1)).unwrap());
        let detail = image_key(&images.reference(&signed("detail", "x", 1)).unwrap());
        let holder = |external_id: &str, is_image_url, seen_at| ImageHolder {
            external_id: external_id.into(),
            is_image_url,
            seen_at,
        };
        assert_eq!(
            store.image_holders("game", &banner).unwrap(),
            vec![holder("1", true, now())]
        );
        // 新しく記録した記事から並べる。
        assert_eq!(
            store.image_holders("game", &detail).unwrap(),
            vec![holder("2", false, later), holder("1", false, now())]
        );
        assert!(store.image_holders("other", &banner).unwrap().is_empty());
    }

    #[test]
    fn signed_images_are_saved_as_references_and_do_not_update_entries() {
        let mut store = Store::open_in_memory().unwrap();
        let images = signed_images("game", &[]);
        let report = store
            .apply_fetch(
                "game",
                Channel::News,
                &images,
                &[signed_draft("AAA", 100)],
                None,
                now(),
            )
            .unwrap();
        let id = report.added[0].id;
        store.set_read(id, true, now()).unwrap();
        let entry = store.entry(id).unwrap().unwrap();
        let banner = images.reference(&signed("banner", "AAA", 100)).unwrap();
        let detail = images.reference(&signed("detail", "AAA2", 100)).unwrap();
        // 本文と記事の画像は参照で持ち、配信元の URL は取得 URL として記録する。
        assert_eq!(entry.summary.image_url.as_deref(), Some(banner.as_str()));
        assert_eq!(
            entry.body_html,
            format!(
                r#"<p><img src="{banner}"></p><p>開催します。<img src="{detail}" width="70%"></p>"#
            )
        );
        assert_eq!(entry.body_text, "開催します。");
        assert_eq!(
            store.image_fetch_url("game", &image_key(&banner)).unwrap(),
            Some(signed("banner", "AAA", 100))
        );

        // 署名と期限が変わっても、記事は更新しない (版・既読・更新日時もそのまま)。取得 URL は今のものにする。
        let later = now() + Duration::hours(1);
        let again = store
            .apply_fetch(
                "game",
                Channel::News,
                &images,
                &[signed_draft("BBB", 200)],
                None,
                later,
            )
            .unwrap();
        assert_eq!(again, ApplyReport::default());
        let same = store.entry(id).unwrap().unwrap();
        assert_eq!(same.body_html, entry.body_html);
        assert_eq!(same.updated_at, now());
        assert_eq!(same.body_checked_at, Some(later));
        assert!(same.summary.read);
        assert_eq!(revision(&store, id), 1);
        assert_eq!(
            store.image_fetch_url("game", &image_key(&banner)).unwrap(),
            Some(signed("banner", "BBB", 200))
        );
        assert_eq!(
            store.image_fetch_url("game", &image_key(&detail)).unwrap(),
            Some(signed("detail", "BBB2", 200))
        );
        // ほかのソースの参照としては引けない。
        assert_eq!(
            store.image_fetch_url("other", &image_key(&banner)).unwrap(),
            None
        );

        // 詳細を取り直さなかった記事は、一覧の画像の取得 URL だけを差し替え、本文の画像の記録は残す。
        let mut skipped = signed_draft("CCC", 300);
        skipped.body_html = None;
        let report = store
            .apply_fetch("game", Channel::News, &images, &[skipped], None, later)
            .unwrap();
        assert_eq!(report, ApplyReport::default());
        assert_eq!(
            store.image_fetch_url("game", &image_key(&banner)).unwrap(),
            Some(signed("banner", "CCC", 300))
        );
        assert_eq!(
            store.image_fetch_url("game", &image_key(&detail)).unwrap(),
            Some(signed("detail", "BBB2", 200))
        );

        // 本文が変われば更新。本文から消えた画像の記録は消し、記事を消せば記録も消える。
        let mut changed = signed_draft("DDD", 400);
        changed.body_html = Some(format!(
            r#"<p><img src="{}"></p><p>開催しました。</p>"#,
            crate::text::escape_html(&signed("banner", "DDD", 400))
        ));
        let report = store
            .apply_fetch("game", Channel::News, &images, &[changed], None, later)
            .unwrap();
        assert_eq!(report.updated, 1);
        assert_eq!(revision(&store, id), 2);
        assert_eq!(
            store.image_fetch_url("game", &image_key(&detail)).unwrap(),
            None
        );
        assert_eq!(store.delete_source("game").unwrap(), 1);
        assert_eq!(
            store.image_fetch_url("game", &image_key(&banner)).unwrap(),
            None
        );
    }

    #[test]
    fn values_saved_before_the_declaration_are_rewritten_without_updates() {
        let mut store = Store::open_in_memory().unwrap();
        // 宣言を足す前の保存: 配信元の URL のまま (前の整形は本文の画像から `t` を外していた)。
        let mut before = signed_draft("AAA", 100);
        before.body_html = Some(before.body_html.unwrap().replace("&amp;t=100", ""));
        before.list_signature = "old".into();
        let id = store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[before],
                None,
                now(),
            )
            .unwrap()
            .added[0]
            .id;
        store.set_read(id, true, now()).unwrap();

        // 宣言を足して取り直すと、設定の指紋が変わって一覧の署名も変わる。それでも記事は同じ。
        let images = signed_images("game", &[]);
        let mut after = signed_draft("BBB", 200);
        after.list_signature = "new".into();
        let later = now() + Duration::hours(1);
        let report = store
            .apply_fetch(
                "game",
                Channel::News,
                &images,
                &[after.clone()],
                None,
                later,
            )
            .unwrap();
        assert_eq!(report, ApplyReport::default());
        assert_eq!(revision(&store, id), 1);
        let entry = store.entry(id).unwrap().unwrap();
        assert!(entry.summary.read);
        assert_eq!(entry.updated_at, now());
        let banner = images.reference(&signed("banner", "BBB", 200)).unwrap();
        assert_eq!(entry.summary.image_url.as_deref(), Some(banner.as_str()));
        assert!(entry.body_html.contains(&banner), "{}", entry.body_html);
        assert!(!entry.body_html.contains("cdn.example.com"));
        assert_eq!(
            store.known_items("game").unwrap()["1"].list_signature,
            "new"
        );
        assert_eq!(
            store.image_fetch_url("game", &image_key(&banner)).unwrap(),
            Some(signed("banner", "BBB", 200))
        );

        // 直した本文は次の取得と同じなので、そのあとも変わらない。
        let mut next = signed_draft("CCC", 300);
        next.list_signature = "new".into();
        let report = store
            .apply_fetch("game", Channel::News, &images, &[next], None, later)
            .unwrap();
        assert_eq!(report, ApplyReport::default());
        assert_eq!(revision(&store, id), 1);
    }

    #[test]
    fn changed_rules_compare_through_recorded_fetch_urls() {
        let mut store = Store::open_in_memory().unwrap();
        // 版の問い合わせ `w` も付いた画像。
        let with_w = |sig: &str| {
            let mut draft = signed_draft(sig, 100);
            draft.image_url = draft
                .image_url
                .map(|url| url.replace("&t=100", "&t=100&w=1"));
            draft.body_html = draft
                .body_html
                .map(|body| body.replace("&amp;t=100", "&amp;t=100&amp;w=1"));
            draft
        };
        let narrow = signed_images("game", &[]);
        let id = store
            .apply_fetch(
                "game",
                Channel::News,
                &narrow,
                &[with_w("AAA")],
                None,
                now(),
            )
            .unwrap()
            .added[0]
            .id;
        let banner_url = |sig: &str| format!("{}&w=1", signed("banner", sig, 100));
        let narrow_banner = narrow.reference(&banner_url("AAA")).unwrap();

        // 決まりを広げる (`w` も見ない) と参照の鍵が変わる。保存済みの参照を記録した取得 URL に
        // 戻してから今の決まりで直して比べるので、記事は同じまま。
        let wide = signed_images("game", &["w"]);
        let report = store
            .apply_fetch("game", Channel::News, &wide, &[with_w("BBB")], None, now())
            .unwrap();
        assert_eq!(report, ApplyReport::default());
        assert_eq!(revision(&store, id), 1);
        let wide_banner = wide.reference(&banner_url("BBB")).unwrap();
        assert_ne!(wide_banner, narrow_banner);
        let entry = store.entry(id).unwrap().unwrap();
        assert_eq!(
            entry.summary.image_url.as_deref(),
            Some(wide_banner.as_str())
        );
        assert!(entry.body_html.contains(&wide_banner));
        // 使わなくなった参照の記録は消える。
        assert_eq!(
            store
                .image_fetch_url("game", &image_key(&narrow_banner))
                .unwrap(),
            None
        );

        // 決まりをなくしても、同じ署名なら同じ本文 (配信元の URL に戻すだけ)。
        let none = ImageIdentity::default();
        let report = store
            .apply_fetch("game", Channel::News, &none, &[with_w("BBB")], None, now())
            .unwrap();
        assert_eq!(report, ApplyReport::default());
        let entry = store.entry(id).unwrap().unwrap();
        assert!(entry.body_html.contains("s=BBB"), "{}", entry.body_html);
        // 決まりがなければ、署名の違いは本文の違い。
        let report = store
            .apply_fetch("game", Channel::News, &none, &[with_w("CCC")], None, now())
            .unwrap();
        assert_eq!(report.updated, 1);
        assert_eq!(revision(&store, id), 2);
    }

    #[test]
    fn list_signature_changes_alone_are_not_updates() {
        let mut store = Store::open_in_memory().unwrap();
        let none = ImageIdentity::default();
        let mut base = draft("1", "告知", "<p>本文</p>");
        store
            .apply_fetch("game", Channel::News, &none, &[base.clone()], None, now())
            .unwrap();
        // 設定の指紋が変わって取り直しても、記事が同じなら更新ではない。署名は次の判断のために残す。
        base.list_signature = "changed".into();
        let report = store
            .apply_fetch("game", Channel::News, &none, &[base.clone()], None, now())
            .unwrap();
        assert_eq!(report, ApplyReport::default());
        assert_eq!(
            store.known_items("game").unwrap()["1"].list_signature,
            "changed"
        );
        base.list_signature = "again".into();
        base.body_html = None;
        let report = store
            .apply_fetch("game", Channel::News, &none, &[base], None, now())
            .unwrap();
        assert_eq!(report, ApplyReport::default());
        assert_eq!(
            store.known_items("game").unwrap()["1"].list_signature,
            "again"
        );
    }

    #[test]
    fn legacy_rows_expand_image_references() {
        let mut store = Store::open_in_memory().unwrap();
        let images = signed_images("game", &[]);
        store
            .apply_fetch(
                "game",
                Channel::News,
                &images,
                &[signed_draft("AAA", 100)],
                None,
                now(),
            )
            .unwrap();
        let rows = store.legacy_rows().unwrap();
        let body = &rows[0].1.body_html;
        // アプリの外 (旧形式を読む別のアプリ) では参照を使えないので、配信元の URL に戻す。
        assert!(
            body.contains(&crate::text::escape_html(&signed("banner", "AAA", 100))),
            "{body}"
        );
        assert!(!body.contains(image_ref::REFERENCE_HOST), "{body}");
    }

    #[test]
    fn pending_analyses_track_extractor_revision_and_context() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[
                    draft("1", "メンテナンス", "<p>11/12</p>"),
                    draft("2", "ガチャ", ""),
                ],
                None,
                now(),
            )
            .unwrap();
        store
            .apply_fetch(
                "other",
                Channel::News,
                &ImageIdentity::default(),
                &[draft("3", "告知", "")],
                None,
                now(),
            )
            .unwrap();
        // 使っていないソースは読まない。
        let pending = store
            .pending_analyses("ext/1", &contexts(&["game"]), 10)
            .unwrap();
        let titles: Vec<&str> = pending.iter().map(|input| input.title.as_str()).collect();
        assert_eq!(titles.len(), 2);
        assert!(titles.contains(&"メンテナンス") && titles.contains(&"ガチャ"));
        assert_eq!(
            pending[0].body_text.is_empty(),
            pending[0].title == "ガチャ"
        );
        assert_eq!(
            store
                .pending_analyses("ext/1", &contexts(&["game"]), 1)
                .unwrap()
                .len(),
            1
        );
        assert!(store.pending_analyses("ext/1", &[], 10).unwrap().is_empty());

        // 予定あり・該当なし・失敗のどれも、同じ条件なら読み直さない。
        let results: Vec<AnalysisResult> = pending
            .iter()
            .map(|input| {
                if input.title == "メンテナンス" {
                    result(
                        input,
                        Ok(maintenance_on(12, 14, 17, Disposition::Scheduled)),
                    )
                } else {
                    result(input, Err("panic".into()))
                }
            })
            .collect();
        let saved = store.save_analyses(&results, &mut as_written).unwrap();
        assert_eq!((saved.saved, saved.discarded), (2, 0));
        assert_eq!(saved.rebuilt, vec!["game".to_string()]);
        assert!(
            store
                .pending_analyses("ext/1", &contexts(&["game"]), 10)
                .unwrap()
                .is_empty()
        );

        // 抽出器が変わった・文脈が変わったら読み直す。
        assert_eq!(
            store
                .pending_analyses("ext/2", &contexts(&["game"]), 10)
                .unwrap()
                .len(),
            2
        );
        let shanghai = vec![("game".to_string(), "tz=Asia/Shanghai".to_string())];
        assert_eq!(
            store
                .pending_analyses("ext/1", &shanghai, 10)
                .unwrap()
                .len(),
            2
        );

        // 中身が変わった記事だけ読み直す。
        store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[draft("2", "ガチャ", "<p>ピックアップ追加</p>")],
                None,
                now(),
            )
            .unwrap();
        let pending = store
            .pending_analyses("ext/1", &contexts(&["game"]), 10)
            .unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(
            (pending[0].title.as_str(), pending[0].revision),
            ("ガチャ", 2)
        );
    }

    #[test]
    fn saving_checks_revision_and_keeps_read_state() {
        let mut store = Store::open_in_memory().unwrap();
        let id = store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[draft("1", "メンテナンス", "<p>予告</p>")],
                None,
                now(),
            )
            .unwrap()
            .added[0]
            .id;
        store.set_read(id, true, now()).unwrap();
        let input = store
            .pending_analyses("ext/1", &contexts(&["game"]), 10)
            .unwrap()
            .remove(0);
        let before = store.entry(id).unwrap().unwrap();

        // 読んでいるあいだに本文が変わった。古い本文の結果は捨てる。
        store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[draft("1", "メンテナンス", "<p>追記: 延長します</p>")],
                None,
                now() + Duration::minutes(1),
            )
            .unwrap();
        let schedule = maintenance_on(12, 14, 17, Disposition::Scheduled);
        let saved = store
            .save_analyses(&[result(&input, Ok(schedule.clone()))], &mut as_written)
            .unwrap();
        assert_eq!((saved.saved, saved.discarded), (0, 1));
        assert_eq!(store.entry_schedule(id).unwrap(), None);
        let input = store
            .pending_analyses("ext/1", &contexts(&["game"]), 10)
            .unwrap()
            .remove(0);
        assert_eq!(input.revision, 2);
        assert!(input.body_text.contains("延長"));

        // 今の版の結果は保存する。既読・更新時刻・並びの時刻は変えない。
        let changed = store.entry(id).unwrap().unwrap();
        let saved = store
            .save_analyses(&[result(&input, Ok(schedule))], &mut as_written)
            .unwrap();
        assert_eq!(saved.saved, 1);
        let after = store.entry(id).unwrap().unwrap();
        assert!(after.summary.read && before.summary.read);
        assert_eq!(after.updated_at, changed.updated_at);
        assert_eq!(after.summary.sort_at, before.summary.sort_at);
        assert_eq!(after.summary.title, before.summary.title);
        let stored = store.entry_schedule(id).unwrap().unwrap();
        assert_eq!(stored.status, AnalysisStatus::Found);
        assert!(stored.current);
        assert_eq!(stored.items.len(), 1);

        // 解析中に記事が消えた。結果を保存せず、行も作らない。
        let gone_input = AnalysisInput { ..input.clone() };
        store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[],
                Some(&[]),
                now(),
            )
            .unwrap();
        let saved = store
            .save_analyses(
                &[result(
                    &gone_input,
                    Ok(maintenance_on(12, 14, 17, Disposition::Scheduled)),
                )],
                &mut as_written,
            )
            .unwrap();
        assert_eq!((saved.saved, saved.discarded), (0, 1));
        let count: i64 = store
            .conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM schedule_analysis) + (SELECT COUNT(*) FROM schedule_items)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn rebuilds_the_agenda_from_reconciled_sources() {
        let mut store = Store::open_in_memory().unwrap();
        let report = store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[
                    draft(
                        "notice",
                        "メンテナンスのお知らせ",
                        "<p>11/12 14:00～17:00</p>",
                    ),
                    draft("done", "メンテナンス終了のお知らせ", "<p>終了しました</p>"),
                    draft("news", "新キャラ", "<p>登場</p>"),
                ],
                None,
                now(),
            )
            .unwrap();
        let ids: HashMap<String, i64> = report
            .added
            .iter()
            .map(|entry| (entry.title.clone(), entry.id))
            .collect();
        let notice = ids["メンテナンスのお知らせ"];
        let done = ids["メンテナンス終了のお知らせ"];
        let inputs = store
            .pending_analyses("ext/1", &contexts(&["game"]), 10)
            .unwrap();
        let results: Vec<AnalysisResult> = inputs
            .iter()
            .map(|input| {
                let schedule = if input.entry_id == notice {
                    maintenance_on(12, 14, 17, Disposition::Scheduled)
                } else if input.entry_id == done {
                    maintenance_on(12, 14, 17, Disposition::Completed)
                } else {
                    Schedule::default()
                };
                result(input, Ok(schedule))
            })
            .collect();
        // 完了の報告がある記事があれば、予告の記事のメンテナンスも終わったことにする。
        let mut reconcile = |articles: &[SourceSchedule]| -> Vec<Resolution> {
            let reporter = articles.iter().find(|article| {
                article
                    .schedule
                    .maintenance
                    .iter()
                    .any(|m| m.disposition == Disposition::Completed)
            });
            articles
                .iter()
                .filter(|article| article.entry_id != reporter.map_or(0, |r| r.entry_id))
                .flat_map(|article| {
                    article
                        .schedule
                        .maintenance
                        .iter()
                        .enumerate()
                        .map(move |(index, m)| Resolution {
                            entry_id: article.entry_id,
                            index,
                            disposition: if reporter.is_some() {
                                Disposition::Completed
                            } else {
                                m.disposition
                            },
                            planned: m.planned.clone(),
                            actual: m.actual.clone(),
                            changes: m.changes.clone(),
                            ongoing: m.ongoing.clone(),
                            resolved_by: reporter.map(|r| r.entry_id),
                        })
                })
                .collect()
        };
        let saved = store.save_analyses(&results, &mut reconcile).unwrap();
        assert_eq!(saved.saved, 3);
        assert_eq!(saved.rebuilt, vec!["game".to_string()]);

        let items = wide(&store, Some(&["game".to_string()]));
        assert_eq!(items.len(), 2);
        let resolved = |items: &[AgendaItem], id: i64| -> ResolvedMaintenance {
            match &items
                .iter()
                .find(|item| item.entry_id == id)
                .unwrap()
                .detail
            {
                AgendaDetail::Maintenance(m) => m.as_ref().clone(),
                AgendaDetail::Period(_) => panic!("メンテナンス"),
            }
        };
        assert_eq!(resolved(&items, notice).disposition, Disposition::Completed);
        assert_eq!(resolved(&items, notice).resolved_by, Some(done));
        let schedule = store.entry_schedule(notice).unwrap().unwrap();
        assert_eq!(
            schedule.resolvers.get(&done).map(String::as_str),
            Some("メンテナンス終了のお知らせ")
        );
        let news = store.entry_schedule(ids["新キャラ"]).unwrap().unwrap();
        assert_eq!(news.status, AnalysisStatus::Nothing);
        assert!(news.items.is_empty());

        // 範囲の外・ほかのソースの行は返さない。
        assert!(
            store
                .agenda(None, now() + Duration::days(10), now() + Duration::days(20))
                .unwrap()
                .is_empty()
        );
        assert!(wide(&store, Some(&["other".to_string()])).is_empty());
        assert_eq!(wide(&store, None).len(), 2);

        // 本文を取り直す候補は、メンテナンスの行と外部 ID。
        let candidates = store
            .recheck_candidates("game", now(), now() + Duration::days(10))
            .unwrap();
        let mut external: Vec<&str> = candidates.iter().map(|(id, _)| id.as_str()).collect();
        external.sort();
        assert_eq!(external, vec!["done", "notice"]);

        // 完了を報告した記事が変わった。読み直すまで、その記事の予定は出さず、突き合わせも作り直す。
        store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[draft(
                    "done",
                    "メンテナンス終了のお知らせ",
                    "<p>終了しました (追記)</p>",
                )],
                None,
                now(),
            )
            .unwrap();
        assert!(store.has_stale_schedules().unwrap());
        assert_eq!(wide(&store, None).len(), 1);
        let saved = store.save_analyses(&[], &mut reconcile).unwrap();
        assert_eq!(saved.rebuilt, vec!["game".to_string()]);
        assert!(!store.has_stale_schedules().unwrap());
        let items = wide(&store, None);
        assert_eq!(items.len(), 1);
        assert_eq!(resolved(&items, notice).disposition, Disposition::Scheduled);
        assert_eq!(resolved(&items, notice).resolved_by, None);

        // 予定のない記事が、また予定なしになっただけなら作り直さない。
        let input = store
            .pending_analyses("ext/1", &contexts(&["game"]), 10)
            .unwrap()
            .remove(0);
        assert_eq!(input.entry_id, done);
        let saved = store
            .save_analyses(&[result(&input, Ok(Schedule::default()))], &mut reconcile)
            .unwrap();
        assert!(saved.rebuilt.is_empty());

        // 記事を消したソースは作り直し待ちになる。
        store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[],
                Some(&["done".to_string(), "news".to_string()]),
                now(),
            )
            .unwrap();
        assert!(store.has_stale_schedules().unwrap());
        assert!(wide(&store, None).is_empty());
        store.save_analyses(&[], &mut reconcile).unwrap();
        assert!(!store.has_stale_schedules().unwrap());

        // ソースを消せば、読み取り結果も予定の表も消える。
        assert_eq!(store.delete_source("game").unwrap(), 2);
        let count: i64 = store
            .conn
            .query_row(
                "SELECT (SELECT COUNT(*) FROM schedule_analysis) + (SELECT COUNT(*) FROM schedule_items)
                    + (SELECT COUNT(*) FROM schedule_stale_sources)",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn agenda_rows_keep_periods_and_search_columns() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[draft("1", "イベント", "")],
                None,
                now(),
            )
            .unwrap();
        let input = store
            .pending_analyses("ext/1", &contexts(&["game"]), 10)
            .unwrap()
            .remove(0);
        let mut schedule = maintenance_on(12, 14, 17, Disposition::Scheduled);
        schedule.maintenance[0].scope = Scope::Named("PC版".into());
        schedule.periods.push(Period {
            kind: PeriodKind::Event,
            label: Some("ハロウィン".into()),
            window: Window {
                start: at(10, 15),
                end: at(20, 13),
            },
            changes: Vec::new(),
            evidence: Vec::new(),
        });
        store
            .save_analyses(&[result(&input, Ok(schedule))], &mut as_written)
            .unwrap();
        // 検索用の列。`-` は値なし。
        let rows: Vec<String> = {
            let mut statement = store
                .conn
                .prepare(
                    "SELECT kind || ' ' || IFNULL(scope_kind, '-') || ' ' || IFNULL(scope_name, '-') || ' '
                        || IFNULL(disposition, '-') || ' ' || IFNULL(period_kind, '-') || ' '
                        || IFNULL(label, '-') || ' ' || start_kind || ' ' || end_kind
                     FROM schedule_items ORDER BY kind",
                )
                .unwrap();
            statement
                .query_map([], |row| row.get(0))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(
            rows,
            vec![
                "maintenance named PC版 scheduled - - instant instant",
                "period - - - event ハロウィン instant instant",
            ]
        );
        let items = wide(&store, None);
        assert!(items.iter().any(|item| matches!(
            &item.detail,
            AgendaDetail::Period(row) if row.period.label.as_deref() == Some("ハロウィン")
        )));
    }

    #[test]
    fn reads_agenda_payloads_written_before_new_fields() {
        // 実施中の報告・延長する前の終わり・日程変更の後・期間の変更が入る前の形でも、既定の値で読める。
        let maintenance = r#"{"index":0,"scope":{"kind":"named","name":"PC版"},"disposition":"scheduled",
            "planned":{"start":{"kind":"value","value":{"when":{"kind":"instant","at":"2026-11-12T05:00:00Z",
            "zone":{"kind":"named","value":"Asia/Tokyo"}},"certainty":"explicit"}},"end":{"kind":"undecided"}},
            "actual":{},"changes":[{"kind":"extended","end":{"kind":"undecided"}},{"kind":"rescheduled","from":{}}]}"#;
        let item = parse_item((1, "game".into(), "maintenance".into(), maintenance.into()))
            .expect("前の形のメンテナンス");
        let AgendaDetail::Maintenance(read) = &item.detail else {
            panic!("メンテナンス")
        };
        assert_eq!(read.ongoing, None);
        assert_eq!(
            read.changes,
            vec![
                Change::Extended {
                    from: Slot::Unspecified,
                    end: Slot::Undecided,
                    evidence: Vec::new(),
                },
                Change::Rescheduled {
                    from: Window::default(),
                    to: Window::default(),
                    evidence: Vec::new(),
                },
            ]
        );
        let period = r#"{"index":2,"period":{"kind":"event","window":{"start":{"kind":"unspecified"},"end":{"kind":"unspecified"}}}}"#;
        let item =
            parse_item((1, "game".into(), "period".into(), period.into())).expect("前の形の期間");
        let AgendaDetail::Period(read) = &item.detail else {
            panic!("期間")
        };
        assert_eq!(read.index, 2);
        assert!(read.period.changes.is_empty());
    }

    #[test]
    fn stores_ongoing_reports_and_changes() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[draft("1", "緊急メンテナンス", "<p>実施中です</p>")],
                None,
                now(),
            )
            .unwrap();
        let input = store
            .pending_analyses("ext/1", &contexts(&["game"]), 10)
            .unwrap()
            .remove(0);
        // 11/6 11:00 (日本時間) の時点で実施中。開始の時刻は書かれていない。
        let Slot::Value(as_of) = at(6, 11) else {
            unreachable!()
        };
        let schedule = Schedule {
            maintenance: vec![Maintenance {
                scope: Scope::Named("PC版".into()),
                planned: Window {
                    start: Slot::Unspecified,
                    end: Slot::Undecided,
                },
                actual: Window::default(),
                disposition: Disposition::Scheduled,
                ongoing: Some(Ongoing {
                    as_of,
                    evidence: Vec::new(),
                }),
                changes: vec![Change::Extended {
                    from: at(6, 10),
                    end: Slot::Undecided,
                    evidence: Vec::new(),
                }],
                evidence: Vec::new(),
            }],
            periods: vec![Period {
                kind: PeriodKind::Event,
                label: None,
                window: Window {
                    start: at(1, 15),
                    end: at(20, 13),
                },
                changes: vec![Change::Rescheduled {
                    from: Window {
                        start: at(1, 15),
                        end: at(13, 13),
                    },
                    to: Window {
                        start: at(1, 15),
                        end: at(20, 13),
                    },
                    evidence: Vec::new(),
                }],
                evidence: Vec::new(),
            }],
            notes: Vec::new(),
        };
        store
            .save_analyses(&[result(&input, Ok(schedule.clone()))], &mut as_written)
            .unwrap();
        let expected = agenda::build_items(
            "game",
            &[SourceSchedule {
                entry_id: input.entry_id,
                published: None,
                first_seen_at: now(),
                schedule,
            }],
            &[],
        );
        let stored = store.entry_schedule(input.entry_id).unwrap().unwrap();
        assert_eq!(stored.items, expected);
        // 開始が書かれていなくても、実施中の報告の時点から表を引く範囲に入る。
        let near = store
            .agenda(None, now() - Duration::hours(2), now())
            .unwrap();
        assert_eq!(near.len(), 2);
    }

    #[test]
    fn searches_with_trigram_and_short_queries() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[
                    draft(
                        "1",
                        "定期メンテナンスのお知らせ",
                        "<p>ガチャを更新します</p>",
                    ),
                    draft("2", "イベント開催", "<p>ＳＳＲ確定チケット</p>"),
                ],
                None,
                now(),
            )
            .unwrap();
        let search = |text: &str| -> Vec<String> {
            let mut query = EntryQuery::channel(Channel::News);
            query.search = Some(text.into());
            store
                .query(&query)
                .unwrap()
                .into_iter()
                .map(|entry| entry.title)
                .collect()
        };
        assert_eq!(search("メンテナンス"), vec!["定期メンテナンスのお知らせ"]);
        assert_eq!(search("ssr"), vec!["イベント開催"]);
        assert_eq!(search("ｶﾞﾁｬ"), vec!["定期メンテナンスのお知らせ"]);
        assert_eq!(search("開"), vec!["イベント開催"]);
        assert_eq!(search("\"quote"), Vec::<String>::new());
        assert_eq!(search("  ").len(), 2);
    }

    #[test]
    fn queries_by_ids_with_the_same_filters() {
        let mut store = Store::open_in_memory().unwrap();
        let report = store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[draft("1", "メンテナンス", ""), draft("2", "ガチャ", "")],
                None,
                now(),
            )
            .unwrap();
        let first = report.added[0].id;
        let second = report.added[1].id;
        store.set_read(second, true, now()).unwrap();
        let mut query = EntryQuery::channel(Channel::News);
        query.ids = Some(vec![first, second]);
        assert_eq!(store.query(&query).unwrap().len(), 2);
        query.unread_only = true;
        let titles: Vec<String> = store
            .query(&query)
            .unwrap()
            .into_iter()
            .map(|entry| entry.title)
            .collect();
        assert_eq!(titles, vec!["メンテナンス"]);
        query.ids = Some(Vec::new());
        assert!(store.query(&query).unwrap().is_empty());
    }

    #[test]
    fn snapshot_removes_missing_entries() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .apply_fetch(
                "new",
                Channel::News,
                &ImageIdentity::default(),
                &[draft("a", "A", ""), draft("b", "B", "")],
                Some(&[]),
                now(),
            )
            .unwrap();
        let report = store
            .apply_fetch(
                "new",
                Channel::News,
                &ImageIdentity::default(),
                &[draft("b", "B", "")],
                Some(&[]),
                now(),
            )
            .unwrap();
        assert_eq!(report.removed, 1);
        let titles: Vec<String> = store
            .query(&EntryQuery::channel(Channel::News))
            .unwrap()
            .into_iter()
            .map(|entry| entry.title)
            .collect();
        assert_eq!(titles, vec!["B"]);
    }

    #[test]
    fn adopts_legacy_entries_by_title() {
        let mut store = Store::open_in_memory().unwrap();
        let mut legacy = draft("legacy:x", "告知", "<p>旧</p>");
        legacy.url = None;
        assert_eq!(
            store
                .import_legacy(&[("game".into(), legacy.clone())], now())
                .unwrap(),
            1
        );
        assert_eq!(
            store
                .import_legacy(&[("game".into(), legacy)], now())
                .unwrap(),
            0
        );

        let report = store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[draft("42", "告知", "<p>新</p>")],
                None,
                now(),
            )
            .unwrap();
        assert!(report.added.is_empty());
        assert_eq!(report.updated, 1);
        assert!(store.known_items("game").unwrap().contains_key("42"));

        // 引き継いだあとに同じ旧形式をもう一度取り込んでも、重ならない。
        let mut again = draft("legacy:x", "告知", "<p>旧</p>");
        again.url = None;
        assert_eq!(
            store
                .import_legacy(&[("game".into(), again)], now())
                .unwrap(),
            0
        );
        assert_eq!(
            store
                .query(&EntryQuery::channel(Channel::News))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn does_not_adopt_legacy_entries_from_another_day() {
        let mut store = Store::open_in_memory().unwrap();
        let mut legacy = draft("legacy:old", "定期メンテナンスのお知らせ", "<p>11/1</p>");
        legacy.published = Some(Published::datetime(now() - Duration::days(5)));
        store
            .import_legacy(&[("game".into(), legacy)], now())
            .unwrap();

        let report = store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[draft("new", "定期メンテナンスのお知らせ", "<p>11/6</p>")],
                None,
                now(),
            )
            .unwrap();
        assert_eq!(report.added.len(), 1);
        assert_eq!(
            store
                .query(&EntryQuery::channel(Channel::News))
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn snapshot_keeps_listed_entries_that_were_not_read() {
        let mut store = Store::open_in_memory().unwrap();
        store
            .apply_fetch(
                "new",
                Channel::News,
                &ImageIdentity::default(),
                &[
                    draft("a", "A", ""),
                    draft("b", "B", ""),
                    draft("c", "C", ""),
                ],
                Some(&[]),
                now(),
            )
            .unwrap();
        // 上限で "a" だけを読んだが、一覧には "b" もあった。一覧から消えた "c" だけを消す。
        let keep = vec!["a".to_string(), "b".to_string()];
        let report = store
            .apply_fetch(
                "new",
                Channel::News,
                &ImageIdentity::default(),
                &[draft("a", "A", "")],
                Some(&keep),
                now(),
            )
            .unwrap();
        assert_eq!(report.removed, 1);
        assert_eq!(
            store
                .query(&EntryQuery::channel(Channel::News))
                .unwrap()
                .len(),
            2
        );
    }

    #[test]
    fn marks_only_given_entries_read() {
        let mut store = Store::open_in_memory().unwrap();
        let report = store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[draft("1", "メンテナンス", ""), draft("2", "ガチャ", "")],
                None,
                now(),
            )
            .unwrap();
        let first = report.added[0].id;
        assert_eq!(store.mark_read_ids(&[first], now()).unwrap(), 1);
        assert_eq!(store.mark_read_ids(&[first], now()).unwrap(), 0);
        assert_eq!(store.unread_counts(Channel::News).unwrap()["game"], 1);
    }

    #[test]
    fn records_runs_and_tracks_failures() {
        let mut store = Store::open_in_memory().unwrap();
        let run = |status| FetchRun {
            source_id: "game".into(),
            started_at: now(),
            finished_at: now(),
            status,
            listed: 3,
            added: 1,
            updated: 0,
            message: None,
        };
        store.record_run(&run(RunStatus::Failed)).unwrap();
        store.record_run(&run(RunStatus::Failed)).unwrap();
        let state = &store.source_states().unwrap()["game"];
        assert_eq!(state.consecutive_failures, 2);
        assert_eq!(state.last_success_at, None);

        store.record_run(&run(RunStatus::Ok)).unwrap();
        let state = &store.source_states().unwrap()["game"];
        assert_eq!(state.consecutive_failures, 0);
        assert_eq!(state.last_success_at, Some(now()));
        assert_eq!(store.recent_runs(10).unwrap().len(), 3);
    }

    #[test]
    fn marks_read_counts_unread_and_prunes() {
        let mut store = Store::open_in_memory().unwrap();
        let mut old = draft("old", "古い", "");
        old.published = Some(Published::datetime(now() - Duration::days(400)));
        store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[draft("1", "A", ""), draft("2", "B", ""), old],
                None,
                now(),
            )
            .unwrap();
        assert_eq!(store.unread_counts(Channel::News).unwrap()["game"], 3);
        assert_eq!(
            store
                .mark_all_read(Channel::News, Some(&["game".into()]), now())
                .unwrap(),
            3
        );
        assert!(store.unread_counts(Channel::News).unwrap().is_empty());

        assert_eq!(store.prune(now() - Duration::days(365)).unwrap(), 1);
        assert_eq!(
            store
                .query(&EntryQuery::channel(Channel::News))
                .unwrap()
                .len(),
            2
        );
        assert_eq!(store.delete_source("game").unwrap(), 2);
    }

    /// 版 2 (この変更の前) の表。
    const V2_SCHEMA: &str = r#"
CREATE TABLE entries (
    id INTEGER PRIMARY KEY,
    source_id TEXT NOT NULL,
    channel TEXT NOT NULL,
    external_id TEXT NOT NULL,
    origin TEXT NOT NULL DEFAULT 'fetch',
    title TEXT NOT NULL,
    url TEXT,
    image_url TEXT,
    note TEXT,
    published_at INTEGER,
    published_precision TEXT,
    first_seen_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    sort_at INTEGER NOT NULL,
    body_html TEXT NOT NULL DEFAULT '',
    body_text TEXT NOT NULL DEFAULT '',
    content_hash TEXT NOT NULL DEFAULT '',
    list_signature TEXT NOT NULL DEFAULT '',
    notable_dates TEXT NOT NULL DEFAULT '',
    read_at INTEGER,
    legacy_id TEXT,
    UNIQUE (source_id, external_id)
);
CREATE INDEX entries_channel_sort ON entries (channel, sort_at DESC);
CREATE INDEX entries_source_sort ON entries (source_id, sort_at DESC);
CREATE INDEX entries_legacy ON entries (source_id, legacy_id);
CREATE VIRTUAL TABLE entries_fts USING fts5 (search, tokenize = 'trigram');
CREATE TABLE source_state (
    source_id TEXT PRIMARY KEY,
    last_attempt_at INTEGER,
    last_success_at INTEGER,
    last_status TEXT,
    last_message TEXT,
    consecutive_failures INTEGER NOT NULL DEFAULT 0
);
CREATE TABLE fetch_log (
    id INTEGER PRIMARY KEY,
    source_id TEXT NOT NULL,
    started_at INTEGER NOT NULL,
    finished_at INTEGER NOT NULL,
    status TEXT NOT NULL,
    listed INTEGER NOT NULL,
    added INTEGER NOT NULL,
    updated INTEGER NOT NULL,
    message TEXT
);
"#;

    /// 版 2 のデータベースを作る。`legacy_id` の列を消せば版 1 の形になる。
    fn old_database(path: &Path, version: i32) {
        let conn = Connection::open(path).unwrap();
        let schema = if version == 1 {
            V2_SCHEMA.replace("    legacy_id TEXT,\n", "").replace(
                "CREATE INDEX entries_legacy ON entries (source_id, legacy_id);\n",
                "",
            )
        } else {
            V2_SCHEMA.to_string()
        };
        conn.execute_batch(&schema).unwrap();
        conn.execute(
            "INSERT INTO entries (id, source_id, channel, external_id, title, url, published_at, published_precision,
                first_seen_at, updated_at, sort_at, body_html, body_text, content_hash, list_signature, notable_dates, read_at)
             VALUES (1, 'game', 'news', 'n1', 'メンテナンスのお知らせ', 'https://example.com/n1', ?1, 'datetime',
                ?1, ?1, ?1, '<p>11/12 メンテ</p>', '11/12 メンテ', ?2, 'sig', '2026-11-12', ?1)",
            params![now().timestamp(), content_hash("<p>11/12 メンテ</p>")],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO entries_fts (rowid, search) VALUES (1, 'メンテナンスのお知らせ 11/12 メンテ')",
            [],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO source_state (source_id, last_attempt_at, consecutive_failures) VALUES ('game', ?1, 2)",
            [now().timestamp()],
        )
        .unwrap();
        conn.pragma_update(None, "user_version", version).unwrap();
    }

    #[test]
    fn migrates_databases_of_earlier_versions() {
        for version in [1, 2] {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("gnn.db");
            old_database(&path, version);

            let mut store = Store::open(&path).unwrap();
            let columns = column_names(&store);
            assert!(
                !columns.contains(&"notable_dates".to_string()),
                "{columns:?}"
            );
            for column in ["legacy_id", "revision", "body_checked_at"] {
                assert!(columns.contains(&column.to_string()), "{version}: {column}");
            }
            let version_now: i32 = store
                .conn
                .pragma_query_value(None, "user_version", |row| row.get(0))
                .unwrap();
            assert_eq!(version_now, DATABASE_VERSION);

            // 記事・既読・検索・取得状況はそのまま。
            let entries = store.query(&EntryQuery::channel(Channel::News)).unwrap();
            assert_eq!(entries.len(), 1);
            assert!(entries[0].read);
            let entry = store.entry(1).unwrap().unwrap();
            assert_eq!(entry.body_text, "11/12 メンテ");
            assert_eq!(entry.body_checked_at, None);
            assert_eq!(revision(&store, 1), 1);
            let mut search = EntryQuery::channel(Channel::News);
            search.search = Some("メンテナンス".into());
            assert_eq!(store.query(&search).unwrap().len(), 1);
            assert_eq!(
                store.source_states().unwrap()["game"].consecutive_failures,
                2
            );

            // 移した記事は未解析なので、予定を読み直す。
            let pending = store
                .pending_analyses("ext/1", &contexts(&["game"]), 10)
                .unwrap();
            assert_eq!(pending.len(), 1);
            assert_eq!(pending[0].revision, 1);
            let saved = store
                .save_analyses(
                    &[result(
                        &pending[0],
                        Ok(maintenance_on(12, 14, 17, Disposition::Scheduled)),
                    )],
                    &mut as_written,
                )
                .unwrap();
            assert_eq!(saved.saved, 1);
            assert_eq!(wide(&store, None).len(), 1);

            // 同じ本文を取得し直しても版は変わらない。旧形式の引き継ぎ (`legacy_id`) も使える。
            let mut again = draft("n1", "メンテナンスのお知らせ", "<p>11/12 メンテ</p>");
            again.url = Some("https://example.com/n1".into());
            again.published = Some(Published::datetime(now()));
            again.list_signature = "sig".into();
            store
                .apply_fetch(
                    "game",
                    Channel::News,
                    &ImageIdentity::default(),
                    &[again],
                    None,
                    now(),
                )
                .unwrap();
            assert_eq!(revision(&store, 1), 1);
            drop(store);

            // 開き直しても同じ形のまま。
            let store = Store::open(&path).unwrap();
            assert_eq!(column_names(&store), columns);
            assert_eq!(wide(&store, None).len(), 1);
        }
    }

    #[test]
    fn migrates_version_3_databases_with_signed_images() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gnn.db");
        // 版 3: 画像の取得 URL の表 (`entry_images`) がなく、本文と画像は配信元の URL のまま。
        let old_body = signed_draft("AAA", 100)
            .body_html
            .unwrap()
            .replace("&amp;t=100", "");
        {
            let conn = Connection::open(&path).unwrap();
            let v3 = SCHEMA
                .split("-- 取得の定義の url_identity.images")
                .next()
                .unwrap();
            assert!(!v3.contains("entry_images"));
            conn.execute_batch(v3).unwrap();
            conn.execute(
                "INSERT INTO entries (id, source_id, channel, external_id, title, url, image_url, published_at,
                    published_precision, first_seen_at, updated_at, sort_at, body_html, body_text, content_hash,
                    list_signature, read_at)
                 VALUES (1, 'game', 'news', '1', 'イベント開催', 'https://example.com/1', ?1, ?2, 'datetime', ?2, ?2,
                    ?2, ?3, '開催します。', ?4, 'old', ?2)",
                params![
                    signed("banner", "AAA", 100),
                    (now() - Duration::hours(1)).timestamp(),
                    old_body,
                    content_hash(&old_body)
                ],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO entries_fts (rowid, search) VALUES (1, 'イベント開催 開催します。')",
                [],
            )
            .unwrap();
            conn.pragma_update(None, "user_version", 3).unwrap();
        }

        let mut store = Store::open(&path).unwrap();
        let version: i32 = store
            .conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .unwrap();
        assert_eq!(version, DATABASE_VERSION);
        let rows: i64 = store
            .conn
            .query_row("SELECT COUNT(*) FROM entry_images", [], |row| row.get(0))
            .unwrap();
        assert_eq!(rows, 0);

        // 宣言を足したカタログで取り直しても、記事は更新にならず、既読と版もそのまま。
        let images = signed_images("game", &[]);
        let mut fetched = signed_draft("BBB", 200);
        fetched.published = Some(Published::datetime(now() - Duration::hours(1)));
        let report = store
            .apply_fetch("game", Channel::News, &images, &[fetched], None, now())
            .unwrap();
        assert_eq!(report, ApplyReport::default());
        assert_eq!(revision(&store, 1), 1);
        let entry = store.entry(1).unwrap().unwrap();
        assert!(entry.summary.read);
        let banner = images.reference(&signed("banner", "BBB", 200)).unwrap();
        assert_eq!(entry.summary.image_url.as_deref(), Some(banner.as_str()));
        assert_eq!(
            store.image_fetch_url("game", &image_key(&banner)).unwrap(),
            Some(signed("banner", "BBB", 200))
        );
        drop(store);

        // 開き直しても同じ形のまま。
        let store = Store::open(&path).unwrap();
        assert_eq!(
            store.image_fetch_url("game", &image_key(&banner)).unwrap(),
            Some(signed("banner", "BBB", 200))
        );
    }

    #[test]
    fn close_writes_the_wal_back_and_keeps_the_entries() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gnn.db");
        let wal = dir.path().join("gnn.db-wal");
        let mut store = Store::open(&path).unwrap();
        store
            .apply_fetch(
                "game",
                Channel::News,
                &ImageIdentity::default(),
                &[draft("n1", "お知らせ", "<p>本文</p>")],
                None,
                now(),
            )
            .unwrap();
        assert!(wal.exists(), "書き込みは WAL に入る");
        store.close().unwrap();
        assert!(!wal.exists(), "閉じると WAL をデータベースに書き戻して消す");

        let store = Store::open(&path).unwrap();
        let entries = store.query(&EntryQuery::channel(Channel::News)).unwrap();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].title, "お知らせ");
    }

    #[test]
    fn refuses_newer_databases() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("gnn.db");
        {
            let conn = Connection::open(&path).unwrap();
            conn.pragma_update(None, "user_version", DATABASE_VERSION + 1)
                .unwrap();
        }
        let err = Store::open(&path).err().unwrap();
        assert!(err.to_string().contains("新しい"), "{err:#}");
    }
}
