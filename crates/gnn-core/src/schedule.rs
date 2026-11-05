//! お知らせから読み取った予定 (メンテナンスの時間帯と結果、イベントなどの期間)。
//!
//! 読み取りは `gnn-schedule` (hasami の形態素解析) が保存済みの記事から行う。結果は記事から作り直せる
//! 派生データで、読み取り方 (抽出器の識別子) か記事の中身が変わったら読み直す。
//!
//! 型の考え方:
//! - 日時は「どの地域時刻で書かれていたか」と「確かさ」(明記・概算・文脈からの補完) を持つ。
//! - 開始・終了は「書かれていない」と「未定と書かれている」を分ける。
//! - メンテナンスは予定の時間帯と実際の時間帯を分け、結果 (予定どおり・完了・中止・延期・判断できない) と、
//!   延長や日程変更の履歴を持つ。時間帯は変更の後のもので、変更の前は履歴 (`Change`) に持つ。
//! - 1 件のメンテナンスの対象は 1 つ。1 つの時間帯に複数の対象が書かれていれば、対象ごとに別の件にする
//!   (一部の対象だけの完了・延長を突き合わせられるように)。画面はまとめて出してよい。
//! - 「実施中」と書かれていれば、開始の時刻が書かれていなくても、いつの時点で実施中だったかを持つ (`Ongoing`)。
//!   開始の時刻とは別のもので、開始には入れない。
//! - イベントなどの期間も、延長や日程変更の履歴を持つ。
//! - 根拠として、題名・本文のどの記述から読んだか (原文のまま) を持つ。

use chrono::{DateTime, NaiveDate, Utc};
use serde::{Deserialize, Serialize};

/// 記事 1 件の読み取り結果。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Schedule {
    /// メンテナンス。対象 (プラットフォームなど) と時間帯ごとに 1 件。記事に書かれた順。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub maintenance: Vec<Maintenance>,
    /// イベント・ガチャ・販売・配布などの期間。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub periods: Vec<Period>,
    /// 決められなかったことの記録 (年の候補が割れた、時刻の並びが逆、など)。評価と調査に使う。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notes: Vec<Note>,
}

impl Schedule {
    /// メンテナンスも期間も読み取れなかった。
    pub fn is_empty(&self) -> bool {
        self.maintenance.is_empty() && self.periods.is_empty()
    }
}

/// メンテナンス 1 件。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Maintenance {
    /// 対象 (1 つ)。
    pub scope: Scope,
    /// 予定の時間帯 (予告・再告知・延長と日程変更の後の予定)。
    pub planned: Window,
    /// 実際の時間帯 (終了・完了の報告に書かれたもの)。
    pub actual: Window,
    /// この記事に書かれた時点の結果。
    pub disposition: Disposition,
    /// 「実施中」と書かれていた報告 (いちばん新しいもの)。結果が予定どおりのときだけ意味を持つ。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ongoing: Option<Ongoing>,
    /// 延長や日程変更。書かれた順。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<Change>,
    /// 根拠 (メンテナンスについての記述の位置)。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
}

/// 「実施中」という報告。
///
/// 開始の時刻が書かれていない実施中の告知 (「現在、緊急メンテナンスを実施しております。終了時間は未定です。」)
/// でも、いつの時点で実施中だったかが分かる。開始はこれより前で、開始の時刻としては使わない。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Ongoing {
    /// 実施中と書かれた時点。追記の日時、なければ記事の投稿日時、それもなければ初めて見た日時
    /// (記事の日時から決めたものは `Certainty::Inferred`)。
    pub as_of: TimeValue,
    /// 根拠 (「実施中」の記述)。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
}

/// メンテナンスの対象。
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "name")]
pub enum Scope {
    /// 書かれていない。
    #[default]
    Unknown,
    /// 全体 (「全サーバー」「全プラットフォーム」など、全体と明記されている)。
    All,
    /// 名前のある対象 1 つ (「Android版」「SPクラウド版」「PC版」など。書かれた表記を正規化したもの)。
    /// 複数の対象は 1 つの名前にまとめず、対象ごとに別のメンテナンスにする。
    Named(String),
}

/// 時間帯。終わりは含まない (`[start, end)`)。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Window {
    #[serde(default)]
    pub start: Slot<TimeValue>,
    #[serde(default)]
    pub end: Slot<TimeValue>,
}

impl Window {
    /// 開始も終了も書かれていない。
    pub fn is_unspecified(&self) -> bool {
        self.start == Slot::Unspecified && self.end == Slot::Unspecified
    }
}

/// 開始・終了の値。
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum Slot<T> {
    /// 書かれていない。
    #[default]
    Unspecified,
    /// 「未定」と書かれている。
    Undecided,
    /// 値。
    Value(T),
}

impl<T> Slot<T> {
    pub fn value(&self) -> Option<&T> {
        match self {
            Slot::Value(value) => Some(value),
            Slot::Unspecified | Slot::Undecided => None,
        }
    }
}

/// 日時の値。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TimeValue {
    pub when: When,
    pub certainty: Certainty,
    /// 根拠 (日時の表記の位置)。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
}

/// いつか。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum When {
    /// 日付だけ (その地域の暦日)。「今メンテ中か」の判断には使わない。
    Date { date: NaiveDate, zone: Zone },
    /// 時刻まで分かる。
    Instant { at: DateTime<Utc>, zone: Zone },
}

/// 日時を読んだ地域時刻。表示 (「JST」など) と、日付だけの値の暦日の境目に使う。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind", content = "value")]
pub enum Zone {
    /// IANA のタイムゾーン名 (`Asia/Tokyo` など)。
    Named(String),
    /// UTC からの固定の差 (秒。東が正)。「UTC+8」のような明記。
    Offset(i32),
}

/// 日時の確かさ。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Certainty {
    /// 記事に日時として書かれている (年や日付を文脈から補ったものは `Inferred`)。
    Explicit,
    /// 概算 (「約5時間」から求めた終了など)。
    Approximate,
    /// 文脈から補った (年のない日付の年、時刻だけの終了の日付、「本日」の日付など)。
    Inferred,
}

/// メンテナンスの結果 (記事に書かれた時点)。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Disposition {
    /// 行う予定 (行っている途中を含む。実施中と書かれていれば `Maintenance::ongoing`、延長は `changes` に持つ)。
    Scheduled,
    /// 終わった (終了・完了の報告)。
    Completed,
    /// 行わない (中止・取りやめ)。
    Cancelled,
    /// 延期した (新しい日時が決まっていれば `Scheduled` の別の件として持つか、`changes` に持つ)。
    Postponed,
    /// 記述が食い違うなどで決められない。
    Unresolved,
}

/// 延長や日程変更 (メンテナンスとイベントなどの期間で共通)。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Change {
    /// 延長した。
    Extended {
        /// 延長する前の終わり (「17:00より延長」の 17:00)。延長の記述に書かれていなければ、延長する前の
        /// 予定の終わり (`Certainty::Inferred`)。分からなければ `Unspecified`。
        #[serde(default)]
        from: Slot<TimeValue>,
        /// 新しい終わり (未定もある)。
        end: Slot<TimeValue>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        evidence: Vec<Evidence>,
    },
    /// 日程を変えた。変更が 2 回以上あっても、それぞれの変更の前と後を持つ (すべての変更を反映した
    /// 最後の時間帯は、メンテナンスの `planned`、期間の `window`)。
    Rescheduled {
        /// この変更の前の時間帯。
        from: Window,
        /// この変更の後の時間帯。
        #[serde(default)]
        to: Window,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        evidence: Vec<Evidence>,
    },
}

/// イベントなどの期間。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Period {
    pub kind: PeriodKind,
    /// 何の期間か (「ハロウィンイベント」など。書かれていれば)。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    /// 期間 (延長・日程変更の後のもの)。
    pub window: Window,
    /// 延長や日程変更 (「開催期間を延長いたします」など)。書かれた順。
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub changes: Vec<Change>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
}

/// 期間の種類。「今日・明日の予定」に数えるかは種類で決める。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PeriodKind {
    /// イベントの開催期間。
    Event,
    /// ガチャ (召喚・跳躍などを含む) の期間。
    Gacha,
    /// キャンペーンの期間。
    Campaign,
    /// 販売・ショップの期間。
    Sale,
    /// 配布・受け取り・交換の期限 (補填・補償の受け取りを含む)。
    Claim,
    /// そのほか。
    Other,
}

/// 根拠。どの記述から読んだか。
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Evidence {
    pub field: Field,
    /// 根拠の記述 (正規化する前の原文のまま。長いときは前後を切る)。画面に出し、誤りの調査にも使う。
    pub text: String,
}

/// 根拠がある場所。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    Title,
    Body,
}

/// 決められなかったことの記録。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    /// 種類 (`ambiguous_year`、`reversed_range` など。gnn-schedule が決める短い識別子)。
    pub code: String,
    /// 人が読む説明 (日本語)。
    pub message: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence: Vec<Evidence>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone as _;
    use pretty_assertions::assert_eq;

    fn instant(h: u32) -> Slot<TimeValue> {
        Slot::Value(TimeValue {
            when: When::Instant {
                at: Utc.with_ymd_and_hms(2026, 11, 12, h, 0, 0).unwrap(),
                zone: Zone::Named("Asia/Tokyo".into()),
            },
            certainty: Certainty::Explicit,
            evidence: vec![Evidence {
                field: Field::Body,
                text: "2026/11/12(木) 14:00～17:00".into(),
            }],
        })
    }

    #[test]
    fn round_trips_as_json() {
        let schedule = Schedule {
            maintenance: vec![Maintenance {
                scope: Scope::Named("Android版".into()),
                // 延長して終わりが未定になった後の予定。
                planned: Window {
                    start: instant(5),
                    end: Slot::Undecided,
                },
                actual: Window {
                    start: instant(5),
                    end: Slot::Unspecified,
                },
                disposition: Disposition::Scheduled,
                ongoing: Some(Ongoing {
                    as_of: TimeValue {
                        when: When::Instant {
                            at: Utc.with_ymd_and_hms(2026, 11, 12, 7, 30, 0).unwrap(),
                            zone: Zone::Named("Asia/Tokyo".into()),
                        },
                        certainty: Certainty::Inferred,
                        evidence: Vec::new(),
                    },
                    evidence: vec![Evidence {
                        field: Field::Body,
                        text: "現在メンテナンスを実施しております".into(),
                    }],
                }),
                changes: vec![Change::Extended {
                    from: instant(8),
                    end: Slot::Undecided,
                    evidence: Vec::new(),
                }],
                evidence: Vec::new(),
            }],
            periods: vec![Period {
                kind: PeriodKind::Claim,
                label: None,
                window: Window {
                    start: Slot::Unspecified,
                    end: Slot::Value(TimeValue {
                        when: When::Date {
                            date: NaiveDate::from_ymd_opt(2026, 11, 19).unwrap(),
                            zone: Zone::Offset(9 * 3600),
                        },
                        certainty: Certainty::Inferred,
                        evidence: Vec::new(),
                    }),
                },
                changes: vec![Change::Rescheduled {
                    from: Window {
                        start: Slot::Unspecified,
                        end: instant(14),
                    },
                    to: Window {
                        start: Slot::Unspecified,
                        end: Slot::Value(TimeValue {
                            when: When::Date {
                                date: NaiveDate::from_ymd_opt(2026, 11, 19).unwrap(),
                                zone: Zone::Offset(9 * 3600),
                            },
                            certainty: Certainty::Inferred,
                            evidence: Vec::new(),
                        }),
                    },
                    evidence: Vec::new(),
                }],
                evidence: Vec::new(),
            }],
            notes: vec![Note {
                code: "ambiguous_year".into(),
                message: "年を決められません".into(),
                evidence: Vec::new(),
            }],
        };
        let json = serde_json::to_string(&schedule).unwrap();
        assert_eq!(serde_json::from_str::<Schedule>(&json).unwrap(), schedule);
        assert_eq!(serde_json::to_string(&Schedule::default()).unwrap(), "{}");
    }

    #[test]
    fn reads_extensions_without_a_start() {
        // 延長する前の終わりが分からない延長は `from` を書かない形でも読める。
        let change: Change =
            serde_json::from_str(r#"{"kind":"extended","end":{"kind":"undecided"}}"#).unwrap();
        assert_eq!(
            change,
            Change::Extended {
                from: Slot::Unspecified,
                end: Slot::Undecided,
                evidence: Vec::new(),
            }
        );
    }
}
