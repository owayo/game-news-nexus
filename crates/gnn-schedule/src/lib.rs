//! お知らせの題名と本文から、メンテナンスの予定と結果、イベントなどの期間を読み取る。
//!
//! 結果の型は `gnn_core::schedule`。日本語の文は hasami (IPAdic を埋め込み) で形態素に分けて読む。
//! 通信・保存・画面には依存しない。保存済みの記事から何度でも同じ結果を作れる (端末の状態や
//! 今の時刻に左右されない) ようにする。
//!
//! 読み方 (段ごとにモジュールを分ける):
//! 1. 文書の構造 (`document`): 本文の HTML (なければテキスト) を行に分け、見出し・リスト項目・表の
//!    セル・取り消し線・「項目名 + 値」の行・追記の区画を付ける。根拠は正規化する前の原文から切り出す
//!    (`normalize` が正規化した位置から原文の位置へ戻す)。
//! 2. 日時の候補 (`temporal`): 日付・時刻・範囲・所要時間・地域時刻の明記を字句として拾う。年は決めない。
//! 3. 文の意味 (`clause` と `reading`): hasami の形態素から、節ごとに対象・動作・肯定否定・
//!    予定/推量/条件/完了・修飾節かどうかと、係っている日時を読む。
//! 4. メンテナンスとの結び付け (`facts`): 節の読み・見出し・項目名から、日時がメンテナンスの何
//!    (予定・実際の開始と終了、延長後の終わり、変更前) か、ほかの対象 (販売・ガチャ・受け取り) かを決める。
//! 5. 日時の確定 (`resolve`): 年・相対の日付・地域時刻を決める。決められないものは `Note` に残す。
//! 6. 記事の中の統合 (`merge`): 対象と時間帯ごとにまとめ、追記・変更前後・取り消し線から状態と変更を決める。
//!
//! 使い方:
//! - [`Extractor::extract`] で記事 1 件を読む。
//! - [`reconcile`] で同じソースの記事どうしを突き合わせ、別の記事で報告された完了・中止・延長を
//!   予告の記事のメンテナンスにも反映する。
//! - 保存した結果が [`extractor_id`] と違えば、読み直す。

use chrono::{DateTime, Utc};
use chrono_tz::Tz;
use gnn_core::Published;
use gnn_core::schedule::{Change, Disposition, Ongoing, Schedule, Window};

mod clause;
mod dict;
mod document;
mod facts;
mod html;
mod lexicon;
mod merge;
mod normalize;
mod reading;
mod reconcile;
mod resolve;
mod temporal;

/// 読み取りの規則の版。規則を変えたら上げる (保存済みの記事を読み直す)。
///
/// 1 は何も読まない仮の実装。2 が最初の読み取りの規則。3 で実施中の報告・延長する前の終わり・日程変更の後・
/// 期間の延長と日程変更を読み、複数の対象を挙げたメンテナンスを対象ごとの件に分けた。
pub const RULES_VERSION: u32 = 3;

/// 抽出器の識別子。規則の版・hasami の版・辞書の SHA-256 を含む。保存した結果の識別子と違えば読み直す。
pub fn extractor_id() -> String {
    format!(
        "gnn-schedule/{RULES_VERSION} hasami/{} ipadic/{}",
        env!("GNN_HASAMI_TAG"),
        &env!("GNN_DICT_SHA256")[..12]
    )
}

/// 読み取る記事。
#[derive(Debug, Clone)]
pub struct Article<'a> {
    pub title: &'a str,
    /// 整形済みの本文の HTML (保存済みの `body_html`)。見出し・リスト・表・取り消し線を読むのに使う。
    /// 空なら `body_text` だけを読む。
    pub body_html: &'a str,
    /// 本文のテキスト (保存済みの `body_text`)。
    pub body_text: &'a str,
    pub context: Context,
}

/// 日時を決めるための文脈。
#[derive(Debug, Clone, Copy)]
pub struct Context {
    /// 記事の投稿日時。
    pub published: Option<Published>,
    /// 初めて見つけた日時 (保存済みの値。読み直しても変わらない)。投稿日時が分からないときの基準。
    pub first_seen_at: DateTime<Utc>,
    /// 地域時刻の明記がない本文の日時を読む地域時刻 (ソースの `schedule.timezone`、なければ `timezone`)。
    pub timezone: Tz,
    /// 「サーバー時間」と書かれた日時の地域時刻 (ソースの `schedule.server_timezone`)。
    pub server_timezone: Option<Tz>,
}

/// 抽出器。解析器 (hasami) の作業領域を持つので、スレッドごとに 1 つ作る (辞書は共有する)。
pub struct Extractor {
    analyzer: hasami::Analyzer,
}

impl Default for Extractor {
    fn default() -> Self {
        Self::new()
    }
}

impl Extractor {
    pub fn new() -> Self {
        Self {
            analyzer: dict::analyzer(),
        }
    }

    /// 記事 1 件を読む。
    pub fn extract(&mut self, article: &Article<'_>) -> Schedule {
        let (collected, mut resolver) =
            facts::collect(article, facts::Engine::Morphology(&mut self.analyzer));
        merge::merge(collected, &mut resolver)
    }

    /// 読みの途中 (単位・塊・節・読み) を人が見る形にする。調査用。
    #[doc(hidden)]
    pub fn debug(&mut self, article: &Article<'_>) -> String {
        facts::debug(article, &mut self.analyzer)
    }

    /// 形態素解析を使わずに読む (正規表現と文書の構造だけ)。評価で形態素解析が効いた箇所を比べるためのもの。
    #[doc(hidden)]
    pub fn extract_without_morphology(&mut self, article: &Article<'_>) -> Schedule {
        let (collected, mut resolver) = facts::collect(article, facts::Engine::Baseline);
        merge::merge(collected, &mut resolver)
    }
}

/// 突き合わせる記事 1 件。
#[derive(Debug, Clone, Copy)]
pub struct SourceArticle<'a> {
    pub entry_id: i64,
    pub published: Option<Published>,
    pub first_seen_at: DateTime<Utc>,
    pub schedule: &'a Schedule,
}

/// 突き合わせたあとのメンテナンス 1 件。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolved {
    pub entry_id: i64,
    /// 記事の `Schedule::maintenance` の位置。
    pub index: usize,
    /// 別の記事の報告も反映した結果。
    pub disposition: Disposition,
    /// 別の記事の延長・日程変更も反映した予定の時間帯。
    pub planned: Window,
    /// 別の記事の報告で分かったものも含む実際の時間帯。
    pub actual: Window,
    /// 延長・日程変更。記事の `Maintenance::changes` に、別の記事で報告された延長・日程変更を、
    /// 報告の記事の古い順 (同じ記事の中は書かれた順) に足したもの。同じ変更は 1 つだけ。
    pub changes: Vec<Change>,
    /// 「実施中」の報告。記事の `Maintenance::ongoing` と、結び付けた別の記事の報告のうち、いちばん新しいもの。
    pub ongoing: Option<Ongoing>,
    /// 結果や変更を報告した別の記事 (この記事の記述だけで決まったなら `None`)。
    pub resolved_by: Option<i64>,
}

/// 同じソースの記事の読み取り結果を突き合わせる。
///
/// 対象・時間帯で 1 つに決まるときだけ結び付ける (日付が近いだけでは結び付けない)。
/// 返す件数は、入力のメンテナンスの件数と同じ。
pub fn reconcile(articles: &[SourceArticle<'_>]) -> Vec<Resolved> {
    reconcile::reconcile(articles)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extractor_id_names_rules_and_dictionary() {
        let id = extractor_id();
        assert!(id.starts_with("gnn-schedule/"), "{id}");
        assert!(id.contains("hasami/v"), "{id}");
        assert!(id.contains("ipadic/"), "{id}");
    }
}
