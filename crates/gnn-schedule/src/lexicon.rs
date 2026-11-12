//! 語の一覧と、名詞句 (正規化した表層をつないだもの) の意味。
//!
//! IPAdic は複合語を細かく分ける (`日時` → `日` + `時`、`SPクラウド版` → `SP` + `クラ` + `ウド` + `版`) ので、
//! 名詞句の意味は形態素ごとではなく、名詞の並びをつないだ文字列の語の並びで決める。

use gnn_core::schedule::PeriodKind;

/// 述語の意味。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(crate) enum Action {
    /// 実施・行う。
    Execute,
    /// 開始。
    Start,
    /// 終了・完了。
    End,
    /// 延長・継続。
    Extend,
    /// 延期。
    Postpone,
    /// 中止・取りやめ・見送り。
    Cancel,
    /// (サービスの) 再開。メンテナンスの終わりを表す。
    Resume,
    /// 予定・予想・見込み。
    Plan,
    /// 変更。
    Change,
    /// 開催・開放・公開 (イベントなど)。
    Hold,
    /// 販売。
    Sell,
    /// 配布・受け取り・交換。
    Distribute,
    /// ある・ござる。
    Exist,
}

/// 述語の原形 (または サ変名詞) の意味。
pub(crate) fn action_of(lemma: &str) -> Option<Action> {
    Some(match lemma {
        // IPAdic は「行った」を 行く と読むことがある (「メンテナンスを行った」の 行う)。
        "実施" | "行う" | "行く" | "実行" | "執り行う" | "実施中" => Action::Execute,
        "開始" | "始める" | "始まる" | "スタート" => Action::Start,
        "終了" | "完了" | "終える" | "終わる" | "明ける" | "解除" | "完了済み" | "終了済み" => {
            Action::End
        }
        "延長" | "継続" | "再延長" => Action::Extend,
        "延期" | "順延" | "繰り下げ" | "繰り下げる" | "先送り" => Action::Postpone,
        "中止" | "取りやめる" | "取り止める" | "取りやめ" | "取り止め" | "見送る" | "見送り"
        | "見合わせる" | "見合わせ" | "キャンセル" => Action::Cancel,
        "再開" => Action::Resume,
        "予定" | "予想" | "見込む" | "見込み" | "想定" | "予想される" => {
            Action::Plan
        }
        "変更" | "更新" => Action::Change,
        "開催" | "開放" | "公開" | "実装" | "登場" | "追加" | "配信" | "解放" | "開始予定" => {
            Action::Hold
        }
        "販売" | "発売" | "再販" | "販売開始" => Action::Sell,
        "配布"
        | "付与"
        | "受け取る"
        | "受け取り"
        | "お受け取り"
        | "受取"
        | "交換"
        | "送る"
        | "お送り"
        | "お届け"
        | "プレゼント"
        | "補填"
        | "補てん"
        | "補償"
        | "お受け取りいただける"
        | "受け取れる" => Action::Distribute,
        "ある" | "ござる" | "あり" => Action::Exist,
        _ => return None,
    })
}

/// 軽い動詞 (サ変名詞を述語にする・目的語の動作をそのまま表す)。
pub(crate) fn is_light_verb(lemma: &str) -> bool {
    matches!(
        lemma,
        "する" | "いたす" | "致す" | "させる" | "できる" | "なさる" | "行う" | "おこなう"
    )
}

/// メンテナンスの語を含むか (`メンテナンス` `メンテ`)。
pub(crate) fn has_maintenance(text: &str) -> bool {
    text.contains("メンテ")
}

/// メンテナンスに準じる語 (停止を伴うかは文脈で決める)。
pub(crate) fn has_update(text: &str) -> bool {
    text.contains("アップデート") || text.contains("バージョンアップ")
}

/// 名詞句が指す「時」の役割 (`終了時間` `開始日時` `実施期間` など)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TimeRole {
    /// 時間帯 (日時・日程・期間・スケジュール)。
    Window,
    Start,
    End,
    /// 期限 (受け取りなど。終わりとして読む)。
    Deadline,
    /// 所要時間。
    Duration,
    /// 対象の条件 (`補償対象` の日時は期間ではない)。
    Eligibility,
    /// 予備日。
    Reserve,
}

/// 名詞句の「時」の役割。
pub(crate) fn time_role(text: &str) -> Option<TimeRole> {
    let text = text.trim_end_matches(['(', ')', ' ', ':']);
    if text.ends_with("対象") || text.ends_with("条件") {
        return Some(TimeRole::Eligibility);
    }
    if text.contains("予備日") {
        return Some(TimeRole::Reserve);
    }
    if text.contains("所要時間") || text.ends_with("所要") {
        return Some(TimeRole::Duration);
    }
    const START: &[&str] = &[
        "開始日時",
        "開始時間",
        "開始時刻",
        "開始日",
        "開始予定",
        "開始",
        "開催日",
        "受付開始",
        "販売開始",
    ];
    const END: &[&str] = &[
        "終了日時",
        "終了時間",
        "終了時刻",
        "終了日",
        "終了予定時刻",
        "終了予定時間",
        "終了予定日時",
        "終了予定",
        "終了",
        "完了予定",
        "完了",
        "締め切り",
        "締切",
    ];
    const DEADLINE: &[&str] = &["期限", "有効期間"];
    const WINDOW: &[&str] = &[
        "日時",
        "日程",
        "時間",
        "時刻",
        "期間",
        "スケジュール",
        "予定日",
        "予定",
        "実施日",
        "日付",
        "時間帯",
        "開催期間",
    ];
    if END.iter().any(|word| text.ends_with(word)) {
        return Some(TimeRole::End);
    }
    if START.iter().any(|word| text.ends_with(word)) {
        return Some(TimeRole::Start);
    }
    if DEADLINE.iter().any(|word| text.ends_with(word)) {
        return Some(TimeRole::Deadline);
    }
    if WINDOW.iter().any(|word| text.ends_with(word)) {
        return Some(TimeRole::Window);
    }
    None
}

/// 「項目名 値」の項目名になる語か (期間・日時などで終わる短い語)。
pub(crate) fn is_time_label(label: &str) -> bool {
    let label = label.trim();
    !label.is_empty()
        && label.chars().count() <= 16
        && !label.contains('。')
        && (time_role(label).is_some_and(|role| role != TimeRole::Eligibility)
            || is_change_label(label))
}

/// 変更の前後を表す項目名 (`変更前` `変更後` `延長後`)。
pub(crate) fn is_change_label(label: &str) -> bool {
    ["変更前", "変更後", "延長前", "延長後", "修正前", "修正後"]
        .iter()
        .any(|word| label.trim().ends_with(word))
}

/// 参照の語 (メンテナンス「後」「中」など。その名詞句はメンテナンスそのものではなく、その前後を指す)。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Reference {
    After,
    Before,
    During,
}

/// 名詞句の末尾の参照の語。
pub(crate) fn reference_of(text: &str) -> Option<Reference> {
    let text = text.trim_end();
    if is_change_label(text) {
        // `変更前` `延長後` は前後の参照ではなく、変更の前後の値の項目名。
        return None;
    }
    const AFTER: &[&str] = &[
        "終了後",
        "明け",
        "後",
        "以降",
        "以後",
        "直後",
        "完了後",
        "終了直後",
        "あけ",
    ];
    const BEFORE: &[&str] = &["前", "以前", "直前", "開始前", "まで"];
    const DURING: &[&str] = &["中", "期間中", "時", "の際", "際", "時間中", "作業中"];
    if AFTER.iter().any(|word| text.ends_with(word)) {
        return Some(Reference::After);
    }
    if BEFORE.iter().any(|word| text.ends_with(word)) {
        return Some(Reference::Before);
    }
    if DURING.iter().any(|word| text.ends_with(word)) && !text.ends_with("実施中") {
        return Some(Reference::During);
    }
    None
}

/// 名詞句に含まれる動作の語 (`メンテナンス延長` の `延長`、`メンテナンス終了` の `終了`)。
pub(crate) fn actions_in(text: &str) -> Vec<Action> {
    const WORDS: &[(&str, Action)] = &[
        ("実施", Action::Execute),
        ("開始", Action::Start),
        ("終了", Action::End),
        ("完了", Action::End),
        ("延長", Action::Extend),
        ("継続", Action::Extend),
        ("延期", Action::Postpone),
        ("中止", Action::Cancel),
        ("見送り", Action::Cancel),
        ("取りやめ", Action::Cancel),
        ("再開", Action::Resume),
        ("予定", Action::Plan),
        ("変更", Action::Change),
    ];
    let mut found: Vec<(usize, Action)> = WORDS
        .iter()
        .filter_map(|(word, action)| text.find(word).map(|at| (at, *action)))
        .collect();
    found.sort_by_key(|(at, _)| *at);
    found.into_iter().map(|(_, action)| action).collect()
}

/// メンテナンスではない対象の種類 (名詞句の後ろの語ほど優先する)。
pub(crate) fn period_kind(text: &str) -> Option<PeriodKind> {
    const WORDS: &[(&str, PeriodKind)] = &[
        ("ガチャ", PeriodKind::Gacha),
        ("召喚", PeriodKind::Gacha),
        ("跳躍", PeriodKind::Gacha),
        ("祈願", PeriodKind::Gacha),
        ("調律", PeriodKind::Gacha),
        ("スカウト", PeriodKind::Gacha),
        ("ピックアップ", PeriodKind::Gacha),
        ("変調", PeriodKind::Gacha),
        ("イベント", PeriodKind::Event),
        ("開催", PeriodKind::Event),
        ("解放", PeriodKind::Event),
        ("開放", PeriodKind::Event),
        ("公開", PeriodKind::Event),
        ("総力戦", PeriodKind::Event),
        ("討伐", PeriodKind::Event),
        ("ランキング", PeriodKind::Event),
        ("レイド", PeriodKind::Event),
        ("大会", PeriodKind::Event),
        ("キャンペーン", PeriodKind::Campaign),
        ("ミッション", PeriodKind::Campaign),
        ("ログインボーナス", PeriodKind::Campaign),
        ("ログボ", PeriodKind::Campaign),
        ("投票", PeriodKind::Campaign),
        ("販売", PeriodKind::Sale),
        ("ショップ", PeriodKind::Sale),
        ("パック", PeriodKind::Sale),
        ("セール", PeriodKind::Sale),
        ("商品", PeriodKind::Sale),
        ("価格", PeriodKind::Sale),
        ("購入", PeriodKind::Sale),
        ("発売", PeriodKind::Sale),
        ("SALE", PeriodKind::Sale),
        ("受け取り", PeriodKind::Claim),
        ("受取", PeriodKind::Claim),
        ("受け取", PeriodKind::Claim),
        ("配布", PeriodKind::Claim),
        ("補填", PeriodKind::Claim),
        ("補てん", PeriodKind::Claim),
        ("補償", PeriodKind::Claim),
        ("お詫び", PeriodKind::Claim),
        ("交換", PeriodKind::Claim),
        ("プレゼント", PeriodKind::Claim),
        ("報酬", PeriodKind::Claim),
        ("シリアルコード", PeriodKind::Claim),
        ("使用期限", PeriodKind::Other),
        ("有効期限", PeriodKind::Other),
        ("有効期間", PeriodKind::Other),
    ];
    let mut best: Option<(usize, PeriodKind)> = None;
    for (word, kind) in WORDS {
        if let Some(at) = text.rfind(word) {
            let end = at + word.len();
            // 後ろの語ほど優先 (同じ位置なら長い語)。
            if best.is_none_or(|(best_end, _)| end > best_end) {
                best = Some((end, *kind));
            }
        }
    }
    best.map(|(_, kind)| kind)
}

/// 種類の強さ。「開催」だけで決めたイベントは、見出しなどの手がかりで上書きしてよい。
pub(crate) fn is_generic_kind(text: &str, kind: PeriodKind) -> bool {
    kind == PeriodKind::Event
        && !text.contains("イベント")
        && !text.contains("総力戦")
        && !text.contains("ランキング")
}

/// 対象 (プラットフォーム) の名前を拾う。`全プラットフォーム` は `None` (全体) として返す。
pub(crate) fn scope_names(text: &str) -> Vec<Option<String>> {
    let mut out: Vec<Option<String>> = Vec::new();
    for word in [
        "全プラットフォーム",
        "すべてのプラットフォーム",
        "全てのプラットフォーム",
        "全サーバー",
        "全環境",
    ] {
        if text.contains(word) {
            out.push(None);
        }
    }
    // `〇〇版` を拾う。版の前の英数字・カタカナ・空白をさかのぼる。
    let mut search = 0;
    while let Some(found) = text[search..].find('版') {
        let at = search + found;
        search = at + '版'.len_utf8();
        let before = &text[..at];
        let mut start = at;
        for (index, ch) in before.char_indices().rev() {
            if ch.is_ascii_alphanumeric()
                || ch == ' '
                || ('\u{30a1}'..='\u{30fa}').contains(&ch)
                || matches!(ch, 'ー' | '/' | '.')
            {
                start = index;
            } else {
                break;
            }
        }
        let name = text[start..search]
            .trim()
            .trim_start_matches(|ch: char| !ch.is_alphanumeric());
        let core = name.trim_end_matches('版').trim();
        // 表記の空白は 1 つにそろえる (`GAMES PLAYER版` `iOS GAMES STORE版`)。
        let name = name.split_whitespace().collect::<Vec<_>>().join(" ");
        if core.chars().count() < 2 || core.chars().all(|ch| ch.is_ascii_digit() || ch == '.') {
            continue;
        }
        if !is_platform(core) {
            continue;
        }
        if !out.iter().any(|known| known.as_deref() == Some(&name)) {
            out.push(Some(name));
        }
    }
    out
}

fn is_platform(core: &str) -> bool {
    const WORDS: &[&str] = &[
        "PC",
        "ブラウザ",
        "PLAYER",
        "Android",
        "iOS",
        "iPhone",
        "App Store",
        "AppStore",
        "Google Play",
        "GooglePlay",
        "SP",
        "クラウド",
        "スマートフォン",
        "スマホ",
        "Steam",
        "PS5",
        "PS4",
        "Switch",
        "Mac",
        "Windows",
        "Xbox",
        "STORE",
        "DMM",
    ];
    WORDS.iter().any(|word| core.contains(word))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_roles_and_kinds() {
        assert_eq!(time_role("メンテナンス終了時間"), Some(TimeRole::End));
        assert_eq!(time_role("メンテナンス実施日時"), Some(TimeRole::Window));
        assert_eq!(time_role("アップデート開始日時"), Some(TimeRole::Start));
        assert_eq!(time_role("受取期限"), Some(TimeRole::Deadline));
        assert_eq!(time_role("補償対象"), Some(TimeRole::Eligibility));
        assert_eq!(period_kind("受け取り期間"), Some(PeriodKind::Claim));
        assert_eq!(period_kind("ピックアップガチャ"), Some(PeriodKind::Gacha));
        assert_eq!(period_kind("記念パック販売"), Some(PeriodKind::Sale));
        assert_eq!(period_kind("ガチャの証交換所"), Some(PeriodKind::Claim));
        assert_eq!(reference_of("メンテナンス終了後"), Some(Reference::After));
        assert_eq!(reference_of("メンテナンス中"), Some(Reference::During));
        assert_eq!(reference_of("メンテナンス"), None);
        assert_eq!(
            actions_in("メンテナンス延長終了"),
            vec![Action::Extend, Action::End]
        );
    }

    #[test]
    fn finds_platforms() {
        assert_eq!(
            scope_names("・Android版:11月6日 / SPクラウド版 / iOS GAMES STORE版"),
            vec![
                Some("Android版".to_string()),
                Some("SPクラウド版".to_string()),
                Some("iOS GAMES STORE版".to_string())
            ]
        );
        assert_eq!(scope_names("全プラットフォーム"), vec![None]);
        assert!(scope_names("第2版の資料").is_empty());
    }
}
