//! 記事 (お知らせ・リリース情報の 1 件) のモデル。

use chrono::{DateTime, Duration, NaiveDate, TimeZone, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};

use crate::source::Channel;

/// 投稿日時の精度。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimePrecision {
    /// 日時まで分かっている。
    DateTime,
    /// 日付だけ分かっている。
    Date,
}

impl TimePrecision {
    pub fn as_str(self) -> &'static str {
        match self {
            TimePrecision::DateTime => "datetime",
            TimePrecision::Date => "date",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "datetime" => Some(TimePrecision::DateTime),
            "date" => Some(TimePrecision::Date),
            _ => None,
        }
    }
}

/// 投稿日時。日付だけのときは、その日の 0 時 (ソースのタイムゾーン) を持つ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Published {
    pub at: DateTime<Utc>,
    pub precision: TimePrecision,
}

impl Published {
    pub fn datetime(at: DateTime<Utc>) -> Self {
        Self {
            at,
            precision: TimePrecision::DateTime,
        }
    }

    /// 日付だけの投稿日。`tz` のその日の 0 時を持つ。
    pub fn date(date: NaiveDate, tz: Tz) -> Self {
        let midnight = date.and_hms_opt(0, 0, 0).expect("midnight");
        let at = tz
            .from_local_datetime(&midnight)
            .earliest()
            .unwrap_or_else(|| tz.from_utc_datetime(&midnight))
            .with_timezone(&Utc);
        Self {
            at,
            precision: TimePrecision::Date,
        }
    }
}

/// 一覧に並べる時刻を決める。
///
/// 日時まで分かる記事はその日時。日付だけの記事は、その日のうちに初めて見つけたなら
/// 見つけた時刻、そうでなければその日の 0 時にする。架空の時刻を作らずに、
/// 同じ日の記事を見つけた順に並べるため。日時が分からない記事は見つけた時刻。
pub fn sort_time(published: Option<Published>, first_seen_at: DateTime<Utc>) -> DateTime<Utc> {
    match published {
        Some(Published {
            at,
            precision: TimePrecision::DateTime,
        }) => at,
        Some(Published {
            at,
            precision: TimePrecision::Date,
        }) => {
            if first_seen_at >= at && first_seen_at < at + Duration::days(1) {
                first_seen_at
            } else {
                at
            }
        }
        None => first_seen_at,
    }
}

/// 取得処理が作る、保存前の記事。
#[derive(Debug, Clone, PartialEq)]
pub struct EntryDraft {
    /// ソースの中で記事を見分ける値。
    pub external_id: String,
    pub title: String,
    pub url: Option<String>,
    pub published: Option<Published>,
    /// 整形済みの本文。`None` は「今回は本文を取り直していない」。
    pub body_html: Option<String>,
    pub image_url: Option<String>,
    pub note: Option<String>,
    /// 一覧で読めた値から作った署名。変わったら詳細を取り直す。
    pub list_signature: String,
}

/// 保存済みの記事の要約。一覧の表示に使う。
#[derive(Debug, Clone, PartialEq)]
pub struct EntrySummary {
    pub id: i64,
    pub source_id: String,
    pub channel: Channel,
    pub title: String,
    pub url: Option<String>,
    pub image_url: Option<String>,
    pub note: Option<String>,
    pub published: Option<Published>,
    pub first_seen_at: DateTime<Utc>,
    pub sort_at: DateTime<Utc>,
    pub read: bool,
}

/// 保存済みの記事。
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub summary: EntrySummary,
    pub body_html: String,
    pub body_text: String,
    /// 記事の中身 (題名・本文・投稿日時など) が最後に変わった時刻。
    pub updated_at: DateTime<Utc>,
    /// 本文を最後に取得できた時刻 (中身が変わらなくても進む)。旧形式から取り込んだだけなら `None`。
    pub body_checked_at: Option<DateTime<Utc>>,
}

/// 一覧で読めた値から署名を作る。
pub fn list_signature(parts: &[Option<&str>]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.unwrap_or("").as_bytes());
        hasher.update([0u8]);
    }
    hex::encode(&hasher.finalize()[..12])
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn tokyo() -> Tz {
        chrono_tz::Asia::Tokyo
    }

    #[test]
    fn date_precision_uses_midnight_in_source_timezone() {
        let published = Published::date(NaiveDate::from_ymd_opt(2026, 11, 6).unwrap(), tokyo());
        assert_eq!(
            published.at,
            Utc.with_ymd_and_hms(2026, 11, 5, 15, 0, 0).unwrap()
        );
        assert_eq!(published.precision, TimePrecision::Date);
    }

    #[test]
    fn sort_time_prefers_first_seen_within_the_day() {
        let published = Published::date(NaiveDate::from_ymd_opt(2026, 11, 6).unwrap(), tokyo());
        let same_day = Utc.with_ymd_and_hms(2026, 11, 6, 3, 30, 0).unwrap();
        assert_eq!(sort_time(Some(published), same_day), same_day);

        let later = Utc.with_ymd_and_hms(2026, 11, 9, 3, 30, 0).unwrap();
        assert_eq!(sort_time(Some(published), later), published.at);

        let exact = Published::datetime(Utc.with_ymd_and_hms(2026, 11, 6, 1, 0, 0).unwrap());
        assert_eq!(sort_time(Some(exact), later), exact.at);
        assert_eq!(sort_time(None, later), later);
    }

    #[test]
    fn signature_depends_on_every_part() {
        let a = list_signature(&[Some("a"), Some("b")]);
        assert_eq!(a, list_signature(&[Some("a"), Some("b")]));
        assert_ne!(a, list_signature(&[Some("ab"), None]));
        assert_ne!(a, list_signature(&[Some("a"), Some("c")]));
    }
}
