//! 本文の画像を読み込む gpui 用の HTTP クライアント。
//!
//! gpui の `img` (TextView の `<img>` を含む) は URL の画像を App の HTTP クライアントで取りに行き、
//! 結果を (失敗も) App のアセットとして覚え続ける。ここでディスクキャッシュを先に見て、なければ取得して
//! キャッシュに入れる。
//!
//! アプリの中だけの URL (`.invalid` のホスト) は外へは取りに行かない。
//! - 画像の参照 (`https://source-image.invalid/<ソース ID>/<鍵>`。取得の定義の `url_identity.images`
//!   に合った画像) は、キャッシュになければ最後に見た配信元の URL (署名付き) をストアで引いて取り、
//!   参照の鍵でキャッシュに入れる。期限切れのことがある応答 (401・403・404・410) なら、ソースの取り直しを
//!   頼む ([`ReferenceImages::request_recovery`])。
//! - 旧形式から取り込んだ記事の画像 (`https://legacy-image.invalid/...`) は、キャッシュにあるものだけ。
//!
//! キャッシュの画像は、書いたときの長さと SHA-256 で照らしてから返す (`ImageCache::read_checked`)。
//! - 照らして合わない (書いたあとで壊れた) 画像は返さずに捨て、キャッシュにないときと同じ道で取り直す。
//!   旧形式の画像は取り直せないので、捨てずに誤りにする。
//! - 照らすものがない画像 (これを入れる前の版で書いたもの。一時ファイルの名前が PID だけだった頃に、先読みと
//!   画面が同時に書いて混ざったものもありうる) は返すが、gpui が読めなければ ([`WatchedImages`] が描画の中で
//!   見つけて [`CachingHttpClient::decode_failed`] を呼ぶ)、まだそのまま入っていれば捨てて読み直させ、取り
//!   直す。鍵ごとに起動中 1 回まで。照らして合ったのに読めない画像は、配信元のバイト列が読めないので取り直さない。
//!
//! gpui が覚えている失敗は、次のときに消して読み直させる (表示されている画像は次の描画で取り直す)。
//! - 先読み・取り直しで画像がキャッシュに入った ([`CachingHttpClient::reload_cached`])。確かめ直しで中身が
//!   変わった画像は、表示できていても読み直す。
//! - 一時的に取れなかった (通信の失敗・5xx・429) 画像の、読み直す時刻になった ([`RETRY_DELAYS`])。
//! - 記事を開き直した ([`CachingHttpClient::retry_failed`]。読み込めてもデコードに失敗した画像も)。
//! - gpui が読めなかったキャッシュの画像を捨てた ([`CachingHttpClient::reload_discarded`])。
//!
//! gpui はアセットを、画面に渡した URI の文字列で覚える。このクライアントが受け取る URI は HTTP の
//! リクエストの形で、フラグメントが落ちる。食い違わないよう、画面には [`display_uri`]・[`display_html`] で
//! フラグメントを外した URI を渡す (画像の取得にはフラグメントを使わない)。
//!
//! 終わるとき (サービスの止める合図) は、キャッシュにない画像を取りに行かず (参照の取得 URL も
//! ストアで引かない)、取得中の通信も取り消す。取り直しも自動の読み直しも頼まない。

use std::borrow::Cow;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use futures::future::BoxFuture;
use gnn_fetch::ImageCache;
use gnn_fetch::image_cache::{CacheRead, CacheState, Discard, ImageFetchError};
use gpui_kit::http_client::{self, AsyncBody, HttpClient, Response, Url, http};
use gpui_kit::{
    App, Global, ImageCacheError, ImgResourceLoader, RenderImage, Resource, SharedUri, Window,
};
use parking_lot::Mutex;
use regex::Regex;
use tokio_util::sync::CancellationToken;

use crate::services::{LEGACY_IMAGE_HOST, ServiceEvent};

/// 一時的に取れなかった画像を、自動で読み直すまでの時間 (1 回目・2 回目)。それより後は、記事を開き直したとき。
pub const RETRY_DELAYS: [Duration; 2] = [Duration::from_secs(30), Duration::from_secs(120)];
/// 配信元が指定した待ち時間 (429 の Retry-After) がこれより長ければ、自動では読み直さない
/// (指定より早くは取りに行かない。記事を開き直したときに読み直す)。
const RETRY_AFTER_MAX: Duration = Duration::from_secs(600);

/// 画像の参照の取得 URL を引き、取れなかったときにソースの取り直しを頼む先 (アプリでは Services)。
pub trait ReferenceImages: Send + Sync + 'static {
    /// 参照 (ソース ID, 鍵) を取りに行く配信元の URL。ストアを読むので、画面のスレッドでは呼ばない
    /// (このクライアントは `spawn_blocking` の中で呼ぶ)。
    fn fetch_url(&self, source_id: &str, key: &str) -> Option<String>;
    /// 記録した取得 URL (`failed_url`) で取れなかった (署名の期限が切れたかもしれない) 参照の取り直しを頼む。
    /// 待たない。
    fn request_recovery(&self, source_id: &str, key: &str, failed_url: Option<&str>);
}

pub struct CachingHttpClient {
    client: reqwest::Client,
    runtime: tokio::runtime::Handle,
    images: ImageCache,
    references: Arc<dyn ReferenceImages>,
    loads: Arc<Mutex<Loads>>,
    /// 一時的に取れなかった画像を読み直す時刻になったら、画面に知らせる ([`ServiceEvent::RetryImages`])。
    events: flume::Sender<ServiceEvent>,
    /// 手元のネットワークへ取りに行かない。テストで手元のサーバーを使うときだけ外す。
    public_only: bool,
    /// サービスの止める合図 (`Services::stop_signal`)。
    stop: CancellationToken,
}

/// 画面の部品から、このクライアントを使う (記事を開き直したときの読み直し)。
pub struct ImageLoader(pub Arc<CachingHttpClient>);

impl Global for ImageLoader {}

/// 画面に渡す画像の URI。フラグメントを外す (このクライアントが受け取る URI とそろえ、gpui が覚えている
/// 画像を読み直せるようにする)。
pub fn display_uri(url: &str) -> Cow<'_, str> {
    match url.split_once('#') {
        Some((uri, _)) if url.starts_with("http://") || url.starts_with("https://") => {
            Cow::Owned(uri.to_string())
        }
        _ => Cow::Borrowed(url),
    }
}

/// 画面に渡す本文の HTML。`<img src>` のフラグメントを外す ([`display_uri`])。
pub fn display_html(html: &str) -> Cow<'_, str> {
    static IMG_SRC: LazyLock<Regex> = LazyLock::new(|| {
        Regex::new(r##"(<img\b[^>]*?\bsrc=")(https?://[^"#]*)#[^"]*(")"##).expect("regex")
    });
    IMG_SRC.replace_all(html, "$1$2$3")
}

impl CachingHttpClient {
    pub fn new(
        client: reqwest::Client,
        runtime: tokio::runtime::Handle,
        images: ImageCache,
        references: Arc<dyn ReferenceImages>,
        events: flume::Sender<ServiceEvent>,
        stop: CancellationToken,
    ) -> Arc<Self> {
        Arc::new(Self {
            client,
            runtime,
            images,
            references,
            loads: Arc::default(),
            events,
            public_only: true,
            stop,
        })
    }

    /// 画像がキャッシュに入った (`available`) か、入れ直した (`changed`)。gpui が覚えている失敗・読み込み中の
    /// 画像を読み直させる。入れ直した画像は、読み込めていても (テクスチャを外して) 読み直させる。
    pub fn reload_cached(&self, available: &[String], changed: &[String], cx: &mut App) {
        let candidates = self.loads.lock().requested_for(available, changed);
        self.reload(candidates, cx);
    }

    /// 一時的に取れなかった画像の、読み直す時刻になった (値は URI と予約の番号)。開き直したあとの古い予約は
    /// 使わない。
    pub fn retry(&self, due: &[(String, u64)], cx: &mut App) {
        let due: Vec<(String, Reload)> = {
            let mut loads = self.loads.lock();
            due.iter()
                .filter(|(uri, token)| loads.take_retry(uri, *token))
                .map(|(uri, _)| (uri.clone(), Reload::IfFailed))
                .collect()
        };
        self.reload(due, cx);
    }

    /// 記事を開き直した。その記事の画像 (`uris`) のうち、gpui が失敗を覚えているもの (取れなかった・デコード
    /// できなかった) を読み直させる。自動で読み直す回数も戻す。
    pub fn retry_failed(&self, uris: &[String], cx: &mut App) {
        let requested: Vec<(String, Reload)> = self
            .loads
            .lock()
            .reopen(uris)
            .into_iter()
            .map(|uri| (uri, Reload::IfFailed))
            .collect();
        self.reload(requested, cx);
    }

    /// gpui が `uri` の画像を読めなかった ([`WatchedImages`] が描画の中で呼ぶので、ロックと表を見るだけで、
    /// ディスクは触らない)。
    ///
    /// 今の読み込みで返したのが確かめていないキャッシュの画像なら、そのバイト列がまだ入っているうちに捨て
    /// (`ImageCache::discard_unverified`)、画面に読み直させる ([`ServiceEvent::ImagesDiscarded`])。キャッシュに
    /// ないので、普通の URL はその URL で、参照は記録した取得 URL で取り直す (期限切れなら既存の取り直し)。
    /// 書いたときのままと照らせた画像 (取ってきたばかりのものを含む。配信元のバイト列が読めない)・旧形式の画像
    /// (取り直せない)・この起動で一度取り直した鍵・取れずに返していない画像は何もしない。止める合図のあとも
    /// 何もしない。
    pub fn decode_failed(&self, uri: &str) {
        if self.stop.is_cancelled() {
            return;
        }
        let Some(repair) = self.loads.lock().decode_failed(uri) else {
            return;
        };
        let images = self.images.clone();
        let events = self.events.clone();
        // 画像キャッシュの掃除なので、終わるときに待たない (切れても、壊れた画像がもう 1 回残るだけ)。
        self.runtime.spawn_blocking(move || {
            match images.discard_unverified(&repair.key, repair.generation, &repair.sha256) {
                // 先読みが同じバイト列を配信元と同じと確かめていた。取り直しても同じなので読み直さない。
                Ok(Discard::Verified) => {}
                Ok(discard) => {
                    tracing::info!(
                        "表示できなかったキャッシュの画像を取り直します ({discard:?}): {}",
                        repair.key
                    );
                    let _ = events.send(ServiceEvent::ImagesDiscarded(vec![repair.key]));
                }
                Err(err) => tracing::warn!(
                    "表示できなかったキャッシュの画像を消せません: {}: {err:#}",
                    repair.key
                ),
            }
        });
    }

    /// gpui が読めなかったキャッシュの画像を捨てた (値はキャッシュの鍵)。gpui が覚えている失敗を消して読み直させる。
    pub fn reload_discarded(&self, keys: &[String], cx: &mut App) {
        let candidates = self.loads.lock().discarded(keys);
        self.reload(candidates, cx);
    }

    fn reload(&self, candidates: Vec<(String, Reload)>, cx: &mut App) {
        let removed: Vec<String> = candidates
            .into_iter()
            .filter(|(uri, reload)| forget_asset(uri, *reload, cx))
            .map(|(uri, _)| uri)
            .collect();
        if !removed.is_empty() {
            self.loads.lock().forget(&removed);
            cx.refresh_windows();
        }
    }
}

/// gpui の画像の読み込みを見て、読めなかった画像を HTTP クライアントに知らせる。ウィンドウの根に
/// `image_cache(..)` で置くと、その下の `img` (本文の TextView・リリースのサムネイル・アイコン) の読み込みが通る。
/// 読み込みと結果の覚え方は gpui の既定のまま (`ImgResourceLoader` のアセット。`remove_asset` で消せる)。
pub struct WatchedImages(pub Arc<CachingHttpClient>);

impl gpui_kit::ImageCache for WatchedImages {
    fn load(
        &mut self,
        resource: &Resource,
        window: &mut Window,
        cx: &mut App,
    ) -> Option<Result<Arc<RenderImage>, ImageCacheError>> {
        let result = window.use_asset::<ImgResourceLoader>(resource, cx);
        if let (Some(Err(_)), Resource::Uri(uri)) = (&result, resource) {
            self.0.decode_failed(uri.as_ref());
        }
        result
    }
}

/// gpui が覚えている画像を、どの状態なら消すか。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reload {
    /// 失敗したもの (取れなかった・デコードできなかった) だけ。
    IfFailed,
    /// 失敗したものと、読み込み中のもの (キャッシュに入る前に取りに行ったもの)。
    UnlessLoaded,
    /// 読み込めたものも (テクスチャを外して)。中身が変わった画像。
    Always,
}

/// gpui が覚えている `uri` の画像を消す。消したら `true`。`uri` は gpui が求めた (覚えている) ものに限る
/// (`fetch_asset` は、覚えていない画像なら読み込みを始めてしまう)。
fn forget_asset(uri: &str, reload: Reload, cx: &mut App) -> bool {
    let resource = Resource::Uri(SharedUri::from(uri.to_string()));
    match (cx.fetch_asset::<ImgResourceLoader>(&resource), reload) {
        (Some(Err(_)), _) => {}
        (None, Reload::UnlessLoaded | Reload::Always) => {}
        (Some(Ok(image)), Reload::Always) => cx.drop_image(image, None),
        _ => return false,
    }
    cx.remove_asset::<ImgResourceLoader>(&resource);
    true
}

/// gpui に求められた画像の覚え。
#[derive(Debug, Default)]
struct Loads {
    /// gpui が求めて覚えている URI と、そのキャッシュの鍵。gpui から消したら外す。
    requested: HashMap<String, String>,
    /// 失敗を返した URI。取れたら外す (gpui から消しても、自動で読み直した回数は残す)。
    failures: HashMap<String, Failure>,
    /// 読み直しの予約の通し番号。
    next_token: u64,
    /// URI ごとの今の読み込みの番号 (gpui に求められるたびに進める)。古い読み込みが遅れて終わっても、`served`
    /// を書き換えない。gpui から消したら外す。
    attempts: HashMap<String, u64>,
    /// 読み込みの番号の通し番号。
    next_attempt: u64,
    /// 今の読み込みで gpui に返したキャッシュの画像 (gpui が読めなかったときに、キャッシュの中身が悪いのかを決める)。
    served: HashMap<String, Served>,
    /// この起動で、gpui が読めなかったので捨てて取り直した鍵 (取り直しの試みは鍵ごとに 1 回まで)。
    repaired: HashSet<String>,
    /// 壊れていると警告した旧形式の画像の URI (URI ごとに 1 回)。
    warned: HashSet<String>,
}

/// gpui に返したキャッシュの画像。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Served {
    /// キャッシュの鍵。
    key: String,
    /// 返したバイト列の、キャッシュでの世代。
    generation: u64,
    /// 返したバイト列の SHA-256。
    sha256: String,
    /// 返したバイト列を、書いたときの長さと SHA-256 で照らせた (`Intact`)。照らすものがなければ (`Unverified`) `false`。
    verified: bool,
    /// gpui が読めなかったと知らせを受けた (同じ応答で 2 度は考えない)。
    reported: bool,
}

/// gpui が読めなかったので、捨てて取り直すキャッシュの画像。
#[derive(Debug, Clone, PartialEq, Eq)]
struct Repair {
    key: String,
    generation: u64,
    sha256: String,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Failure {
    /// 一時的な失敗で、自動で読み直した (読み直しを予約した) 回数。
    retries: u32,
    /// 予約している読み直しの番号。
    scheduled: Option<u64>,
}

impl Loads {
    fn requested(&mut self, uri: &str) {
        self.requested
            .insert(uri.to_string(), ImageCache::normalize(uri));
    }

    /// gpui が `uri` を求めた。読み込みの番号を返す (前の読み込みで返した画像の覚えは外す)。
    fn begin(&mut self, uri: &str) -> u64 {
        self.requested(uri);
        self.next_attempt += 1;
        self.attempts.insert(uri.to_string(), self.next_attempt);
        self.served.remove(uri);
        self.next_attempt
    }

    /// `attempt` の読み込みで、キャッシュの画像 `served` を返した。今の読み込みでなければ (gpui から消したあと・
    /// 次の読み込みが始まったあとに遅れて終わった) 覚えない。
    fn serve(&mut self, uri: &str, attempt: u64, served: Served) {
        if self.attempts.get(uri) == Some(&attempt) {
            self.served.insert(uri.to_string(), served);
        }
    }

    /// gpui が `uri` を読めなかった。今の読み込みで返したのが確かめていないキャッシュの画像で、旧形式でなく、
    /// その鍵をこの起動でまだ取り直していなければ、捨てて取り直す画像を返す (知らせは返した画像ごとに 1 回だけ見る)。
    fn decode_failed(&mut self, uri: &str) -> Option<Repair> {
        let served = self.served.get_mut(uri)?;
        if std::mem::replace(&mut served.reported, true)
            || served.verified
            || is_legacy(&served.key)
            || !self.repaired.insert(served.key.clone())
        {
            return None;
        }
        Some(Repair {
            key: served.key.clone(),
            generation: served.generation,
            sha256: served.sha256.clone(),
        })
    }

    /// 壊れた旧形式の画像を、まだ警告していなければ `true`。
    fn warn_once(&mut self, uri: &str) -> bool {
        self.warned.insert(uri.to_string())
    }

    fn succeeded(&mut self, uri: &str) {
        self.failures.remove(uri);
    }

    /// 失敗を返した。一時的な失敗 (`transient`) なら、自動で読み直すまでの時間と予約の番号を返す
    /// ([`RETRY_DELAYS`] の回数まで。予約しているあいだは重ねない。配信元の待ち時間が長すぎれば予約しない)。
    fn failed(
        &mut self,
        uri: &str,
        transient: bool,
        retry_after: Option<Duration>,
    ) -> Option<(Duration, u64)> {
        let failure = self.failures.entry(uri.to_string()).or_default();
        if !transient || failure.scheduled.is_some() {
            return None;
        }
        let delay = *RETRY_DELAYS.get(failure.retries as usize)?;
        let delay = match retry_after {
            Some(wait) if wait > RETRY_AFTER_MAX => return None,
            Some(wait) => wait.max(delay),
            None => delay,
        };
        self.next_token += 1;
        failure.retries += 1;
        failure.scheduled = Some(self.next_token);
        Some((delay, self.next_token))
    }

    /// 予約した読み直しの時刻になった。その予約が今も生きていて、gpui が覚えている失敗を消してよければ `true`。
    fn take_retry(&mut self, uri: &str, token: u64) -> bool {
        let Some(failure) = self.failures.get_mut(uri) else {
            return false;
        };
        if failure.scheduled != Some(token) {
            return false;
        }
        failure.scheduled = None;
        self.requested.contains_key(uri)
    }

    /// 記事を開き直した。その画像のうち gpui が覚えているもの (失敗していれば読み直す)。失敗の覚えは戻す
    /// (自動で読み直す回数と、予約していた読み直し)。
    fn reopen(&mut self, uris: &[String]) -> Vec<String> {
        let mut requested = Vec::new();
        for uri in uris {
            if !self.requested.contains_key(uri) || requested.contains(uri) {
                continue;
            }
            if let Some(failure) = self.failures.get_mut(uri) {
                *failure = Failure::default();
            }
            requested.push(uri.clone());
        }
        requested
    }

    /// キャッシュに入った・入れ直した鍵の、gpui が覚えている URI。
    fn requested_for(&self, available: &[String], changed: &[String]) -> Vec<(String, Reload)> {
        let mut candidates: Vec<(String, Reload)> = Vec::new();
        for (keys, reload) in [(changed, Reload::Always), (available, Reload::UnlessLoaded)] {
            for key in keys {
                let key = ImageCache::normalize(key);
                for (uri, requested) in &self.requested {
                    if *requested == key && !candidates.iter().any(|(known, _)| known == uri) {
                        candidates.push((uri.clone(), reload));
                    }
                }
            }
        }
        candidates.sort_by(|(a, _), (b, _)| a.cmp(b));
        candidates
    }

    /// 捨てた鍵の、gpui が覚えている URI。失敗を覚えているときだけ読み直す (同じ鍵の別の URI で読み込めて
    /// いるもの・読み込み中のものは触らない)。
    fn discarded(&self, keys: &[String]) -> Vec<(String, Reload)> {
        self.requested_for(&[], keys)
            .into_iter()
            .map(|(uri, _)| (uri, Reload::IfFailed))
            .collect()
    }

    /// gpui から消した。次に求められたら覚え直す (消す前の読み込みが遅れて終わっても、返した画像を覚えない)。
    fn forget(&mut self, uris: &[String]) {
        for uri in uris {
            self.requested.remove(uri);
            self.attempts.remove(uri);
            self.served.remove(uri);
        }
    }
}

/// 旧形式から取り込んだ画像か (取り直せない)。
fn is_legacy(uri: &str) -> bool {
    Url::parse(uri)
        .ok()
        .is_some_and(|parsed| parsed.host_str() == Some(LEGACY_IMAGE_HOST))
}

/// キャッシュを照らして読んだ結果。
enum Lookup {
    /// 返せる画像 (書いたときのまま・照らすものがない) と、その覚え。
    Hit(Vec<u8>, Served),
    /// 書いたときの長さか SHA-256 に合わない。
    Corrupted,
    Miss,
}

fn lookup(images: &ImageCache, uri: &str) -> Lookup {
    let CacheRead { generation, state } = images.read_checked(uri);
    let (bytes, sha256, verified) = match state {
        CacheState::Intact { bytes, sha256, .. } => (bytes, sha256, true),
        CacheState::Unverified { bytes, sha256 } => (bytes, sha256, false),
        CacheState::Corrupted => return Lookup::Corrupted,
        CacheState::Missing => return Lookup::Miss,
    };
    let served = Served {
        key: ImageCache::normalize(uri),
        generation,
        sha256,
        verified,
        reported: false,
    };
    Lookup::Hit(bytes, served)
}

fn ok(bytes: Vec<u8>) -> Result<Response<AsyncBody>> {
    Response::builder()
        .status(200)
        .body(AsyncBody::from(bytes))
        .map_err(|err| anyhow!("応答を組み立てられません: {err}"))
}

impl HttpClient for CachingHttpClient {
    fn user_agent(&self) -> Option<&http::HeaderValue> {
        None
    }

    fn proxy(&self) -> Option<&Url> {
        None
    }

    fn send(
        &self,
        request: http::Request<AsyncBody>,
    ) -> BoxFuture<'static, Result<Response<AsyncBody>>> {
        let uri = request.uri().to_string();
        let method = request.method().clone();
        let client = self.client.clone();
        let runtime = self.runtime.clone();
        let images = self.images.clone();
        let references = self.references.clone();
        let loads = self.loads.clone();
        let events = self.events.clone();
        let public_only = self.public_only;
        let stop = self.stop.clone();
        // 結果より先に「求められた」と覚える (取り直しの知らせが失敗の応答より先に届いても読み直せるように)。
        let attempt = loads.lock().begin(&uri);

        Box::pin(async move {
            if method != http::Method::GET {
                bail!("画像の取得 (GET) だけを受け付けます: {method} {uri}");
            }
            let mut fetch_url = None;
            let result = match lookup(&images, &uri) {
                Lookup::Hit(bytes, served) => Ok((bytes, served)),
                // 旧形式の画像は取り直せないので、捨てずに残す (照らして合わないバイト列は gpui に渡さない)。
                Lookup::Corrupted if is_legacy(&uri) => {
                    if loads.lock().warn_once(&uri) {
                        tracing::warn!(
                            "キャッシュの旧形式の画像が壊れています (取り直せないので残します): {uri}"
                        );
                    }
                    Err(ImageFetchError::Refused(format!(
                        "キャッシュの画像 `{uri}` が壊れています"
                    )))
                }
                found => {
                    // 書いたあとで壊れた画像は捨てて、キャッシュにないときと同じ道で取り直す。
                    if matches!(found, Lookup::Corrupted) {
                        match images.discard_corrupted(&uri) {
                            Ok(true) => tracing::info!(
                                "壊れていたキャッシュの画像を捨てて取り直します: {uri}"
                            ),
                            // ロックの中で照らし直したら合った (読んでいるあいだに入れ直された)。
                            Ok(false) => {}
                            Err(err) => tracing::warn!(
                                "壊れていたキャッシュの画像を消せません: {uri}: {err:#}"
                            ),
                        }
                    }
                    // 終わる途中なら取りに行かない (参照の取得 URL を引くと、閉じたストアを読むことになる)。
                    if stop.is_cancelled() {
                        bail!("終了の途中のため、画像を取りに行きません");
                    }
                    let (url, fetched) = fetch(
                        &client,
                        &runtime,
                        &images,
                        &references,
                        &uri,
                        public_only,
                        &stop,
                    )
                    .await;
                    fetch_url = url;
                    // 取れた画像か、取っているあいだに先読みが入れた画像を、キャッシュから照らして返す (取れた
                    // 画像がほかの書き込みに負けて入らなかったときは、今入っているものを返す)。
                    match (lookup(&images, &uri), fetched) {
                        (Lookup::Hit(bytes, served), _) => Ok((bytes, served)),
                        (_, Ok(())) => {
                            Err(ImageFetchError::Cache(anyhow!("入れた画像を読めません")))
                        }
                        (_, Err(err)) => Err(err),
                    }
                }
            };
            match result {
                Ok((bytes, served)) => {
                    let mut loads = loads.lock();
                    loads.succeeded(&uri);
                    loads.serve(&uri, attempt, served);
                    ok(bytes)
                }
                // 終わる途中で取り消した。取り直しも読み直しも頼まない。
                Err(err) if stop.is_cancelled() => Err(anyhow!(err)),
                Err(err) => {
                    if err.may_be_expired()
                        && let Some((source_id, key)) = gnn_core::image_ref::parse_reference(&uri)
                    {
                        references.request_recovery(source_id, key, fetch_url.as_deref());
                    }
                    let retry = loads
                        .lock()
                        .failed(&uri, err.is_transient(), err.retry_after());
                    if let Some((delay, token)) = retry {
                        let uri = uri.clone();
                        runtime.spawn(async move {
                            tokio::time::sleep(delay).await;
                            let _ = events.send(ServiceEvent::RetryImages(vec![(uri, token)]));
                        });
                    }
                    Err(anyhow!(err))
                }
            }
        })
    }
}

/// キャッシュにない画像を取ってキャッシュに入れる (返すバイト列は、呼び出し側がキャッシュから照らして読む)。
/// 取りに行った URL も返す (参照なら記録した取得 URL。取りに行く前に断ったら `None`)。
async fn fetch(
    client: &reqwest::Client,
    runtime: &tokio::runtime::Handle,
    images: &ImageCache,
    references: &Arc<dyn ReferenceImages>,
    uri: &str,
    public_only: bool,
    stop: &CancellationToken,
) -> (Option<String>, Result<(), ImageFetchError>) {
    // アプリの中だけの URL は、参照から引いた配信元の URL でだけ取りに行く。
    let fetch_url = if ImageCache::is_internal(uri) {
        let Some((source_id, key)) = gnn_core::image_ref::parse_reference(uri)
            .map(|(source_id, key)| (source_id.to_string(), key.to_string()))
        else {
            return (
                None,
                Err(ImageFetchError::Refused(format!(
                    "画像 `{uri}` はキャッシュにありません"
                ))),
            );
        };
        let references = references.clone();
        let recorded = runtime
            .spawn_blocking(move || references.fetch_url(&source_id, &key))
            .await;
        match recorded {
            Ok(Some(url)) => url,
            Ok(None) => {
                return (
                    None,
                    Err(ImageFetchError::Refused(format!(
                        "画像 `{uri}` の取得 URL が記録されていません"
                    ))),
                );
            }
            Err(_) => {
                return (
                    None,
                    Err(ImageFetchError::Refused(
                        "画像の取得 URL を引けませんでした".into(),
                    )),
                );
            }
        }
    } else {
        uri.to_string()
    };

    // 本文の画像は第三者の HTML から来るので、手元のネットワークへは取りに行かない。
    // 名前のホストは名前解決のときに確かめ、IP アドレスで書かれたホストはここで確かめる。
    let checked = url::Url::parse(&fetch_url)
        .map_err(|_| ImageFetchError::Refused(format!("URL `{fetch_url}` を解釈できません")))
        .and_then(|parsed| {
            if public_only {
                gnn_core::net::check_public_url(&parsed).map_err(ImageFetchError::Refused)
            } else {
                Ok(())
            }
        });
    if let Err(err) = checked {
        return (None, Err(err));
    }
    let task = {
        let client = client.clone();
        let images = images.clone();
        let key = uri.to_string();
        let fetch_url = fetch_url.clone();
        let stop = stop.clone();
        runtime.spawn(async move {
            // 取り消すのは runtime の上で行う (待つ側を落としても、通信は止まらないため)。取り消したら
            // キャッシュにも書かない (書くのは取れたあとなので)。
            stop.run_until_cancelled(gnn_fetch::image_cache::fetch_image(
                &client,
                &images,
                &fetch_url,
                &key,
                crate::services::MAX_IMAGE_BYTES,
            ))
            .await
        })
    };
    let result = match task.await {
        Ok(Some(Ok(_))) => Ok(()),
        Ok(Some(Err(err))) => Err(err),
        Ok(None) => Err(ImageFetchError::Refused(
            "終了の途中のため、画像の取得をやめました".into(),
        )),
        Err(err) => Err(ImageFetchError::Transport(anyhow!(
            "画像の取得が中断されました: {err}"
        ))),
    };
    (Some(fetch_url), result)
}

#[allow(dead_code)]
fn _assert_http_client(client: CachingHttpClient) -> Arc<dyn http_client::HttpClient> {
    Arc::new(client)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;
    use tokio::io::{AsyncReadExt as _, AsyncWriteExt as _};

    const KEY: &str = "0123456789abcdef0123456789abcdef";
    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n";

    /// 取得 URL を引いた・取り直しを頼んだ記録を残す。
    #[derive(Default)]
    struct FakeReferences {
        asked: Mutex<Vec<String>>,
        recoveries: Mutex<Vec<String>>,
        urls: Mutex<HashMap<String, String>>,
    }

    impl ReferenceImages for FakeReferences {
        fn fetch_url(&self, source_id: &str, key: &str) -> Option<String> {
            let id = format!("{source_id}/{key}");
            self.asked.lock().push(id.clone());
            self.urls.lock().get(&id).cloned()
        }

        fn request_recovery(&self, source_id: &str, key: &str, failed_url: Option<&str>) {
            self.recoveries
                .lock()
                .push(format!("{source_id}/{key} {}", failed_url.unwrap_or("-")));
        }
    }

    struct Setup {
        _dir: tempfile::TempDir,
        runtime: tokio::runtime::Runtime,
        images: ImageCache,
        references: Arc<FakeReferences>,
        events: flume::Receiver<ServiceEvent>,
        client: Arc<CachingHttpClient>,
    }

    impl Setup {
        fn new(public_only: bool) -> Self {
            let dir = tempfile::tempdir().unwrap();
            let images = ImageCache::new(dir.path().join("images"));
            let runtime = tokio::runtime::Builder::new_multi_thread()
                .worker_threads(1)
                .enable_all()
                .build()
                .unwrap();
            let references = Arc::new(FakeReferences::default());
            let (sender, events) = flume::unbounded();
            let client = Arc::new(CachingHttpClient {
                client: reqwest::Client::new(),
                runtime: runtime.handle().clone(),
                images: images.clone(),
                references: references.clone(),
                loads: Arc::default(),
                events: sender,
                public_only,
                stop: CancellationToken::new(),
            });
            Self {
                _dir: dir,
                runtime,
                images,
                references,
                events,
                client,
            }
        }

        fn record(&self, id: &str, url: &str) {
            self.references
                .urls
                .lock()
                .insert(id.to_string(), url.to_string());
        }

        fn get(&self, uri: &str) -> Result<Vec<u8>> {
            let request = http::Request::get(uri).body(AsyncBody::empty()).unwrap();
            self.runtime.block_on(async {
                let response = self.client.send(request).await?;
                let mut body = response.into_body();
                let mut bytes = Vec::new();
                futures::AsyncReadExt::read_to_end(&mut body, &mut bytes).await?;
                Ok(bytes)
            })
        }

        /// 決まった応答を順に返す HTTP サーバー。`before` は応答を返す前に呼ぶ。
        fn serve(&self, responses: Vec<Vec<u8>>, before: impl Fn() + Send + 'static) -> String {
            let listener = self
                .runtime
                .block_on(tokio::net::TcpListener::bind("127.0.0.1:0"))
                .unwrap();
            let address = listener.local_addr().unwrap();
            self.runtime.spawn(async move {
                for response in responses {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        break;
                    };
                    let mut buffer = vec![0u8; 8192];
                    let _ = stream.read(&mut buffer).await;
                    before();
                    let _ = stream.write_all(&response).await;
                }
            });
            format!("http://{address}")
        }
    }

    fn response(status: &str, headers: &[&str], body: &[u8]) -> Vec<u8> {
        let mut text = format!("HTTP/1.1 {status}\r\nConnection: close\r\n");
        for header in headers {
            text.push_str(header);
            text.push_str("\r\n");
        }
        text.push_str(&format!("Content-Length: {}\r\n\r\n", body.len()));
        let mut bytes = text.into_bytes();
        bytes.extend_from_slice(body);
        bytes
    }

    fn failure(setup: &Setup, uri: &str) -> Option<Failure> {
        setup.client.loads.lock().failures.get(uri).copied()
    }

    #[test]
    fn internal_images_come_from_the_cache_or_the_recorded_url_only() {
        let setup = Setup::new(true);
        // 記録された取得 URL が手元のネットワークを指していても、取りに行かない。
        setup.record(&format!("local/{KEY}"), "http://127.0.0.1:9/a.png");

        // キャッシュにある参照は、そのまま返す (取得 URL も引かない)。
        let cached = format!("https://source-image.invalid/gc/{KEY}");
        setup.images.write(&cached, b"png").unwrap();
        assert_eq!(setup.get(&cached).unwrap(), b"png");
        assert!(setup.references.asked.lock().is_empty());

        // 取得 URL の記録がない参照は、参照のまま外へ取りに行かずに失敗する。
        let missing = format!("https://source-image.invalid/other/{KEY}");
        let err = setup.get(&missing).unwrap_err();
        assert!(err.to_string().contains("記録されていません"), "{err:#}");
        assert_eq!(
            setup.references.asked.lock().as_slice(),
            [format!("other/{KEY}")]
        );

        // 引いた取得 URL にも、手元のネットワークの制限をかける。
        let local = format!("https://source-image.invalid/local/{KEY}");
        let err = setup.get(&local).unwrap_err();
        assert!(err.to_string().contains("手元のネットワーク"), "{err:#}");
        assert!(setup.images.read(&local).is_none());

        // 旧形式の画像は、キャッシュになければ失敗する (取得 URL は引かない)。
        setup.references.asked.lock().clear();
        let legacy = "https://legacy-image.invalid/0123456789abcdef".to_string();
        let err = setup.get(&legacy).unwrap_err();
        assert!(
            err.to_string().contains("キャッシュにありません"),
            "{err:#}"
        );
        assert!(setup.references.asked.lock().is_empty());
        // どれも取り直しても時間をおいても直らないので、取り直しも自動の読み直しも頼まない。
        assert!(setup.references.recoveries.lock().is_empty());
        for uri in [&missing, &local, &legacy] {
            assert_eq!(failure(&setup, uri), Some(Failure::default()), "{uri}");
        }
    }

    #[test]
    fn expired_references_ask_for_recovery_and_transient_failures_retry() {
        let setup = Setup::new(false);
        let base = setup.serve(
            vec![
                // 1: 署名の期限切れ。
                response("403 Forbidden", &[], b"expired"),
                // 2: 混んでいる。
                response("503 Service Unavailable", &[], b""),
                // 3: 取れた。
                response("200 OK", &["Content-Type: image/png", "ETag: \"v1\""], PNG),
                // 4: 普通の URL の 404 (取り直しの対象ではない)。
                response("404 Not Found", &[], b""),
            ],
            || {},
        );
        let reference = format!("https://source-image.invalid/gc/{KEY}");
        setup.record(&format!("gc/{KEY}"), &format!("{base}/a.png?s=old"));

        // 期限切れのことがある応答なら、取れなかった取得 URL を添えてソースの取り直しを頼む。時間をおいても
        // 直らないので、自動では読み直さない。
        assert!(setup.get(&reference).is_err());
        assert_eq!(
            setup.references.recoveries.lock().as_slice(),
            [format!("gc/{KEY} {base}/a.png?s=old")]
        );
        assert_eq!(failure(&setup, &reference), Some(Failure::default()));

        // 一時的な失敗は、取り直しを頼まず、30 秒後に読み直す (ここでは予約したことを見る)。
        assert!(setup.get(&reference).is_err());
        assert_eq!(setup.references.recoveries.lock().len(), 1);
        assert_eq!(
            failure(&setup, &reference),
            Some(Failure {
                retries: 1,
                scheduled: Some(1)
            })
        );

        // 取れたら参照の鍵で検証子と一緒にキャッシュに入れ、失敗の覚えを消す。
        assert_eq!(setup.get(&reference).unwrap(), PNG);
        assert_eq!(
            setup.images.validators(&reference).etag.as_deref(),
            Some("\"v1\"")
        );
        assert_eq!(failure(&setup, &reference), None);

        // 普通の URL は取り直しを頼まない。
        assert!(setup.get(&format!("{base}/plain.png")).is_err());
        assert_eq!(setup.references.recoveries.lock().len(), 1);
        assert!(setup.events.try_recv().is_err());
    }

    #[test]
    fn fetches_nothing_after_the_stop_signal() {
        let setup = Setup::new(false);
        let base = setup.serve(
            vec![response("200 OK", &["Content-Type: image/png"], PNG)],
            || {},
        );
        setup.record(&format!("gc/{KEY}"), &format!("{base}/a.png?s=1"));
        setup.client.stop.cancel();

        // キャッシュにある画像は、終わる途中でも返す。
        let cached = format!("https://source-image.invalid/cached/{KEY}");
        setup.images.write(&cached, b"png").unwrap();
        assert_eq!(setup.get(&cached).unwrap(), b"png");
        // キャッシュにない画像は取りに行かず、参照の取得 URL もストアで引かない (閉じたストアを読まない)。
        // 取り直しも自動の読み直しも頼まない。
        for uri in [
            format!("https://source-image.invalid/gc/{KEY}"),
            format!("{base}/x.png"),
        ] {
            let err = setup.get(&uri).unwrap_err();
            assert!(err.to_string().contains("終了の途中"), "{err:#}");
            assert_eq!(failure(&setup, &uri), None);
        }
        assert!(setup.references.asked.lock().is_empty());
        assert!(setup.references.recoveries.lock().is_empty());
        assert!(setup.events.try_recv().is_err());
    }

    #[test]
    fn returns_images_that_the_prefetch_cached_while_fetching() {
        let setup = Setup::new(false);
        let reference = format!("https://source-image.invalid/gc/{KEY}");
        let images = setup.images.clone();
        let key = reference.clone();
        // 古い取得 URL で取っているあいだに、取り直しの先読みが画像を入れた。
        let base = setup.serve(vec![response("403 Forbidden", &[], b"")], move || {
            images.write(&key, b"GIF89a").unwrap();
        });
        setup.record(&format!("gc/{KEY}"), &format!("{base}/a.png?s=old"));
        assert_eq!(setup.get(&reference).unwrap(), b"GIF89a");
        assert!(setup.references.recoveries.lock().is_empty());
        assert_eq!(failure(&setup, &reference), None);
    }

    #[test]
    fn schedules_retries_for_transient_failures_only_twice() {
        let mut loads = Loads::default();
        let uri = format!("https://source-image.invalid/gc/{KEY}");
        loads.requested(&uri);
        // 一時的な失敗は 30 秒後・2 分後の 2 回まで。予約しているあいだは重ねない。
        let (delay, first) = loads.failed(&uri, true, None).unwrap();
        assert_eq!(delay, RETRY_DELAYS[0]);
        assert_eq!(loads.failed(&uri, true, None), None);
        assert!(loads.take_retry(&uri, first));
        assert!(!loads.take_retry(&uri, first));
        // 429 の Retry-After は、読み直すまでの時間より長ければそれまで待つ。
        let (delay, second) = loads
            .failed(&uri, true, Some(Duration::from_secs(300)))
            .unwrap();
        assert_eq!(delay, Duration::from_secs(300));
        assert!(loads.take_retry(&uri, second));
        assert_eq!(loads.failed(&uri, true, None), None);
        // 一時的でない失敗 (403 など) は、自動では読み直さない。
        let other = "https://cdn.example.com/a.png";
        loads.requested(other);
        assert_eq!(loads.failed(other, false, None), None);
        // 配信元の待ち時間が長すぎれば、自動では読み直さない (指定より早くは取りに行かない)。
        let busy = "https://cdn.example.com/busy.png";
        loads.requested(busy);
        assert_eq!(
            loads.failed(busy, true, Some(Duration::from_secs(3600))),
            None
        );

        // 記事を開き直すと、gpui が覚えている画像 (失敗していれば読み直す) を返し、自動で読み直す回数も戻す。
        let reopened = loads.reopen(&[
            uri.clone(),
            other.to_string(),
            uri.clone(),
            "https://cdn.example.com/none.png".into(),
        ]);
        assert_eq!(reopened, vec![uri.clone(), other.to_string()]);
        let (_, third) = loads.failed(&uri, true, None).unwrap();
        // 開き直す前の予約が時刻になっても、開き直したあとの予約を消費しない。
        loads.reopen(std::slice::from_ref(&uri));
        let (_, fourth) = loads.failed(&uri, true, None).unwrap();
        assert!(!loads.take_retry(&uri, third));
        assert!(loads.take_retry(&uri, fourth));
        // gpui から消したあと (まだ求められていない) は、予約の時刻が来ても触らない。
        let (_, fifth) = loads.failed(&uri, true, None).unwrap();
        loads.forget(std::slice::from_ref(&uri));
        assert!(!loads.take_retry(&uri, fifth));
        assert!(loads.reopen(std::slice::from_ref(&uri)).is_empty());
        // 取れたら失敗の覚えを消す。
        loads.requested(&uri);
        loads.succeeded(&uri);
        assert!(!loads.failures.contains_key(&uri));
    }

    #[test]
    fn hands_fragment_free_uris_to_the_screen() {
        // このクライアントが受け取る URI (HTTP のリクエスト) にはフラグメントがないので、画面にもそろえて渡す。
        assert_eq!(
            display_uri("https://cdn.example.com/a.png#top"),
            "https://cdn.example.com/a.png"
        );
        assert!(matches!(
            display_uri("https://cdn.example.com/a.png?v=1"),
            Cow::Borrowed(_)
        ));
        assert_eq!(
            display_uri("data:image/png;base64,AAAA#x"),
            "data:image/png;base64,AAAA#x"
        );
        let request = http::Request::get("https://cdn.example.com/a.png#top")
            .body(AsyncBody::empty())
            .unwrap();
        assert_eq!(
            request.uri().to_string(),
            display_uri("https://cdn.example.com/a.png#top")
        );
        assert_eq!(
            display_html(
                r#"<p><img alt="a" src="https://cdn.example.com/a.png?x=1#top"><img src="https://source-image.invalid/gc/0123456789abcdef0123456789abcdef"><a href="https://x/#y">y</a></p>"#
            ),
            r#"<p><img alt="a" src="https://cdn.example.com/a.png?x=1"><img src="https://source-image.invalid/gc/0123456789abcdef0123456789abcdef"><a href="https://x/#y">y</a></p>"#
        );
    }

    #[test]
    fn reloads_requested_uris_of_cached_keys() {
        let mut loads = Loads::default();
        let reference = format!("https://source-image.invalid/gc/{KEY}");
        loads.requested(&reference);
        loads.requested("https://cdn.example.com/a.png#top");
        loads.requested("https://cdn.example.com/b.png");
        // 鍵 (フラグメントを外した URL・参照) が同じ URI を、入れた画像なら読み込めていないときだけ、
        // 入れ直した画像なら読み込めていても読み直す。
        assert_eq!(
            loads.requested_for(
                &[reference.clone(), "https://cdn.example.com/a.png".into()],
                &["https://cdn.example.com/b.png".into()]
            ),
            vec![
                (
                    "https://cdn.example.com/a.png#top".to_string(),
                    Reload::UnlessLoaded
                ),
                ("https://cdn.example.com/b.png".to_string(), Reload::Always),
                (reference.clone(), Reload::UnlessLoaded),
            ]
        );
        loads.forget(std::slice::from_ref(&reference));
        assert!(loads.requested_for(&[reference], &[]).is_empty());
    }

    fn served(setup: &Setup, uri: &str) -> Option<Served> {
        setup.client.loads.lock().served.get(uri).cloned()
    }

    /// キャッシュに、前の版の書き方 (`.meta` なし。書いたときの長さと SHA-256 がない) で画像を置く。
    fn put_unverified(images: &ImageCache, uri: &str, bytes: &[u8]) {
        images.write(uri, bytes).unwrap();
        std::fs::remove_file(images.path_for(uri).with_extension("meta")).unwrap();
    }

    /// 画面に読み直しを頼んだ鍵 (`ImagesDiscarded`)。待つのは知らせが届くまで (届かなければ落ちる)。
    fn discarded_keys(setup: &Setup) -> Vec<String> {
        match setup.events.recv_timeout(Duration::from_secs(10)) {
            Ok(ServiceEvent::ImagesDiscarded(keys)) => keys,
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn corrupted_images_are_discarded_and_fetched_again() {
        let setup = Setup::new(false);
        let base = setup.serve(
            vec![response(
                "200 OK",
                &["Content-Type: image/gif"],
                b"GIF89a-good",
            )],
            || {},
        );
        let uri = format!("{base}/a.gif");
        setup.images.write(&uri, b"GIF89a-good").unwrap();
        // 書いたあとで壊れた (電源が落ちて書ききれなかったなど。長さは同じで中身が違う)。
        std::fs::write(setup.images.path_for(&uri), b"GIF89a-BAD!").unwrap();
        // 壊れたバイト列は返さず、捨てて取り直す。
        assert_eq!(setup.get(&uri).unwrap(), b"GIF89a-good");
        assert!(matches!(
            setup.images.read_checked(&uri).state,
            CacheState::Intact { .. }
        ));
        assert!(served(&setup, &uri).is_some_and(|served| served.verified));
        assert!(setup.events.try_recv().is_err());
    }

    #[test]
    fn corrupted_legacy_images_are_kept_but_not_shown() {
        let setup = Setup::new(true);
        let legacy = format!("https://{LEGACY_IMAGE_HOST}/0123456789abcdef");
        setup.images.write(&legacy, b"GIF89a-old").unwrap();
        let path = setup.images.path_for(&legacy);
        std::fs::write(&path, b"GIF89a-OLD").unwrap();
        // 照らして合わないバイト列は gpui に渡さない。取り直せないので消さず、取り直しも自動の読み直しも頼まない。
        let err = setup.get(&legacy).unwrap_err();
        assert!(err.to_string().contains("壊れています"), "{err:#}");
        assert_eq!(std::fs::read(&path).unwrap(), b"GIF89a-OLD");
        assert!(setup.references.asked.lock().is_empty());
        assert_eq!(failure(&setup, &legacy), Some(Failure::default()));
        assert!(setup.events.try_recv().is_err());
    }

    #[test]
    fn unreadable_unverified_images_are_discarded_once_and_fetched_again() {
        let setup = Setup::new(false);
        let base = setup.serve(
            vec![response(
                "200 OK",
                &["Content-Type: image/gif"],
                b"GIF89a-good",
            )],
            || {},
        );
        let uri = format!("{base}/a.gif");
        // 前の版で書いた画像。先頭の印は正しいが、中身が混ざっていて gpui が読めない。
        put_unverified(&setup.images, &uri, b"GIF89a-mixed");
        assert_eq!(setup.get(&uri).unwrap(), b"GIF89a-mixed");
        assert!(served(&setup, &uri).is_some_and(|served| !served.verified));

        // gpui が読めなかった: まだそのまま入っているので捨て、画面に読み直させる。
        setup.client.decode_failed(&uri);
        assert_eq!(discarded_keys(&setup), std::slice::from_ref(&uri));
        assert!(!setup.images.contains(&uri));
        // 描画のたびに同じ知らせが届いても、同じ応答では 2 度考えない。
        setup.client.decode_failed(&uri);
        // 読み直すと、キャッシュにないので配信元から取り、書いたときのままと照らせる。
        assert_eq!(setup.get(&uri).unwrap(), b"GIF89a-good");
        assert!(served(&setup, &uri).is_some_and(|served| served.verified));
        // 取り直した画像も読めなければ、配信元のバイト列が読めないので、もう取り直さない。
        setup.client.decode_failed(&uri);
        assert!(setup.images.contains(&uri));
        assert!(setup.events.try_recv().is_err());
    }

    #[test]
    fn unreadable_images_that_vanished_are_still_reloaded() {
        let setup = Setup::new(true);
        let uri = "https://cdn.example.com/a.gif".to_string();
        put_unverified(&setup.images, &uri, b"GIF89a-mixed");
        assert_eq!(setup.get(&uri).unwrap(), b"GIF89a-mixed");
        // 捨てる前に prune で消えた。gpui は失敗を覚えたままなので、読み直させる。
        std::fs::remove_file(setup.images.path_for(&uri)).unwrap();
        setup.client.decode_failed(&uri);
        assert_eq!(discarded_keys(&setup), std::slice::from_ref(&uri));

        // 止める合図のあとは、捨ても読み直しもしない。
        let other = "https://cdn.example.com/b.gif".to_string();
        put_unverified(&setup.images, &other, b"GIF89a-mixed");
        assert_eq!(setup.get(&other).unwrap(), b"GIF89a-mixed");
        setup.client.stop.cancel();
        setup.client.decode_failed(&other);
        assert!(setup.images.contains(&other));
        assert!(served(&setup, &other).is_some_and(|served| !served.reported));
    }

    #[test]
    fn remembers_only_the_latest_load_and_repairs_each_key_once() {
        let mut loads = Loads::default();
        let uri = "https://cdn.example.com/a.gif";
        let served = |key: &str, sha256: &str, verified| Served {
            key: key.to_string(),
            generation: 1,
            sha256: sha256.to_string(),
            verified,
            reported: false,
        };
        // 古い読み込みが、次の読み込みが始まったあとに遅れて終わっても覚えない。
        let first = loads.begin(uri);
        let second = loads.begin(uri);
        loads.serve(uri, first, served(uri, "old", false));
        assert_eq!(loads.decode_failed(uri), None);
        loads.serve(uri, second, served(uri, "new", false));
        assert_eq!(
            loads.decode_failed(uri),
            Some(Repair {
                key: uri.to_string(),
                generation: 1,
                sha256: "new".to_string(),
            })
        );
        // 同じ応答の知らせは 1 回だけ見る。取り直しの試みは、鍵ごとに起動中 1 回まで。
        assert_eq!(loads.decode_failed(uri), None);
        let third = loads.begin(uri);
        loads.serve(uri, third, served(uri, "again", false));
        assert_eq!(loads.decode_failed(uri), None);

        // 書いたときのままと照らせた画像 (配信元のバイト列が読めない)・旧形式の画像 (取り直せない) は取り直さない。
        let verified = "https://cdn.example.com/b.gif";
        let attempt = loads.begin(verified);
        loads.serve(verified, attempt, served(verified, "x", true));
        assert_eq!(loads.decode_failed(verified), None);
        let legacy = format!("https://{LEGACY_IMAGE_HOST}/0123456789abcdef");
        let attempt = loads.begin(&legacy);
        loads.serve(&legacy, attempt, served(&legacy, "x", false));
        assert_eq!(loads.decode_failed(&legacy), None);

        // gpui から消したあとに終わった読み込みは覚えない。返していない (取れなかった) 画像は考えない。
        let gone = "https://cdn.example.com/c.gif";
        let attempt = loads.begin(gone);
        loads.forget(&[gone.to_string()]);
        loads.serve(gone, attempt, served(gone, "x", false));
        assert_eq!(loads.decode_failed(gone), None);
        assert_eq!(
            loads.decode_failed("https://cdn.example.com/none.gif"),
            None
        );
    }

    #[test]
    fn reloads_failed_uris_of_discarded_keys() {
        let mut loads = Loads::default();
        loads.requested("https://cdn.example.com/a.png#top");
        loads.requested("https://cdn.example.com/b.png");
        // 捨てた鍵の URI は、gpui が失敗を覚えているときだけ読み直す (読み込めているものは触らない)。
        assert_eq!(
            loads.discarded(&["https://cdn.example.com/a.png".into()]),
            vec![(
                "https://cdn.example.com/a.png#top".to_string(),
                Reload::IfFailed
            )]
        );
        assert!(
            loads
                .discarded(&["https://cdn.example.com/none.png".into()])
                .is_empty()
        );
    }
}
