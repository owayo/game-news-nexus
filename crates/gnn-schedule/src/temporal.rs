//! 日時の字句。正規化した文字列から、日付・時刻・範囲・所要時間・地域時刻の明記を拾う。
//!
//! 年はここでは決めない (年のない日付は年のないまま持ち、`resolve` が文脈から決める)。
//! 拾うもの:
//! - 日付: `2026年11月12日` `2026/11/12` `26年11月2日` `11月12日` `11/12`、曜日 `(木)` `(木・祝)`、
//!   相対の日付 `本日` `明日` `翌日` `同日` など。
//! - 時刻: `14:00` `14:00:00` `14時` `23時59分` `24:59` (24 時以降もそのまま)、`午後3時`、`翌2:00`、`頃`。
//! - 地域時刻: `(JST)` `(日本時間)` `(UTC+8)` `(サーバー時間)`。
//! - 範囲: `A ~ B` `A - B` `AからB` `AよりB`、終わりの `未定` `終了時間未定`、終わりのない `A ~`、
//!   始まりのない `~B`。
//! - メンテナンスを起点にした日付: `2026年10月30日メンテナンス後` `11月9日(月)メンテナンスまで`、
//!   起点だけの `Ver.4.6アップデート後` `メンテナンス明け` (範囲の始まりにだけ使う)。
//! - 所要時間: `約5時間` `およそ5時間` `30分程度` (概算の印があるものだけ)。

use std::ops::Range;

use chrono::Weekday;

/// 拾った日時の表記。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Temporal {
    /// 正規化した文字列の範囲。
    pub span: Range<usize>,
    pub kind: TemporalKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum TemporalKind {
    Point(Point),
    Range {
        start: Option<Point>,
        end: RangeEnd,
    },
    /// 所要時間 (分)。
    Duration {
        minutes: u32,
    },
}

#[cfg(test)]
impl Temporal {
    /// 範囲か。
    pub fn is_range(&self) -> bool {
        matches!(self.kind, TemporalKind::Range { .. })
    }
}

/// 範囲の終わり。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RangeEnd {
    Point(Point),
    /// 「未定」と書かれている。
    Undecided,
    /// 書かれていない (`A ~`)。
    Open,
}

/// 日時の点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Point {
    pub span: Range<usize>,
    pub date: Option<DateSpec>,
    pub time: Option<TimeSpec>,
    pub zone: Option<ZoneSpec>,
    pub anchor: Option<Anchor>,
    /// `頃` `ごろ` が付いている。
    pub approx: bool,
}

/// 日付。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DateSpec {
    Absolute {
        year: Option<i32>,
        month: u32,
        day: u32,
        weekday: Option<Weekday>,
    },
    Relative(RelativeDay),
}

/// 相対の日付。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RelativeDay {
    /// 本日・今日 (基準の日)。
    Today,
    Tomorrow,
    DayAfterTomorrow,
    Yesterday,
    /// 翌日 (直前に書かれた日付の次の日)。
    NextDay,
    /// 同日・当日 (直前に書かれた日付)。
    SameDay,
    /// 前日 (直前に書かれた日付の前の日)。
    PreviousDay,
}

/// 時刻。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TimeSpec {
    /// 24 時以降もそのまま (`24:59` は 24)。
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    /// `翌2:00` の `翌`。
    pub next_day: bool,
}

/// 地域時刻の明記。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ZoneSpec {
    /// `JST` `日本時間`。
    Japan,
    /// `UTC+8` (秒)。
    Offset(i32),
    /// `サーバー時間`。
    Server,
}

/// 日付の起点。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Anchor {
    /// `メンテナンス後` `メンテナンス明け` `メンテナンス終了後` (その日のメンテナンスが終わってから)。
    MaintenanceAfter,
    /// `メンテナンスまで` `メンテナンス前` (その日のメンテナンスが始まるまで)。
    MaintenanceUntil,
    /// `Ver.4.6アップデート後` `アップデート後`。
    UpdateAfter,
}

/// 文字列の中の日時の表記を、前から順に拾う。
pub(crate) fn scan(text: &str) -> Vec<Temporal> {
    let mut out = Vec::new();
    let mut at = 0;
    while at < text.len() {
        if let Some(temporal) = scan_at(text, at) {
            at = temporal.span.end.max(at + 1);
            out.push(temporal);
            continue;
        }
        at += text[at..].chars().next().map_or(1, char::len_utf8);
    }
    out
}

/// 日時の表記を除いた文字列。
pub(crate) fn strip_temporals(text: &str) -> String {
    let mut out = String::new();
    let mut last = 0;
    for temporal in scan(text) {
        out.push_str(&text[last..temporal.span.start]);
        last = temporal.span.end;
    }
    out.push_str(&text[last..]);
    out
}

/// 日時の表記を含むか。
pub(crate) fn has_temporal(text: &str) -> bool {
    !scan(text).is_empty()
}

/// 先頭 (記号と空白の後) が日時の表記か。
pub(crate) fn starts_with_temporal(text: &str) -> bool {
    let trimmed = text
        .trim_start_matches(|ch: char| ch.is_whitespace() || "・•●○〇◆◇■□▼▽◼◾-*~(".contains(ch));
    let offset = text.len() - trimmed.len();
    scan_at(text, offset).is_some()
}

fn scan_at(text: &str, at: usize) -> Option<Temporal> {
    if !boundary_before(text, at) {
        return None;
    }
    let rest = &text[at..];
    // 始まりのない範囲 `~B`。
    if let Some(after) = rest.strip_prefix('~') {
        let start = at + 1 + (after.len() - after.trim_start().len());
        if let Some(end) = parse_point(text, start, true) {
            return Some(Temporal {
                span: at..end.span.end,
                kind: TemporalKind::Range {
                    start: None,
                    end: RangeEnd::Point(end),
                },
            });
        }
        return None;
    }
    if let Some(duration) = parse_duration(text, at) {
        return Some(duration);
    }
    let start = parse_point(text, at, false)?;
    Some(parse_range(text, start))
}

/// `at` の前が日時の表記の始まりとして切れているか (数字・英字・`/` `.` の直後ではない。`:` `,` は
/// 数字の後ろのとき (時刻・桁区切りの途中) だけ切れていないとみなす)。
fn boundary_before(text: &str, at: usize) -> bool {
    let mut before = text[..at].chars().rev();
    match before.next() {
        None => true,
        Some(ch) if ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.') => false,
        Some(':' | ',') => !before.next().is_some_and(|ch| ch.is_ascii_digit()),
        Some(_) => true,
    }
}

/// 範囲の後ろ半分を読む。
fn parse_range(text: &str, start: Point) -> Temporal {
    let mut cursor = Cursor::new(text, start.span.end);
    cursor.skip_spaces();
    let connector_at = cursor.pos;
    let connector = if cursor.eat("~") || cursor.eat_any(&["–", "—", "―", "‐", "−"]) {
        Some(Connector::Tilde)
    } else if cursor.peek() == Some('-') && cursor.next_is_temporal_after(1) {
        cursor.eat("-");
        Some(Connector::Tilde)
    } else if cursor.eat("から") || cursor.eat("より") {
        Some(Connector::Particle)
    } else {
        None
    };
    let Some(connector) = connector else {
        return point_temporal(start);
    };
    cursor.skip_spaces();
    if let Some(end) = parse_point(text, cursor.pos, true) {
        let span = start.span.start..end.span.end;
        return Temporal {
            span,
            kind: TemporalKind::Range {
                start: Some(start),
                end: RangeEnd::Point(end),
            },
        };
    }
    const UNDECIDED: &[&str] = &[
        "終了時間未定",
        "終了時刻未定",
        "終了日時未定",
        "終了期日未定",
        "終了日未定",
        "終了未定",
        "未定",
    ];
    if let Some(word) = UNDECIDED
        .iter()
        .find(|word| cursor.rest().starts_with(*word))
    {
        let span = start.span.start..cursor.pos + word.len();
        return Temporal {
            span,
            kind: TemporalKind::Range {
                start: Some(start),
                end: RangeEnd::Undecided,
            },
        };
    }
    match connector {
        Connector::Tilde => Temporal {
            span: start.span.start..connector_at + 1,
            kind: TemporalKind::Range {
                start: Some(start),
                end: RangeEnd::Open,
            },
        },
        // 「12:00より実施」の「より」は助詞。
        Connector::Particle => point_temporal(start),
    }
}

#[derive(Clone, Copy)]
enum Connector {
    Tilde,
    Particle,
}

fn point_temporal(point: Point) -> Temporal {
    if point.date.is_none() && point.time.is_none() {
        // 起点だけ (`アップデート後`) は範囲の始まりにしか使わない。
        return Temporal {
            span: point.span.start..point.span.start,
            kind: TemporalKind::Point(point),
        };
    }
    Temporal {
        span: point.span.clone(),
        kind: TemporalKind::Point(point),
    }
}

/// 日時の点を読む。`time_only` なら時刻だけの点 (範囲の終わり) も読む。
fn parse_point(text: &str, at: usize, in_range: bool) -> Option<Point> {
    if !boundary_before(text, at) && !in_range {
        return None;
    }
    let mut cursor = Cursor::new(text, at);
    let mut next_day = false;
    let mut date = None;
    if cursor.rest().starts_with("翌日") {
        cursor.eat("翌日");
        date = Some(DateSpec::Relative(RelativeDay::NextDay));
    } else if cursor.rest().starts_with('翌') {
        let save = cursor.pos;
        cursor.eat("翌");
        cursor.skip_spaces();
        if parse_time(&mut cursor.clone()).is_some() {
            next_day = true;
        } else {
            cursor.pos = save;
        }
    }
    if date.is_none() && !next_day {
        date = parse_date(&mut cursor);
        // `本日10月22日(木)21:00` の `本日` は、続く日付と同じ日。
        if matches!(date, Some(DateSpec::Relative(_))) {
            let save = cursor.pos;
            cursor.skip_spaces();
            match parse_date(&mut cursor) {
                Some(absolute @ DateSpec::Absolute { .. }) => date = Some(absolute),
                _ => cursor.pos = save,
            }
        }
    }
    if let Some(DateSpec::Absolute { weekday, .. }) = &mut date {
        *weekday = parse_weekday(&mut cursor);
    } else if date.is_some() {
        // 「本日(木)」の曜日は読み飛ばす。
        let _ = parse_weekday(&mut cursor);
    }
    let save = cursor.pos;
    cursor.skip_spaces();
    let mut time = parse_time(&mut cursor);
    if time.is_none() {
        cursor.pos = save;
    }
    if let Some(time) = &mut time {
        time.next_day = next_day;
    }
    if date.is_none() && time.is_none() {
        // 起点だけの点 (`Ver.4.6アップデート後`)。
        return parse_anchor_only(text, at);
    }
    if date.is_none() && !in_range && !has_time_context(text, at) && !next_day {
        // 日付のない時刻は、前後が時刻らしいときだけ (`16:00に` `16時より`)。
        // `5時` のような数え方と紛れないよう、`:` か `時` と助詞・範囲が続くものに限る。
    }
    let mut approx = false;
    for word in ["頃", "ごろ"] {
        if cursor.eat(word) {
            approx = true;
        }
    }
    let zone = parse_zone(&mut cursor);
    let mut anchor = None;
    if date.is_some() && time.is_none() {
        anchor = parse_anchor(&mut cursor);
    }
    Some(Point {
        span: at..cursor.pos,
        date,
        time,
        zone,
        anchor,
        approx,
    })
}

fn has_time_context(_text: &str, _at: usize) -> bool {
    true
}

/// `メンテナンス後` などの起点を読む (日付の直後)。
fn parse_anchor(cursor: &mut Cursor<'_>) -> Option<Anchor> {
    let save = cursor.pos;
    cursor.skip_spaces();
    cursor.eat("の");
    if !(cursor.eat("メンテナンス") || cursor.eat("メンテ")) {
        cursor.pos = save;
        return None;
    }
    cursor.skip_spaces();
    for word in ["終了後", "明け", "後", "以降", "終了以降"] {
        if cursor.eat(word) {
            return Some(Anchor::MaintenanceAfter);
        }
    }
    for word in ["開始前", "前", "開始以前", "以前"] {
        if cursor.eat(word) {
            return Some(Anchor::MaintenanceUntil);
        }
    }
    if cursor.rest().starts_with("まで") {
        return Some(Anchor::MaintenanceUntil);
    }
    cursor.pos = save;
    None
}

/// 起点だけの点 (`Ver.4.6アップデート後` `アップデート後` `メンテナンス後` `メンテナンス明け`)。
fn parse_anchor_only(text: &str, at: usize) -> Option<Point> {
    let mut cursor = Cursor::new(text, at);
    let mut anchor = None;
    if cursor.eat_ascii_ci("ver") {
        cursor.eat(".");
        cursor.skip_spaces();
        let digits = cursor.pos;
        while cursor
            .peek()
            .is_some_and(|ch| ch.is_ascii_digit() || ch == '.')
        {
            cursor.advance();
        }
        if cursor.pos == digits {
            return None;
        }
        cursor.skip_spaces();
    }
    if cursor.eat("アップデート後") || cursor.eat("バージョンアップ後") {
        anchor = Some(Anchor::UpdateAfter);
    } else if (cursor.eat("メンテナンス") || cursor.eat("メンテ"))
        && (cursor.eat("終了後") || cursor.eat("明け") || cursor.eat("後"))
    {
        anchor = Some(Anchor::MaintenanceAfter);
    }
    let anchor = anchor?;
    // 範囲の始まりとしてだけ使う。
    let mut probe = cursor.clone();
    probe.skip_spaces();
    if !probe.rest().starts_with('~') {
        return None;
    }
    Some(Point {
        span: at..cursor.pos,
        date: None,
        time: None,
        zone: None,
        anchor: Some(anchor),
        approx: false,
    })
}

fn parse_date(cursor: &mut Cursor<'_>) -> Option<DateSpec> {
    let save = cursor.pos;
    // 年月日。
    if let Some(year) = cursor.digits(4, 4)
        && (1990..=2100).contains(&year)
    {
        let after_year = cursor.pos;
        cursor.skip_spaces();
        let kanji = cursor.eat("年");
        if kanji || cursor.eat_any(&["/", ".", "-"]) {
            cursor.skip_spaces();
            if let Some(month) = cursor.digits(1, 2) {
                cursor.skip_spaces();
                let separated = if kanji {
                    cursor.eat("月")
                } else {
                    cursor.eat_any(&["/", ".", "-"])
                };
                if separated {
                    cursor.skip_spaces();
                    if let Some(day) = cursor.digits(1, 2)
                        && !cursor.peek().is_some_and(|ch| ch.is_ascii_digit())
                    {
                        cursor.eat("日");
                        if valid_month_day(month, day) {
                            return Some(DateSpec::Absolute {
                                year: Some(year as i32),
                                month,
                                day,
                                weekday: None,
                            });
                        }
                    }
                }
            }
        }
        let _ = after_year;
        cursor.pos = save;
    }
    // 2 桁の年 (`26年11月2日`)。
    if let Some(year) = cursor.digits(2, 2)
        && cursor.eat("年")
        && let Some(month) = cursor.digits(1, 2)
        && cursor.eat("月")
        && let Some(day) = cursor.digits(1, 2)
        && cursor.eat("日")
        && valid_month_day(month, day)
    {
        return Some(DateSpec::Absolute {
            year: Some(2000 + year as i32),
            month,
            day,
            weekday: None,
        });
    }
    cursor.pos = save;
    // 月日。
    if let Some(month) = cursor.digits(1, 2) {
        let after_month = cursor.pos;
        cursor.skip_spaces();
        if cursor.eat("月") {
            cursor.skip_spaces();
            if let Some(day) = cursor.digits(1, 2)
                && cursor.eat("日")
                && valid_month_day(month, day)
            {
                return Some(DateSpec::Absolute {
                    year: None,
                    month,
                    day,
                    weekday: None,
                });
            }
        }
        cursor.pos = after_month;
        if cursor.eat("/")
            && let Some(day) = cursor.digits(1, 2)
            && !cursor
                .peek()
                .is_some_and(|ch| ch.is_ascii_digit() || ch == '/')
            && valid_month_day(month, day)
        {
            return Some(DateSpec::Absolute {
                year: None,
                month,
                day,
                weekday: None,
            });
        }
        cursor.pos = save;
    }
    // 相対の日付。
    const RELATIVE: &[(&str, RelativeDay)] = &[
        ("本日", RelativeDay::Today),
        ("今日", RelativeDay::Today),
        ("明後日", RelativeDay::DayAfterTomorrow),
        ("明日", RelativeDay::Tomorrow),
        ("翌日", RelativeDay::NextDay),
        ("同日", RelativeDay::SameDay),
        ("当日", RelativeDay::SameDay),
        ("昨日", RelativeDay::Yesterday),
        ("前日", RelativeDay::PreviousDay),
    ];
    for (word, day) in RELATIVE {
        if cursor.rest().starts_with(word) {
            // 「毎日」「休日」のような語の一部ではない。
            let previous = cursor.text[..cursor.pos].chars().next_back();
            if previous.is_some_and(is_kanji) {
                return None;
            }
            cursor.eat(word);
            return Some(DateSpec::Relative(*day));
        }
    }
    None
}

fn is_kanji(ch: char) -> bool {
    matches!(ch, '\u{4e00}'..='\u{9fff}' | '\u{3400}'..='\u{4dbf}' | '々')
}

fn valid_month_day(month: u32, day: u32) -> bool {
    (1..=12).contains(&month) && (1..=31).contains(&day)
}

fn parse_weekday(cursor: &mut Cursor<'_>) -> Option<Weekday> {
    let save = cursor.pos;
    cursor.skip_spaces();
    if !cursor.eat("(") {
        cursor.pos = save;
        return None;
    }
    cursor.skip_spaces();
    let weekday = match cursor.peek() {
        Some('月') => Weekday::Mon,
        Some('火') => Weekday::Tue,
        Some('水') => Weekday::Wed,
        Some('木') => Weekday::Thu,
        Some('金') => Weekday::Fri,
        Some('土') => Weekday::Sat,
        Some('日') => Weekday::Sun,
        _ => {
            cursor.pos = save;
            return None;
        }
    };
    cursor.advance();
    cursor.eat("曜日");
    cursor.eat("曜");
    if cursor.eat_any(&["・", "/", "･", ","]) {
        cursor.eat_any(&["祝日", "祝", "休日", "休", "祭日"]);
    }
    cursor.skip_spaces();
    if !cursor.eat(")") {
        cursor.pos = save;
        return None;
    }
    Some(weekday)
}

fn parse_time(cursor: &mut Cursor<'_>) -> Option<TimeSpec> {
    let save = cursor.pos;
    let mut pm = None;
    if cursor.eat("午前") || cursor.eat_ascii_ci("am") {
        pm = Some(false);
        cursor.skip_spaces();
    } else if cursor.eat("午後") || cursor.eat_ascii_ci("pm") {
        pm = Some(true);
        cursor.skip_spaces();
    }
    if cursor.eat("正午") {
        return Some(TimeSpec {
            hour: 12,
            minute: 0,
            second: 0,
            next_day: false,
        });
    }
    let Some(hour) = cursor.digits(1, 2) else {
        cursor.pos = save;
        return None;
    };
    let mut minute = 0;
    let mut second = 0;
    let colon = {
        let probe = cursor.pos;
        cursor.skip_spaces();
        if cursor.eat(":") {
            cursor.skip_spaces();
            true
        } else {
            cursor.pos = probe;
            false
        }
    };
    if colon {
        let Some(m) = cursor.digits(2, 2) else {
            cursor.pos = save;
            return None;
        };
        minute = m;
        let probe = cursor.pos;
        if cursor.eat(":") {
            if let Some(s) = cursor.digits(2, 2) {
                second = s;
            } else {
                cursor.pos = probe;
            }
        }
    } else if cursor.rest().starts_with('時') && !cursor.rest().starts_with("時間") {
        cursor.eat("時");
        if cursor.eat("半") {
            minute = 30;
        } else {
            let probe = cursor.pos;
            if let Some(m) = cursor.digits(1, 2)
                && cursor.eat("分")
            {
                minute = m;
                let probe = cursor.pos;
                if let Some(s) = cursor.digits(1, 2)
                    && cursor.eat("秒")
                {
                    second = s;
                } else {
                    cursor.pos = probe;
                }
            } else {
                cursor.pos = probe;
            }
        }
    } else {
        cursor.pos = save;
        return None;
    }
    if cursor.peek().is_some_and(|ch| ch.is_ascii_digit()) {
        cursor.pos = save;
        return None;
    }
    if pm.is_none() {
        let probe = cursor.pos;
        cursor.skip_spaces();
        if cursor.eat_ascii_ci("am") {
            pm = Some(false);
        } else if cursor.eat_ascii_ci("pm") {
            pm = Some(true);
        } else {
            cursor.pos = probe;
        }
    }
    let mut hour = hour;
    match pm {
        Some(true) if hour < 12 => hour += 12,
        Some(false) if hour == 12 => hour = 0,
        _ => {}
    }
    if hour > 35 || minute > 59 || second > 59 {
        cursor.pos = save;
        return None;
    }
    Some(TimeSpec {
        hour,
        minute,
        second,
        next_day: false,
    })
}

fn parse_zone(cursor: &mut Cursor<'_>) -> Option<ZoneSpec> {
    let save = cursor.pos;
    cursor.skip_spaces();
    let open = cursor.eat("(");
    cursor.skip_spaces();
    let zone = if cursor.eat("JST") || cursor.eat("日本時間") || cursor.eat("日本標準時") {
        Some(ZoneSpec::Japan)
    } else if cursor.eat("サーバー時間") || cursor.eat("サーバ時間") {
        Some(ZoneSpec::Server)
    } else if cursor.eat("UTC") || cursor.eat("GMT") {
        cursor.skip_spaces();
        let sign = if cursor.eat("+") {
            1
        } else if cursor.eat("-") || cursor.eat("−") {
            -1
        } else {
            0
        };
        if sign == 0 {
            Some(ZoneSpec::Offset(0))
        } else {
            cursor.skip_spaces();
            match cursor.digits(1, 2) {
                Some(hours) => {
                    let mut minutes = 0;
                    let probe = cursor.pos;
                    if cursor.eat(":") {
                        match cursor.digits(2, 2) {
                            Some(m) => minutes = m,
                            None => cursor.pos = probe,
                        }
                    }
                    Some(ZoneSpec::Offset(
                        sign * (hours as i32 * 3600 + minutes as i32 * 60),
                    ))
                }
                None => None,
            }
        }
    } else {
        None
    };
    let Some(zone) = zone else {
        cursor.pos = save;
        return None;
    };
    cursor.skip_spaces();
    if open && !cursor.eat(")") {
        cursor.pos = save;
        return None;
    }
    Some(zone)
}

/// 概算の所要時間 (`約5時間` `およそ5時間` `30分程度`)。
fn parse_duration(text: &str, at: usize) -> Option<Temporal> {
    let mut cursor = Cursor::new(text, at);
    let prefixed = ["約", "およそ", "概ね", "おおよそ", "大体", "最大"]
        .iter()
        .any(|word| cursor.eat(word));
    cursor.skip_spaces();
    let amount_at = cursor.pos;
    let amount = cursor.digits(1, 3)?;
    let mut half = false;
    if cursor.eat(".") {
        if cursor.eat("5") {
            half = true;
        } else {
            return None;
        }
    }
    let minutes = if cursor.eat("時間") {
        amount * 60 + if half { 30 } else { 0 }
    } else if cursor.eat("分間") || cursor.eat("分") {
        if half {
            return None;
        }
        amount
    } else {
        return None;
    };
    let suffixed = ["程度", "ほど", "前後", "くらい", "ぐらい"]
        .iter()
        .any(|word| cursor.eat(word));
    if !(prefixed || suffixed) || amount_at == cursor.pos {
        return None;
    }
    Some(Temporal {
        span: at..cursor.pos,
        kind: TemporalKind::Duration { minutes },
    })
}

#[derive(Clone)]
struct Cursor<'a> {
    text: &'a str,
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(text: &'a str, pos: usize) -> Self {
        Self { text, pos }
    }

    fn rest(&self) -> &'a str {
        &self.text[self.pos..]
    }

    fn peek(&self) -> Option<char> {
        self.rest().chars().next()
    }

    fn advance(&mut self) {
        if let Some(ch) = self.peek() {
            self.pos += ch.len_utf8();
        }
    }

    fn eat(&mut self, word: &str) -> bool {
        if self.rest().starts_with(word) {
            self.pos += word.len();
            true
        } else {
            false
        }
    }

    fn eat_any(&mut self, words: &[&str]) -> bool {
        words.iter().any(|word| self.eat(word))
    }

    fn eat_ascii_ci(&mut self, word: &str) -> bool {
        let rest = self.rest().as_bytes();
        if rest.len() >= word.len() && rest[..word.len()].eq_ignore_ascii_case(word.as_bytes()) {
            // 英単語の途中ではない。
            if rest
                .get(word.len())
                .is_some_and(|byte| byte.is_ascii_alphabetic())
            {
                return false;
            }
            self.pos += word.len();
            true
        } else {
            false
        }
    }

    fn skip_spaces(&mut self) {
        while self.peek() == Some(' ') {
            self.pos += 1;
        }
    }

    fn digits(&mut self, min: usize, max: usize) -> Option<u32> {
        let bytes = self.rest().as_bytes();
        let count = bytes
            .iter()
            .take(max + 1)
            .take_while(|byte| byte.is_ascii_digit())
            .count();
        if count < min || count > max {
            return None;
        }
        let value = self.rest()[..count].parse().ok()?;
        self.pos += count;
        Some(value)
    }

    /// `offset` バイト先から日時の表記が始まるか (`-` を範囲の区切りと読んでよいか)。
    fn next_is_temporal_after(&self, offset: usize) -> bool {
        let mut probe = Cursor::new(self.text, self.pos + offset);
        probe.skip_spaces();
        parse_point(self.text, probe.pos, true).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn point(temporal: &Temporal) -> &Point {
        match &temporal.kind {
            TemporalKind::Point(point) => point,
            other => panic!("点ではない: {other:?}"),
        }
    }

    fn date(point: &Point) -> (Option<i32>, u32, u32) {
        match point.date {
            Some(DateSpec::Absolute {
                year, month, day, ..
            }) => (year, month, day),
            other => panic!("日付ではない: {other:?}"),
        }
    }

    #[test]
    fn scans_points() {
        let text = "2026/11/12(木) 14:00~17:00 終了予定";
        let found = scan(text);
        assert_eq!(found.len(), 1);
        let TemporalKind::Range { start, end } = &found[0].kind else {
            panic!()
        };
        let start = start.as_ref().unwrap();
        assert_eq!(date(start), (Some(2026), 11, 12));
        assert_eq!(
            start.date,
            Some(DateSpec::Absolute {
                year: Some(2026),
                month: 11,
                day: 12,
                weekday: Some(Weekday::Thu)
            })
        );
        let RangeEnd::Point(end) = end else { panic!() };
        assert!(end.date.is_none());
        assert_eq!(end.time.unwrap().hour, 17);
        assert_eq!(&text[found[0].span.clone()], "2026/11/12(木) 14:00~17:00");
    }

    #[test]
    fn scans_time_words_and_zones() {
        let found = scan("2026/09/28 07:00 (JST)からおよそ5時間を予定。");
        assert_eq!(found.len(), 2);
        let start = point(&found[0]);
        assert_eq!(start.zone, Some(ZoneSpec::Japan));
        assert_eq!(found[1].kind, TemporalKind::Duration { minutes: 300 });

        let found = scan("2026年10月19日23時59分まで、午後3時、翌2:00、24:59(UTC+8)");
        let points: Vec<&Point> = found.iter().map(point).collect();
        assert_eq!(points[0].time.unwrap().hour, 23);
        assert_eq!(points[0].time.unwrap().minute, 59);
        assert_eq!(points[1].time.unwrap().hour, 15);
        assert!(points[2].time.unwrap().next_day);
        assert_eq!(points[3].time.unwrap().hour, 24);
        assert_eq!(points[3].zone, Some(ZoneSpec::Offset(8 * 3600)));
    }

    #[test]
    fn scans_ranges() {
        let found = scan("10月21日(水)21:30 ~ 終了時間未定");
        assert!(matches!(
            found[0].kind,
            TemporalKind::Range {
                end: RangeEnd::Undecided,
                ..
            }
        ));
        let found = scan("開催期間を『~2026年10月22日4:59』から『~2026年10月25日4:59』へ延長");
        assert_eq!(found.len(), 2);
        assert!(matches!(
            found[0].kind,
            TemporalKind::Range { start: None, .. }
        ));
        let found = scan("2026年10月30日メンテナンス後 ~ 2026年11月9日11:59まで");
        assert_eq!(found.len(), 1);
        let TemporalKind::Range { start, .. } = &found[0].kind else {
            panic!()
        };
        assert_eq!(
            start.as_ref().unwrap().anchor,
            Some(Anchor::MaintenanceAfter)
        );
        let found = scan("Ver.4.6アップデート後 ~ 2026/11/11 07:00 (JST)。");
        assert_eq!(found.len(), 1);
        assert!(found[0].is_range());
        let found = scan("10月13日(火)~ 10月19日(月)メンテナンスまで");
        let TemporalKind::Range { end, .. } = &found[0].kind else {
            panic!()
        };
        let RangeEnd::Point(end) = end else { panic!() };
        assert_eq!(end.anchor, Some(Anchor::MaintenanceUntil));
        // 「より」は後ろに日時がなければ助詞。
        let found = scan("12:00より定期メンテナンスを実施");
        assert_eq!(found.len(), 1);
        assert!(!found[0].is_range());
        let found = scan("2026/10/21 22:00 ~ 2026/10/22 21:00");
        assert!(found[0].is_range());
    }

    #[test]
    fn ignores_non_dates() {
        assert!(scan("Ver.7.2 と Lv.5/6/13/15 と 1.5周年 と 131F~150F").is_empty());
        assert!(scan("1時間につき原石×60").is_empty());
        assert!(scan("毎日の更新").is_empty());
        let found = scan("本日15:00より");
        assert_eq!(
            point(&found[0]).date,
            Some(DateSpec::Relative(RelativeDay::Today))
        );
    }
}
