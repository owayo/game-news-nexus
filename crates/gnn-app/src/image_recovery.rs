//! 取得 URL で取れなかった画像の参照の、取り直しの待ち行列。
//!
//! 取得の定義の `url_identity.images` に合う画像は、記事には参照 (`https://source-image.invalid/...`) で持ち、
//! 取りに行くときは記録した配信元の URL (署名付き) を使う。キャッシュになく、その URL の署名の期限も切れて
//! いると取れないので、画面の HTTP クライアントはソースの取り直しを頼む ([`RecoveryQueue::request`])。
//! 取り直せば新しい取得 URL が記録され、取得のあとの先読みが画像をキャッシュに入れる。
//!
//! - 1 つの記事の画像はほぼ同時に失敗するので、同じソースの頼みは [`GATHER`] 待ってまとめ、1 回の取得にする
//!   (あとから来た頼みで待ち時間を延ばさない)。
//! - 403 は配信元の WAF のこともあり、取り直しても取れるとは限らない。同じ画像は [`IMAGE_INTERVAL`] あけて、
//!   アプリを起動しているあいだに [`IMAGE_ATTEMPTS`] 回まで。新しい取得 URL でも取れなかった画像・新しい取得 URL が
//!   見つからなかった画像は諦める ([`RecoveryQueue::give_up`])。同じソースは、前の取り直しが終わってから
//!   [`SOURCE_INTERVAL`] あけ、[`SOURCE_WINDOW`] に [`SOURCE_RUNS`] 回まで。新しい取得 URL でも取れない画像が
//!   重なったソースは [`pause`](RecoveryQueue::pause) で休ませる。
//! - 取得中のソース (定期の取得など) は、終わるまで待つ (その取得で直るかもしれない)。取得に失敗したときなど、
//!   時間をおけば取れそうなら、取り直す側が回数の上限の中で予約し直す ([`RecoveryQueue::retry_later`])。
//!
//! 時刻は呼び出し側が渡す。ここは通信も保存もしない。

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::time::{Duration, Instant};

/// 頼みをまとめる時間。
pub const GATHER: Duration = Duration::from_secs(2);
/// 同じソースの取り直しのあいだ (前の取り直しが終わってから)。
pub const SOURCE_INTERVAL: Duration = Duration::from_secs(60);
/// 同じソースの取り直しの回数を数える時間。
pub const SOURCE_WINDOW: Duration = Duration::from_secs(3600);
/// [`SOURCE_WINDOW`] のあいだに同じソースを取り直す回数の上限。
pub const SOURCE_RUNS: usize = 3;
/// 同じ画像の取り直しのあいだ。
pub const IMAGE_INTERVAL: Duration = Duration::from_secs(30 * 60);
/// 同じ画像を取り直す回数の上限 (アプリを起動しているあいだ)。
pub const IMAGE_ATTEMPTS: u32 = 3;
/// 取得中のソースの頼みを、また見に来るまでの時間。
pub const BUSY_RETRY: Duration = Duration::from_secs(5);

/// 取り直しの待ち行列。
#[derive(Debug, Default)]
pub struct RecoveryQueue {
    images: HashMap<(String, String), Attempts>,
    sources: HashMap<String, SourceQueue>,
}

/// 画像ごとの試み。
#[derive(Debug, Default)]
struct Attempts {
    count: u32,
    last: Option<Instant>,
}

/// ソースごとの待っている頼み。
#[derive(Debug, Default)]
struct SourceQueue {
    /// 待っている画像の鍵と、画面が取れなかった取得 URL (予約し直した頼みは `None`)。
    pending: BTreeMap<String, Option<String>>,
    /// 待っている頼みのうち、いちばん古いものを受けた時刻。
    since: Option<Instant>,
    /// これより前には取り直さない (取得の失敗の待ち時間・予約し直した頼み)。
    not_before: Option<Instant>,
    /// 休ませている期限 (新しい取得 URL でも取れない画像が重なった)。
    paused_until: Option<Instant>,
    /// 取り直しが終わった時刻 ([`SOURCE_WINDOW`] の中のもの)。
    runs: VecDeque<Instant>,
}

/// 取り直す画像 1 つ。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryItem {
    pub key: String,
    /// 画面がこの取得 URL で取れなかった。記録した取得 URL がこれと違えば (失敗のあとで取得があった)、
    /// ソースを取り直す前にその URL で取ってみる。予約し直した頼みでは `None` (記録した URL で取ってみる)。
    pub failed_url: Option<String>,
}

/// 1 つのソースで、まとめて取り直す画像。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryBatch {
    pub source_id: String,
    pub items: Vec<RecoveryItem>,
}

impl RecoveryQueue {
    /// 画面からの頼みを受ける。待ち行列に入れたら `true`。同じ画像をもう待っている (取れなかった取得 URL だけ
    /// 新しくする)・上限を超えた・諦めた・休ませているソースの頼みは `false`。
    pub fn request(
        &mut self,
        source_id: &str,
        key: &str,
        failed_url: Option<&str>,
        now: Instant,
    ) -> bool {
        if self
            .sources
            .get(source_id)
            .and_then(|queue| queue.paused_until)
            .is_some_and(|until| now < until)
        {
            return false;
        }
        let attempts = self
            .images
            .entry((source_id.to_string(), key.to_string()))
            .or_default();
        if attempts.count >= IMAGE_ATTEMPTS
            || attempts
                .last
                .is_some_and(|last| now < last + IMAGE_INTERVAL)
        {
            return false;
        }
        let queue = self.sources.entry(source_id.to_string()).or_default();
        if let Some(known) = queue.pending.get_mut(key) {
            if failed_url.is_some() {
                *known = failed_url.map(str::to_string);
            }
            return false;
        }
        queue
            .pending
            .insert(key.to_string(), failed_url.map(str::to_string));
        queue.since.get_or_insert(now);
        true
    }

    /// 今取り直してよいソースの頼みを取り出す。`busy` は取得中のソース (取り出さない)。
    /// 取り出した画像は試みに数える。取り直しが終わったら [`RecoveryQueue::finished`] を呼ぶ。
    pub fn take_due(&mut self, now: Instant, busy: &HashSet<String>) -> Vec<RecoveryBatch> {
        let mut due: Vec<String> = self
            .sources
            .iter_mut()
            .filter(|(id, _)| !busy.contains(*id))
            .filter_map(|(id, queue)| {
                queue.forget_old_runs(now);
                queue
                    .due_at()
                    .is_some_and(|at| at <= now)
                    .then(|| id.clone())
            })
            .collect();
        due.sort();
        due.into_iter()
            .map(|source_id| {
                let queue = self.sources.get_mut(&source_id).expect("source");
                let pending = std::mem::take(&mut queue.pending);
                queue.since = None;
                queue.not_before = None;
                let items: Vec<RecoveryItem> = pending
                    .into_iter()
                    .map(|(key, failed_url)| RecoveryItem { key, failed_url })
                    .collect();
                for item in &items {
                    let attempts = self
                        .images
                        .entry((source_id.clone(), item.key.clone()))
                        .or_default();
                    attempts.count += 1;
                    attempts.last = Some(now);
                }
                RecoveryBatch { source_id, items }
            })
            .collect()
    }

    /// 取り直しが終わった (ソースを取得した)。次の取り直しの間隔と回数を、ここから数える。
    pub fn finished(&mut self, source_id: &str, now: Instant) {
        let queue = self.sources.entry(source_id.to_string()).or_default();
        queue.runs.push_back(now);
        queue.forget_old_runs(now);
    }

    /// 取り出したが取り直さなかった頼み (取得中だった・取得の失敗の待ち時間の中だった・1 回の取得で詳細を
    /// 取り直せる記事の数を超えた) を戻す。試みには数えない。`not_before` を渡すと、それまでは取り直さない。
    pub fn put_back(
        &mut self,
        source_id: &str,
        items: Vec<RecoveryItem>,
        now: Instant,
        not_before: Option<Instant>,
    ) {
        if items.is_empty() {
            return;
        }
        for item in &items {
            if let Some(attempts) = self
                .images
                .get_mut(&(source_id.to_string(), item.key.clone()))
            {
                attempts.count = attempts.count.saturating_sub(1);
            }
        }
        let queue = self.sources.entry(source_id.to_string()).or_default();
        for item in items {
            let failed_url = queue.pending.entry(item.key).or_insert(None);
            if failed_url.is_none() {
                *failed_url = item.failed_url;
            }
        }
        queue.since.get_or_insert(now);
        if let Some(at) = not_before {
            queue.not_before = Some(queue.not_before.map_or(at, |known| known.max(at)));
        }
    }

    /// 取り直したが、時間をおけば取れそうな画像 (取得の失敗・通信の失敗) を予約し直す。回数の上限の中で、
    /// 前の試みから [`IMAGE_INTERVAL`] あけ、`not_before` (配信元が指定した待ち時間) より後に取り直す。
    /// 予約し直した画像の数を返す。
    pub fn retry_later(
        &mut self,
        source_id: &str,
        keys: &[String],
        now: Instant,
        not_before: Option<Instant>,
    ) -> usize {
        let mut retried = 0;
        let mut at = not_before.unwrap_or(now).max(now);
        for key in keys {
            let Some(attempts) = self.images.get(&(source_id.to_string(), key.clone())) else {
                continue;
            };
            if attempts.count >= IMAGE_ATTEMPTS {
                continue;
            }
            at = at.max(attempts.last.unwrap_or(now) + IMAGE_INTERVAL);
            let queue = self.sources.entry(source_id.to_string()).or_default();
            queue.pending.entry(key.clone()).or_insert(None);
            retried += 1;
        }
        if retried > 0 {
            let queue = self.sources.entry(source_id.to_string()).or_default();
            queue.since.get_or_insert(now);
            queue.not_before = Some(queue.not_before.map_or(at, |known| known.max(at)));
        }
        retried
    }

    /// 画像の取り直しを諦める (新しい取得 URL でも取れなかった・新しい取得 URL が見つからなかった)。
    /// アプリを起動しているあいだは、画面から頼まれても受けない。
    pub fn give_up(&mut self, source_id: &str, key: &str) {
        let attempts = self
            .images
            .entry((source_id.to_string(), key.to_string()))
            .or_default();
        attempts.count = IMAGE_ATTEMPTS;
        if let Some(queue) = self.sources.get_mut(source_id) {
            queue.pending.remove(key);
        }
    }

    /// ソースの取り直しを `until` まで休ませ、待っている頼みも捨てる (新しい取得 URL でも取れない画像が
    /// 重なった。WAF など、取り直しても取れない理由があるらしい)。
    pub fn pause(&mut self, source_id: &str, until: Instant) {
        let queue = self.sources.entry(source_id.to_string()).or_default();
        queue.paused_until = Some(until);
        queue.pending.clear();
        queue.since = None;
        queue.not_before = None;
    }

    /// 次に [`RecoveryQueue::take_due`] を呼ぶ時刻。待っている頼みがなければ `None`。
    pub fn next_due(&self, now: Instant, busy: &HashSet<String>) -> Option<Instant> {
        self.sources
            .iter()
            .filter_map(|(id, queue)| {
                let at = queue.due_at()?;
                Some(if busy.contains(id) {
                    at.max(now + BUSY_RETRY)
                } else {
                    at
                })
            })
            .min()
    }
}

impl SourceQueue {
    fn forget_old_runs(&mut self, now: Instant) {
        while self
            .runs
            .front()
            .is_some_and(|run| *run + SOURCE_WINDOW <= now)
        {
            self.runs.pop_front();
        }
    }

    /// 待っている頼みを取り直してよくなる時刻。
    fn due_at(&self) -> Option<Instant> {
        let since = self.since.filter(|_| !self.pending.is_empty())?;
        let mut at = since + GATHER;
        if let Some(last) = self.runs.back() {
            at = at.max(*last + SOURCE_INTERVAL);
        }
        if self.runs.len() >= SOURCE_RUNS
            && let Some(first) = self.runs.get(self.runs.len() - SOURCE_RUNS)
        {
            at = at.max(*first + SOURCE_WINDOW);
        }
        for limit in [self.not_before, self.paused_until].into_iter().flatten() {
            at = at.max(limit);
        }
        Some(at)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    fn item(key: &str, failed_url: Option<&str>) -> RecoveryItem {
        RecoveryItem {
            key: key.into(),
            failed_url: failed_url.map(str::to_string),
        }
    }

    fn batch(source_id: &str, keys: &[&str]) -> RecoveryBatch {
        RecoveryBatch {
            source_id: source_id.into(),
            items: keys.iter().map(|key| item(key, None)).collect(),
        }
    }

    fn keys(batch: &RecoveryBatch) -> Vec<&str> {
        batch.items.iter().map(|item| item.key.as_str()).collect()
    }

    #[test]
    fn gathers_requests_per_source_and_image() {
        let start = Instant::now();
        let none = HashSet::new();
        let mut queue = RecoveryQueue::default();
        assert_eq!(queue.next_due(start, &none), None);

        // 1 つの記事の画像がそろって失敗する。同じ画像の頼みは 1 つにまとめ、取れなかった URL は新しいものにする。
        assert!(queue.request("gc", "b", Some("https://cdn/b?s=1"), start));
        assert!(queue.request("gc", "a", Some("https://cdn/a?s=1"), start + GATHER / 4));
        assert!(!queue.request("gc", "a", Some("https://cdn/a?s=2"), start + GATHER / 2));
        assert!(queue.request("abyss", "x", None, start + Duration::from_secs(1)));

        // まとめる時間が過ぎるまでは取り出さない。あとから来た頼みで延ばさない。
        assert!(
            queue
                .take_due(start + Duration::from_secs(1), &none)
                .is_empty()
        );
        assert_eq!(queue.next_due(start, &none), Some(start + GATHER));
        assert_eq!(
            queue.take_due(start + GATHER, &none),
            vec![RecoveryBatch {
                source_id: "gc".into(),
                items: vec![
                    item("a", Some("https://cdn/a?s=2")),
                    item("b", Some("https://cdn/b?s=1")),
                ],
            }]
        );
        assert_eq!(
            queue.take_due(start + Duration::from_secs(3), &none),
            vec![batch("abyss", &["x"])]
        );
        assert_eq!(queue.next_due(start + Duration::from_secs(3), &none), None);
    }

    #[test]
    fn limits_how_often_a_source_and_an_image_are_recovered() {
        let start = Instant::now();
        let none = HashSet::new();
        let mut queue = RecoveryQueue::default();
        assert!(queue.request("gc", "a", None, start));
        assert_eq!(queue.take_due(start + GATHER, &none).len(), 1);
        let done = start + Duration::from_secs(10);
        queue.finished("gc", done);

        // 同じ画像は 30 分あける (WAF の 403 などで、取り直しても取れない画像を何度も頼まない)。
        let soon = start + Duration::from_secs(11);
        assert!(!queue.request("gc", "a", None, soon));
        // 別の画像は受けるが、同じソースの取り直しは、前の取り直しが終わってから 60 秒あける。
        assert!(queue.request("gc", "b", None, soon));
        assert!(queue.take_due(soon + GATHER, &none).is_empty());
        assert_eq!(queue.next_due(soon, &none), Some(done + SOURCE_INTERVAL));
        assert_eq!(
            queue.take_due(done + SOURCE_INTERVAL, &none),
            vec![batch("gc", &["b"])]
        );
        queue.finished("gc", done + SOURCE_INTERVAL);

        // 同じソースは 1 時間に 3 回まで (画像ごとの上限だけでは、別の画像の頼みで毎分取り直してしまう)。
        let third = done + SOURCE_INTERVAL * 2;
        assert!(queue.request("gc", "c", None, third));
        assert_eq!(queue.take_due(third + GATHER, &none).len(), 1);
        queue.finished("gc", third + GATHER);
        let fourth = third + GATHER + SOURCE_INTERVAL;
        assert!(queue.request("gc", "d", None, fourth));
        assert!(queue.take_due(fourth + GATHER, &none).is_empty());
        assert_eq!(queue.next_due(fourth, &none), Some(done + SOURCE_WINDOW));
        assert_eq!(queue.take_due(done + SOURCE_WINDOW, &none).len(), 1);

        // 同じ画像は、起動しているあいだに 3 回まで。
        let mut taken = start + GATHER;
        let mut at = start;
        for _ in 1..IMAGE_ATTEMPTS {
            at = taken + IMAGE_INTERVAL + SOURCE_WINDOW;
            queue.finished("gc", at);
            assert!(queue.request("gc", "a", None, at));
            taken = at + SOURCE_INTERVAL;
            assert_eq!(queue.take_due(taken, &none), vec![batch("gc", &["a"])]);
        }
        assert!(!queue.request("gc", "a", None, at + IMAGE_INTERVAL * 4));
        assert_eq!(queue.retry_later("gc", &["a".into()], at, None), 0);
    }

    #[test]
    fn waits_for_running_fetches_and_failure_backoff() {
        let start = Instant::now();
        let mut queue = RecoveryQueue::default();
        let busy: HashSet<String> = HashSet::from(["gc".to_string()]);
        let none = HashSet::new();
        assert!(queue.request("gc", "a", Some("https://cdn/a?s=1"), start));
        // 取得中のソースは取り出さず、少しあとで見に来る。
        assert!(queue.take_due(start + GATHER, &busy).is_empty());
        assert_eq!(
            queue.next_due(start + GATHER, &busy),
            Some(start + GATHER + BUSY_RETRY)
        );
        let taken = queue.take_due(start + GATHER, &none).remove(0);

        // 取り出したあとで取得が始まっていたら戻す。試みには数えず、取れなかった URL も残す。
        queue.put_back("gc", taken.items, start + GATHER, None);
        let taken = queue.take_due(start + GATHER * 2, &none).remove(0);
        assert_eq!(taken.items, vec![item("a", Some("https://cdn/a?s=1"))]);
        // 取得の失敗の待ち時間の中なら、それが明けるまで待つ。
        let backoff = start + Duration::from_secs(600);
        queue.put_back("gc", taken.items, start + GATHER * 2, Some(backoff));
        assert!(
            queue
                .take_due(backoff - Duration::from_secs(1), &none)
                .is_empty()
        );
        assert_eq!(queue.next_due(start, &none), Some(backoff));
        assert_eq!(keys(&queue.take_due(backoff, &none)[0]), ["a"]);
        queue.finished("gc", backoff);

        // 戻した分は数えていないので、あと 2 回試せる。
        let mut at = backoff;
        for _ in 1..IMAGE_ATTEMPTS {
            at += GATHER + IMAGE_INTERVAL;
            assert!(queue.request("gc", "a", None, at));
            assert_eq!(queue.take_due(at + GATHER, &none).len(), 1);
            queue.finished("gc", at + GATHER);
        }
        assert!(!queue.request("gc", "a", None, at + IMAGE_INTERVAL));
    }

    #[test]
    fn retries_later_within_limits_gives_up_and_pauses_sources() {
        let start = Instant::now();
        let none = HashSet::new();
        let mut queue = RecoveryQueue::default();
        assert!(queue.request("gc", "a", None, start));
        assert!(queue.request("gc", "b", None, start));
        assert!(queue.request("gc", "c", None, start));
        assert_eq!(queue.take_due(start + GATHER, &none).len(), 1);
        let done = start + Duration::from_secs(30);
        queue.finished("gc", done);

        // 取得に失敗した (時間をおけば取れそう)。前の試みから 30 分あけて、もう一度試す。配信元が指定した
        // 待ち時間 (Retry-After) がそれより長ければ、それまで待つ。
        assert_eq!(
            queue.retry_later("gc", &["a".into(), "b".into()], done, None),
            2
        );
        assert_eq!(
            queue.next_due(done, &none),
            Some(start + GATHER + IMAGE_INTERVAL)
        );
        let later = done + IMAGE_INTERVAL * 2;
        assert_eq!(queue.retry_later("gc", &["a".into()], done, Some(later)), 1);
        assert_eq!(queue.next_due(done, &none), Some(later));
        let retried = queue.take_due(later, &none).remove(0);
        assert_eq!(retried, batch("gc", &["a", "b"]));

        // 新しい取得 URL でも取れなかった・見つからなかった画像は、起動しているあいだは諦める。
        queue.give_up("gc", "c");
        assert!(!queue.request("gc", "c", None, later + IMAGE_INTERVAL * 2));
        assert_eq!(queue.retry_later("gc", &["c".into()], later, None), 0);

        // 新しい取得 URL でも取れない画像が重なったら、ソースを休ませ、待っている頼みも捨てる。
        let after = later + Duration::from_secs(5);
        queue.finished("gc", after);
        assert!(queue.request("gc", "d", None, after));
        queue.pause("gc", after + Duration::from_secs(3600));
        assert_eq!(queue.next_due(after, &none), None);
        assert!(!queue.request("gc", "e", None, after + Duration::from_secs(60)));
        // ほかのソースは休ませない。
        assert!(queue.request("abyss", "x", None, after));
        // 休みが明ければ受ける。
        assert!(queue.request("gc", "e", None, after + Duration::from_secs(3600)));
    }
}
